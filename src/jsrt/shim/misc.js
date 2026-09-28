// ≈ yaml / node-schedule / lodash / os / process shim
export const YAML = {
  parse(text) {
    return __yz_op("yaml_parse", { text: String(text) })
  },
  stringify(obj) {
    return __yz_op("yaml_stringify", { obj }) || ""
  },
}
globalThis.YAML = YAML

export const schedule = {
  scheduleJob(cron, fn) {
    const id = __yz_op("cron_add", { cron: String(cron) })
    if (id) __yz_jobs[String(id)] = fn
    return { cancel: () => __yz_op("cron_del", { id }) }
  },
}
globalThis.__yz_jobs = {}
globalThis.nodeSchedule = schedule

export const lodash = new Proxy(
  {
    truncate(str, opts) {
      const s = String(str ?? "")
      const len = opts?.length || 30
      return s.length > len ? s.slice(0, len) + "..." : s
    },
    orderBy(arr, keys, orders) {
      const a = [...(arr || [])]
      a.sort((x, y) => {
        for (let i = 0; i < (keys || []).length; i++) {
          const k = keys[i]
          const dir = (Array.isArray(orders) ? orders[i] : orders) === "desc" ? -1 : 1
          if (x[k] < y[k]) return -1 * dir
          if (x[k] > y[k]) return 1 * dir
        }
        return 0
      })
      return a
    },
    forEach: (obj, fn) => {
      if (Array.isArray(obj)) obj.forEach(fn)
      else if (obj) Object.entries(obj).forEach(([k, v]) => fn(v, k))
      return obj
    },
  },
  {
    get(target, prop) {
      if (prop in target) return target[prop]
      // 常见函数式操作兜底：_.xxx(collection, fn)
      return (obj, fn) => {
        if (typeof fn !== "function") return obj
        if (Array.isArray(obj)) {
          if (prop === "map") return obj.map(fn)
          if (prop === "filter") return obj.filter(fn)
          if (prop === "find") return obj.find(fn)
          if (prop === "some") return obj.some(fn)
          if (prop === "every") return obj.every(fn)
          if (prop === "sortBy") return [...obj].sort((a, b) => (fn(a) < fn(b) ? -1 : 1))
        }
        if (prop === "get") return obj?.[fn]
        return obj
      }
    },
  },
)
globalThis.lodash = lodash

export const os = {
  homedir: () => __yz_op("os_home", {}),
  platform: () => __yz_op("os_platform", {}),
  arch: () => __yz_op("os_arch", {}),
  cpus: () => __yz_op("os_cpus", {}) || [],
  totalmem: () => __yz_op("os_totalmem", {}) || 0,
  freemem: () => __yz_op("os_freemem", {}) || 0,
  uptime: () => __yz_op("os_uptime", {}) || 0,
  hostname: () => __yz_op("os_hostname", {}),
}
globalThis.os = os

globalThis.process = globalThis.process || {
  cwd: () => __yz_op("process_cwd", {}),
  env: {},
  platform: "linux",
  arch: "arm64",
  argv: [],
  execve: undefined,
}
