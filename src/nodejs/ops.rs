//! op 语义平移（自 jsrt/op_dispatch 拆分）
//! 同步集：node 线程直调（纯内存，无 I/O）
//! 异步集：主 tokio 运行时执行（redis/http/exec/协议 API）

use serde_json::{json, Value as J};
use std::path::Path;

pub static GLOBAL_BOT: std::sync::OnceLock<ArcBot> = std::sync::OnceLock::new();
pub type ArcBot = std::sync::Arc<crate::bot::Bot>;

/// 当前派发事件上下文：reg_key → (bot, e_data)（同进程直接访问）
static CURRENT_EVENTS: once_cell::sync::Lazy<Mutex<HashMap<String, (ArcBot, J)>>> =
    once_cell::sync::Lazy::new(|| Mutex::new(HashMap::new()));

pub struct EventGuard;
impl EventGuard {
    pub fn set(key: &str, bot: ArcBot, data: J) {
        if let Ok(mut map) = CURRENT_EVENTS.lock() {
            map.insert(key.to_string(), (bot, data));
        }
    }
    pub fn clear(key: &str) {
        // 延迟清理：插件内部的异步回复（如渲染后 send）可能在调用返回后才触达
        let k = key.to_string();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(60));
            if let Ok(mut map) = CURRENT_EVENTS.lock() {
                map.remove(&k);
            }
        });
    }
    pub fn get(key: &str) -> Option<(ArcBot, J)> {
        CURRENT_EVENTS.lock().ok().and_then(|m| m.get(key).cloned())
    }
}

use std::collections::HashMap;
use std::sync::Mutex;

/// ============================ 同步 op 集 ============================

pub fn op_sync(name: &str, args: &J) -> J {
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
        "yaml_parse" => serde_yaml::from_str::<J>(&s("text")).unwrap_or(J::Null),
        "yaml_stringify" => serde_yaml::to_string(args.get("obj").unwrap_or(&J::Null)).map(J::String).unwrap_or(J::Null),
        "crypto_hash" => hash_op(&s("algo"), &s("data"), args.get("enc").map(crate::util::string).unwrap_or_else(|| "hex".into())),
        "render" => crate::renderer::render_op(args),
        "crypto_uuid" => json!(ulid::Ulid::new().to_string()),
        "buffer_from" => json!(s("data")),
        "ctx_set" => {
            let data: J = serde_json::from_str(&s("e")).unwrap_or(J::Null);
            let time = args.get("time").and_then(J::as_u64).unwrap_or(120);
            let is_group = args.get("isGroup").and_then(J::as_bool).unwrap_or(false);
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
            let (self_id, scope) = EventGuard::get(&s("key"))
                .map(|(_, d)| {
                    let sid = d.get("self_id").map(crate::util::string).unwrap_or_default();
                    let sc = if is_group {
                        d.get("group_id").map(crate::util::string).unwrap_or_default()
                    } else {
                        d.get("user_id").map(crate::util::string).unwrap_or_default()
                    };
                    (sid, sc)
                })
                .unwrap_or_default();
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
        "bot_stat" => json!({ "start_time": GLOBAL_BOT.get().map(|b| b.start_time).unwrap_or(0.0) }),
        "bot_get" => {
            match s("prop").as_str() {
                "uin" => json!(GLOBAL_BOT.get().map(|b| b.uin.lock().unwrap().clone()).unwrap_or_default()),
                "bots" => json!(GLOBAL_BOT.get().map(|b| b.bots.read().unwrap().keys().cloned().collect::<Vec<_>>()).unwrap_or_default()),
                "adapter" => json!(GLOBAL_BOT.get().map(|b| b.adapters.read().unwrap().iter().map(|a| json!({"id": a.id, "name": a.name})).collect::<Vec<_>>()).unwrap_or_default()),
                _ => {
                    if let Some(b) = GLOBAL_BOT.get() {
                        if let Some(instance) = b.get_bot(&s("prop")) {
                            return json!({ "uin": instance.self_id, "nickname": crate::util::string(&instance.nickname()) });
                        }
                    }
                    J::Null
                }
            }
        }
        "time_diff" => json!(crate::util::get_time_diff(args.get("t").and_then(J::as_u64).unwrap_or(0), crate::util::now_ms())),
        "cfg_get" => crate::GLOBAL_CFG.get().map(|c| c.get(&s("name"))).unwrap_or(J::Null),
        "cfg_get_group" => crate::GLOBAL_CFG
            .get()
            .map(|c| c.get_group(&s("bot_id"), &s("group_id")))
            .unwrap_or(J::Null),
        "cfg_get_config" => crate::GLOBAL_CFG.get().map(|c| c.get_config(&s("name"))).unwrap_or(J::Null),
        "os_home" => json!(std::env::var("HOME").unwrap_or_default()),
        "os_platform" => json!(std::env::consts::OS),
        "os_arch" => json!(std::env::consts::ARCH),
        "os_cpus" => json!(std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1)),
        "os_totalmem" | "os_freemem" => J::from(0),
        "os_uptime" => json!(crate::util::now_ms() / 1000),
        "os_hostname" => json!("yunzai-rust"),
        "process_cwd" => json!(std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()),
        "file_to_url" => {
            // ≈ Bot.fileToUrl — 存 buf 到 Bot.fs，返回 http://bot.url/File/name?auth...
            let data_b64 = s("data");
            let name_raw = s("name");
            let mime = {
                let m = s("mime");
                if m.is_empty() { "application/octet-stream".to_string() } else { m }
            };
            let buf = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &data_b64).unwrap_or_default();
            let name = if name_raw.is_empty() { ulid::Ulid::new().to_string() } else { urlencoding_encode(&name_raw) };
            let times_n = args.get("times").and_then(J::as_u64).map(|n| n as u32);
            if let Some(bot) = GLOBAL_BOT.get() {
                bot.fs.write().unwrap().insert(name.clone(), crate::util::FileEntry {
                    buffer: std::sync::Arc::new(buf),
                    content_type: mime,
                    times: std::sync::Mutex::new(times_n),
                });
                let base = bot.url.read().unwrap().clone();
                let mut url = format!("{}/File/{}", base.trim_end_matches('/'), name);
                // auth query 拼接（与原版一致：cfg.server.auth 全键入 query）
                if let Some(J::Object(auth_map)) = bot.cfg.get("server").get("auth").cloned() {
                    for (k, v) in auth_map.iter() {
                        url.push_str(if url.contains('?') { "&" } else { "?" });
                        url.push_str(k.as_str());
                        url.push('=');
                        url.push_str(&crate::util::string(v));
                    }
                }
                json!(url)
            } else {
                J::Null
            }
        }
        "cfg_get" => crate::GLOBAL_CFG.get().map(|c| c.get(&s("name"))).unwrap_or(J::Null),
        "cfg_get_config" => crate::GLOBAL_CFG.get().map(|c| c.get_def_set(&s("name"))).unwrap_or(J::Null),
        "cfg_get_def" => crate::GLOBAL_CFG.get().map(|c| c.get_config(&s("name"))).unwrap_or(J::Null),
        "cfg_get_other" => crate::GLOBAL_CFG.get().map(|c| c.get_other()).unwrap_or(J::Null),
        "cfg_get_group" => crate::GLOBAL_CFG
            .get()
            .map(|c| c.get_group(&s("bot_id"), &s("group_id")))
            .unwrap_or(J::Null),
        _ => {
            // 异步 op 兜底：node 同步线程内 block_on 宿主 runtime（Handle::block_on 可跨线程）
            match crate::nodejs::host::MAIN_HANDLE.get() {
                Some(h) => h.block_on(op_async(name, args.clone())),
                None => J::Null,
            }
        }
    }
}

pub fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn hash_op(algo: &str, data: &str, enc: String) -> J {
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

/// ============================ 异步 op 集 ============================

pub async fn op_async(name: &str, args: J) -> J {
    let s = |k: &str| args.get(k).map(crate::util::string).unwrap_or_default();
    match name {
        "exec" => {
            let (code, stdout, stderr) = crate::util::exec(&s("cmd")).await;
            json!({ "code": code, "stdout": stdout, "stderr": stderr })
        }
        "redis_get" => {
            let v = async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(r) = b.redis_arc() {
                        return r.get(&s("key")).await;
                    }
                }
                None
            }
            .await;
            v.map(J::String).unwrap_or(J::Null)
        }
        "redis_set" => {
            let ex = args.get("ex").and_then(J::as_i64);
            async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(r) = b.redis_arc() {
                        if let Some(secs) = ex {
                            r.set_ex(&s("key"), &s("val"), secs).await;
                        } else {
                            r.set(&s("key"), &s("val")).await;
                        }
                    }
                }
            }
            .await;
            J::Null
        }
        "redis_del" => {
            async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(r) = b.redis_arc() {
                        r.del(&s("key")).await;
                    }
                }
            }
            .await;
            J::Null
        }
        "redis_keys" => {
            let v = async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(r) = b.redis_arc() {
                        return r.keys(&s("pattern")).await;
                    }
                }
                vec![]
            }
            .await;
            json!(v)
        }
        "redis_incrby" => {
            let n = args.get("n").and_then(J::as_i64).unwrap_or(1);
            async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(r) = b.redis_arc() {
                        r.incr_by(&s("key"), n).await;
                    }
                }
            }
            .await;
            J::Null
        }
        "redis_expire" => {
            let secs = args.get("secs").and_then(J::as_i64).unwrap_or(0);
            async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(r) = b.redis_arc() {
                        r.expire(&s("key"), secs).await;
                    }
                }
            }
            .await;
            J::Null
        }
        "redis_ttl" => J::from(-1),
        "http" => http_op(&args).await,
        "e_reply" => {
            // 无状态：直接用事件参数构造上下文（bot 从全局注册表取，与 recall 同模式）
            let bot = GLOBAL_BOT.get().and_then(|b| b.get_bot(&s("self_id")));
            let ctx = json!({
                "self_id": s("self_id"),
                "group_id": args.get("group_id").cloned().unwrap_or(J::Null),
                "user_id": args.get("user_id").cloned().unwrap_or(J::Null),
            });
            crate::util::make_log1(
                crate::logger::Level::Debug,
                Some("reply"),
                format!("e_reply op: key={} ctx={}", s("key"), ctx),
            );
            let ret = bot.and_then(|bot| {
                let e = crate::plugins::plugin::E::new(bot, ctx);
                let msg: J = serde_json::from_str(&s("msg")).unwrap_or(J::Null);
                let opts: J = serde_json::from_str(&s("data")).unwrap_or(json!({}));
                let h = tokio::runtime::Handle::try_current()
                    .ok()
                    .or_else(|| crate::GLOBAL_RT.get().cloned());
                h.and_then(|h| tokio::task::block_in_place(|| h.block_on(e.reply_with(msg, args.get("quote").and_then(J::as_bool).unwrap_or(false), opts))).ok())
            });
            ret.unwrap_or(J::Null)
        }
        "recall" => {
            let ctx_v = json!({ "self_id": s("self_id"), "group_id": args.get("group_id").cloned().unwrap_or(J::Null), "user_id": args.get("user_id").cloned().unwrap_or(J::Null) });
            async {
                if let Some(b) = GLOBAL_BOT.get() {
                    if let Some(instance) = b.get_bot(&s("self_id")) {
                        let _ = instance
                            .protocol
                            .recall_msg(&ctx_v, args.get("message_id").cloned().unwrap_or(J::Null))
                            .await;
                    }
                }
            }
            .await;
            J::Null
        }
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
            async {
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
            }
            .await
        }
        "bot_em" => {
            let data: J = serde_json::from_str(&s("data")).unwrap_or(json!({}));
            async {
                if let Some(b) = GLOBAL_BOT.get() {
                    b.em(b, &s("name"), data).await;
                }
            }
            .await;
            J::Null
        }
        "send_master_msg" => {
            let msg: J = serde_json::from_str(&s("msg")).unwrap_or(J::Null);
            async {
                if let Some(b) = GLOBAL_BOT.get() {
                    b.send_master_msg(b, msg).await;
                }
            }
            .await;
            J::Null
        }
        "bot_exit" => {
            async {
                if let Some(b) = GLOBAL_BOT.get() {
                    b.exit(args.get("code").and_then(J::as_i64).unwrap_or(0) as i32).await;
                }
            }
            .await;
            J::Null
        }
        "sleep" => {
            crate::util::sleep(args.get("ms").and_then(J::as_u64).unwrap_or(0)).await;
            J::Null
        }
        _ => op_sync(name, &args),
    }
}

/// query 值百分号编码（保留 JS encodeURIComponent 的空格→%20 语义）
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            other => out.push_str(&format!("%{:02X}", other)),
        }
    }
    out
}

async fn http_op(args: &J) -> J {
    let s = |k: &str| args.get(k).map(crate::util::string).unwrap_or_default();
    let method = s("method");
    let url = s("url");
    if url.is_empty() {
        return J::Null;
    }
    let url = if let Some(J::Object(params)) = args.get("config").and_then(|c| c.get("params")) {
        let query: Vec<String> = params
            .iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| {
                let val = match v {
                    J::Array(a) => a.iter().map(crate::util::string).collect::<Vec<_>>().join(","),
                    other => crate::util::string(other),
                };
                format!("{}={}", k, urlencode(&val))
            })
            .collect();
        if query.is_empty() {
            url
        } else {
            format!("{}{}{}", url, if url.contains('?') { '&' } else { '?' }, query.join("&"))
        }
    } else {
        url
    };
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
            crate::util::make_log1(crate::logger::Level::Debug, Some("Http"), format!("请求失败 {} {}", url, err));
            J::Null
        }
    }
}
