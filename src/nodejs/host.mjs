// yunzai-rust 嵌入 Node 的插件运行时（真 Node 生态：真 fs/path/crypto/process/fetch）
// 协议（stdin/JSON 双向由 bridge tsfn 承载）：
//   Rust→JS 指令: { __id, cmd: load|instantiate|call|accept|regex_test, ... }
//   JS→Rust 应答: bridge.resolve(__id, JSON 字符串)
//   JS→Rust op:   bridge.op(name, argsJson) 同步 / await bridge.op_async(name, argsJson)
// 启动参数: node <flags> host.mjs <bridge.node 绝对路径>

import fs from 'node:fs'
import path from 'node:path'
import { Buffer } from 'node:buffer'
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)

const bridgePath = process.argv[process.argv.length - 1]
// 内存桥（/dev/fd/N memfd）必须 process.dlopen 直载：require 内部 realpath 会
// 把 fd 路径解析成 memfd 假名（/memfd:xxx (deleted)）然后 lstat ENOENT
let bridge
if (bridgePath.startsWith('/dev/fd/') || bridgePath.startsWith('/proc/self/fd/')) {
  const mod = { exports: {} }
  process.dlopen(mod, bridgePath)
  bridge = mod.exports
} else {
  bridge = require(bridgePath)
}

// 插件异步异常兜底：未捕获 rejection/异常只记日志，不杀引擎进程
process.on('unhandledRejection', (err) => log(3, `unhandledRejection: ${err?.stack ?? err}`))
process.on('uncaughtException', (err) => log(3, `uncaughtException: ${err?.stack ?? err}`))

// ============ process.exit 拦截（插件不得杀整进程） ============
const _reallyExit = process.reallyExit?.bind(process)
const _exit = process.exit.bind(process)
process.exit = (code) => { log(2, 'process.exit 被拦截（由 Rust 侧管理退出）') ; return undefined }
process.reallyExit = (code) => { log(2, 'process.reallyExit 被拦截'); return undefined }

// ============ 日志桥 ============
const LV = { 0: 'debug', 1: 'info', 2: 'warn', 3: 'error' }
function log(level, ...parts) { try { bridge.log(level, parts.map(String).join(' ')) } catch {} }
globalThis.logger = new Proxy({}, {
  get(_, prop) {
    if (prop === 'then') return undefined
    const m = { trace: 0, debug: 0, info: 1, mark: 1, warn: 2, error: 3, fatal: 3 }
    const lv = m[prop] ?? 1
    return (...parts) => {
      const msg = parts.map(p => (typeof p === 'string' ? p : utilInspect(p))).join(' ')
      log(lv, msg)
    }
  },
})
function utilInspect(v) { try { return require('node:util').inspect(v, { depth: 3 }) } catch { return String(v) } }

// ============ op 桥：同步/异步分集 ============
// 异步 op 集（需 await）：redis/http/exec/协议 API/e_reply/sleep
const ASYNC_OPS = new Set(['exec', 'redis_get', 'redis_set', 'redis_del', 'redis_keys', 'redis_incrby',
  'redis_expire', 'redis_ttl', 'http', 'e_reply', 'recall', 'friend_send', 'group_send', 'friend_info',
  'group_info', 'group_mute', 'group_kick', 'group_mute_all', 'group_quit', 'group_set_name',
  'group_history', 'bot_em', 'send_master_msg', 'bot_exit', 'sleep'])

function rawOp(name, args) {
  if (ASYNC_OPS.has(name)) return bridge.op_async(name, JSON.stringify(args ?? {}))
  return JSON.parse(bridge.op(name, JSON.stringify(args ?? {})))
}
// 统一调用点：任何 op 都可 await（同步 op 的非 Promise 值自动包装）
const op = (name, args) => Promise.resolve(rawOp(name, args))

// ============ segment（oicq 扁平消息段构造器） ============
export function makeSegment() {
  const s = (type, data = {}) => ({ type, ...data })
  return {
    custom: s, raw: (data) => s('raw', data),
    button: (...items) => s('button', { items }),
    markdown: (content) => s('markdown', { content }),
    text: (content) => s('text', { content }),
    image: (file) => s('image', { file }),
    at: (qq) => s('at', { qq }),
    record: (file) => s('record', { file }),
    video: (file) => s('video', { file }),
    file: (file) => s('file', { file }),
    reply: (id) => s('reply', { id }),
    node: (data) => s('node', { data }),
  }
}
globalThis.segment = makeSegment()

// ============ redis（node-redis v4 子集，走 op） ============
globalThis.redis = {
  // op 失败按"缓存未命中"返回 null：插件（如锅巴）在 try 外调用，抛异常会杀进程
  get: async (key) => { try { return await op('redis_get', { key }) } catch { return null } },
  set: async (key, val, opts) => op('redis_set', { key, val, ex: opts?.EX ?? (typeof opts === 'number' ? opts : undefined) }),
  setEx: async (key, secs, val) => op('redis_set', { key, val, ex: secs }),
  del: async (...keys) => { for (const k of keys) await op('redis_del', { key: k }); return null },
  keys: async (pattern) => op('redis_keys', { pattern }),
  incrBy: async (key, n) => op('redis_incrby', { key, n }),
  expire: async (key, secs) => op('redis_expire', { key, secs }),
  ttl: async () => op('redis_ttl', {}),
}

// ============ YAML / axios / ulid / schedule 桥 ============
globalThis.YAML = {
  parse: (text) => JSON.parse(bridge.op('yaml_parse', JSON.stringify({ text }))) ?? null,
  stringify: (obj) => bridge.op('yaml_stringify', JSON.stringify({ obj })) || '',
}
globalThis.axios = new Proxy({}, {
  get(_, method) {
    if (method === 'then') return undefined
    if (method === 'all') return (list) => Promise.all(list)
    return async (urlOrCfg, maybeCfg) => {
      const cfg = typeof urlOrCfg === 'object' ? urlOrCfg : { ...(maybeCfg || {}), url: urlOrCfg }
      return op('http', { method: String(method).toUpperCase(), url: cfg.url, data: cfg.data, config: { params: cfg.params, headers: cfg.headers, responseType: cfg.responseType } })
    }
  },
})
globalThis.ulid = () => bridge.op('crypto_uuid', '{}')
// node-schedule 桥：cron 统一由 Rust 调度器处理 inst.task，插件级动态注册暂为占位
globalThis.nodeSchedule = { scheduleJob: (...args) => { log(2, `nodeSchedule.scheduleJob(${args[0]}) 暂不支持，请使用插件 task 声明`); return { cancel: () => {} } } }
globalThis.moment = (d) => {
  const t = d ? new Date(d) : new Date()
  return { format: (f) => f?.replaceAll('YYYY', String(t.getFullYear())).replaceAll('MM', String(t.getMonth() + 1).padStart(2, '0')).replaceAll('DD', String(t.getDate()).padStart(2, '0')).replaceAll('HH', String(t.getHours()).padStart(2, '0')).replaceAll('mm', String(t.getMinutes()).padStart(2, '0')).replaceAll('ss', String(t.getSeconds()).padStart(2, '0')) ?? t.toISOString() }
}

// ============ Bot / cfg 门面（同进程 op 直调） ============
function contact(e, kind) {
  const ids = { self_id: e.self_id, user_id: kind === 'group' ? e.group_id : e.user_id }
  const base = {
    sendMsg: async (msg) => op(kind === 'group' ? 'group_send' : 'friend_send', { ...ids, msg: JSON.stringify(msg) }),
    recallMsg: async (mid) => op('recall', { ...ids, message_id: mid }),
    getInfo: async () => op(kind === 'group' ? 'group_info' : 'friend_info', { ...ids }),
    getAvatarUrl: () => '',
    sendFile: async (f) => base.sendMsg(segment.file(f)),
  }
  if (kind === 'group') Object.assign(base, {
    pickMember: () => ({ kick: async () => op('group_kick', { ...ids }) }),
    muteMember: async (_m, dur) => op('group_mute', { ...ids, duration: dur ?? 600 }),
    kickMember: async () => op('group_kick', { ...ids }),
    muteAll: async (en) => op('group_mute_all', { ...ids, enable: en ?? true }),
    quit: async () => op('group_quit', { ...ids }),
    setName: async (name) => op('group_set_name', { ...ids, name }),
    getChatHistory: async (seq, count) => op('group_history', { ...ids, seq, count }),
  })
  return base
}

function buildE(key, eJson) {
  const e = typeof eJson === 'string' ? JSON.parse(eJson || 'null') : eJson
  if (!e || typeof e !== 'object') return e
  e.reply = async (msg, quote, data) => op('e_reply', { key, msg: JSON.stringify(msg), quote: !!quote, data: data ? JSON.stringify(data) : '' })
  e.recall = async () => op('recall', { self_id: e.self_id, message_id: e.message_id, group_id: e.group_id ?? null, user_id: e.user_id ?? null })
  e.setContext = (type, isGroup, time = 120, timeout = '操作超时已取消') =>
    op('ctx_set', { key, plugin: e._plugin ?? '', type, isGroup: !!isGroup, time, timeout, e: JSON.stringify(e) })
  e.getContext = (type, isGroup) => op('ctx_get', { key, plugin: e._plugin ?? '', type, isGroup: !!isGroup })
  e.finishContext = (type, isGroup) => op('ctx_finish', { key, plugin: e._plugin ?? '', type, isGroup: !!isGroup })
  e.finish = async (type, isGroup) => { await e.finishContext(type, isGroup) }
  e.friend = () => contact(e, 'friend')
  e.group = () => contact(e, 'group')
  e.member = () => ({ ...contact(e, 'friend'), getInfo: async () => op('friend_info', { self_id: e.self_id, user_id: e.user_id }) })
  e.bot = globalThis.Bot
  // ≈ e.runtime.render — art-template（TRSS 同款引擎）→ 自研渲染器 op → base64 png
  e.runtime = {
    render: async (plugin, tplPath, params, opts = {}) => {
      const fs = await import('node:fs')
      const art = (await import('art-template')).default
      const base = `${process.cwd()}/plugins/${plugin}/resources/${tplPath}`
      const file = fs.existsSync(base) ? base : `${base}.html`
      const data = { ...params }
      if (typeof opts.beforeRender === 'function') {
        const extra = opts.beforeRender({ data: {
          ...data,
          pluResPath: `${process.cwd()}/plugins/${plugin}/resources/`,
        } })
        Object.assign(data, extra ?? {})
      }
      const html = art(file, data)
      // Rust 侧直接落盘返回路径（大 base64 过 bridge 曾被污染）
      // baseDir：HTML 相对资源（img/background url）的基准 = 模板目录；fontDirs：插件自带字体
      const path = await import('node:path')
      const r = await op('render', {
        html,
        width: opts.scale ? Math.round(1280 * opts.scale) : 1280,
        baseDir: path.dirname(file),
        fontDirs: [`${process.cwd()}/plugins/${plugin}/resources/common/font`],
      })
      const img = r?.file ?? null
      if (!img) {
        log(3, `[render] 渲染失败: ${r?.error ?? '无输出'}`)
        return null
      }
      log(2, `[render] 渲染成功 ${((r.size ?? 0) / 1024) | 0}kb → ${img}`)
      if (opts.retType === 'base64') {
        return fs.readFileSync(img).toString('base64')
      }
      const mid = await e.reply({ type: 'image', file: `file://${img}` })
      log(2, `[render] reply 完成: ${mid}`)
      // 延迟 5 分钟清理本图；render/ 中超过 1 小时的残留也清掉
      try {
        setTimeout(() => { try { fs.rmSync(img, { force: true }) } catch {} }, 300_000)
        const now = Date.now()
        const dir = `${process.cwd()}/data/render`
        if (fs.existsSync(dir)) {
          for (const f of fs.readdirSync(dir)) {
            const p = `${dir}/${f}`
            if (now - fs.statSync(p).mtimeMs > 3600_000) fs.rmSync(p, { force: true })
          }
        }
      } catch {}
      return mid ?? true
    },
  }
  return e
}
globalThis.__yz_buildE = buildE

globalThis.Bot = new Proxy({}, {
  get(_, prop) {
    if (prop === 'then') return undefined
    if (prop === 'uin' || prop === 'bots') return op('bot_get', { prop })
    if (prop === 'express') return __yzExpress
    if (prop === 'server') return __yzServer
    if (prop === 'adapter') {
      const ids = JSON.parse(op('bot_get', { prop: 'bots' }) || '[]')
      const first = Object.keys(ids)[0]
      return first ? [first] : []
    }
    if (prop === 'pickFriend' || prop === 'pickUser') return (id) => ({ sendMsg: async (m) => op('friend_send', { self_id: globalThis.Bot?.uin ?? '', user_id: id, msg: JSON.stringify(m) }) })
    if (prop === 'pickGroup') return (id) => ({ sendMsg: async (m) => op('group_send', { self_id: globalThis.Bot?.uin ?? '', group_id: id, msg: JSON.stringify(m) }) })
    if (prop === 'pickMember') return (gid, uid) => ({ getInfo: async () => op('friend_info', { self_id: globalThis.Bot?.uin ?? '', user_id: uid ?? gid }) })
    if (prop === 'fileToUrl') return async (data, opts = {}) => {
      // data: Buffer/base64 串；opts.name 文件名、opts.times 剩余下载次数、opts.mime
      const b64 = Buffer.isBuffer(data) ? data.toString('base64') : Buffer.isBuffer(data?.buffer) ? data.buffer.toString('base64') : String(data ?? '')
      return await op('file_to_url', { data: b64, name: opts.name ?? data?.name ?? '', times: opts.times ?? null, mime: opts.mime ?? '' })
    }
    if (prop === 'sendFriendMsg') return async (a, b) => op('friend_send', { self_id: a.self_id, user_id: a.user_id, msg: JSON.stringify(b ?? a.msg ?? '') })
    if (prop === 'sendGroupMsg') return async (a, b) => op('group_send', { self_id: a.self_id, group_id: a.group_id, msg: JSON.stringify(b ?? a.msg ?? '') })
    if (prop === 'sendMasterMsg') return async (m) => op('send_master_msg', { msg: JSON.stringify(m) })
    if (prop === 'makeForwardMsg' || prop === 'makeForwardArray') return async (data) => segment.node(data)
    if (prop === 'em') return async (name, data) => op('bot_em', { name, data: JSON.stringify(data ?? {}) })
    if (prop === 'getTimeDiff') return (t) => JSON.parse(bridge.op('time_diff', JSON.stringify({ t })))
    if (prop === 'sleep') return (ms) => new Promise((r) => setTimeout(r, ms))
    if (prop === 'fsStat') return (p) => op('fs_stat', { path: p })
    if (prop === 'mkdir') return (p) => op('fs_mkdir', { path: p })
    if (prop === 'exec') return async (cmd) => op('exec', { cmd })
    if (prop === 'String') return (s) => String(s)
    if (prop === 'exit') return async (code) => op('bot_exit', { code })
    if (prop === 'once' || prop === 'on') return (ev, fn) => { log(0, `Bot.${prop}(${ev}) 事件订阅暂不支持`) }
    return op('bot_get', { prop: String(prop) })
  },
})

globalThis.cfg = new Proxy({}, {
  get(_, prop) {
    if (prop === 'then') return undefined
    if (prop === 'getGroup') return async (botId, gid) => op('cfg_get_group', { bot_id: botId ?? '', group_id: gid ?? '' })
    if (prop === 'getOther') return async () => op('cfg_get', { name: 'other' })
    if (prop === 'getdefSet') return async (n) => op('cfg_get_config', { name: n })
    if (prop === 'getConfig') return async (n) => op('cfg_get', { name: n })
    return op('cfg_get', { name: String(prop) })
  },
})

// ============ Bot.express / Bot.server（guoba 等插件挂载面） ============
// 主端口由 Rust(axum) 持有，express 以独立端口挂载（guoba_port，默认 5099）
let __yzExpress = null, __yzServer = null
try {
  const express = (await import('express')).default
  const guobaPort = Number(cfg?.server?.guoba_port) || 5099
  __yzExpress = express()
  __yzExpress.quiet = []
  __yzExpress.skip_auth = []
  __yzServer = __yzExpress.listen(guobaPort, () => log(2, `[express] 插件服务已挂载 http://localhost:${guobaPort}`))
} catch (e) {
  log(3, `[express] 初始化失败 ${e?.message ?? e}`)
}

globalThis.plugin = class plugin {
  constructor(data = {}) {
    this.name = data.name
    this.dsc = data.dsc || ''
    this.event = data.event || 'message'
    this.priority = data.priority ?? 5000
    this.task = data.task || []
    this.rule = data.rule || []
    this.e = null
    this.self_id = null
    this.user_id = null
    this.group_id = null
  }
  async init() { return null }
  async accept() { return null }
}

// ============ registry + dispatcher ============
const registry = new Map()

async function loadPlugin(absPath, key) {
  const metas = []
  try {
    const mod = await import(`file://${absPath.startsWith('/') ? absPath : '/' + absPath}`)
    // ≈ TRSS loader 同款：module.apps 存在（miao 等聚合导出）则展开，否则用 module 本身
    const app = mod.apps ? { ...mod.apps } : mod
    for (const [name, Cls] of Object.entries(app)) {
      const isCls = typeof Cls === 'function' && Cls.prototype
      if (!isCls) continue
      let inst
      try { inst = new Cls() } catch { continue }
      let skip = false
      if (typeof inst.init === 'function') {
        try { if ((await inst.init()) === 'return') skip = true } catch (e) { log(3, `${key}.${name} init 异常: ${e?.stack || e?.message || e}`); skip = true }
      }
      if (skip) continue
      const regKey = `${key}::${name}`
      const rules = Array.isArray(inst.rule) ? inst.rule.map((r) => ({
        reg_src: String(r.reg ?? ''), fnc: String(r.fnc ?? ''), log: !!r.log,
        permission: String(r.permission ?? 'all'), event: r.event ? String(r.event) : null,
      })) : []
      const tasks = inst.task && inst.task.cron ? [{ name: String(inst.task.name ?? name), cron: String(inst.task.cron), fnc: String(inst.task.fnc ?? ''), log: !!inst.task.log }] : []
      registry.set(regKey, { cls: Cls, name, key })
      metas.push({ reg_key: regKey, sub: name, name: String(inst.name ?? name), dsc: String(inst.dsc ?? ''),
        event: String(inst.event ?? 'message'), priority: Number(inst.priority ?? 5000), rules, tasks, _plugin: String(inst.name ?? name) })
    }
  } catch (e) {
    log(3, `插件加载失败 ${key}: ${e?.stack || e?.message || e}`)
  }
  return metas
}

async function instantiate(regKey, eJson) {
  const entry = registry.get(regKey)
  if (!entry) return null
  const e = buildE(regKey, eJson)
  if (e && typeof e === 'object') e._plugin = entry.cls?.name ?? entry.name
  const inst = entry.inst ?? new entry.cls()
  inst.e = e
  if (typeof eJson === 'object' && eJson) {
    inst.self_id = eJson.self_id; inst.user_id = eJson.user_id; inst.group_id = eJson.group_id
  }
  let rules = Array.isArray(inst.rule) ? inst.rule.map((r) => ({
    reg_src: String(r.reg ?? ''), fnc: String(r.fnc ?? ''), log: !!r.log,
    permission: String(r.permission ?? 'all'), event: r.event ? String(r.event) : null,
  })) : []
  return JSON.stringify({ rules })
}

async function callMethod(regKey, fnc, eJson) {
  const entry = registry.get(regKey)
  if (!entry) return 'null'
  try {
    const e = buildE(regKey, eJson)
    const inst = entry.inst ?? new entry.cls()
    inst.e = e
    const ret = await inst[fnc](e)
    if (ret === false) return 'false'
    return String(ret ?? '')
  } catch (err) {
    log(3, `${regKey}.${fnc} 异常: ${err?.stack ?? err}`)
    return 'null'
  }
}

async function accept(regKey, eJson) {
  const entry = registry.get(regKey)
  if (!entry) return 'null'
  try {
    const e = buildE(regKey, eJson)
    const inst = new entry.cls()
    inst.e = e
    if (typeof inst.accept !== 'function') return 'null'
    const ret = await inst.accept(e) ?? 'null'
    // TRSS 语义：check/accept 可改写 e.msg（如 #刻晴 → #喵喵角色卡片），回传给宿主更新
    return JSON.stringify({ ret: String(ret ?? 'null'), msg: typeof e.msg === 'string' ? e.msg : null })
  } catch (err) {
    log(3, `${regKey}.accept 异常: ${err?.stack ?? err}`)
    return 'null'
  }
}

function regexTest(regKey, idx, msg) {
  const entry = registry.get(regKey)
  if (!entry) return false
  try {
    const inst = entry.inst ?? new entry.cls()
    const reg = inst.rule?.[idx]?.reg
    if (!reg) return false
    return reg instanceof RegExp ? reg.test(msg) : new RegExp(String(reg)).test(msg)
  } catch { return false }
}

// ============ dispatcher：Rust 指令泵入口 ============
// napi CalleeHandled 策略：JS 回调签名为 (err, value)
async function dispatcher(err, cmdJson) {
  if (err) return
  let cmd
  try { cmd = JSON.parse(cmdJson) } catch { return }
  if (!cmd || typeof cmd !== 'object') return
  const { __id: id, cmd: c } = cmd
  let result = 'null'
  try {
    switch (c) {
      case 'load': result = JSON.stringify(await loadPlugin(cmd.path, cmd.key)); break
      case 'instantiate': result = await instantiate(cmd.key, cmd.e); break
      case 'call': result = await callMethod(cmd.key, cmd.fnc, cmd.e); break
      case 'accept': result = await accept(cmd.key, cmd.e); break
      case 'regex_test': result = JSON.stringify(regexTest(cmd.key, cmd.idx, cmd.msg)); break
      case 'shutdown': result = 'ok'; break
      default: log(2, `未知指令 ${c}`)
    }
  } catch (err) {
    result = JSON.stringify({ error: String(err?.message ?? err) })
  }
  try { bridge.resolve(id, result) } catch (e) { log(3, `resolve 失败: ${e}`) }
}
globalThis.__yz_dispatcher = dispatcher

// ============ 保活 ============
setInterval(() => {}, 2 ** 31)
log(1, `host.mjs 就绪 (node ${process.version})`)
bridge.ready(dispatcher)
