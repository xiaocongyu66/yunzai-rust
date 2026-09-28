//! E2E 集成测试：模拟 OneBotv11 协议端反向 WS 连入
//! 流程：lifecycle → connect（应答 API echo）→ #复读 → 上下文 → 回复断言
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

const PORT: u16 = 25361;
const SELF_ID: u64 = 10001;
const USER_ID: u64 = 222;
const GROUP_ID: u64 = 333;

async fn api_response(data: Value) -> Value {
    let echo = data["echo"].clone();
    let action = data["action"].as_str().unwrap_or("").to_string();
    let ret = match action.as_str() {
        "get_login_info" => json!({ "user_id": SELF_ID, "nickname": "TestBot" }),
        "get_version_info" => json!({ "app_name": "TestNapCat", "app_version": "9.9.9" }),
        "get_friend_list" => json!([]),
        "get_group_list" => json!([{ "group_id": GROUP_ID, "group_name": "测试群" }]),
        "get_group_member_list" => json!([
            { "user_id": SELF_ID, "role": "owner", "nickname": "TestBot", "card": "" },
            { "user_id": USER_ID, "role": "member", "nickname": "测试用户", "card": "测试卡片" }
        ]),
        "send_msg" => json!({ "message_id": format!("msg-{}", rand_suffix()) }),
        "send_group_forward_msg" => json!({ "message_id": "fwd-1" }),
        _ => json!(null),
    };
    json!({ "status": "ok", "retcode": 0, "data": ret, "echo": echo })
}

fn rand_suffix() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

struct LogDumper;
impl Drop for LogDumper {
    fn drop(&mut self) {
        for f in ["/tmp/e2e-child.log", "/tmp/e2e-child-err.log"] {
            if let Ok(s) = std::fs::read_to_string(f) {
                println!("=== 子进程日志 {} ===\n{}", f, s);
            }
        }
    }
}

#[tokio::test]
async fn onebotv11_e2e() {
    let _dumper = LogDumper;
    // 1. 准备临时运行目录
    let dir = tempfile::tempdir().unwrap();
    let cfg_dir = dir.path().join("config/config");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::copy("config/default_config/bot.yaml", dir.path().join("config/default_bot.tmp")).ok();
    // 复制 default_config（保持与运行时目录结构一致）
    let def = dir.path().join("config/default_config");
    std::fs::create_dir_all(&def).unwrap();
    for f in ["bot.yaml", "server.yaml", "redis.yaml", "group.yaml", "other.yaml"] {
        std::fs::copy(format!("config/default_config/{}", f), def.join(f)).unwrap();
    }
    std::fs::write(cfg_dir.join("bot.yaml"), "log_level: debug\n").unwrap();
    std::fs::write(
        cfg_dir.join("server.yaml"),
        format!("url: http://localhost:{}\nport: {}\nredirect: https://example.com\n", PORT, PORT),
    )
    .unwrap();
    // 用户 222 为主人
    std::fs::write(
        cfg_dir.join("other.yaml"),
        format!("master:\n  - \"{}:{}\"\n", SELF_ID, USER_ID),
    )
    .unwrap();

    // 2. 启动主程序（无 Redis、无 TTY）
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_yunzai"))
        .current_dir(dir.path())
        .env("YZ_NO_REDIS", "1")
        .stdout(std::fs::File::create("/tmp/e2e-child.log").unwrap())
        .stderr(std::fs::File::create("/tmp/e2e-child-err.log").unwrap())
        .spawn()
        .unwrap();

    // 3. 等待端口就绪
    let mut ready = false;
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", PORT)).await.is_ok() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(ready, "服务器未在预期时间内启动");

    // 4. WS 连入
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/OneBotv11", PORT))
        .await
        .expect("WS 连接失败");

    // 5. lifecycle connect
    ws.send(Message::Text(
        json!({
            "time": 1700000000,
            "self_id": SELF_ID,
            "post_type": "meta_event",
            "meta_event_type": "lifecycle",
            "sub_type": "connect"
        })
        .to_string()
        .into(),
    ))
    .await
    .unwrap();

    // 6. echo 应答循环 + 收集 send_msg
    let (write, mut read) = ws.split();
    let write = std::sync::Arc::new(tokio::sync::Mutex::new(write));
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    {
        let write = write.clone();
        tokio::spawn(async move {
            while let Some(Ok(msg)) = read.next().await {
                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => break,
                    _ => continue,
                };
                let data: Value = match serde_json::from_str(&text) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                if data.get("echo").is_some() && data.get("action").is_some() {
                    let resp = api_response(data.clone()).await;
                    let _ = write.lock().await.send(Message::Text(resp.to_string().into())).await;
                }
                if data.get("action").and_then(Value::as_str) == Some("send_msg") {
                    let _ = tx.send(data);
                }
            }
        });
    }

    // 7. 发送 #复读（主人）
    tokio::time::sleep(Duration::from_millis(800)).await;
    let send_group = |text: &str| {
        json!({
            "time": 1700000001,
            "self_id": SELF_ID,
            "post_type": "message",
            "message_type": "group",
            "sub_type": "normal",
            "user_id": USER_ID,
            "group_id": GROUP_ID,
            "message": [{ "type": "text", "data": { "text": text } }],
            "raw_message": text,
            "sender": { "user_id": USER_ID, "nickname": "测试用户", "card": "测试卡片" }
        })
    };
    write.lock().await.send(Message::Text(send_group("#复读").to_string().into())).await.unwrap();

    // 断言收到回复「请发送要复读的内容」
    let first = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("未收到第一条回复")
        .expect("通道关闭");
    assert_eq!(first["action"], "send_msg");
    assert_eq!(first["params"]["group_id"], json!(GROUP_ID));
    let content = util::string(&first["params"]["message"]);
    assert!(content.contains("请发送要复读的内容"), "第一条回复错误: {}", content);

    // 8. 上下文续接：发送内容 → 复读（等待 groupCD 500ms 过期）
    tokio::time::sleep(Duration::from_millis(600)).await;
    write.lock().await.send(Message::Text(send_group("hello yunzai").to_string().into()))
        .await
        .unwrap();
    let second = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("未收到第二条回复")
        .expect("通道关闭");
    assert_eq!(second["action"], "send_msg");
    let content2 = util::string(&second["params"]["message"]);
    assert!(
        content2.contains("hello yunzai"),
        "复读内容错误: {}",
        content2
    );

    // 清理
    let _ = child.kill();
}

mod util {
    use serde_json::Value;
    pub fn string(v: &Value) -> String {
        match v {
            Value::String(s) => s.clone(),
            _ => v.to_string(),
        }
    }
}
