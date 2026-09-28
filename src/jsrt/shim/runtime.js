// ≈ jsrt 运行时胶水 — e 对象构建、插件实例化与方法派发
// Rust 侧只传 JSON 字符串；函数桥在 JS 侧定义、经 __yz_op 回 Rust
import 'yunzai:plugin-base'
import 'yunzai:segment'
import 'yunzai:logger'
import 'yunzai:redis'
import 'yunzai:node-shims'
import 'yunzai:http'
import 'yunzai:misc'

__yz_log("mark", "[JSRT] runtime body 开始执行")

const registry = (globalThis.__yz_registry = {})

// 异步结果中转：Rust 端「启动 → idle 推进 → 回读 __yz_result」三段式
globalThis.__yz_result = null
globalThis.__yz_run_load = async (path, key) => {
  __yz_log("mark", "[JSRT] run_load 入口 " + path)
  globalThis.__yz_result = null
  try {
    globalThis.__yz_result = await globalThis.__yz_load_plugin(path, key)
  } catch (err) {
    __yz_log("error", "[JSRT] run_load 异常 " + (err && err.stack ? err.stack : String(err)))
    globalThis.__yz_result = "[]"
  }
}
globalThis.__yz_run_call = async (key, fnc) => {
  globalThis.__yz_result = null
  globalThis.__yz_result = await globalThis.__yz_call(key, fnc)
}
globalThis.__yz_run_accept = async (key) => {
  globalThis.__yz_result = null
  globalThis.__yz_result = await globalThis.__yz_accept(key)
}
globalThis.__yz_read_result = () => String(globalThis.__yz_result ?? "null")
// 每次启动前清空，防止上一轮结果的陈旧残留
globalThis.__yz_clear_result = () => {
  globalThis.__yz_result = null
}

// __yz_op 包装：对象参数 → JSON 字符串（Rust 端 __yz_raw_op 只收字符串）
if (!globalThis.__yz_op) {
  globalThis.__yz_op = (name, args) => globalThis.__yz_raw_op(String(name), JSON.stringify(args ?? {}))
}

function mkContact(e, kind, ids) {
  const base = { ...ids }
  if (kind === 'group') {
    base.pickMember = (user_id) => mkContact(e, 'member', { ...ids, user_id })
    base.muteMember = async (user_id, duration) => __yz_op('group_mute', { ...ids, user_id, duration })
    base.kickMember = async (user_id) => __yz_op('group_kick', { ...ids, user_id })
    base.muteAll = async (enable = true) => __yz_op('group_mute_all', { ...ids, enable })
    base.quit = async () => __yz_op('group_quit', ids)
    base.setName = async (name) => __yz_op('group_set_name', { ...ids, name })
    base.getChatHistory = async (seq, count) => __yz_op('group_history', { ...ids, seq, count })
  }
  base.sendMsg = async (...args) => __yz_op(kind + '_send', { ...ids, msg: JSON.stringify(args.length === 1 ? args[0] : args) })
  base.recallMsg = async (mid) => __yz_op('recall', { ...ids, message_id: mid })
  base.getInfo = async (no_cache) => __yz_op(kind + '_info', { ...ids, no_cache })
  base.getAvatarUrl = () => (kind === 'group'
    ? `https://p.qlogo.cn/gh/${ids.group_id}/${ids.group_id}/0`
    : `https://q.qlogo.cn/g?b=qq&s=0&nk=${ids.user_id}`)
  base.sendFile = async (file, name) => base.sendMsg({ type: 'file', file, name })
  return base
}

function buildE(key, eJson) {
  const e = JSON.parse(eJson)
  e.plugin_name = key
  e.reply = async (msg = '', quote = false, data = {}) =>
    __yz_op('e_reply', { key, msg: JSON.stringify(msg), quote: !!quote, data: JSON.stringify(data || {}) })
  e.recall = async (mid) => __yz_op('recall', { self_id: e.self_id, group_id: e.group_id, user_id: e.user_id, message_id: mid })
  e.setContext = (plugin, type, isGroup, time, timeout) =>
    __yz_op('ctx_set', { plugin, type, isGroup, time, timeout, e: eJson })
  e.getContext = (plugin, type, isGroup) => __yz_op('ctx_get', { key, plugin, type, isGroup })
  e.finishContext = (plugin, type, isGroup) => __yz_op('ctx_finish', { key, plugin, type, isGroup })
  const ids = { self_id: e.self_id, group_id: e.group_id, user_id: e.user_id }
  e.friend = mkContact(e, 'friend', ids)
  e.group = mkContact(e, 'group', ids)
  e.member = e.group ? e.group.pickMember(e.user_id) : undefined
  e.bot = e.self_id ? mkContact(e, 'friend', ids) : undefined
  if (e.group_id) e.reply_to = e.group
  return e
}

globalThis.__yz_load_plugin = async (path, key) => {
  __yz_log("mark", "[JSRT] load_plugin 动态 import 前 " + path)
  const mod = await import(path)
  __yz_log("mark", "[JSRT] load_plugin import 完成，exports=" + Object.keys(mod).length)
  const metas = []
  for (const name of Object.keys(mod)) {
    const Cls = mod[name]
    if (typeof Cls !== 'function' || !Cls.prototype) continue
    let inst
    try {
      inst = new Cls()
    } catch (err) {
      logger.error(`插件实例化失败 [${key}][${name}]`, err)
      continue
    }
    if (typeof inst.init === 'function') {
      const r = await inst.init()
      if (r === 'return') continue
    }
    const regKey = key + '::' + name
    registry[regKey] = { cls: Cls, instance: inst }
    const rules = []
    if (Array.isArray(inst.rule)) {
      for (const v of inst.rule) {
        rules.push({
          reg: String(v.reg),
          fnc: String(v.fnc || ''),
          log: v.log !== false,
          permission: String(v.permission || 'all'),
          event: String(v.event || ''),
        })
      }
    }
    metas.push({
      sub: name,
      name: String(inst.name || name),
      dsc: String(inst.dsc || ''),
      event: String(inst.event || 'message'),
      priority: Number(inst.priority) || 5000,
      rules,
    })
  }
  return JSON.stringify(metas)
}

globalThis.__yz_instantiate = (key, eJson) => {
  const entry = registry[key]
  if (!entry) return 'null'
  const e = buildE(key, eJson)
  const inst = new entry.cls()
  inst.e = e
  inst.self_id = e.self_id
  inst.user_id = e.user_id
  inst.group_id = e.group_id
  entry.instance = inst
  const rules = []
  if (Array.isArray(inst.rule)) {
    for (const v of inst.rule) {
      rules.push({
        reg: String(v.reg),
        fnc: String(v.fnc || ''),
        log: v.log !== false,
        permission: String(v.permission || 'all'),
        event: String(v.event || ''),
      })
    }
  }
  return JSON.stringify({
    name: String(inst.name || key),
    dsc: String(inst.dsc || ''),
    event: String(inst.event || 'message'),
    priority: Number(inst.priority) || 5000,
    rules,
  })
}

globalThis.__yz_call = async (key, fnc) => {
  const entry = registry[key]
  if (!entry || !entry.instance || typeof entry.instance[fnc] !== 'function') return 'false'
  try {
    const r = await entry.instance[fnc](entry.instance.e)
    return r === undefined ? 'null' : JSON.stringify(r === false ? false : r)
  } catch (err) {
    logger.error(`插件处理错误 [${key}][${fnc}]`, err)
    return 'null'
  }
}

globalThis.__yz_accept = async (key) => {
  const entry = registry[key]
  if (!entry || !entry.instance || typeof entry.instance.accept !== 'function') return 'null'
  const r = await entry.instance.accept(entry.instance.e)
  return r === undefined ? 'null' : String(r)
}

globalThis.__yz_test = (key, idx, msg) => {
  const entry = registry[key]
  if (!entry || !entry.instance || !Array.isArray(entry.instance.rule)) return 'false'
  const r = entry.instance.rule[idx]
  if (!r) return 'false'
  return String(r.reg.test(msg))
}

// ≈ global.Bot — 常用门面桥
globalThis.Bot = new Proxy(
  {
    fl: {},
    gl: {},
    stat: __yz_op('bot_stat', {}) || {},
    pickFriend: (user_id) => mkContact({}, 'friend', { user_id: String(user_id) }),
    pickUser: globalThis.Bot?.pickFriend,
    pickGroup: (group_id) => mkContact({}, 'group', { group_id: String(group_id) }),
    pickMember: (group_id, user_id) => mkContact({}, 'member', { group_id: String(group_id), user_id: String(user_id) }),
    sendFriendMsg: async (bot_id, user_id, msg) => __yz_op('friend_send', { self_id: String(bot_id), user_id: String(user_id), msg: JSON.stringify(msg) }),
    sendGroupMsg: async (bot_id, group_id, msg) => __yz_op('group_send', { self_id: String(bot_id), group_id: String(group_id), msg: JSON.stringify(msg) }),
    sendMasterMsg: async (msg) => __yz_op('send_master_msg', { msg: JSON.stringify(msg) }),
    makeForwardMsg: (msg) => ({ type: 'node', data: msg }),
    makeForwardArray: (msg = [], node = {}) => ({ type: 'node', data: (Array.isArray(msg) ? msg : [msg]).map((m) => ({ ...node, message: m })) }),
    em: async (name, data) => __yz_op('bot_em', { name: String(name), data: JSON.stringify(data || {}) }),
    getTimeDiff: (t) => __yz_op('time_diff', { t }) || '0秒',
    sleep: async (ms) => __yz_op('sleep', { ms }),
    fsStat: async (p) => __yz_op('fs_stat', { path: String(p) }),
    mkdir: async (p) => __yz_op('fs_mkdir', { path: String(p) }),
    exec: async (cmd) => __yz_op('exec', { cmd: String(cmd) }) || {},
    String: (v) => (typeof v === 'string' ? v : JSON.stringify(v)),
    exit: async (code = 0) => __yz_op('bot_exit', { code }),
  },
  {
    get(target, prop) {
      if (prop in target) return target[prop]
      if (typeof prop === 'string') {
        // Bot.<self_id> 账号门面 / Bot.uin / Bot.adapter
        const v = __yz_op('bot_get', { prop: String(prop) })
        return v === null || v === undefined ? undefined : JSON.parse(v)
      }
      return undefined
    },
  },
)

// ≈ global.cfg
globalThis.cfg = new Proxy(
  {
    getGroup: (bot_id = '', group_id = '') => __yz_op('cfg_get_group', { bot_id: String(bot_id), group_id: String(group_id) }) || {},
    getOther: () => __yz_op('cfg_get', { name: 'other' }) || {},
    getdefSet: (name) => __yz_op('cfg_get', { name: String(name) }) || {},
    getConfig: (name) => __yz_op('cfg_get_config', { name: String(name) }) || {},
  },
  {
    get(target, prop) {
      if (prop in target) return target[prop]
      if (typeof prop === 'string') return __yz_op('cfg_get', { name: prop }) || {}
      return undefined
    },
  },
)
