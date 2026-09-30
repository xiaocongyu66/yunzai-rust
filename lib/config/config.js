// 生态映射：TRSS-Yunzai 的 cfg 门面（经 yz-bridge 与 Rust 主进程通信）
import { createRequire } from 'node:module'
const require = createRequire(import.meta.url)
const bridge = require("data/libnode/yz_bridge.node")
const parse = (s) => { try { return JSON.parse(s) } catch { return null } }
const cfgProxy = new Proxy({}, {
  get(_, prop) {
    if (prop === 'then') return undefined
    if (prop === 'getGroup') return async (botId, gid) => parse(bridge.op('cfg_get_group', JSON.stringify({ bot_id: botId ?? '', group_id: gid ?? '' })))
    if (prop === 'getOther') return async () => parse(bridge.op('cfg_get', JSON.stringify({ name: 'other' })))
    if (prop === 'getdefSet') return async (n) => parse(bridge.op('cfg_get_config', JSON.stringify({ name: n })))
    if (prop === 'getConfig') return async (n) => parse(bridge.op('cfg_get', JSON.stringify({ name: n })))
    return parse(bridge.op('cfg_get', JSON.stringify({ name: String(prop) })))
  },
})
export default cfgProxy
export { cfgProxy as config, cfgProxy as cfg }
