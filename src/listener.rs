//! ≈ lib/listener/loader.js — 加载事件监听与适配器
use crate::bot::Bot;
use futures::future::BoxFuture;
use serde_json::Value;
use std::sync::Arc;

pub async fn load(bot: &Arc<Bot>) {
    crate::util::make_log1(crate::logger::Level::Info, Some("Listener"), "-----------".into());
    crate::util::make_log1(crate::logger::Level::Info, Some("Listener"), "加载监听事件中...".into());

    listener_fn(bot, "message", |bot, e| Box::pin(crate::events::message_event(bot, e)));
    listener_fn(bot, "notice", |bot, e| Box::pin(crate::events::notice_event(bot, e)));
    listener_fn(bot, "request", |bot, e| Box::pin(crate::events::request_event(bot, e)));
    listener_fn(bot, "connect", |bot, e| Box::pin(crate::events::connect_event(bot, e)));
    bot.once("online", Arc::new(|_bot: Arc<Bot>, e: Value| {
        Box::pin(crate::events::online_event(_bot, e))
    }));
    crate::util::make_log1(crate::logger::Level::Info, Some("Listener"), "加载监听事件[5个]".into());

    crate::util::make_log1(crate::logger::Level::Info, Some("Adapter"), "-----------".into());
    crate::util::make_log1(crate::logger::Level::Info, Some("Adapter"), "加载适配器中...".into());
    crate::adapters::stdin::load(bot).await;
    crate::util::make_log1(crate::logger::Level::Info, Some("Adapter"), "加载适配器[1个]".into());
}

fn listener_fn(
    bot: &Arc<Bot>,
    event: &str,
    f: impl Fn(Arc<Bot>, Value) -> BoxFuture<'static, ()> + Send + Sync + 'static,
) {
    bot.on(event, Arc::new(f));
}
