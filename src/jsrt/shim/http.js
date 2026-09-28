// ≈ axios / node-fetch shim — 经 __yz_op 同步桥（Rust 侧 HTTP）
function wrapResponse(ret) {
  if (!ret) throw new Error("请求失败")
  return {
    status: ret.status || 0,
    statusText: ret.statusText || "",
    headers: ret.headers || {},
    data: ret.data,
    ok: ret.status >= 200 && ret.status < 300,
    text: () => (typeof ret.data === "string" ? ret.data : JSON.stringify(ret.data)),
    json: () => (typeof ret.data === "string" ? JSON.parse(ret.data) : ret.data),
    arrayBuffer: () => ret.base64 ? Buffer.from(ret.base64, "base64") : ret.data,
  }
}

export const axios = {
  async get(url, config = {}) {
    return wrapResponse(__yz_op("http", { method: "GET", url: String(url), config }))
  },
  async post(url, data, config = {}) {
    return wrapResponse(__yz_op("http", { method: "POST", url: String(url), data, config }))
  },
  async put(url, data, config = {}) {
    return wrapResponse(__yz_op("http", { method: "PUT", url: String(url), data, config }))
  },
  async delete(url, config = {}) {
    return wrapResponse(__yz_op("http", { method: "DELETE", url: String(url), config }))
  },
  async request(config) {
    return wrapResponse(
      __yz_op("http", {
        method: (config.method || "GET").toUpperCase(),
        url: String(config.url || ""),
        data: config.data,
        config,
      }),
    )
  },
  getUri: (config) => config?.url || "",
  defaults: { headers: {} },
  interceptors: {
    request: { use() {} },
    response: { use() {} },
  },
}
globalThis.axios = axios
export default axios

export async function fetch(url, opts = {}) {
  const ret = __yz_op("http", {
    method: (opts.method || "GET").toUpperCase(),
    url: String(url),
    data: opts.body,
    config: { headers: opts.headers || {} },
  })
  return wrapResponse(ret)
}
globalThis.fetch = fetch
export default fetch

// Buffer（数据以 base64 在桥间传递）
export const Buffer = {
  from(data, enc) {
    if (enc === "base64") return { __buffer: true, base64: String(data) }
    return { __buffer: true, base64: __yz_op("buffer_from", { data: String(data) }) }
  },
  isBuffer(v) {
    return !!(v && v.__buffer)
  },
}
globalThis.Buffer = Buffer
export default Buffer
