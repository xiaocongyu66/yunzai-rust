//! Tier 2 — JS 插件引擎：rquickjs(QuickJS) 后端 + Yunzai API shim
pub mod shim {
    pub const PLUGIN_BASE: &str = include_str!("shim/plugin.js");
    pub const SEGMENT: &str = include_str!("shim/segment.js");
    pub const LOGGER: &str = include_str!("shim/logger.js");
    pub const REDIS: &str = include_str!("shim/redis.js");
    pub const NODE_SHIMS: &str = include_str!("shim/node_shims.js");
    pub const HTTP: &str = include_str!("shim/http.js");
    pub const MISC: &str = include_str!("shim/misc.js");
    pub const RUNTIME: &str = include_str!("shim/runtime.js");
}

use crate::bot::Bot;
use std::sync::OnceLock;
use rquickjs::loader::{Loader, Resolver};
use rquickjs::prelude::Func;
use rquickjs::{AsyncContext, AsyncRuntime, Ctx, Module, Value};
use serde_json::{json, Value as J};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub static GLOBAL_BOT: OnceLock<Arc<Bot>> = OnceLock::new();

fn shim_source(name: &str) -> Option<&'static str> {
    match name {
        "plugin-base" => Some(shim::PLUGIN_BASE),
        "segment" => Some(shim::SEGMENT),
        "logger" => Some(shim::LOGGER),
        "redis" => Some(shim::REDIS),
        "node-shims" => Some(shim::NODE_SHIMS),
        "http" => Some(shim::HTTP),
        "misc" => Some(shim::MISC),
        _ => None,
    }
}

struct YzResolver;

impl Resolver for YzResolver {
    fn resolve<'js>(&mut self, _ctx: &Ctx<'js>, base: &str, name: &str) -> rquickjs::Result<String> {
        if name.starts_with("yunzai:") {
            return Ok(name.to_string());
        }
        if name.contains("lib/plugins/plugin.js") {
            return Ok("yunzai:plugin-base".into());
        }
        if name.contains("lib/common/common.js") {
            return Ok("yunzai:common".into());
        }
        if !name.starts_with('.') && !name.starts_with('/') {
            let bare = name.strip_prefix("node:").unwrap_or(name).split('/').next().unwrap_or(name);
            return Ok(format!("yunzai:node:{}", bare));
        }
        // 相对路径：基于导入方路径解析
        let base_dir = Path::new(base).parent().unwrap_or(Path::new("."));
        let mut p = base_dir.to_path_buf();
        for seg in name.split('/') {
            match seg {
                "." => {}
                ".." => {
                    p.pop();
                }
                s => p.push(s),
            }
        }
        if p.extension().is_none() {
            p.set_extension("js");
        }
        Ok(p.to_string_lossy().to_string())
    }
}

struct YzLoader;

impl Loader for YzLoader {
    fn load<'js>(&mut self, ctx: &Ctx<'js>, name: &str) -> rquickjs::Result<Module<'js>> {
        if let Some(rest) = name.strip_prefix("yunzai:") {
            let src = match shim_source(rest) {
                Some(s) => s,
                None => "",
            };
            return Module::declare(ctx.clone(), name, src);
        }
        let src = std::fs::read_to_string(name).map_err(rquickjs::Error::Io)?;
        Module::declare(ctx.clone(), name, src)
    }
}

thread_local! {
    static CURRENT_EVENT: std::cell::RefCell<Option<(Arc<Bot>, J)>> = const { std::cell::RefCell::new(None) };
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(h) => tokio::task::block_in_place(|| h.block_on(fut)),
        Err(_) => futures::executor::block_on(fut),
    }
}

/// 同步桥：name + args(serde Value) → serde Value
fn op_dispatch(name: &str, args: &J) -> J {
    let s = |k: &str| args.get(k).map(crate::util::string).unwrap_or_default();
    match name {
        "fs_readdir" => {
            let dir = s("path");
            let ret: Vec<J> = std::fs::read_dir(&dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .map(|e| json!({"name": e.file_name().to_string_lossy(), "isFile": e.file_type().map(|t| t.is_file()).unwrap_or(false)}))
                        .collect()
                })
                .unwrap_or_default();
            json!(ret)
        }
        "fs_exists" => json!(Path::new(&s("path")).exists()),
        "fs_stat" => match std::fs::metadata(s("path")) {
            Ok(m) => json!({
                "isDirectory": m.is_dir(), "isFile": m.is_file(),
                "size": m.len(), "mtime": m.modified().ok().map(|t| t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)),
            }),
            Err(_) => J::Null,
        },
        "fs_read" => {
            let path = s("path");
            let want_b64 = args.get("base64").and_then(J::as_bool).unwrap_or(false);
            match std::fs::read(&path) {
                Ok(bytes) => {
                    if want_b64 {
                        use base64::Engine;
                        json!(base64::engine::general_purpose::STANDARD.encode(&bytes))
                    } else {
                        match String::from_utf8(bytes) {
                            Ok(t) => json!(t),
                            Err(e) => {
                                use base64::Engine;
                                json!(base64::engine::general_purpose::STANDARD.encode(e.into_bytes()))
                            }
                        }
                    }
                }
                Err(_) => J::Null,
            }
        }
        "fs_write" => {
            let path = s("path");
            if let Some(parent) = Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            json!(std::fs::write(&path, s("data")).is_ok())
        }
        "fs_append" => {
            use std::io::Write;
            let path = s("path");
            if let Some(parent) = Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
                Ok(mut f) => json!(f.write_all(s("data").as_bytes()).is_ok()),
                Err(_) => json!(false),
            }
        }
        "fs_mkdir" => json!(std::fs::create_dir_all(s("path")).is_ok()),
        "fs_unlink" => json!(std::fs::remove_file(s("path")).is_ok()),
        "fs_rm" => {
            let p = s("path");
            json!(std::fs::remove_dir_all(&p).is_ok() || std::fs::remove_file(&p).is_ok())
        }
        "path_join" => {
            let parts: Vec<String> = args
                .get("parts")
                .and_then(J::as_array)
                .map(|a| a.iter().map(crate::util::string).collect())
                .unwrap_or_default();
            json!(parts.join("/").replace("//", "/"))
        }
        "path_dirname" => json!(Path::new(&s("path")).parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| ".".into())),
        "path_basename" => json!(Path::new(&s("path")).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()),
        "path_extname" => json!(Path::new(&s("path")).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default()),
        "path_resolve" => {
            let parts: Vec<String> = args
                .get("parts")
                .and_then(J::as_array)
                .map(|a| a.iter().map(crate::util::string).collect())
                .unwrap_or_default();
            let joined = parts.join("/");
            json!(std::fs::canonicalize(&joined).map(|p| p.to_string_lossy().to_string()).unwrap_or(joined))
        }
        "exec" => {
            let (code, stdout, stderr) = block_on(crate::util::exec(&s("cmd")));
            json!({ "code": code, "stdout": stdout, "stderr": stderr })
        }
        "redis_get" => {
            let v = block_on(async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(r) = b.redis_arc() {
                        return r.get(&s("key")).await;
                    }
                }
                None
            });
            v.map(J::String).unwrap_or(J::Null)
        }
        "redis_set" => {
            let ex = args.get("ex").and_then(J::as_i64);
            block_on(async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(r) = b.redis_arc() {
                        if let Some(secs) = ex {
                            r.set_ex(&s("key"), &s("val"), secs).await;
                        } else {
                            r.set(&s("key"), &s("val")).await;
                        }
                    }
                }
            });
            J::Null
        }
        "redis_del" => {
            block_on(async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(r) = b.redis_arc() {
                        r.del(&s("key")).await;
                    }
                }
            });
            J::Null
        }
        "redis_keys" => {
            let v = block_on(async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(r) = b.redis_arc() {
                        return r.keys(&s("pattern")).await;
                    }
                }
                vec![]
            });
            json!(v)
        }
        "redis_incrby" => {
            let n = args.get("n").and_then(J::as_i64).unwrap_or(1);
            block_on(async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(r) = b.redis_arc() {
                        r.incr_by(&s("key"), n).await;
                    }
                }
            });
            J::Null
        }
        "redis_expire" => {
            let secs = args.get("secs").and_then(J::as_i64).unwrap_or(0);
            block_on(async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(r) = b.redis_arc() {
                        r.expire(&s("key"), secs).await;
                    }
                }
            });
            J::Null
        }
        "redis_ttl" => J::from(-1),
        "yaml_parse" => serde_yaml::from_str::<J>(&s("text")).unwrap_or(J::Null),
        "yaml_stringify" => serde_yaml::to_string(args.get("obj").unwrap_or(&J::Null)).map(J::String).unwrap_or(J::Null),
        "http" => crate::jsrt::http_op(args),
        "crypto_hash" => crate::jsrt::hash_op(&s("algo"), &s("data"), args.get("enc").map(crate::util::string).unwrap_or_else(|| String::from("hex"))),
        "crypto_uuid" => json!(ulid::Ulid::new().to_string()),
        "buffer_from" => json!(s("data")),
        "e_reply" => {
            let ret = CURRENT_EVENT.with(|cur| {
                if let Some((bot, data)) = cur.borrow().as_ref() {
                    let e = crate::plugins::plugin::E::new(bot.clone(), data.clone());
                    let msg: J = serde_json::from_str(&s("msg")).unwrap_or(J::Null);
                    let opts: J = serde_json::from_str(&s("data")).unwrap_or(json!({}));
                    block_on(e.reply_with(msg, args.get("quote").and_then(J::as_bool).unwrap_or(false), opts)).ok()
                } else {
                    None
                }
            });
            ret.map(|r| r).unwrap_or(J::Null)
        }
        "recall" => {
            let ctx_v = json!({ "self_id": s("self_id"), "group_id": args.get("group_id").cloned().unwrap_or(J::Null), "user_id": args.get("user_id").cloned().unwrap_or(J::Null) });
            block_on(async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(instance) = b.get_bot(&s("self_id")) {
                        let _ = instance
                            .protocol
                            .recall_msg(&ctx_v, args.get("message_id").cloned().unwrap_or(J::Null))
                            .await;
                    }
                }
            });
            J::Null
        }
        "ctx_set" => {
            let data: J = serde_json::from_str(&s("e")).unwrap_or(J::Null);
            let time = args.get("time").and_then(J::as_u64).unwrap_or(120);
            let is_group = args.get("isGroup").and_then(J::as_bool).unwrap_or(false);
            // conKey 语义：plugin.self_id.(group|user)
            let self_id = data.get("self_id").map(crate::util::string).unwrap_or_default();
            let scope = if is_group {
                data.get("group_id").map(crate::util::string).unwrap_or_default()
            } else {
                data.get("user_id").map(crate::util::string).unwrap_or_default()
            };
            let key = format!("{}.{}.{}", s("plugin"), self_id, scope);
            let mut store = crate::plugins::plugin::CONTEXTS.write().unwrap();
            store
                .entry(key)
                .or_default()
                .insert(s("type"), crate::plugins::plugin::CtxEntry::new(data, time, s("timeout")));
            J::Null
        }
        "ctx_get" | "ctx_finish" => {
            let is_group = args.get("isGroup").and_then(J::as_bool).unwrap_or(false);
            let self_id = CURRENT_EVENT.with(|cur| {
                cur.borrow()
                    .as_ref()
                    .map(|(_, d)| d.get("self_id").map(crate::util::string).unwrap_or_default())
                    .unwrap_or_default()
            });
            let scope = if is_group {
                CURRENT_EVENT.with(|cur| {
                    cur.borrow()
                        .as_ref()
                        .map(|(_, d)| d.get("group_id").map(crate::util::string).unwrap_or_default())
                        .unwrap_or_default()
                })
            } else {
                CURRENT_EVENT.with(|cur| {
                    cur.borrow()
                        .as_ref()
                        .map(|(_, d)| d.get("user_id").map(crate::util::string).unwrap_or_default())
                        .unwrap_or_default()
                })
            };
            let key = format!("{}.{}.{}", s("plugin"), self_id, scope);
            let mut store = crate::plugins::plugin::CONTEXTS.write().unwrap();
            if name == "ctx_finish" {
                if let Some(entry) = store.get_mut(&key) {
                    entry.remove(&s("type"));
                    if entry.is_empty() {
                        store.remove(&key);
                    }
                }
                J::Null
            } else {
                match args.get("type").map(crate::util::string) {
                    Some(t) if !t.is_empty() => store.get(&key).and_then(|e| e.get(&t)).map(|c| c.data.clone()).unwrap_or(J::Null),
                    _ => store.get(&key).and_then(|e| e.values().next()).map(|c| c.data.clone()).unwrap_or(J::Null),
                }
            }
        }
        // 联系人门面 → 直接走协议 send_api
        "friend_send" | "group_send" | "friend_info" | "group_info" | "group_mute" | "group_kick"
        | "group_mute_all" | "group_quit" | "group_set_name" | "group_history" => {
            let action = match name {
                "friend_send" | "group_send" => {
                    let msg: J = serde_json::from_str(&s("msg")).unwrap_or(J::Null);
                    if name == "friend_send" {
                        json!({ "action": "send_msg", "params": { "user_id": args.get("user_id").cloned().unwrap_or(J::Null), "message": msg } })
                    } else {
                        json!({ "action": "send_msg", "params": { "group_id": args.get("group_id").cloned().unwrap_or(J::Null), "message": msg } })
                    }
                }
                "friend_info" => json!({ "action": "get_stranger_info", "params": { "user_id": args.get("user_id").cloned().unwrap_or(J::Null), "no_cache": args.get("no_cache").and_then(J::as_bool).unwrap_or(false) } }),
                "group_info" => json!({ "action": "get_group_info", "params": { "group_id": args.get("group_id").cloned().unwrap_or(J::Null), "no_cache": args.get("no_cache").and_then(J::as_bool).unwrap_or(false) } }),
                "group_mute" => json!({ "action": "set_group_ban", "params": { "group_id": args.get("group_id").cloned().unwrap_or(J::Null), "user_id": args.get("user_id").cloned().unwrap_or(J::Null), "duration": args.get("duration").and_then(J::as_i64).unwrap_or(600) } }),
                "group_kick" => json!({ "action": "set_group_kick", "params": { "group_id": args.get("group_id").cloned().unwrap_or(J::Null), "user_id": args.get("user_id").cloned().unwrap_or(J::Null) } }),
                "group_mute_all" => json!({ "action": "set_group_whole_ban", "params": { "group_id": args.get("group_id").cloned().unwrap_or(J::Null), "enable": args.get("enable").and_then(J::as_bool).unwrap_or(true) } }),
                "group_quit" => json!({ "action": "set_group_leave", "params": { "group_id": args.get("group_id").cloned().unwrap_or(J::Null) } }),
                "group_set_name" => json!({ "action": "set_group_name", "params": { "group_id": args.get("group_id").cloned().unwrap_or(J::Null), "group_name": s("name") } }),
                "group_history" => json!({ "action": "get_group_msg_history", "params": { "group_id": args.get("group_id").cloned().unwrap_or(J::Null), "message_seq": args.get("seq").cloned().unwrap_or(J::Null), "count": args.get("count").cloned().unwrap_or(J::Null) } }),
                _ => J::Null,
            };
            block_on(async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(instance) = b.get_bot(&s("self_id")) {
                        if let Ok(v) = instance
                            .protocol
                            .send_api(
                                action.get("action").and_then(J::as_str).unwrap_or(""),
                                action.get("params").cloned().unwrap_or(J::Null),
                            )
                            .await
                        {
                            return v;
                        }
                    }
                }
                J::Null
            })
        }
        "bot_stat" => json!({ "start_time": block_on(async { GLOBAL_BOT.get().map(|b| b.start_time).unwrap_or(0.0) }) }),
        "bot_get" => block_on(async {
            match s("prop").as_str() {
                "uin" => json!(GLOBAL_BOT.get().map(|b| b.uin.lock().unwrap().clone()).unwrap_or_default()),
                "bots" => json!(GLOBAL_BOT.get().map(|b| b.bots.read().unwrap().keys().cloned().collect::<Vec<_>>()).unwrap_or_default()),
                "adapter" => json!(GLOBAL_BOT.get().map(|b| b.adapters.read().unwrap().iter().map(|a| json!({"id": a.id, "name": a.name})).collect::<Vec<_>>()).unwrap_or_default()),
                _ => {
                    // Bot.<self_id> 账号门面
                    if let Some(b) = GLOBAL_BOT.get() {
                        if let Some(instance) = b.get_bot(&s("prop")) {
                            return json!({ "uin": instance.self_id, "nickname": crate::util::string(&instance.nickname()) });
                        }
                    }
                    J::Null
                }
            }
        }),
        "bot_em" => {
            let data: J = serde_json::from_str(&s("data")).unwrap_or(json!({}));
            block_on(async {
                if let Some(b) = GLOBAL_BOT.get() {
                    b.em(b, &s("name"), data).await;
                }
            });
            J::Null
        }
        "send_master_msg" => {
            let msg: J = serde_json::from_str(&s("msg")).unwrap_or(J::Null);
            block_on(async {
                if let Some(b) = GLOBAL_BOT.get() {
                    b.send_master_msg(b, msg).await;
                }
            });
            J::Null
        }
        "bot_exit" => {
            block_on(async {
                if let Some(b) = GLOBAL_BOT.get() {
                    b.exit(args.get("code").and_then(J::as_i64).unwrap_or(0) as i32).await;
                }
            });
            J::Null
        }
        "time_diff" => json!(crate::util::get_time_diff(args.get("t").and_then(J::as_u64).unwrap_or(0), crate::util::now_ms())),
        "sleep" => {
            block_on(crate::util::sleep(args.get("ms").and_then(J::as_u64).unwrap_or(0)));
            J::Null
        }
        "cfg_get" => crate::GLOBAL_CFG.get().map(|c| c.get(&s("name"))).unwrap_or(J::Null),
        "cfg_get_group" => crate::GLOBAL_CFG
            .get()
            .map(|c| c.get_group(&s("bot_id"), &s("group_id")))
            .unwrap_or(J::Null),
        "cfg_get_config" => crate::GLOBAL_CFG.get().map(|c| c.get_config(&s("name"))).unwrap_or(J::Null),
        "os_home" => json!(std::env::var("HOME").unwrap_or_default()),
        "os_platform" => json!("linux"),
        "os_arch" => json!(std::env::consts::ARCH),
        "os_cpus" => json!(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)),
        "os_totalmem" | "os_freemem" => J::from(0),
        "os_uptime" => json!(crate::util::now_ms() / 1000),
        "os_hostname" => json!("yunzai-rust"),
        "process_cwd" => json!(std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()),
        _ => J::Null,
    }
}

pub fn hash_op(algo: &str, data: &str, enc: String) -> J {
    use md5::Digest;
    let bytes = data.as_bytes();
    let hex = |out: &[u8]| -> J {
        match enc.as_str() {
            "base64" => {
                use base64::Engine;
                json!(base64::engine::general_purpose::STANDARD.encode(out))
            }
            _ => json!(out.iter().map(|b| format!("{:02x}", b)).collect::<String>()),
        }
    };
    match algo {
        "md5" => {
            let mut h = md5::Md5::new();
            h.update(bytes);
            hex(&h.finalize())
        }
        "sha256" => {
            use sha2::Sha256;
            let mut h = Sha256::new();
            h.update(bytes);
            hex(&h.finalize())
        }
        _ => J::Null,
    }
}

pub fn http_op(args: &J) -> J {
    block_on(async {
        let s = |k: &str| args.get(k).map(crate::util::string).unwrap_or_default();
        let method = s("method");
        let url = s("url");
        if url.is_empty() {
            return J::Null;
        }
        let client = match reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build() {
            Ok(c) => c,
            Err(_) => return J::Null,
        };
        let mut req = match method.as_str() {
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "DELETE" => client.delete(&url),
            _ => client.get(&url),
        };
        if let Some(J::Object(headers)) = args.get("config").and_then(|c| c.get("headers")) {
            for (k, v) in headers {
                if let (Ok(k), Ok(v)) = (
                        k.parse::<reqwest::header::HeaderName>(),
                        crate::util::string(v).parse::<reqwest::header::HeaderValue>(),
                    ) {
                    req = req.header(k, v);
                }
            }
        }
        if let Some(data) = args.get("data") {
            if !data.is_null() {
                if data.is_object() {
                    req = req.json(data);
                } else {
                    req = req.body(crate::util::string(data));
                }
            }
        }
        match req.send().await {
            Ok(resp) => {
                let status = resp.status().as_u16();
                let mut headers = json!({});
                for (k, v) in resp.headers().iter() {
                    headers[k.as_str()] = json!(v.to_str().unwrap_or(""));
                }
                let content_type = args
                    .get("config")
                    .and_then(|c| c.get("responseType"))
                    .map(crate::util::string)
                    .unwrap_or_default();
                if content_type == "arraybuffer" || content_type == "blob" {
                    match resp.bytes().await {
                        Ok(b) => {
                            use base64::Engine;
                            json!({ "status": status, "headers": headers, "data": J::Null, "base64": base64::engine::general_purpose::STANDARD.encode(&b) })
                        }
                        Err(_) => J::Null,
                    }
                } else {
                    let text = resp.text().await.unwrap_or_default();
                    let data = serde_json::from_str::<J>(&text).unwrap_or(J::String(text));
                    json!({ "status": status, "headers": headers, "data": data })
                }
            }
            Err(err) => {
                crate::util::make_log1(
                    crate::logger::Level::Debug,
                    Some("Http"),
                    format!("请求失败 {} {}", url, err),
                );
                J::Null
            }
        }
    })
}

/// JS 插件元数据
#[derive(Clone, Debug)]
pub struct JsRule {
    pub reg_src: String,
    pub rust: Option<regex::Regex>,
    pub fnc: String,
    pub log: bool,
    pub permission: crate::plugins::plugin::Permission,
    pub event: Option<String>,
}

#[derive(Clone, Debug)]
pub struct JsPluginData {
    pub reg_key: String,
    pub sub: String,
    pub name: String,
    pub dsc: String,
    pub event: String,
    pub priority: i64,
    pub rules: Vec<JsRule>,
}

pub struct JsEngine {
    rt: AsyncRuntime,
    ctx: AsyncContext,
}

impl JsEngine {
    pub async fn new() -> rquickjs::Result<JsEngine> {
        let rt = AsyncRuntime::new()?;
        let ctx = AsyncContext::full(&rt).await?;
        rt.set_loader(YzResolver, YzLoader).await;
        rt.set_max_stack_size(1024 * 1024).await;
        ctx.with(|ctx| {
            let g = ctx.globals();
            g.set(
                "__yz_raw_op",
                Func::from(|name: String, args: String| -> String {
                    let args: J = serde_json::from_str(&args).unwrap_or(json!({}));
                    serde_json::to_string(&op_dispatch(&name, &args)).unwrap_or_else(|_| "null".to_string())
                }),
            )
            .ok();
            g.set(
                "__yz_log",
                Func::from(|level: String, msg: String| {
                    crate::util::make_log1(crate::logger::Level::parse(&level), None, msg);
                }),
            )
            .ok();
            Ok::<_, rquickjs::Error>(())
        })
        .await?;
        // 预载运行时（注册 globalThis.plugin/segment/logger/redis/Bot/cfg 等）
        rquickjs::async_with!(ctx.clone() => |ctx| {
            let promise = Module::evaluate(ctx.clone(), "yunzai:runtime", shim::RUNTIME)?;
            let _: rquickjs::Value = promise.into_future().await?;
            Ok::<_, rquickjs::Error>(())
        })
        .await?;
        Ok(JsEngine { rt, ctx })
    }

    /// 加载单个插件文件 → 元数据
    pub async fn load_plugin(&self, path: &str, key: &str) -> Vec<JsPluginData> {
        let path = PathBuf::from(path).canonicalize().unwrap_or_else(|_| PathBuf::from(path));
        let path = path.to_string_lossy().to_string();
        let key = key.to_string();
        let metas: Vec<J> = rquickjs::async_with!(self.ctx.clone() => |ctx| {
                let g = ctx.globals();
                let f: rquickjs::Function<'_> = g.get("__yz_load_plugin").ok()?;
                let promise: rquickjs::Promise = f.call((path, key)).ok()?;
                let ret: String = promise.into_future().await.ok()?;
                serde_json::from_str(&ret).ok()
            })
            .await
            .unwrap_or_default();
        metas
            .into_iter()
            .filter_map(|m| {
                let rules = m
                    .get("rules")
                    .and_then(J::as_array)
                    .map(|a| a.iter().map(js_rule_from).collect())
                    .unwrap_or_default();
                Some(JsPluginData {
                    reg_key: format!("{}::{}", key, m.get("sub").map(crate::util::string).unwrap_or_default()),
                    sub: m.get("sub").map(crate::util::string).unwrap_or_default(),
                    name: m.get("name").map(crate::util::string).unwrap_or_default(),
                    dsc: m.get("dsc").map(crate::util::string).unwrap_or_default(),
                    event: m.get("event").map(crate::util::string).unwrap_or_default(),
                    priority: m.get("priority").and_then(J::as_i64).unwrap_or(5000),
                    rules,
                })
            })
            .collect()
    }

    /// 为一次消息实例化并派发：返回 handler 结果 JSON 字符串
    pub async fn instantiate(&self, reg_key: &str, e_data: &J) -> Option<J> {
        let e_json = serde_json::to_string(e_data).ok()?;
        let reg_key = reg_key.to_string();
        rquickjs::async_with!(self.ctx.clone() => |ctx| {
                let g = ctx.globals();
                let f: rquickjs::Function<'_> = g.get("__yz_instantiate").ok()?;
                let promise: rquickjs::Promise = f.call((reg_key, e_json)).ok()?;
                let ret: String = promise.into_future().await.ok()?;
                serde_json::from_str(&ret).ok()
            })
            .await
    }

    pub async fn call(&self, reg_key: &str, fnc: &str, e_data: &J) -> String {
        let e_json = serde_json::to_string(e_data).unwrap_or_default();
        let reg_key = reg_key.to_string();
        let fnc = fnc.to_string();
        rquickjs::async_with!(self.ctx.clone() => |ctx| {
                let g = ctx.globals();
                let f: rquickjs::Function<'_> = g.get("__yz_call").ok()?;
                let promise: rquickjs::Promise = f.call((reg_key, fnc)).ok()?;
                let ret: String = promise.into_future().await.unwrap_or_else(|_| "null".to_string());
                ret
            })
            .await
    }

    pub async fn accept(&self, reg_key: &str, e_data: &J) -> String {
        let e_json = serde_json::to_string(e_data).unwrap_or_default();
        let reg_key = reg_key.to_string();
        rquickjs::async_with!(self.ctx.clone() => |ctx| {
                let g = ctx.globals();
                let f: rquickjs::Function<'_> = g.get("__yz_accept").ok()?;
                let promise: rquickjs::Promise = f.call((reg_key,)).ok()?;
                let ret: String = promise.into_future().await.unwrap_or_else(|_| "null".to_string());
                ret
            })
            .await
    }

    pub async fn regex_test(&self, reg_key: &str, idx: usize, msg: &str) -> bool {
        let reg_key = reg_key.to_string();
        let msg = msg.to_string();
        rquickjs::async_with!(self.ctx.clone() => |ctx| {
                let g = ctx.globals();
                let f: rquickjs::Function<'_> = g.get("__yz_test").ok()?;
                let promise: rquickjs::Promise = f.call((reg_key, idx, msg)).ok()?;
                let r: String = promise.into_future().await.unwrap_or_else(|_| "false".to_string());
                r == "true"
            })
            .await
    }
}

fn js_rule_from(v: &J) -> JsRule {
    let reg_src = v.get("reg").map(crate::util::string).unwrap_or_default();
    let rust = regex::Regex::new(&reg_src).ok();
    JsRule {
        reg_src,
        rust,
        fnc: v.get("fnc").map(crate::util::string).unwrap_or_default(),
        log: v.get("log").and_then(J::as_bool).unwrap_or(true),
        permission: crate::plugins::plugin::Permission::parse(
            &v.get("permission").map(crate::util::string).unwrap_or_default(),
        ),
        event: {
            let e = v.get("event").map(crate::util::string).unwrap_or_default();
            if e.is_empty() { None } else { Some(e) }
        },
    }
}

/// 插件目录扫描（≈ loader.js getPlugins）
pub fn scan_plugin_files(dir: &str) -> Vec<(String, String)> {
    let mut ret = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else { return ret };
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let index = path.join("index.js");
        if index.exists() {
            ret.push((format!("{}/index.js", name), index.to_string_lossy().to_string()));
            continue;
        }
        if let Ok(apps) = std::fs::read_dir(&path) {
            for app in apps.filter_map(|e| e.ok()) {
                let ap = app.path();
                if ap.extension().map(|e| e == "js").unwrap_or(false) {
                    let fname = app.file_name().to_string_lossy().to_string();
                    ret.push((format!("{}/{}", name, fname), ap.to_string_lossy().to_string()));
                }
            }
        }
    }
    ret
}

/// 当前事件上下文的设置/清除（instantiate/call 期间）
pub struct EventGuard;
impl EventGuard {
    pub fn set(bot: Arc<Bot>, data: J) {
        CURRENT_EVENT.with(|cur| *cur.borrow_mut() = Some((bot, data)));
    }
    pub fn clear() {
        CURRENT_EVENT.with(|cur| *cur.borrow_mut() = None);
    }
}

#[allow(dead_code)]
fn _unused(_: &Mutex<HashMap<String, String>>) {}
