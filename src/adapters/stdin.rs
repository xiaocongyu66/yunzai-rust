//! ≈ plugins/adapter/stdin.js — 标准输入适配器：终端即机器人
use crate::bot::{AdapterMeta, Bot, BotInstance, ProtocolImpl};
use crate::logger::Level;
use crate::util;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::BufRead;
use std::io::IsTerminal;
use std::sync::{Arc, RwLock};

pub const ID: &str = "stdin";
pub const NAME: &str = "标准输入";

#[allow(dead_code)]
pub fn meta() -> AdapterMeta {
    AdapterMeta { id: ID.into(), name: NAME.into(), path: "data/stdin/".into() }
}

struct StdinProtocol;

#[async_trait::async_trait]
impl ProtocolImpl for StdinProtocol {
    async fn send_friend_msg(&self, _ctx: &Value, msg: Value) -> Result<Value, String> {
        send_msg(msg).await;
        Ok(json!({ "message_id": format!("{:x}", util::now_ms()) }))
    }
    async fn send_group_msg(&self, ctx: &Value, msg: Value) -> Result<Value, String> {
        util::make_log1(Level::Info, Some(ID), format!("发送群消息：{}", util::string(ctx)));
        send_msg(msg).await;
        Ok(json!({ "message_id": format!("{:x}", util::now_ms()) }))
    }
    async fn recall_msg(&self, _ctx: &Value, message_id: Value) -> Result<Value, String> {
        util::make_log1(Level::Info, Some(ID), format!("撤回消息: {}", util::string(&message_id)));
        Ok(json!(null))
    }
}

async fn send_msg(msg: Value) {
    for seg in crate::segment::to_array(&msg) {
        let typ = seg.get("type").and_then(Value::as_str).unwrap_or("");
        match typ {
            "text" => {
                let mut text = seg.get("text").map(util::string).unwrap_or_default();
                if text.contains('\n') {
                    text = format!("发送文本: \n{}", text);
                }
                util::make_log1(Level::Info, Some(ID), text);
            }
            "image" => {
                let file = seg.get("file").map(util::string).unwrap_or_default();
                let name = seg.get("name").map(util::string).unwrap_or_else(|| "image".into());
                let path = format!("data/stdin/{}", name);
                if let Ok(bytes) = resolve_bytes(&file).await {
                    let _ = tokio::fs::write(&path, &bytes).await;
                    util::make_log1(Level::Info, Some(ID), format!("发送图片: 路径: {}", path));
                } else {
                    util::make_log1(Level::Info, Some(ID), format!("发送图片: {}", file));
                }
            }
            "record" | "video" | "file" => {
                util::make_log1(
                    Level::Info,
                    Some(ID),
                    format!(
                        "发送{}: {}",
                        seg_type_name(typ),
                        seg.get("file").map(util::string).unwrap_or_default()
                    ),
                );
            }
            "node" => {
                let nodes = seg.get("data").cloned().unwrap_or(Value::Null);
                for node in nodes.as_array().cloned().unwrap_or_default() {
                    let m = node.get("message").cloned().unwrap_or(Value::Null);
                    Box::pin(send_msg(m)).await;
                }
            }
            "reply" | "at" => {}
            other => {
                util::make_log1(Level::Info, Some(ID), other.to_string());
            }
        }
    }
}

fn seg_type_name(t: &str) -> &'static str {
    match t {
        "record" => "音频",
        "video" => "视频",
        "file" => "文件",
        _ => "消息",
    }
}

async fn resolve_bytes(file: &str) -> Result<Vec<u8>, String> {
    if let Some(b64) = file.strip_prefix("base64://") {
        use base64::Engine;
        return base64::engine::general_purpose::STANDARD
            .decode(b64)
            .map_err(|e| e.to_string());
    }
    let path = file.trim_start_matches("file://");
    tokio::fs::read(path).await.map_err(|e| e.to_string())
}

/// ≈ stdin adapter load
pub async fn load(bot: &Arc<Bot>) {
    let is_tty = std::io::stdin().is_terminal()
        || std::env::var("FORCE_TTY").map(|v| v == "1").unwrap_or(false);
    if !is_tty {
        return;
    }
    util::mkdir("data/stdin").await;

    let mut fl = HashMap::new();
    fl.insert(
        ID.to_string(),
        json!({ "user_id": ID, "nickname": NAME, "group_id": ID, "group_name": NAME }),
    );
    let mut gml = HashMap::new();
    gml.insert(ID.to_string(), fl.clone());

    let instance = BotInstance {
        self_id: ID.into(),
        adapter_id: ID.into(),
        adapter_name: NAME.into(),
        protocol: Arc::new(StdinProtocol),
        conn_id: 0,
        info: RwLock::new(json!({ "user_id": ID, "nickname": NAME })),
        version: RwLock::new(json!({ "id": ID, "name": NAME })),
        fl: RwLock::new(fl.clone()),
        gl: RwLock::new(fl),
        gml: RwLock::new(gml),
        request_list: RwLock::new(vec![]),
        stat: RwLock::new(json!({})),
        start_time: util::now_ms() as f64 / 1000.0,
    };
    bot.register_bot(Arc::new(instance));
    bot.add_uin(ID);

    // stdin 读取线程 → tokio 通道
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) => {
                    if tx.send(l).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let bot2 = bot.clone();
    tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            message(&bot2, line).await;
        }
        // stdin 关闭 → 等待在途事件处理完成后退出（≈ readline close → Bot.exit(5)）
        util::sleep(500).await;
        bot2.exit(5).await;
    });

    util::make_log1(Level::Mark, Some(ID), format!("{}({}) 已连接", NAME, ID));
    bot.em(bot, &format!("connect.{}", ID), json!({ "self_id": ID })).await;
}

/// ≈ stdin adapter message
async fn message(bot: &Arc<Bot>, msg: String) {
    // history 追加
    let line = format!("{:x}:{}\n", util::now_ms(), msg);
    use tokio::io::AsyncWriteExt;
    if let Ok(mut f) = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("data/stdin/history")
        .await
    {
        let _ = f.write_all(line.as_bytes()).await;
    }
    let data = json!({
        "self_id": ID,
        "user_id": ID,
        "post_type": "message",
        "message_type": "private",
        "sender": { "user_id": ID, "nickname": NAME },
        "message": [{ "type": "text", "text": msg }],
        "raw_message": msg,
    });
    let raw = data.get("raw_message").map(util::string).unwrap_or_default();
    util::make_log1(Level::Info, Some(ID), format!("系统消息: {}", raw));
    bot.em(bot, "message.private", data).await;
}
