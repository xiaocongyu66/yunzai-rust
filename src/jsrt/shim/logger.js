// ≈ global.logger — 日志桥（Rust logger::log）+ chalk 风格颜色链
const colorCodes = {
  red: 31, green: 32, yellow: 33, blue: 34, magenta: 35, cyan: 36, white: 37, gray: 90,
}

function makeColor(fn) {
  for (const name of Object.keys(colorCodes)) {
    fn[name] = (...args) => {
      const msg = args.map(String).join(" ")
      return `\x1b[${colorCodes[name]}m${msg}\x1b[0m`
    }
  }
  return fn
}

function fmt(args) {
  return args
    .map((a) => (typeof a === "string" ? a : JSON.stringify(a)))
    .join(" ")
}

const raw = {
  trace: (...args) => __yz_log("trace", fmt(args)),
  debug: (...args) => __yz_log("debug", fmt(args)),
  info: (...args) => __yz_log("info", fmt(args)),
  warn: (...args) => __yz_log("warn", fmt(args)),
  mark: (...args) => __yz_log("mark", fmt(args)),
  error: (...args) => __yz_log("error", fmt(args)),
  fatal: (...args) => __yz_log("fatal", fmt(args)),
}

export const logger = makeColor({ ...raw, logger: raw })
globalThis.logger = logger
