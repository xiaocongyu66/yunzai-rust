// ≈ Node 内置模块 shim — fs/path/util/child_process/querystring/crypto 子集
// 全部经 __yz_op 同步桥（QuickJS 单线程语义）

export const fs = {
  readdirSync(p) {
    return __yz_op("fs_readdir", { path: String(p) }) || []
  },
  readdirSyncWithTypes(p) {
    return __yz_op("fs_readdir_types", { path: String(p) }) || []
  },
  existsSync(p) {
    return !!__yz_op("fs_exists", { path: String(p) })
  },
  statSync(p) {
    return __yz_op("fs_stat", { path: String(p) })
  },
  readFileSync(p, enc) {
    return __yz_op("fs_read", { path: String(p), base64: enc !== "utf8" && enc !== "utf-8" })
  },
  writeFileSync(p, data) {
    return __yz_op("fs_write", { path: String(p), data: String(data) })
  },
  appendFileSync(p, data) {
    return __yz_op("fs_append", { path: String(p), data: String(data) })
  },
  mkdirSync(p, opts) {
    return __yz_op("fs_mkdir", { path: String(p) })
  },
  unlinkSync(p) {
    return __yz_op("fs_unlink", { path: String(p) })
  },
  rmSync(p, opts) {
    return __yz_op("fs_rm", { path: String(p) })
  },
  createReadStream(p) {
    // 简化：返回路径引用（axios/form-data 上传场景由 Rust 侧处理）
    return { path: String(p), __stream: true }
  },
  promises: {
    async readFile(p, enc) {
      return __yz_op("fs_read", { path: String(p), base64: enc && enc !== "utf8" && enc !== "utf-8" })
    },
    async writeFile(p, data) {
      return __yz_op("fs_write", { path: String(p), data: String(data) })
    },
    async readdir(p) {
      return __yz_op("fs_readdir", { path: String(p) }) || []
    },
    async stat(p) {
      return __yz_op("fs_stat", { path: String(p) })
    },
    async mkdir(p, opts) {
      return __yz_op("fs_mkdir", { path: String(p) })
    },
    async appendFile(p, data) {
      return __yz_op("fs_append", { path: String(p), data: String(data) })
    },
    async unlink(p) {
      return __yz_op("fs_unlink", { path: String(p) })
    },
    async rm(p) {
      return __yz_op("fs_rm", { path: String(p) })
    },
  },
}
globalThis.fs = fs

export const path = {
  join(...parts) {
    return __yz_op("path_join", { parts: parts.map(String) })
  },
  dirname(p) {
    return __yz_op("path_dirname", { path: String(p) })
  },
  basename(p, ext) {
    return __yz_op("path_basename", { path: String(p), ext: ext ? String(ext) : "" })
  },
  extname(p) {
    return __yz_op("path_extname", { path: String(p) })
  },
  resolve(...parts) {
    return __yz_op("path_resolve", { parts: parts.map(String) })
  },
  sep: "/",
}
globalThis.path = path

export const childProcess = {
  exec(cmd, opts, cb) {
    const ret = __yz_op("exec", { cmd: String(cmd) })
    const result = { stdout: ret?.stdout || "", stderr: ret?.stderr || "" }
    if (typeof opts === "function") opts(result)
    else if (typeof cb === "function") cb(null, result.stdout, result.stderr)
    return result
  },
  execSync(cmd) {
    return (__yz_op("exec", { cmd: String(cmd) }) || {}).stdout || ""
  },
  spawn(cmd, args) {
    __yz_op("exec", { cmd: [cmd, ...(args || [])].join(" ") })
    return { on() {}, stdout: { on() {} }, stderr: { on() {} } }
  },
}
globalThis.child_process = childProcess

export const util = {
  promisify(fn) {
    return (...args) => {
      try {
        const ret = fn(...args)
        return ret
      } catch (err) {
        return Promise.reject(err)
      }
    }
  },
  inspect(v) {
    return JSON.stringify(v, null, 2)
  },
  format(fmt, ...args) {
    let i = 0
    return String(fmt).replace(/%[sdjoO%]/g, (m) => {
      if (m === "%%") return "%"
      return i < args.length ? (typeof args[i] === "object" ? JSON.stringify(args[i++]) : String(args[i++])) : m
    })
  },
}
globalThis.util = util

export const querystring = {
  stringify(obj) {
    return Object.entries(obj || {})
      .map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(String(v))}`)
      .join("&")
  },
  parse(str) {
    const ret = {}
    for (const kv of String(str || "").split("&")) {
      const [k, v] = kv.split("=")
      if (k) ret[decodeURIComponent(k)] = decodeURIComponent(v || "")
    }
    return ret
  },
}
globalThis.querystring = querystring

export const crypto = {
  createHash(algo) {
    let data = ""
    return {
      update(d) {
        data += d
        return this
      },
      digest(enc) {
        return __yz_op("crypto_hash", { algo, data, enc: enc || "hex" }) || ""
      },
    }
  },
  randomUUID() {
    return __yz_op("crypto_uuid", {})
  },
}
globalThis.crypto = crypto
