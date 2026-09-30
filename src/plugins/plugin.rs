//! ≈ lib/plugins/plugin.js — 插件基类、事件上下文 E、多轮对话上下文存储
use crate::bot::{Bot, Friend, Group};
use crate::logger::Level;
use crate::util;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub enum Permission {
    All,
    Master,
    Owner,
    Admin,
}

impl Permission {
    pub fn parse(s: &str) -> Permission {
        match s {
            "master" => Permission::Master,
            "owner" => Permission::Owner,
            "admin" => Permission::Admin,
            _ => Permission::All,
        }
    }
}

#[derive(Clone)]
pub struct Rule {
    pub reg: Regex,
    pub fnc: String,
    pub event: Option<String>,
    pub log: bool,
    pub permission: Permission,
}

impl Rule {
    pub fn new(reg: &str, fnc: &str) -> Rule {
        Rule {
            reg: Regex::new(reg).unwrap_or_else(|_| Regex::new(r"$^").unwrap()),
            fnc: fnc.to_string(),
            event: None,
            log: true,
            permission: Permission::All,
        }
    }
    pub fn permission(mut self, p: &str) -> Rule {
        self.permission = Permission::parse(p);
        self
    }
    pub fn event(mut self, e: &str) -> Rule {
        self.event = Some(e.to_string());
        self
    }
    pub fn log(mut self, log: bool) -> Rule {
        self.log = log;
        self
    }
}

#[derive(Clone)]
pub struct Task {
    pub name: String,
    pub cron: String,
    pub fnc: String,
    pub log: bool,
}

/// accept 处理返回值：≈ 返回 "return" / true / 无
pub enum Accept {
    Next,
    Break,
    Return,
}

/// ≈ plugin 基类 — 原生插件 trait
#[async_trait::async_trait]
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn dsc(&self) -> &str {
        ""
    }
    fn event(&self) -> &str {
        "message"
    }
    fn priority(&self) -> i64 {
        5000
    }
    fn rules(&self) -> Vec<Rule> {
        vec![]
    }
    fn tasks(&self) -> Vec<Task> {
        vec![]
    }
    async fn accept(&self, _e: &mut E) -> Accept {
        Accept::Next
    }
    /// 返回 false ≈ 原版 handler 返回 false → 继续匹配下一条 rule
    async fn handle(&self, _e: &mut E, _fnc: &str) -> bool {
        false
    }
}

/// ≈ JS 真值语义
pub fn truthy(v: &Value) -> bool {
    !matches!(v, Value::Null | Value::Bool(false)) && *v != json!("")
}

// ============ 事件上下文 E ============

pub struct E {
    pub bot: Arc<Bot>,
    pub data: Value,
}

impl E {
    pub fn new(bot: Arc<Bot>, data: Value) -> E {
        E { bot, data }
    }

    pub fn get(&self, key: &str) -> Value {
        self.data.get(key).cloned().unwrap_or(Value::Null)
    }
    pub fn self_id(&self) -> String {
        util::string(&self.get("self_id"))
    }
    pub fn user_id(&self) -> Value {
        self.get("user_id")
    }
    pub fn group_id(&self) -> Value {
        self.get("group_id")
    }
    pub fn msg(&self) -> String {
        util::string(&self.get("msg"))
    }
    pub fn is_group(&self) -> bool {
        truthy(&self.get("isGroup"))
    }
    pub fn is_private(&self) -> bool {
        truthy(&self.get("isPrivate"))
    }
    pub fn is_master(&self) -> bool {
        truthy(&self.get("isMaster"))
    }
    pub fn at_bot(&self) -> bool {
        truthy(&self.get("atBot"))
    }
    pub fn message(&self) -> Value {
        self.get("message")
    }

    /// ≈ e.friend — 好友门面
    pub fn friend(&self) -> Option<Friend> {
        let self_id = self.self_id();
        self.bot
            .pick_friend(&self.bot, self.user_id(), true)
            .map(|mut f| {
                f.self_id = self_id;
                f
            })
    }

    /// ≈ e.group — 群门面
    pub fn group(&self) -> Option<Group> {
        let self_id = self.self_id();
        self.bot
            .pick_group(&self.bot, self.group_id(), true)
            .map(|mut g| {
                g.self_id = self_id;
                g
            })
    }

    /// ≈ e.member 信息（从 gml 缓存）
    pub fn member_info(&self) -> Value {
        let uid = util::string(&self.user_id());
        let gid = util::string(&self.group_id());
        self.bot
            .get_bot(&self.self_id())
            .and_then(|b| b.gml.read().unwrap().get(&gid).and_then(|m| m.get(&uid)).cloned())
            .unwrap_or(Value::Null)
    }

    fn underlying_reply(&self, msg: Value) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + '_>> {
        Box::pin(async move {
            if self.is_group() {
                if let Some(g) = self.group() {
                    return g.send_msg(msg).await;
                }
            }
            if let Some(f) = self.friend() {
                f.send_msg(msg).await
            } else {
                Err("无可用发送通道".into())
            }
        })
    }

    /// ≈ loader.reply 包装 — at/quote 注入、错误捕获、recallMsg 定时撤回
    pub async fn reply(&self, msg: Value) -> Result<Value, String> {
        self.reply_with(msg, false, json!({})).await
    }

    pub async fn reply_with(&self, msg: Value, quote: bool, opts: Value) -> Result<Value, String> {
        util::make_log1(Level::Debug, Some("reply"), format!("reply_with: len={}", util::string(&msg).len()));
        if msg.is_null() || msg == json!("") {
            return Ok(json!(false));
        }
        // ≈ TRSS segment 语义：纯 base64 大字符串按图片发（渲染器返回值）
        if let serde_json::Value::String(ref raw) = msg {
            if raw.len() > 512
                && raw.starts_with("iVBOR")
                && raw.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
            {
                msg = crate::segment::image(format!("base64://{}", raw), None);
            }
        }
        let mut msg = msg;
        let at = opts.get("at").cloned().unwrap_or(json!(""));
        if truthy(&at) && self.is_group() {
            let at_id = if at == json!(true) { self.user_id() } else { at };
            msg = Value::Array(vec![
                crate::segment::at(at_id, None),
                crate::segment::text("\n"),
                msg,
            ]);
        }
        let message_id = self.get("message_id");
        if quote && truthy(&message_id) {
            msg = Value::Array(vec![crate::segment::reply(message_id.clone()), msg]);
        }
        let res = match self.underlying_reply(msg.clone()).await {
            Ok(r) => {
                util::make_log1(Level::Debug, Some("reply"), format!("underlying ok: {}", util::string(&r)));
                r
            }
            Err(err) => {
                let err_msg = err.clone();
                util::make_log(
                    Level::Error,
                    Some(&self.self_id()),
                    false,
                    vec!["发送消息错误".into(), util::string(&msg), err_msg],
                );
                json!({ "error": [err] })
            }
        };
        let recall = opts.get("recallMsg").and_then(Value::as_i64).unwrap_or(0);
        if recall > 0 {
            if let Some(mid) = res.get("message_id").cloned() {
                let bot = self.bot.clone();
                let self_id = self.self_id();
                let gid = self.get("group_id");
                let uid = self.user_id();
                let orig = message_id.clone();
                tokio::spawn(async move {
                    util::sleep((recall as u64) * 1000).await;
                    if let Some(b) = bot.get_bot(&self_id) {
                        let ctx = json!({ "self_id": self_id, "group_id": gid, "user_id": uid });
                        let _ = b.protocol.recall_msg(&ctx, mid).await;
                        if truthy(&orig) {
                            let _ = b.protocol.recall_msg(&ctx, orig).await;
                        }
                    }
                });
            }
        }
        // 消息统计
        if let Some(loader) = self.bot.loader_arc() {
            loader.count_send(self, &msg).await;
        }
        Ok(res)
    }

    // ============ 多轮对话上下文（≈ plugin.js setContext 状态机） ============

    pub fn con_key(&self, is_group: bool) -> String {
        format!(
            "{}.{}.{}",
            self.get("plugin_name").as_str().unwrap_or(""),
            self.self_id(),
            if is_group { util::string(&self.group_id()) } else { util::string(&self.user_id()) }
        )
    }

    pub fn set_context(&self, typ: &str, is_group: bool, time: u64, timeout_reply: &str) {
        let key = self.con_key(is_group);
        let typ = typ.to_string();
        let mut store = CONTEXTS.write().unwrap();
        let entry = store.entry(key.clone()).or_default();
        entry.insert(typ, CtxEntry { data: self.data.clone(), deadline: if time > 0 { Some(Instant::now() + Duration::from_secs(time)) } else { None }, timeout_reply: timeout_reply.to_string() });
    }

    pub fn get_context(&self, typ: Option<&str>, is_group: bool) -> HashMap<String, Value> {
        let key = self.con_key(is_group);
        let store = CONTEXTS.read().unwrap();
        let mut ret = HashMap::new();
        if let Some(entry) = store.get(&key) {
            for (t, ctx) in entry.iter() {
                if !ctx.expired() && typ.map(|x| x == t).unwrap_or(true) {
                    ret.insert(t.clone(), ctx.data.clone());
                }
            }
        }
        ret
    }

    pub fn finish(&self, typ: &str, is_group: bool) {
        let key = self.con_key(is_group);
        let mut store = CONTEXTS.write().unwrap();
        if let Some(entry) = store.get_mut(&key) {
            entry.remove(typ);
            if entry.is_empty() {
                store.remove(&key);
            }
        }
    }
}

// ============ 全局上下文存储 ≈ plugin.js stateArr ============

pub struct CtxEntry {
    pub data: Value,
    deadline: Option<Instant>,
    pub timeout_reply: String,
}

impl CtxEntry {
    pub fn new(data: Value, time: u64, timeout_reply: String) -> CtxEntry {
        CtxEntry {
            data,
            deadline: if time > 0 { Some(Instant::now() + Duration::from_secs(time)) } else { None },
            timeout_reply,
        }
    }

    pub fn expired(&self) -> bool {
        self.deadline.map(|d| Instant::now() > d).unwrap_or(false)
    }
}

pub static CONTEXTS: Lazy<RwLock<HashMap<String, HashMap<String, CtxEntry>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));
