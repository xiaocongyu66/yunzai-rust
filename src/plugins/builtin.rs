//! ≈ plugins/system/master.js + plugins/example/*.js 的原生 Rust 移植
//! （阶段3后由 plugins/ 目录原版 JS 插件接管，此处保留为管线验证）
use crate::plugins::plugin::{Accept, E, Plugin, Rule};
use crate::util;
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

/// ≈ plugins/example/进群退群通知.js — newcomer
pub struct NewcomerPlugin;

#[async_trait::async_trait]
impl Plugin for NewcomerPlugin {
    fn name(&self) -> &str {
        "欢迎新人"
    }
    fn dsc(&self) -> &str {
        "新人入群欢迎"
    }
    fn event(&self) -> &str {
        "notice.group.increase"
    }
    async fn accept(&self, e: &mut E) -> Accept {
        if e.user_id() == json!(e.self_id()) {
            return Accept::Next;
        }
        // cd 30s
        let key = format!("Yz:newcomers:{}", util::string(&e.group_id()));
        if let Some(redis) = e.bot.redis_arc() {
            if redis.get(&key).await.is_some() {
                return Accept::Next;
            }
            redis.set_ex(&key, "1", 30).await;
        }
        let msg = json!([
            crate::segment::at(e.user_id(), None),
            "欢迎新人！"
        ]);
        let _ = e.reply(msg).await;
        Accept::Next
    }
}

/// ≈ plugins/example/进群退群通知.js — outNotice
pub struct OutNoticePlugin;

#[async_trait::async_trait]
impl Plugin for OutNoticePlugin {
    fn name(&self) -> &str {
        "退群通知"
    }
    fn dsc(&self) -> &str {
        "xx退群了"
    }
    fn event(&self) -> &str {
        "notice.group.decrease"
    }
    async fn accept(&self, e: &mut E) -> Accept {
        if e.user_id() == json!(e.self_id()) {
            return Accept::Next;
        }
        let member = e.member_info();
        let name = member
            .get("card")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| member.get("nickname").and_then(Value::as_str))
            .map(String::from);
        let msg = match name {
            Some(n) => format!("{}({}) 退群了", n, util::string(&e.user_id())),
            None => format!("{} 退群了", util::string(&e.user_id())),
        };
        logger_mark(format!("[退出通知]{} {}", util::string(&e.get("logText")), msg));
        let _ = e.reply(json!(msg)).await;
        Accept::Next
    }
}

fn logger_mark(msg: String) {
    util::make_log1(crate::logger::Level::Mark, None, msg);
}

static MASTER_CODES: Lazy<Mutex<HashMap<String, String>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// ≈ plugins/system/master.js — 设置主人
pub struct MasterPlugin;

#[async_trait::async_trait]
impl Plugin for MasterPlugin {
    fn name(&self) -> &str {
        "设置主人"
    }
    fn dsc(&self) -> &str {
        "设置主人"
    }
    fn priority(&self) -> i64 {
        i64::MIN
    }
    fn rules(&self) -> Vec<Rule> {
        vec![
            Rule::new(r"^#设置主人验证码$", "code").permission("master"),
            Rule::new(r"^#设置主人$", "master"),
        ]
    }
    async fn handle(&self, e: &mut E, fnc: &str) -> bool {
        match fnc {
            "code" => {
                self.code(e).await;
                true
            }
            "master" => {
                self.master(e).await;
                true
            }
            "verify" => {
                self.verify(e).await;
                true
            }
            _ => false,
        }
    }
}

impl MasterPlugin {
    async fn code(&self, e: &E) {
        let msg = {
            let codes = MASTER_CODES.lock().unwrap();
            let mut msg = String::new();
            for (k, v) in codes.iter() {
                msg += &format!("[{}] {}\n", k, v);
            }
            msg
        };
        let text = if msg.is_empty() { "暂无验证码".to_string() } else { msg.trim().to_string() };
        let _ = e.reply_with(json!(text), true, json!({})).await;
    }

    async fn master(&self, e: &E) {
        if e.is_master() {
            let _ = e
                .reply_with(json!(format!("[{}] 已经为主人", util::string(&e.user_id()))), true, json!({}))
                .await;
            return;
        }
        let key = format!("{}:{}", e.self_id(), util::string(&e.user_id()));
        let code = ulid::Ulid::new().to_string();
        MASTER_CODES.lock().unwrap().insert(key.clone(), code.clone());
        logger_mark(format!(
            "[{}] 设置主人验证码 {}",
            util::string(&e.user_id()),
            code
        ));
        e.set_context("verify", false, 120, "操作超时已取消");
        let _ = e
            .reply_with(
                json!(format!("[{}] 请输入验证码", util::string(&e.user_id()))),
                true,
                json!({}),
            )
            .await;
    }

    async fn verify(&self, e: &E) {
        let key = format!("{}:{}", e.self_id(), util::string(&e.user_id()));
        let verify_code = MASTER_CODES.lock().unwrap().get(&key).cloned();
        let verified = verify_code
            .as_ref()
            .map(|c| e.msg().trim().to_uppercase() == *c)
            .unwrap_or(false);
        MASTER_CODES.lock().unwrap().remove(&key);
        e.finish("verify", false);
        if verified {
            let uid = util::string(&e.user_id());
            let sid = e.self_id();
            yaml_list_append("config/config/other.yaml", "masterQQ", &format!("\"{}\"", uid));
            yaml_list_append("config/config/other.yaml", "master", &format!("\"{}:{}\"", sid, uid));
            let _ = e
                .reply_with(json!(format!("[{}] 设置主人完成", uid)), true, json!({}))
                .await;
        } else {
            let _ = e.reply_with(json!("验证码错误"), true, json!({})).await;
        }
    }
}

/// 向 YAML 列表键追加一项（保留原文件注释，≈ master.js edit）
fn yaml_list_append(file: &str, key: &str, value: &str) {
    let Ok(content) = std::fs::read_to_string(file) else { return };
    let lines: Vec<String> = content.lines().map(String::from).collect();
    let mut out: Vec<String> = vec![];
    let mut inserted = false;
    let mut in_block = false;
    for line in lines {
        let is_key_line = line.starts_with(&format!("{}:", key));
        if is_key_line {
            out.push(line);
            out.push(format!("  - {}", value));
            inserted = true;
            in_block = true;
            continue;
        }
        if in_block {
            // 列表块结束（遇到新的顶层键或空行后内容）
            let top_level = !line.starts_with(' ') && !line.is_empty();
            if top_level {
                in_block = false;
            }
        }
        out.push(line);
    }
    if !inserted {
        out.push(format!("{}:", key));
        out.push(format!("  - {}", value));
    }
    let _ = std::fs::write(file, out.join("\n") + "\n");
}

/// ≈ plugins/example/主动复读.js — 复读（上下文机制验证）
pub struct RepeatPlugin;

#[async_trait::async_trait]
impl Plugin for RepeatPlugin {
    fn name(&self) -> &str {
        "复读"
    }
    fn dsc(&self) -> &str {
        "复读用户发送的内容，然后撤回"
    }
    fn rules(&self) -> Vec<Rule> {
        vec![Rule::new(r"^#复读$", "repeat").permission("master")]
    }
    async fn handle(&self, e: &mut E, fnc: &str) -> bool {
        match fnc {
            "repeat" => {
                e.set_context("doRep", false, 120, "操作超时已取消");
                let _ = e
                    .reply_with(json!("请发送要复读的内容"), false, json!({ "at": true }))
                    .await;
                true
            }
            "doRep" => {
                e.finish("doRep", false);
                let _ = e
                    .reply_with(e.message(), false, json!({ "recallMsg": 5 }))
                    .await;
                true
            }
            _ => false,
        }
    }
}

/// 原生内置插件注册表
pub fn builtins() -> Vec<std::sync::Arc<dyn Plugin>> {
    vec![
        std::sync::Arc::new(MasterPlugin),
        std::sync::Arc::new(RepeatPlugin),
        std::sync::Arc::new(NewcomerPlugin),
        std::sync::Arc::new(OutNoticePlugin),
    ]
}
