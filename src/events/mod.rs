//! ≈ lib/events/*.js — 事件监听器
use crate::bot::Bot;
use serde_json::{json, Value};
use std::sync::Arc;

/// ≈ events/message.js — 监听消息事件 → 插件分发
pub async fn message_event(bot: Arc<Bot>, e: Value) {
    if let Some(loader) = bot.loader_arc() {
        loader.deal(bot, e).await;
    }
}

/// ≈ events/notice.js
pub async fn notice_event(bot: Arc<Bot>, e: Value) {
    if let Some(loader) = bot.loader_arc() {
        loader.deal(bot, e).await;
    }
}

/// ≈ events/request.js
pub async fn request_event(bot: Arc<Bot>, e: Value) {
    if let Some(loader) = bot.loader_arc() {
        loader.deal(bot, e).await;
    }
}

/// ≈ events/connect.js — 连接事件：uin 注册 + 欢迎消息
pub async fn connect_event(bot: Arc<Bot>, e: Value) {
    let self_id = e
        .get("self_id")
        .map(crate::util::string)
        .unwrap_or_default();
    bot.add_uin(&self_id);

    let online_msg_exp = bot
        .cfg
        .bot()
        .get("online_msg_exp")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if online_msg_exp == 0 {
        return;
    }
    let key = format!("Yz:loginMsg:{}", self_id);
    if let Some(redis) = bot.redis_arc() {
        if redis.get(&key).await.is_some() {
            return;
        }
        redis.set_ex(&key, "1", online_msg_exp * 60).await;
    }
    let masters = bot.cfg.master();
    let version = crate::bot::VERSION.clone();
    let msg = format!(
        "欢迎使用【TRSS-Yunzai {}】\n【#帮助】查看指令说明\n【#状态】查看运行状态\n【#日志】查看运行日志\n【#重启】重新启动\n【#设置主人】设置主人账号",
        version
    );
    for user_id in masters.get(&self_id).cloned().unwrap_or_default() {
        if let Some(f) = bot.pick_friend(&bot, json!(user_id), true) {
            let _ = f.send_msg(json!(msg)).await;
        }
    }
}

/// ≈ events/online.js — 上线事件
pub async fn online_event(_bot: Arc<Bot>, _e: Value) {
    crate::util::make_log1(crate::logger::Level::Mark, None, "----^_^----".to_string());
}
