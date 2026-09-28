//! ≈ lib/bot.js — Yunzai 核心类：事件总线、bots 注册表、HTTP+WS 服务端
use crate::config::Cfg;
use crate::logger::{self, Level};
use crate::util;
use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use once_cell::sync::Lazy;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock};

/// 协议实现 trait —— 适配器为每个接入账号提供（≈ OneBotv11 适配器方法面）
#[async_trait::async_trait]
pub trait ProtocolImpl: Send + Sync {
    async fn send_api(&self, _action: &str, _params: Value) -> Result<Value, String> {
        Err("当前适配器不支持 send_api".into())
    }
    async fn send_friend_msg(&self, ctx: &Value, msg: Value) -> Result<Value, String>;
    async fn send_group_msg(&self, ctx: &Value, msg: Value) -> Result<Value, String>;
    async fn recall_msg(&self, ctx: &Value, message_id: Value) -> Result<Value, String> {
        let _ = (ctx, message_id);
        Err("当前适配器不支持撤回".into())
    }
}

/// ≈ adapter 元信息
#[derive(Clone, Debug)]
pub struct AdapterMeta {
    pub id: String,
    pub name: String,
    pub path: String,
}

/// ≈ Bot[data.self_id] — 每个协议连接一个实例
pub struct BotInstance {
    pub self_id: String,
    pub adapter_id: String,
    pub adapter_name: String,
    pub protocol: Arc<dyn ProtocolImpl>,
    pub conn_id: u64,
    pub info: RwLock<Value>,
    pub version: RwLock<Value>,
    pub fl: RwLock<HashMap<String, Value>>,
    pub gl: RwLock<HashMap<String, Value>>,
    pub gml: RwLock<HashMap<String, HashMap<String, Value>>>,
    pub request_list: RwLock<Vec<Value>>,
    pub stat: RwLock<Value>,
    pub start_time: f64,
}

impl BotInstance {
    pub fn uin(&self) -> Value {
        self.info.read().unwrap().get("user_id").cloned().unwrap_or(json!(self.self_id))
    }
    pub fn nickname(&self) -> Value {
        self.info.read().unwrap().get("nickname").cloned().unwrap_or(json!(""))
    }
}

/// ≈ pickFriend 门面对象
pub struct Friend {
    pub bot: Arc<Bot>,
    pub self_id: String,
    pub user_id: Value,
    pub data: Value,
}

impl Friend {
    fn ctx(&self) -> Value {
        let mut o = self.data.clone();
        if let Some(obj) = o.as_object_mut() {
            obj.insert("self_id".into(), json!(self.self_id));
            obj.insert("user_id".into(), self.user_id.clone());
        }
        o
    }
    pub async fn send_msg(&self, msg: Value) -> Result<Value, String> {
        util::make_log(
            Level::Info,
            Some(&format!("{} => {}", self.self_id, crate::util::string(&self.user_id))),
            true,
            vec![format!("发送好友消息：{}", crate::util::string(&msg))],
        );
        let bot = self.bot.get_bot(&self.self_id).unwrap();
        bot.protocol.send_friend_msg(&self.ctx(), msg).await
    }
    pub async fn recall_msg(&self, message_id: Value) -> Result<Value, String> {
        let bot = self.bot.get_bot(&self.self_id).unwrap();
        bot.protocol.recall_msg(&self.ctx(), message_id).await
    }
    pub fn get_avatar_url(&self) -> String {
        self.data
            .get("avatar")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| format!("https://q.qlogo.cn/g?b=qq&s=0&nk={}", self.user_id))
    }
}

/// ≈ pickGroup 门面对象
pub struct Group {
    pub bot: Arc<Bot>,
    pub self_id: String,
    pub group_id: Value,
    pub data: Value,
}

impl Group {
    fn ctx(&self) -> Value {
        let mut o = self.data.clone();
        if let Some(obj) = o.as_object_mut() {
            obj.insert("self_id".into(), json!(self.self_id));
            obj.insert("group_id".into(), self.group_id.clone());
        }
        o
    }
    pub async fn send_msg(&self, msg: Value) -> Result<Value, String> {
        util::make_log(
            Level::Info,
            Some(&format!("{} => {}", self.self_id, self.group_id)),
            true,
            vec![format!("发送群消息：{}", crate::util::string(&msg))],
        );
        let bot = self.bot.get_bot(&self.self_id).unwrap();
        bot.protocol.send_group_msg(&self.ctx(), msg).await
    }
    pub async fn recall_msg(&self, message_id: Value) -> Result<Value, String> {
        let bot = self.bot.get_bot(&self.self_id).unwrap();
        bot.protocol.recall_msg(&self.ctx(), message_id).await
    }
    pub fn get_avatar_url(&self) -> String {
        self.data
            .get("avatar")
            .and_then(Value::as_str)
            .map(String::from)
            .unwrap_or_else(|| format!("https://p.qlogo.cn/gh/{}/{}/0", self.group_id, self.group_id))
    }
}

pub type Listener = Arc<dyn Fn(Arc<Bot>, Value) -> BoxFuture<'static, ()> + Send + Sync>;
pub type WsfHandler = Arc<
    dyn Fn(tokio::sync::mpsc::Receiver<axum::extract::ws::Message>, Arc<WsConn>) -> BoxFuture<'static, ()>
        + Send
        + Sync,
>;

/// WebSocket 连接封装
pub struct WsConn {
    pub id: u64,
    pub tx: tokio::sync::mpsc::UnboundedSender<axum::extract::ws::Message>,
    pub info: Value,
}

impl WsConn {
    pub fn send_msg(&self, msg: impl Into<Value>) {
        let v = msg.into();
        let s = crate::util::string(&v);
        util::make_log(
            Level::Debug,
            Some(&format!("{}", self.info.get("sid").and_then(Value::as_str).unwrap_or(""))),
            true,
            vec![format!("消息 => {}", s)],
        );
        let _ = self.tx.send(axum::extract::ws::Message::text(s));
    }
}

#[derive(Clone)]
struct BusEntry {
    id: u64,
    once: bool,
    f: Listener,
}

pub struct Bot {
    pub cfg: Arc<Cfg>,
    pub stat: AtomicU8, // 0 未启动 1 启动中 2 在线
    pub start_time: f64,
    pub bots: RwLock<HashMap<String, Arc<BotInstance>>>,
    pub uin: Mutex<Vec<String>>,
    pub adapters: RwLock<Vec<AdapterMeta>>,
    pub wsf: RwLock<HashMap<String, Vec<WsfHandler>>>,
    pub redis: RwLock<Option<Arc<crate::config::redis::RedisHandle>>>,
    pub loader: RwLock<Option<Arc<crate::plugins::loader::PluginsLoader>>>,
    pub url: RwLock<String>,
    bus: RwLock<HashMap<String, Vec<BusEntry>>>,
    next_id: AtomicU64,
    next_conn: AtomicU64,
}

pub static VERSION: Lazy<String> = Lazy::new(|| format!("v{}", env!("CARGO_PKG_VERSION")));

impl Bot {
    pub fn new(cfg: Arc<Cfg>) -> Arc<Bot> {
        Arc::new(Bot {
            cfg,
            stat: AtomicU8::new(0),
            start_time: util::now_ms() as f64 / 1000.0,
            bots: RwLock::new(HashMap::new()),
            uin: Mutex::new(Vec::new()),
            adapters: RwLock::new(Vec::new()),
            wsf: RwLock::new(HashMap::new()),
            redis: RwLock::new(None),
            loader: RwLock::new(None),
            url: RwLock::new(String::new()),
            bus: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            next_conn: AtomicU64::new(1),
        })
    }

    // ============ 事件总线（≈ EventEmitter + em 层级事件） ============

    pub fn on(&self, event: &str, f: Listener) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.bus
            .write()
            .unwrap()
            .entry(event.to_string())
            .or_default()
            .push(BusEntry { id, once: false, f });
        id
    }

    pub fn once(&self, event: &str, f: Listener) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.bus
            .write()
            .unwrap()
            .entry(event.to_string())
            .or_default()
            .push(BusEntry { id, once: true, f });
        id
    }

    pub fn off(&self, event: &str, id: u64) {
        if let Some(list) = self.bus.write().unwrap().get_mut(event) {
            list.retain(|e| e.id != id);
        }
    }

    /// 触发监听器 — 顺序执行（≈ JS 宏任务顺序语义，保证多轮消息的上下文时序）
    async fn emit(&self, self_arc: &Arc<Bot>, event: &str, data: Value) {
        let entries: Vec<BusEntry> = {
            let mut bus = self.bus.write().unwrap();
            match bus.get_mut(event) {
                Some(list) => {
                    let snap: Vec<BusEntry> = list
                        .iter()
                        .map(|e| BusEntry { id: e.id, once: e.once, f: e.f.clone() })
                        .collect();
                    list.retain(|e| !e.once);
                    snap
                }
                None => vec![],
            }
        };
        for entry in entries {
            (entry.f)(self_arc.clone(), data.clone()).await;
        }
    }

    /// ≈ em — 层级事件广播：message.group.normal → message.group → message
    pub async fn em(&self, self_arc: &Arc<Bot>, name: &str, data: Value) {
        let mut data = data;
        self.prepare_event(&mut data);
        let mut name = name.to_string();
        loop {
            self.emit(self_arc, &name, data.clone()).await;
            match name.rfind('.') {
                Some(i) => name = name[..i].to_string(),
                None => break,
            }
        }
    }

    /// ≈ prepareEvent — 填充 sender/group_name/adapter 信息
    fn prepare_event(&self, data: &mut Value) {
        let self_id = data.get("self_id").map(util::string).unwrap_or_default();
        let bot = match self.get_bot(&self_id) {
            Some(b) => b,
            None => return,
        };
        let user_id_val = data.get("user_id").cloned().unwrap_or(Value::Null);
        if let Some(obj) = data.as_object_mut() {
            obj.entry("sender".to_string())
                .or_insert(json!({ "user_id": user_id_val }));
        }
        // sender.nickname ← 好友/成员信息
        let user_id = data.get("user_id").map(util::string);
        if let Some(uid) = &user_id {
            let nick = bot
                .fl
                .read()
                .unwrap()
                .get(uid)
                .and_then(|f| f.get("nickname").or_else(|| f.get("card")).cloned())
                .or_else(|| {
                    bot.gml
                        .read()
                        .unwrap()
                        .values()
                        .filter_map(|m| m.get(uid))
                        .filter_map(|m| m.get("card").or_else(|| m.get("nickname")).cloned())
                        .next()
                });
            if let Some(n) = nick {
                if let Some(sender) = data.get_mut("sender").and_then(Value::as_object_mut) {
                    sender.entry("nickname".to_string()).or_insert(n);
                }
            }
        }
        let group_id = data.get("group_id").map(util::string);
        if let Some(gid) = &group_id {
            let gname = bot
                .gl
                .read()
                .unwrap()
                .get(gid)
                .and_then(|g| g.get("group_name").cloned());
            if let Some(g) = gname {
                if let Some(obj) = data.as_object_mut() {
                    obj.entry("group_name".to_string()).or_insert(g);
                }
            }
        }
        if let Some(obj) = data.as_object_mut() {
            obj.insert("adapter_id".into(), json!(bot.adapter_id));
            obj.insert("adapter_name".into(), json!(bot.adapter_name));
        }
    }

    // ============ bots 注册表 ============

    pub fn get_bot(&self, self_id: &str) -> Option<Arc<BotInstance>> {
        self.bots.read().unwrap().get(self_id).cloned()
    }

    pub fn loader_arc(&self) -> Option<Arc<crate::plugins::loader::PluginsLoader>> {
        self.loader.read().unwrap().clone()
    }

    pub fn redis_arc(&self) -> Option<Arc<crate::config::redis::RedisHandle>> {
        self.redis.read().unwrap().clone()
    }

    pub fn register_bot(&self, instance: BotInstance) {
        self.bots
            .write()
            .unwrap()
            .insert(instance.self_id.clone(), Arc::new(instance));
    }

    pub fn add_uin(&self, self_id: &str) {
        let mut uin = self.uin.lock().unwrap();
        if !uin.iter().any(|i| i == self_id) {
            uin.push(self_id.to_string());
        }
    }

    pub fn remove_bot(&self, self_id: &str) {
        self.bots.write().unwrap().remove(self_id);
        self.uin.lock().unwrap().retain(|i| i != self_id);
    }

    /// ≈ uin.toJSON — 多账号随机选择语义
    pub fn uin_pick(&self) -> Option<String> {
        let uin = self.uin.lock().unwrap();
        match uin.len() {
            0 => None,
            1 | 2 => uin.last().cloned(),
            _ => {
                // 随机选取 slice(1)（跳过第一个连接的账号）
                let array = &uin[1..];
                let i = (util::now_ms() % (array.len() as u64 * 7919)) as usize % array.len();
                Some(array[i].clone())
            }
        }
    }

    pub fn adapter_push(&self, meta: AdapterMeta) {
        let mut list = self.adapters.write().unwrap();
        if !list.iter().any(|a| a.path == meta.path) {
            list.push(meta);
        }
    }

    pub fn wsf_add(&self, path: &str, handler: WsfHandler) {
        self.wsf.write().unwrap().entry(path.to_string()).or_default().push(handler);
    }

    // ============ pick 门面（≈ pickFriend/pickGroup 多账号路由） ============

    pub fn pick_friend(&self, self_arc: &Arc<Bot>, user_id: Value, strict: bool) -> Option<Friend> {
        let user_id = util::to_number(&user_id);
        let uid = util::string(&user_id);
        // 默认账号的好友表
        if let Some(default_id) = self.uin_pick() {
            if let Some(bot) = self.get_bot(&default_id) {
                if bot.fl.read().unwrap().contains_key(&uid) {
                    return Some(Friend { bot: self_arc.clone(), self_id: default_id, user_id, data: json!({}) });
                }
            }
        }
        // 遍历所有账号好友表
        for bot_id in self.uin.lock().unwrap().clone() {
            if let Some(bot) = self.get_bot(&bot_id) {
                if let Some(info) = bot.fl.read().unwrap().get(&uid) {
                    return Some(Friend {
                        bot: self_arc.clone(),
                        self_id: bot_id,
                        user_id,
                        data: info.clone(),
                    });
                }
            }
        }
        // 群成员表
        for bot_id in self.uin.lock().unwrap().clone() {
            if let Some(bot) = self.get_bot(&bot_id) {
                let found = bot.gml.read().unwrap().values().any(|m| m.contains_key(&uid));
                if found {
                    return Some(Friend { bot: self_arc.clone(), self_id: bot_id, user_id, data: json!({}) });
                }
            }
        }
        if strict {
            return None;
        }
        let fallback = self.uin_pick()?;
        util::make_log(
            Level::Debug,
            None,
            false,
            vec![format!("因不存在用户 {} 而随机选择Bot {:?}", uid, self.uin.lock().unwrap())],
        );
        Some(Friend { bot: self_arc.clone(), self_id: fallback, user_id, data: json!({}) })
    }

    pub fn pick_group(&self, self_arc: &Arc<Bot>, group_id: Value, strict: bool) -> Option<Group> {
        let group_id = util::to_number(&group_id);
        let gid = util::string(&group_id);
        if let Some(default_id) = self.uin_pick() {
            if let Some(bot) = self.get_bot(&default_id) {
                if bot.gl.read().unwrap().contains_key(&gid) {
                    return Some(Group { bot: self_arc.clone(), self_id: default_id, group_id, data: json!({}) });
                }
            }
        }
        for bot_id in self.uin.lock().unwrap().clone() {
            if let Some(bot) = self.get_bot(&bot_id) {
                if let Some(info) = bot.gl.read().unwrap().get(&gid) {
                    return Some(Group {
                        bot: self_arc.clone(),
                        self_id: bot_id,
                        group_id,
                        data: info.clone(),
                    });
                }
            }
        }
        if strict {
            return None;
        }
        let fallback = self.uin_pick()?;
        util::make_log(
            Level::Debug,
            None,
            false,
            vec![format!("因不存在群 {} 而随机选择Bot {:?}", gid, self.uin.lock().unwrap())],
        );
        Some(Group { bot: self_arc.clone(), self_id: fallback, group_id, data: json!({}) })
    }

    /// ≈ sendMasterMsg — 发送主人消息
    pub async fn send_master_msg(&self, self_arc: &Arc<Bot>, msg: Value) {
        let masters = self.cfg.master();
        let mut first = true;
        for (_bot_id, users) in masters {
            for user_id in users {
                if first {
                    first = false;
                } else {
                    util::sleep(5000).await;
                }
                if let Some(f) = self.pick_friend(self_arc, json!(user_id), true) {
                    let _ = f.send_msg(msg.clone()).await;
                }
            }
        }
    }

    /// ≈ makeForwardMsg
    pub fn make_forward_msg(msg: Value) -> Value {
        crate::segment::node(msg)
    }

    /// ≈ sendForwardMsg — 逐条发送转发节点
    pub async fn send_forward_msg(
        send: impl Fn(Value) -> BoxFuture<'static, Result<Value, String>>,
        msg: Value,
    ) -> Vec<Value> {
        let mut ret = vec![];
        let nodes = match msg {
            Value::Array(a) => a,
            other => vec![other],
        };
        for node in nodes {
            let message = node.get("message").cloned().unwrap_or(Value::Null);
            if let Ok(r) = send(message).await {
                ret.push(r);
            }
        }
        ret
    }

    /// ≈ cleanupBotConnection — WS 断开清理对应 bot
    pub fn cleanup_bot_connection(&self, conn_id: u64) {
        let to_remove: Vec<String> = self
            .bots
            .read()
            .unwrap()
            .values()
            .filter(|b| b.conn_id == conn_id)
            .map(|b| b.self_id.clone())
            .collect();
        for id in to_remove {
            self.remove_bot(&id);
        }
    }

    // ============ 启动 / 退出 ============

    /// ≈ Bot.run
    pub async fn run(self: &Arc<Self>) {
        if self.stat.load(Ordering::Relaxed) != 0 {
            return;
        }
        self.stat.store(1, Ordering::Relaxed);
        // redis
        let handle = crate::config::redis::redis_init(&self.cfg).await;
        if let Some(h) = handle {
            *self.redis.write().unwrap() = Some(Arc::new(h));
        }
        // HTTP+WS 服务器
        self.server_load().await;
        // 插件加载
        {
            let loader = Arc::new(crate::plugins::loader::PluginsLoader::new());
            loader.load(self).await;
            *self.loader.write().unwrap() = Some(loader);
        }
        // 事件与适配器
        crate::listener::load(self).await;

        self.stat.store(2, Ordering::Relaxed);
        let wsf_keys: Vec<String> = self.wsf.read().unwrap().keys().cloned().collect();
        let url = self.url.read().unwrap().clone();
        util::make_log(
            Level::Info,
            Some("WebSocket"),
            false,
            vec![format!(
                "连接地址：{}{}",
                logger::blue(url.replace("http", "ws")),
                logger::cyan(format!("[{}]", wsf_keys.join(",")))
            )],
        );
        self.em(self, "online", json!({})).await;
    }

    /// ≈ serverLoad + serverEADDRINUSE
    async fn server_load(self: &Arc<Self>) {
        let port = self
            .cfg
            .get("server")
            .get("port")
            .and_then(Value::as_u64)
            .unwrap_or(2536);
        let url = self
            .cfg
            .get("server")
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or(&format!("http://localhost:{}", port))
            .to_string();
        *self.url.write().unwrap() = url;

        let mut listen_time = 0u64;
        loop {
            let addr = format!("0.0.0.0:{}", port);
            match tokio::net::TcpListener::bind(&addr).await {
                Ok(l) => {
                    util::make_log(
                        Level::Mark,
                        Some("Server"),
                        false,
                        vec![format!(
                            "启动 HTTP 服务器 {}",
                            logger::green(format!("http://[0.0.0.0]:{}", port))
                        )],
                    );
                    let bot = self.clone();
                    tokio::spawn(async move {
                        let app = axum::Router::new()
                            .route("/exit", axum::routing::any(server_exit))
                            .route("/status", axum::routing::get(server_status))
                            .fallback(server_fallback)
                            .with_state(bot.clone());
                        let _ = axum::serve(
                            l,
                            app.into_make_service_with_connect_info::<SocketAddr>(),
                        )
                        .await;
                    });
                    break;
                }
                Err(err) => {
                    util::make_log(
                        Level::Error,
                        Some("Server"),
                        false,
                        vec![format!("监听端口 {} 错误 {}", port, err)],
                    );
                    // 尝试让旧实例退出
                    let _ = reqwest_exit(port).await;
                    listen_time += 1;
                    util::sleep(listen_time * 1000).await;
                }
            }
        }
    }

    /// ≈ exit — 退出清理
    pub async fn exit(self: &Arc<Self>, code: i32) {
        // exit 事件监听（最多等待 10 秒）
        let entries: Vec<BusEntry> = {
            let bus = self.bus.read().unwrap();
            bus.get("exit").map(|l| l.clone()).unwrap_or_default()
        };
        if !entries.is_empty() {
            let mut tasks = vec![];
            for e in entries {
                let bot = self.clone();
                tasks.push(tokio::spawn((e.f)(bot, json!({}))));
            }
            for t in tasks {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(10), t).await;
            }
        }
        // 关闭 redis（子进程随进程退出由系统回收）
        util::make_log(
            Level::Mark,
            Some("exit"),
            false,
            vec![format!(
                "TRSS-Yunzai 已停止运行，本次运行时长：{} ({})",
                util::get_time_diff((self.start_time * 1000.0) as u64, util::now_ms()),
                code
            )],
        );
        std::process::exit(code);
    }
}

async fn reqwest_exit(port: u64) -> Result<(), String> {
    let url = format!("http://127.0.0.1:{}/exit", port);
    // 无 reqwest 依赖，用原始 TCP 发送 GET
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Ok(mut stream) = tokio::net::TcpStream::connect(("127.0.0.1", port as u16)).await else {
        return Err("connect fail".into());
    };
    let _ = stream
        .write_all(format!("GET /exit HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n", port).as_bytes())
        .await;
    let mut buf = vec![0u8; 256];
    let _ = stream.read(&mut buf).await;
    let _ = url;
    Ok(())
}

// ============ axum 处理器 ============

async fn server_exit(
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    State(bot): State<Arc<Bot>>,
) -> impl axum::response::IntoResponse {
    if !addr.ip().is_loopback() {
        return axum::http::StatusCode::FORBIDDEN;
    }
    let b = bot.clone();
    tokio::spawn(async move { b.exit(1).await });
    axum::http::StatusCode::OK
}

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequest, State};
use axum::response::IntoResponse;

async fn server_status(State(_bot): State<Arc<Bot>>) -> axum::response::Response {
    let report = json!({
        "pid": std::process::id(),
        "uptime": util::now_ms() as f64 / 1000.0,
    });
    axum::response::Json(report).into_response()
}

async fn server_fallback(
    State(bot): State<Arc<Bot>>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
    req: axum::extract::Request,
) -> axum::response::Response {
    use axum::http::header;
    let is_ws = req.headers().contains_key(header::SEC_WEBSOCKET_KEY);
    let path = req
        .uri()
        .path()
        .trim_start_matches('/')
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    let query: HashMap<String, String> = req
        .uri()
        .query()
        .map(|q| {
            q.split('&')
                .filter_map(|kv| {
                    let mut it = kv.splitn(2, '=');
                    Some((it.next()?.to_string(), it.next().unwrap_or("").to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    let rid = format!("{}:{}", addr.ip(), addr.port());
    let sid = format!("ws://localhost:2536/{}", path);

    // ≈ serverAuth — 逐项校验 headers/query
    let auth = bot.cfg.get("server").get("auth").cloned().unwrap_or(Value::Null);
    if let Value::Object(auth_map) = &auth {
        if !auth_map.is_empty() {
            for (k, v) in auth_map {
                let hv = req
                    .headers()
                    .get(k.as_str())
                    .and_then(|h| h.to_str().ok())
                    .unwrap_or("");
                let qv = query.get(k).map(String::as_str).unwrap_or("");
                if hv != util::string(v) && qv != util::string(v) {
                    util::make_log(
                        Level::Error,
                        Some(&format!("{} <≠ {}", sid, rid)),
                        true,
                        vec![format!("HTTP 请求 {} 鉴权失败", k)],
                    );
                    return axum::http::StatusCode::UNAUTHORIZED.into_response();
                }
            }
        }
    }

    if is_ws {
        // ≈ wsConnect — 路径路由
        let handlers: Vec<WsfHandler> = match bot.wsf.read().unwrap().get(&path) {
            Some(h) => h.clone(),
            None => vec![],
        };
        if handlers.is_empty() {
            util::make_log(
                Level::Error,
                Some(&format!("{} <≠> {}", sid, rid)),
                true,
                vec![format!("WebSocket 处理器 {} 不存在", path)],
            );
            return axum::http::StatusCode::NOT_FOUND.into_response();
        }
        let ws = match WebSocketUpgrade::from_request(req, &()).await {
            Ok(ws) => ws,
            Err(_) => return axum::http::StatusCode::BAD_REQUEST.into_response(),
        };
        let info = json!({ "sid": sid, "rid": rid, "path": path, "query": query });
        util::make_log(
            Level::Mark,
            Some(&format!("{} <=> {}", sid, rid)),
            true,
            vec!["建立连接".into()],
        );
        let bot2 = bot.clone();
        return ws.on_upgrade(move |socket| handle_ws(bot2, path, socket, info));
    }

    // HTTP 回退跳转
    let redirect = bot
        .cfg
        .get("server")
        .get("redirect")
        .and_then(Value::as_str)
        .unwrap_or("https://git.trss.me/Yunzai")
        .to_string();
    axum::response::Redirect::to(&redirect).into_response()
}

async fn handle_ws(bot: Arc<Bot>, path: String, socket: axum::extract::ws::WebSocket, info: Value) {
    let conn_id = bot.next_conn.fetch_add(1, Ordering::Relaxed);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let conn = Arc::new(WsConn { id: conn_id, tx, info: info.clone() });

    // 写泵
    let (mut sink, mut stream) = socket.split();
    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    // 分发到 wsf 处理器
    let handlers: Vec<WsfHandler> = bot
        .wsf
        .read()
        .unwrap()
        .get(&path)
        .cloned()
        .unwrap_or_default();
    let mut rxs = vec![];
    for h in &handlers {
        let (htx, hrx) = tokio::sync::mpsc::channel(64);
        rxs.push(htx);
        let conn = conn.clone();
        tokio::spawn(h(hrx, conn));
    }

    let bot2 = bot.clone();
    let sid = info.get("sid").and_then(Value::as_str).unwrap_or("").to_string();
    let read_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = stream.next().await {
            match &msg {
                axum::extract::ws::Message::Text(t) => {
                    util::make_log(
                        Level::Debug,
                        Some(&sid),
                        true,
                        vec![format!("消息 <= {}", t.as_str())],
                    );
                }
                axum::extract::ws::Message::Close(_) => break,
                _ => {}
            }
            for rtx in &rxs {
                if rtx.send(msg.clone()).await.is_err() {
                    break;
                }
            }
        }
        util::make_log(
            Level::Mark,
            Some(&format!("{} <≠> {}", sid, conn_id)),
            true,
            vec!["断开连接".into()],
        );
        bot2.cleanup_bot_connection(conn_id);
    });

    let _ = writer.await;
    read_task.abort();
}
