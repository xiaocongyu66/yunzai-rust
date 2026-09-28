//! ≈ lib/plugins/handler.js — Handler 注册/删除/调用
use crate::plugins::plugin::E;
use futures::future::BoxFuture;
use once_cell::sync::Lazy;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub type HandlerFn = Arc<dyn Fn(&mut E, Value, Reject) -> BoxFuture<'static, ()> + Send + Sync>;
pub type Reject = Arc<dyn Fn(String) + Send + Sync>;

struct HandlerEntry {
    priority: i64,
    ns: String,
    key: String,
    f: HandlerFn,
}

static EVENTS: Lazy<RwLock<HashMap<String, Vec<HandlerEntry>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

pub struct Handler;

impl Handler {
    pub fn add(ns: &str, key: &str, f: HandlerFn, priority: i64) {
        if key.is_empty() {
            return;
        }
        Self::del(ns, key);
        crate::util::make_log1(
            crate::logger::Level::Mark,
            None,
            format!("[Handler][Reg]: [{}][{}]", ns, key),
        );
        let mut events = EVENTS.write().unwrap();
        let list = events.entry(key.to_string()).or_default();
        list.push(HandlerEntry { priority, ns: ns.to_string(), key: key.to_string(), f });
        list.sort_by_key(|e| e.priority);
    }

    pub fn del(ns: &str, key: &str) {
        let mut events = EVENTS.write().unwrap();
        if key.is_empty() {
            let keys: Vec<String> = events.keys().cloned().collect();
            for k in keys {
                Self::del(ns, &k);
            }
            return;
        }
        if let Some(list) = events.get_mut(key) {
            list.retain(|e| e.ns != ns);
            if list.is_empty() {
                events.remove(key);
            }
        }
    }

    /// ≈ Handler.call — 按优先级调用，未被 reject 时首个即止
    pub async fn call(key: &str, e: &mut E, args: Value) -> Value {
        let list: Vec<HandlerEntry> = {
            let events = EVENTS.read().unwrap();
            events
                .get(key)
                .map(|l| {
                    l.iter()
                        .map(|e| HandlerEntry { priority: e.priority, ns: e.ns.clone(), key: e.key.clone(), f: e.f.clone() })
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut ret = Value::Null;
        for entry in list {
            let done = Arc::new(RwLock::new(true));
            let done2 = done.clone();
            let ns = entry.ns.clone();
            let k = entry.key.clone();
            let reject: Reject = Arc::new(move |msg: String| {
                if !msg.is_empty() {
                    crate::util::make_log1(
                        crate::logger::Level::Mark,
                        None,
                        format!("[Handler][Reject]: [{}][{}] {}", ns, k, msg),
                    );
                }
                *done2.write().unwrap() = false;
            });
            (entry.f)(e, args.clone(), reject).await;
            if *done.read().unwrap() {
                crate::util::make_log1(
                    crate::logger::Level::Mark,
                    None,
                    format!("[Handler][Done]: [{}][{}]", entry.ns, key),
                );
                return ret;
            }
            let _ = &mut ret;
        }
        ret
    }

    pub fn has(key: &str) -> bool {
        EVENTS.read().unwrap().contains_key(key)
    }
}
