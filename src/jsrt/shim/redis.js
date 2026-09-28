// ≈ global.redis — node-redis v4 API 子集桥（get/set/setEx/del/keys/incrBy/expire）
export const redis = {
  async get(key) {
    return __yz_op("redis_get", { key })
  },
  async set(key, val, opts) {
    const args = { key, val: String(val) }
    if (opts && typeof opts === "object") {
      if (typeof opts.EX === "number") args.ex = opts.EX
      else if (typeof opts.ex === "number") args.ex = opts.ex
    }
    return __yz_op("redis_set", args)
  },
  async setEx(key, secs, val) {
    return __yz_op("redis_set", { key, val: String(val), ex: secs })
  },
  async del(key) {
    return __yz_op("redis_del", { key })
  },
  async keys(pattern) {
    return __yz_op("redis_keys", { pattern }) || []
  },
  async incrBy(key, n) {
    return __yz_op("redis_incrby", { key, n })
  },
  async expire(key, secs) {
    return __yz_op("redis_expire", { key, secs })
  },
  async ttl(key) {
    return __yz_op("redis_ttl", { key }) || -1
  },
}
globalThis.redis = redis
