// ≈ lib/modules/oicq/index.js — segment 消息段构造器（oicq 扁平格式）
export const segment = {
  custom(type, data) {
    return { type, ...data }
  },
  raw(data) {
    return { type: "raw", data }
  },
  button(...data) {
    return { type: "button", data }
  },
  markdown(data) {
    return { type: "markdown", data }
  },
  text(text) {
    return { type: "text", text }
  },
  image(file, name) {
    return { type: "image", file, name }
  },
  at(qq, name) {
    return { type: "at", qq, name }
  },
  record(file, name) {
    return { type: "record", file, name }
  },
  video(file, name) {
    return { type: "video", file, name }
  },
  file(file, name) {
    return { type: "file", file, name }
  },
  reply(id, text, qq, time, seq) {
    return { type: "reply", id, text, qq, time, seq }
  },
  node(data) {
    return { type: "node", data }
  },
}
globalThis.segment = segment
