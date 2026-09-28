// ≈ lib/plugins/plugin.js — 插件基类（shim 版）
// 差异：超时上下文由 Rust loader 惰性检查并回复超时提示（原版用 setTimeout 主动清理）
export default class plugin {
  constructor({
    name = "your-plugin",
    dsc = "无",
    handler,
    namespace,
    event = "message",
    priority = 5000,
    task = { name: "", fnc: "", cron: "" },
    rule = [],
  } = {}) {
    this.name = name
    this.dsc = dsc
    this.event = event
    this.priority = priority
    this.task = task
    this.rule = rule
    if (handler) {
      this.handler = handler
      this.namespace = namespace || ""
    }
  }

  reply(msg = "", quote = false, data = {}) {
    if (!this.e?.reply || !msg) return false
    return this.e.reply(msg, quote, data)
  }

  conKey(isGroup = false) {
    return `${this.name}.${this.self_id || this.e.self_id}.${
      isGroup ? this.group_id || this.e.group_id : this.user_id || this.e.user_id
    }`
  }

  setContext(type, isGroup, time = 120, timeout = "操作超时已取消") {
    const e = this.e
    if (!e) return
    e.setContext(this.name, type, isGroup ? true : false, time || 0, timeout)
  }

  getContext(type, isGroup) {
    const e = this.e
    if (!e) return undefined
    return e.getContext(this.name, type, isGroup ? true : false)
  }

  finish(type, isGroup) {
    const e = this.e
    if (!e) return
    e.finishContext(this.name, type, isGroup ? true : false)
  }

  awaitContext(...args) {
    // 简化实现：由 setContext 记录，resolveContext 由后续事件触发
    this.setContext("resolveContext", ...args)
    return undefined
  }

  resolveContext(context) {
    this.finish("resolveContext")
  }

  async renderImg(plugin, tpl, data, cfg) {
    logger.warn("renderImg 需要渲染层支持，当前版本未实现")
    return false
  }
}

globalThis.plugin = plugin
