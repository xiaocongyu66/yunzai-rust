//! ≈ plugins/adapter/OneBotv11.js — OneBot v11 反向 WebSocket 适配器
//! 协议端（NapCat/Lagrange/LLOneBot/go-cqhttp）连入 ws://host:2536/OneBotv11
use crate::bot::{AdapterMeta, Bot, BotInstance, ProtocolImpl, WsConn};
use crate::logger::{self, Level};
use crate::util;
use futures::future::BoxFuture;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use tokio::sync::oneshot;

pub const PATH: &str = "OneBotv11";
pub const ID: &str = "QQ";
pub const NAME: &str = "OneBotv11";
const TIMEOUT_MS: u64 = 60000;

#[allow(dead_code)]
pub fn meta() -> AdapterMeta {
    AdapterMeta { id: ID.into(), name: NAME.into(), path: PATH.into() }
}

/// 每条 WS 连接的共享状态
pub struct ConnState {
    pub bot: RwLock<Option<Arc<Bot>>>,
    pub ws: Arc<WsConn>,
    pub echo: Mutex<HashMap<String, oneshot::Sender<Value>>>,
}

impl ConnState {
    /// ≈ sendApi — {action,params,echo} 请求 + echo 应答 + 60s 超时
    async fn send_api(&self, self_id: &str, action: &str, params: Value) -> Result<Value, String> {
        let echo = ulid::Ulid::new().to_string();
        let request = json!({ "action": action, "params": params, "echo": echo });
        self.ws.send_msg(request);
        let (tx, rx) = oneshot::channel();
        self.echo.lock().unwrap().insert(echo.clone(), tx);
        match tokio::time::timeout(std::time::Duration::from_millis(TIMEOUT_MS), rx).await {
            Ok(Ok(data)) => {
                let retcode = data.get("retcode").and_then(Value::as_i64).unwrap_or(-1);
                if retcode != 0 && retcode != 1 {
                    let msg = data
                        .get("msg")
                        .or_else(|| data.get("wording"))
                        .map(util::string)
                        .unwrap_or_default();
                    return Err(format!("{} ({})", msg, util::string(&data)));
                }
                match data.get("data") {
                    Some(d) if !d.is_null() => Ok(d.clone()),
                    _ => Ok(data),
                }
            }
            Ok(Err(_)) => {
                self.echo.lock().unwrap().remove(echo.as_str());
                Err(format!("{} 连接已断开", self_id))
            }
            Err(_) => {
                self.echo.lock().unwrap().remove(echo.as_str());
                util::make_log(
                    Level::Error,
                    Some(self_id),
                    false,
                    vec![format!("请求超时 {}({})", action, util::string(&params))],
                );
                let _ = self.ws.tx.send(axum::extract::ws::Message::Close(None));
                Err(format!("请求超时 {}", action))
            }
        }
    }

    /// ≈ makeFile — 文件 → base64:// 或原样 URL/path
    async fn make_file(&self, file: Value, opts: Value) -> Value {
        let mut o = json!({ "http": true, "size": 10485760 });
        if let Value::Object(m) = &opts {
            if let Some(map) = o.as_object_mut() {
                for (k, v) in m {
                    map.insert(k.clone(), v.clone());
                }
            }
        }
        util::buffer(file, &o).await
    }

    /// ≈ makeMsg — 段规范化：扁平 oicq → 嵌套 {type,data}，file 剥离，node 转发
    async fn make_msg(
        &self,
        msg: &Value,
        split_file: bool,
    ) -> (Vec<Value>, Vec<Value>, Vec<(Value, Value)>) {
        let mut msgs = vec![];
        let mut forward = vec![];
        let mut files = vec![];
        for i in crate::segment::to_array(msg) {
            let mut i = match i {
                Value::Object(_) => {
                    let mut obj = i.as_object().unwrap().clone();
                    let typ = obj.get("type").cloned().unwrap_or(json!("text"));
                    obj.remove("type");
                    json!({ "type": typ, "data": Value::Object(obj) })
                }
                other => json!({ "type": "text", "data": { "text": other } }),
            };
            let typ = i.get("type").and_then(Value::as_str).unwrap_or("").to_string();
            match typ.as_str() {
                "at" => {
                    i["data"]["qq"] = json!(util::string(&i["data"]["qq"]));
                }
                "reply" => {
                    i["data"]["id"] = json!(util::string(&i["data"]["id"]));
                }
                "button" => continue,
                "node" => {
                    if let Some(arr) = i.get("data").and_then(Value::as_array) {
                        forward.extend(arr.clone());
                    }
                    continue;
                }
                "file" if split_file => {
                    files.push((
                        i["data"]["file"].clone(),
                        i["data"]["name"].clone(),
                    ));
                    continue;
                }
                "raw" => {
                    i = i["data"].clone();
                }
                _ => {}
            }
            if !i["data"]["file"].is_null() {
                let f = self.make_file(i["data"]["file"].clone(), json!({})).await;
                i["data"]["file"] = f;
            }
            msgs.push(i);
        }
        (msgs, forward, files)
    }

    /// ≈ sendMsg 通用拆分发送
    async fn send_split(
        &self,
        self_id: &str,
        msg: &Value,
        with_file: bool,
        mut send: Box<dyn FnOnce(Value) -> BoxFuture<'static, Result<Value, String>> + Send>,
        mut send_forward: Box<dyn FnOnce(Value) -> BoxFuture<'static, Result<Value, String>> + Send>,
        mut send_file: Option<Box<dyn FnOnce(Value, Value) -> BoxFuture<'static, Result<Value, String>> + Send>>,
    ) -> Result<Value, String> {
        let (message, forward, files) = self.make_msg(msg, with_file).await;
        let mut ret: Vec<Value> = vec![];
        if !forward.is_empty() {
            ret.push(send_forward(json!(forward)).await?);
        }
        for (file, name) in files {
            if let Some(cb) = send_file.take() {
                ret.push(cb(file, name).await?);
            }
        }
        if !message.is_empty() {
            ret.push(send(json!(message)).await?);
        }
        if ret.len() == 1 {
            return Ok(ret.remove(0));
        }
        let message_ids: Vec<Value> = ret
            .iter()
            .filter_map(|r| r.get("message_id").cloned())
            .collect();
        Ok(json!({ "data": ret, "message_id": message_ids }))
    }

    /// ≈ makeForwardMsg — 转发节点构造
    async fn make_forward_msg(&self, msg: &Value) -> Value {
        let mut msgs = vec![];
        for i in crate::segment::to_array(msg) {
            let (content, forward, _files) = self.make_msg(&i.get("message").cloned().unwrap_or(Value::Null), false).await;
            if !forward.is_empty() {
                let f = Box::pin(self.make_forward_msg(&json!(forward))).await;
                if let Some(arr) = f.as_array() {
                    msgs.extend(arr.clone());
                }
            }
            if !content.is_empty() {
                let nickname = i
                    .get("nickname")
                    .map(util::string)
                    .unwrap_or_else(|| "匿名消息".into());
                let uin_num = util::to_number(&i.get("user_id").cloned().unwrap_or(Value::Null));
                let uin = if uin_num.is_number() {
                    util::string(&uin_num)
                } else {
                    "80000000".to_string()
                };
                msgs.push(json!({
                    "type": "node",
                    "data": {
                        "name": nickname,
                        "uin": uin,
                        "content": content,
                        "time": i.get("time").cloned().unwrap_or(Value::Null),
                    }
                }));
            }
        }
        json!(msgs)
    }
}

/// 每个账号的协议实现
pub struct Ob11Protocol {
    pub state: Arc<ConnState>,
    pub self_id: String,
}

#[async_trait::async_trait]
impl ProtocolImpl for Ob11Protocol {
    async fn send_api(&self, action: &str, params: Value) -> Result<Value, String> {
        self.state.send_api(&self.self_id, action, params).await
    }

    async fn send_friend_msg(&self, ctx: &Value, msg: Value) -> Result<Value, String> {
        let self_id = util::string(&ctx["self_id"]);
        let user_id = ctx["user_id"].clone();
        self.state
            .send_split(
                &self_id,
                &msg,
                true,
                {
                    let state = self.state.clone();
                    let self_id = self_id.clone();
                    let user_id = user_id.clone();
                    Box::new(move |message| {
                        Box::pin(async move {
                            util::make_log(
                                Level::Info,
                                Some(&format!("{} => {}", self_id, util::string(&user_id))),
                                true,
                                vec![format!("发送好友消息：{}", util::string(&message))],
                            );
                            state
                                .send_api(&self_id, "send_msg", json!({ "user_id": user_id, "message": message }))
                                .await
                        })
                    })
                },
                {
                    let state = self.state.clone();
                    let self_id = self_id.clone();
                    let user_id = user_id.clone();
                    Box::new(move |forward| {
                        Box::pin(async move {
                            util::make_log(
                                Level::Info,
                                Some(&format!("{} => {}", self_id, util::string(&user_id))),
                                true,
                                vec!["发送好友转发消息".into()],
                            );
                            state
                                .send_api(&self_id, "send_private_forward_msg", json!({ "user_id": user_id, "messages": forward }))
                                .await
                        })
                    })
                },
                Some(Box::new({
                    let protocol = Ob11Protocol { state: self.state.clone(), self_id: self_id.clone() };
                    move |file, name| {
                        Box::pin(async move {
                            protocol.send_friend_file_raw(&user_id, file, name).await
                        })
                    }
                })),
            )
            .await
    }

    async fn send_group_msg(&self, ctx: &Value, msg: Value) -> Result<Value, String> {
        let self_id = util::string(&ctx["self_id"]);
        let group_id = ctx["group_id"].clone();
        self.state
            .send_split(
                &self_id,
                &msg,
                true,
                {
                    let state = self.state.clone();
                    let self_id = self_id.clone();
                    let group_id = group_id.clone();
                    Box::new(move |message| {
                        Box::pin(async move {
                            util::make_log(
                                Level::Info,
                                Some(&format!("{} => {}", self_id, util::string(&group_id))),
                                true,
                                vec![format!("发送群消息：{}", util::string(&message))],
                            );
                            state
                                .send_api(&self_id, "send_msg", json!({ "group_id": group_id, "message": message }))
                                .await
                        })
                    })
                },
                {
                    let state = self.state.clone();
                    let self_id = self_id.clone();
                    let group_id = group_id.clone();
                    Box::new(move |forward| {
                        Box::pin(async move {
                            util::make_log(
                                Level::Info,
                                Some(&format!("{} => {}", self_id, util::string(&group_id))),
                                true,
                                vec!["发送群转发消息".into()],
                            );
                            state
                                .send_api(&self_id, "send_group_forward_msg", json!({ "group_id": group_id, "messages": forward }))
                                .await
                        })
                    })
                },
                Some(Box::new({
                    let protocol = Ob11Protocol { state: self.state.clone(), self_id: self_id.clone() };
                    let group_id = group_id.clone();
                    move |file, name| {
                        Box::pin(async move {
                            protocol.send_group_file_raw(&group_id, &Value::Null, file, name).await
                        })
                    }
                })),
            )
            .await
    }

    async fn recall_msg(&self, ctx: &Value, message_id: Value) -> Result<Value, String> {
        let self_id = util::string(&ctx["self_id"]);
        util::make_log1(Level::Info, Some(&self_id), format!("撤回消息：{}", util::string(&message_id)));
        let ids = match message_id {
            Value::Array(a) => a,
            other => vec![other],
        };
        let mut msgs = vec![];
        for id in ids {
            let r = self
                .state
                .send_api(&self_id, "delete_msg", json!({ "message_id": id }))
                .await
                .unwrap_or(Value::Null);
            msgs.push(r);
        }
        Ok(json!(msgs))
    }

    async fn send_friend_forward_msg(&self, ctx: &Value, msg: Value) -> Result<Value, String> {
        let self_id = util::string(&ctx["self_id"]);
        let user_id = ctx["user_id"].clone();
        let messages = self.state.make_forward_msg(&msg).await;
        self.state
            .send_api(&self_id, "send_private_forward_msg", json!({ "user_id": user_id, "messages": messages }))
            .await
    }

    async fn send_group_forward_msg(&self, ctx: &Value, msg: Value) -> Result<Value, String> {
        let self_id = util::string(&ctx["self_id"]);
        let group_id = ctx["group_id"].clone();
        let messages = self.state.make_forward_msg(&msg).await;
        self.state
            .send_api(&self_id, "send_group_forward_msg", json!({ "group_id": group_id, "messages": messages }))
            .await
    }

    async fn send_friend_file(&self, ctx: &Value, file: Value, name: Value) -> Result<Value, String> {
        let self_id = util::string(&ctx["self_id"]);
        let user_id = ctx["user_id"].clone();
        self.send_friend_file_raw(&user_id, file, name).await?;
        let _ = self_id;
        Ok(json!(null))
    }

    async fn send_group_file(&self, ctx: &Value, file: Value, folder: Value, name: Value) -> Result<Value, String> {
        let self_id = util::string(&ctx["self_id"]);
        let group_id = ctx["group_id"].clone();
        self.send_group_file_raw(&group_id, &folder, file, name).await?;
        let _ = self_id;
        Ok(json!(null))
    }

    async fn get_friend_array(&self) -> Result<Value, String> {
        let r = self
            .state
            .send_api(&self.self_id, "get_friend_list", json!({}))
            .await?;
        Ok(r.as_array().cloned().unwrap_or_default().into())
    }

    async fn get_group_array(&self) -> Result<Value, String> {
        let r = self
            .state
            .send_api(&self.self_id, "get_group_list", json!({}))
            .await?;
        Ok(r.as_array().cloned().unwrap_or_default().into())
    }

    async fn get_member_array(&self, group_id: &Value) -> Result<Value, String> {
        let r = self
            .state
            .send_api(&self.self_id, "get_group_member_list", json!({ "group_id": group_id }))
            .await?;
        Ok(r.as_array().cloned().unwrap_or_default().into())
    }

    async fn get_msg(&self, message_id: &Value) -> Result<Value, String> {
        let mut msg = self
            .state
            .send_api(&self.self_id, "get_msg", json!({ "message_id": message_id }))
            .await?;
        if let Some(m) = msg.get_mut("message") {
            *m = parse_msg(m);
        }
        Ok(msg)
    }
}

impl Ob11Protocol {
    async fn send_friend_file_raw(&self, user_id: &Value, file: Value, name: Value) -> Result<Value, String> {
        let self_id = self.self_id.clone();
        let name = match &name {
            Value::String(s) if !s.is_empty() => s.clone(),
            _ => file
                .as_str()
                .map(|f| f.rsplit('/').next().unwrap_or(f).to_string())
                .unwrap_or_default(),
        };
        util::make_log(
            Level::Info,
            Some(&format!("{} => {}", self_id, util::string(user_id))),
            true,
            vec![format!("发送好友文件：{}({})", name, util::string(&file))],
        );
        let f = self.state.make_file(file.clone(), json!({ "file": true })).await;
        let f = util::string(&f).replace("file://", "");
        self.state
            .send_api(&self_id, "upload_private_file", json!({ "user_id": user_id, "file": f, "name": name }))
            .await
    }

    async fn send_group_file_raw(&self, group_id: &Value, folder: &Value, file: Value, name: Value) -> Result<Value, String> {
        let self_id = self.self_id.clone();
        let name = match &name {
            Value::String(s) if !s.is_empty() => s.clone(),
            _ => file
                .as_str()
                .map(|f| f.rsplit('/').next().unwrap_or(f).to_string())
                .unwrap_or_default(),
        };
        util::make_log(
            Level::Info,
            Some(&format!("{} => {}", self_id, util::string(group_id))),
            true,
            vec![format!("发送群文件：{}/{}({})", util::string(folder), name, util::string(&file))],
        );
        let f = self.state.make_file(file.clone(), json!({ "file": true })).await;
        let f = util::string(&f).replace("file://", "");
        self.state
            .send_api(
                &self_id,
                "upload_group_file",
                json!({ "group_id": group_id, "folder": folder, "file": f, "name": name }),
            )
            .await
    }
}

// ============ 消息解析 ============

/// ≈ parseMsg — 嵌套段 → 扁平段
pub fn parse_msg(msg: &Value) -> Value {
    let arr = msg.as_array().cloned().unwrap_or_else(|| vec![msg.clone()]);
    let mut out = vec![];
    for i in arr {
        match &i {
            Value::Object(_) => {
                let mut obj = i.as_object().unwrap().clone();
                let typ = obj.get("type").cloned().unwrap_or(json!("text"));
                let data = obj.get("data").cloned().unwrap_or(json!({}));
                let mut flat = match data {
                    Value::Object(m) => m,
                    _ => {
                        let mut m = serde_json::Map::new();
                        m.insert("data".into(), data);
                        m
                    }
                };
                flat.insert("type".into(), typ);
                out.push(Value::Object(flat));
            }
            other => out.push(json!({ "type": "text", "text": util::string(other) })),
        }
    }
    json!(out)
}

/// ≈ adapter load — 注册 WS 处理器
pub fn load(bot: &Arc<Bot>) {
    bot.adapter_push(meta());
    let bot2 = bot.clone();
    bot.wsf_add(
        PATH,
        Arc::new(move |mut rx, conn| {
            let bot = bot2.clone();
            Box::pin(async move {
                let state = Arc::new(ConnState {
                    bot: RwLock::new(Some(bot)),
                    ws: conn,
                    echo: Mutex::new(HashMap::new()),
                });
                // 事件顺序处理 worker（≈ JS 宏任务顺序）；echo 应答由主循环即时解析，
                // 否则 send_api 等待的应答会排在被阻塞的事件后面形成死锁
                let (evt_tx, mut evt_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                {
                    let state = state.clone();
                    tokio::spawn(async move {
                        while let Some(text) = evt_rx.recv().await {
                            state.on_message(text).await;
                        }
                    });
                }
                while let Some(msg) = rx.recv().await {
                    let text = match msg {
                        axum::extract::ws::Message::Text(t) => t.to_string(),
                        axum::extract::ws::Message::Close(_) => break,
                        _ => continue,
                    };
                    let is_echo = serde_json::from_str::<Value>(&text).map(|v| {
                        v.get("echo").map(|e| !e.is_null()).unwrap_or(false)
                            && v.get("action").is_none()
                            && v.get("post_type").is_none()
                    }).unwrap_or(false);
                    if is_echo {
                        if let Ok(v) = serde_json::from_str::<Value>(&text) {
                            let echo = util::string(&v["echo"]);
                            if let Some(tx) = state.echo.lock().unwrap().remove(&echo) {
                                let _ = tx.send(v);
                            }
                        }
                        continue;
                    }
                    let _ = evt_tx.send(text);
                }
            })
        }),
    );
}

impl ConnState {
    /// ≈ message(data, ws) — WS 消息总入口
    async fn on_message(self: &Arc<Self>, raw: String) {
        let bot = match self.bot.read().unwrap().clone() {
            Some(b) => b,
            None => return,
        };
        let mut data: Value = match serde_json::from_str(&raw) {
            Ok(d) => d,
            Err(err) => {
                util::make_log1(Level::Error, None, format!("解码数据失败 {} {}", raw, err));
                return;
            }
        };
        data["raw"] = json!(raw);

        if !data.get("post_type").map(util::string).unwrap_or_default().is_empty() {
            let meta_type = data.get("meta_event_type").map(util::string).unwrap_or_default();
            let self_id = util::string(&data.get("self_id").cloned().unwrap_or(Value::Null));
            if meta_type != "lifecycle" && !uin_includes(&bot, &self_id) {
                util::make_log1(
                    Level::Warn,
                    Some(&self_id),
                    format!("找不到对应Bot，忽略消息：{}", logger::magenta(&raw)),
                );
                return;
            }

            let post_type = data.get("post_type").map(util::string).unwrap_or_default();
            match post_type.as_str() {
                "meta_event" => self.make_meta(bot, data).await,
                "message" => make_message(bot.clone(), data).await,
                "notice" => make_notice(bot.clone(), data).await,
                "request" => make_request(bot.clone(), data).await,
                "message_sent" => {
                    data["post_type"] = json!("message");
                    make_message(bot.clone(), data).await;
                }
                _ => {
                    util::make_log1(
                        Level::Warn,
                        Some(&self_id),
                        format!("未知消息：{}", logger::magenta(&raw)),
                    );
                }
            }
        } else if !data.get("echo").map(util::string).unwrap_or_default().is_empty() {
            let echo = util::string(&data["echo"]);
            if let Some(tx) = self.echo.lock().unwrap().remove(&echo) {
                let _ = tx.send(data);
            }
        } else {
            util::make_log1(
                Level::Warn,
                Some(&util::string(&data.get("self_id").cloned().unwrap_or(Value::Null))),
                format!("未知消息：{}", logger::magenta(&raw)),
            );
        }
    }

    /// ≈ makeMeta — heartbeat / lifecycle
    async fn make_meta(self: &Arc<Self>, bot: Arc<Bot>, data: Value) {
        match data.get("meta_event_type").map(util::string).unwrap_or_default().as_str() {
            "heartbeat" => {
                if let Some(status) = data.get("status") {
                    if let Some(self_id) = data.get("self_id").map(util::string) {
                        if let Some(instance) = bot.get_bot(&self_id) {
                            if let (Some(st), Some(s)) =
                                (status.as_object(), instance.stat.write().unwrap().as_object_mut())
                            {
                                for (k, v) in st {
                                    s.insert(k.clone(), v.clone());
                                }
                            }
                        }
                    }
                }
            }
            "lifecycle" => connect(self, bot, data).await,
            _ => {
                util::make_log1(
                    Level::Warn,
                    Some(&util::string(&data.get("self_id").cloned().unwrap_or(Value::Null))),
                    format!("未知消息：{}", logger::magenta(&data["raw"].as_str().unwrap_or(""))),
                );
            }
        }
    }
}

fn uin_includes(bot: &Arc<Bot>, self_id: &str) -> bool {
    bot.uin.lock().unwrap().iter().any(|i| util::loose_eq(&json!(i), &json!(self_id)))
}

/// ≈ connect — 注册 Bot[self_id] 并拉取账号信息
async fn connect(state: &Arc<ConnState>, bot: Arc<Bot>, data: Value) {
    let self_id = util::string(&data.get("self_id").cloned().unwrap_or(Value::Null));
    let time = data.get("time").and_then(Value::as_f64).unwrap_or(0.0);
    let protocol = Arc::new(Ob11Protocol { state: state.clone(), self_id: self_id.clone() });
    let instance = Arc::new(BotInstance {
        self_id: self_id.clone(),
        adapter_id: ID.into(),
        adapter_name: NAME.into(),
        protocol: protocol.clone(),
        conn_id: state.ws.id,
        info: RwLock::new(json!({})),
        version: RwLock::new(json!({})),
        fl: RwLock::new(HashMap::new()),
        gl: RwLock::new(HashMap::new()),
        gml: RwLock::new(HashMap::new()),
        request_list: RwLock::new(vec![]),
        stat: RwLock::new(json!({ "start_time": time })),
        start_time: time,
    });
    bot.register_bot(instance.clone());
    bot.add_uin(&self_id);

    // ≈ _set_model_show / get_login_info / version / cookies / bkn
    let _ = protocol
        .send_api("_set_model_show", json!({ "model": "TRSS Yunzai ", "model_show": "TRSS Yunzai " }))
        .await;

    let info = protocol.send_api("get_login_info", json!({})).await.unwrap_or(json!({}));
    *instance.info.write().unwrap() = info.clone();

    let version_info = protocol.send_api("get_version_info", json!({})).await.unwrap_or(json!({}));
    let mut version = version_info.clone();
    if let Some(obj) = version.as_object_mut() {
        obj.insert("id".into(), json!(ID));
        obj.insert("name".into(), json!(NAME));
    }
    *instance.version.write().unwrap() = version.clone();

    // ≈ 好友/群/成员缓存
    refresh_friend_map(&protocol, &instance).await;
    let cache_group_member = bot.cfg.bot().get("cache_group_member").and_then(Value::as_bool).unwrap_or(true);
    if cache_group_member {
        refresh_group_map(&protocol, &instance).await;
    }

    let version_str = version
        .get("version")
        .map(util::string)
        .or_else(|| version.get("app_version").map(util::string))
        .unwrap_or_default();
    util::make_log1(
        Level::Mark,
        Some(&self_id),
        format!("{}({}) {} 已连接", NAME, ID, version_str),
    );
    let mut event = data.clone();
    if let Some(obj) = event.as_object_mut() {
        obj.insert("bot".into(), json!({ "self_id": self_id }));
    }
    bot.em(&bot, &format!("connect.{}", self_id), event).await;
}

async fn refresh_friend_map(protocol: &Arc<Ob11Protocol>, instance: &Arc<BotInstance>) {
    if let Ok(arr) = protocol.get_friend_array().await {
        let mut fl = instance.fl.write().unwrap();
        for u in arr.as_array().cloned().unwrap_or_default() {
            let uid = util::string(&u["user_id"]);
            fl.insert(uid, u);
        }
    }
}

async fn refresh_group_map(protocol: &Arc<Ob11Protocol>, instance: &Arc<BotInstance>) {
    if let Ok(arr) = protocol.get_group_array().await {
        for g in arr.as_array().cloned().unwrap_or_default() {
            let gid = util::string(&g["group_id"]);
            if let Ok(members) = protocol.get_member_array(&g["group_id"]).await {
                let mut gml = instance.gml.write().unwrap();
                let mut m = HashMap::new();
                for u in members.as_array().cloned().unwrap_or_default() {
                    m.insert(util::string(&u["user_id"]), u);
                }
                gml.insert(gid.clone(), m);
            }
            instance.gl.write().unwrap().insert(gid, g);
        }
    }
}

/// ≈ makeMessage — 消息事件
async fn make_message(bot: Arc<Bot>, mut data: Value) {
    let self_id = util::string(&data["self_id"]);
    data["message"] = parse_msg(&data["message"]);
    let message_type = data.get("message_type").map(util::string).unwrap_or_default();
    match message_type.as_str() {
        "private" => {
            let name = data["sender"]["card"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| data["sender"]["nickname"].as_str())
                .map(String::from)
                .unwrap_or_default();
            util::make_log(
                Level::Info,
                Some(&format!("{} <= {}", self_id, util::string(&data["user_id"]))),
                true,
                vec![format!(
                    "好友消息：{}{}",
                    if name.is_empty() { String::new() } else { format!("[{}] ", name) },
                    util::string(&data["raw_message"])
                )],
            );
        }
        "group" => {
            let group_name = data["group_name"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| {
                    bot.get_bot(&self_id)
                        .and_then(|b| b.gl.read().unwrap().get(&util::string(&data["group_id"])).cloned())
                        .and_then(|g| g.get("group_name").and_then(Value::as_str).map(String::from))
                        .unwrap_or_default()
                });
            let user_name = data["sender"]["card"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| data["sender"]["nickname"].as_str())
                .map(String::from)
                .unwrap_or_default();
            util::make_log(
                Level::Info,
                Some(&format!("{} <= {}, {}", self_id, util::string(&data["group_id"]), util::string(&data["user_id"]))),
                true,
                vec![format!(
                    "群消息：[{}, {}] {}",
                    group_name, user_name, util::string(&data["raw_message"])
                )],
            );
        }
        "guild" => {
            data["message_type"] = json!("group");
            data["group_id"] = json!(format!(
                "{}-{}",
                util::string(&data["guild_id"]),
                util::string(&data["channel_id"])
            ));
            util::make_log(
                Level::Info,
                Some(&format!(
                    "{} <= {}, {}",
                    self_id,
                    util::string(&data["group_id"]),
                    util::string(&data["user_id"])
                )),
                true,
                vec![format!(
                    "频道消息：[{}] {}",
                    util::string(&data["sender"]["nickname"]),
                    util::string(&data["message"])
                )],
            );
        }
        _ => {
            util::make_log1(
                Level::Warn,
                Some(&self_id),
                format!("未知消息：{}", logger::magenta(&data["raw"].as_str().unwrap_or(""))),
            );
        }
    }
    let name = format!(
        "{}.{}.{}",
        data["post_type"].as_str().unwrap_or("message"),
        data["message_type"].as_str().unwrap_or(""),
        data["sub_type"].as_str().unwrap_or("")
    );
    bot.em(&bot, &name, data).await;
}

/// ≈ makeNotice — 通知事件
async fn make_notice(bot: Arc<Bot>, mut data: Value) {
    let self_id = util::string(&data["self_id"]);
    let notice_type = data.get("notice_type").map(util::string).unwrap_or_default();
    let scope = format!("{} <= {}", self_id, {
        if data.get("group_id").map(|v| !v.is_null()).unwrap_or(false) {
            util::string(&data["group_id"])
        } else {
            util::string(&data["user_id"])
        }
    });
    match notice_type.as_str() {
        "friend_recall" => {
            util::make_log(
                Level::Info,
                Some(&format!("{} <= {}", self_id, util::string(&data["user_id"]))),
                true,
                vec![format!("好友消息撤回：{}", util::string(&data["message_id"]))],
            );
        }
        "group_recall" => {
            util::make_log(
                Level::Info,
                Some(&scope),
                true,
                vec![format!(
                    "群消息撤回：{} => {} {}",
                    util::string(&data["operator_id"]),
                    util::string(&data["user_id"]),
                    util::string(&data["message_id"])
                )],
            );
        }
        "group_increase" => {
            util::make_log(
                Level::Info,
                Some(&scope),
                true,
                vec![format!(
                    "群成员增加：{} => {} {}",
                    util::string(&data["operator_id"]),
                    util::string(&data["user_id"]),
                    util::string(&data["sub_type"])
                )],
            );
            refresh_single(bot.clone(), &self_id, &data["group_id"]).await;
        }
        "group_decrease" => {
            util::make_log(
                Level::Info,
                Some(&scope),
                true,
                vec![format!(
                    "群成员减少：{} => {} {}",
                    util::string(&data["operator_id"]),
                    util::string(&data["user_id"]),
                    util::string(&data["sub_type"])
                )],
            );
            if util::loose_eq(&data["user_id"], &json!(self_id)) {
                if let Some(instance) = bot.get_bot(&self_id) {
                    let gid = util::string(&data["group_id"]);
                    instance.gl.write().unwrap().remove(&gid);
                    instance.gml.write().unwrap().remove(&gid);
                }
            } else {
                refresh_single(bot.clone(), &self_id, &data["group_id"]).await;
            }
        }
        "group_admin" => {
            util::make_log1(Level::Info, Some(&scope), format!("群管理员变动：{}", util::string(&data["sub_type"])));
            refresh_single(bot.clone(), &self_id, &data["group_id"]).await;
        }
        "group_upload" => {
            util::make_log1(Level::Info, Some(&scope), format!("群文件上传：{}", util::string(&data["file"])));
            // ≈ 转合为群消息事件
            let mut ev = data.clone();
            ev["post_type"] = json!("message");
            ev["message_type"] = json!("group");
            ev["sub_type"] = json!("normal");
            let mut file_seg = data["file"].clone();
            if let Some(obj) = file_seg.as_object_mut() {
                obj.insert("type".into(), json!("file"));
            }
            ev["message"] = json!([file_seg]);
            ev["raw_message"] = json!(format!("[文件：{}]", util::string(&data["file"]["name"])));
            bot.em(&bot, "message.group.normal", ev).await;
        }
        "group_ban" => {
            util::make_log(
                Level::Info,
                Some(&scope),
                true,
                vec![format!(
                    "群禁言：{} => {} {} {}秒",
                    util::string(&data["operator_id"]),
                    util::string(&data["user_id"]),
                    util::string(&data["sub_type"]),
                    util::string(&data["duration"])
                )],
            );
        }
        "group_msg_emoji_like" => {
            util::make_log1(Level::Info, Some(&scope), format!("群消息回应：{}", util::string(&data["message_id"])));
        }
        "friend_add" => {
            util::make_log1(Level::Info, Some(&scope), "好友添加".into());
        }
        "notify" => {
            if data.get("group_id").map(|v| !v.is_null()).unwrap_or(false) {
                data["notice_type"] = json!("group");
            } else {
                data["notice_type"] = json!("friend");
            }
            if data.get("user_id").map(|v| v.is_null()).unwrap_or(true) {
                data["user_id"] = data
                    .get("operator_id")
                    .cloned()
                    .or_else(|| data.get("target_id").cloned())
                    .unwrap_or(Value::Null);
            }
            match data["sub_type"].as_str().unwrap_or("") {
                "poke" | "poke_recall" => {
                    data["operator_id"] = data["user_id"].clone();
                    let what = if data["sub_type"] == json!("poke") { "戳一戳" } else { "戳一戳撤回" };
                    if data.get("group_id").map(|v| !v.is_null()).unwrap_or(false) {
                        util::make_log(
                            Level::Info,
                            Some(&scope),
                            true,
                            vec![format!(
                                "群{}：{} => {}",
                                what,
                                util::string(&data["operator_id"]),
                                util::string(&data["target_id"])
                            )],
                        );
                    } else {
                        util::make_log(
                            Level::Info,
                            Some(&self_id),
                            true,
                            vec![format!(
                                "好友{}：{} => {}",
                                what,
                                util::string(&data["operator_id"]),
                                util::string(&data["target_id"])
                            )],
                        );
                    }
                }
                "honor" => util::make_log1(Level::Info, Some(&scope), format!("群荣誉：{}", util::string(&data["honor_type"]))),
                "title" => util::make_log1(Level::Info, Some(&scope), format!("群头衔：{}", util::string(&data["title"]))),
                "group_name" => util::make_log1(Level::Info, Some(&scope), format!("群名更改：{}", util::string(&data["name_new"]))),
                "input_status" => {
                    data["post_type"] = json!("internal");
                    data["notice_type"] = json!("input");
                    if data.get("end").map(|v| v.is_null()).unwrap_or(true) {
                        data["end"] = json!(data["event_type"] != json!(1));
                    }
                    util::make_log1(Level::Info, Some(&scope), util::string(&data["message"]));
                }
                "profile_like" => util::make_log1(Level::Info, Some(&scope), format!("资料卡点赞：{}次", util::string(&data["times"]))),
                _ => util::make_log1(
                    Level::Warn,
                    Some(&self_id),
                    format!("未知通知：{}", logger::magenta(&data["raw"].as_str().unwrap_or(""))),
                ),
            }
        }
        "group_card" => {
            util::make_log(
                Level::Info,
                Some(&scope),
                true,
                vec![format!(
                    "群名片更新：{} => {}",
                    util::string(&data["card_old"]),
                    util::string(&data["card_new"])
                )],
            );
        }
        "offline_file" => {
            util::make_log1(Level::Info, Some(&scope), format!("离线文件：{}", util::string(&data["file"])));
            let mut ev = data.clone();
            ev["post_type"] = json!("message");
            ev["message_type"] = json!("private");
            ev["sub_type"] = json!("friend");
            let mut file_seg = data["file"].clone();
            if let Some(obj) = file_seg.as_object_mut() {
                obj.insert("type".into(), json!("file"));
            }
            ev["message"] = json!([file_seg]);
            ev["raw_message"] = json!(format!("[文件：{}]", util::string(&data["file"]["name"])));
            bot.em(&bot, "message.private.friend", ev).await;
        }
        "client_status" => {
            util::make_log1(
                Level::Info,
                Some(&self_id),
                format!(
                    "客户端{}：{}",
                    if data["online"] == json!(true) { "上线" } else { "下线" },
                    util::string(&data["client"])
                ),
            );
        }
        "essence" => {
            data["notice_type"] = json!("group_essence");
            util::make_log1(Level::Info, Some(&scope), format!("群精华消息：{}", util::string(&data["sub_type"])));
        }
        "guild_channel_recall" => {
            util::make_log1(Level::Info, Some(&scope), format!("频道消息撤回：{}", util::string(&data["message_id"])));
        }
        "message_reactions_updated" => {
            data["notice_type"] = json!("guild_message_reactions_updated");
            util::make_log1(Level::Info, Some(&scope), "频道消息表情贴".into());
        }
        "channel_updated" => {
            data["notice_type"] = json!("guild_channel_updated");
            util::make_log1(Level::Info, Some(&scope), "子频道更新".into());
        }
        "channel_created" => {
            data["notice_type"] = json!("guild_channel_created");
            util::make_log1(Level::Info, Some(&scope), "子频道创建".into());
        }
        "channel_destroyed" => {
            data["notice_type"] = json!("guild_channel_destroyed");
            util::make_log1(Level::Info, Some(&scope), "子频道删除".into());
        }
        "bot_offline" => {
            data["post_type"] = json!("system");
            data["notice_type"] = json!("offline");
            let msg = format!(
                "{}：{}",
                data.get("tag").map(util::string).unwrap_or_else(|| "账号下线".into()),
                util::string(&data["message"])
            );
            util::make_log1(Level::Info, Some(&self_id), msg.clone());
            let bot2 = bot.clone();
            tokio::spawn(async move { bot2.send_master_msg(&bot2, json!(msg)).await });
        }
        _ => {
            util::make_log1(
                Level::Warn,
                Some(&self_id),
                format!("未知通知：{}", logger::magenta(&data["raw"].as_str().unwrap_or(""))),
            );
        }
    }

    // ≈ notice_type 归一化：首个 "_" 前后拆分
    let nt = data["notice_type"].as_str().unwrap_or("").to_string();
    let mut parts = nt.splitn(2, '_');
    let head = parts.next().unwrap_or("").to_string();
    let tail = parts.next().unwrap_or("").to_string();
    data["notice_type"] = json!(head);
    if !tail.is_empty() {
        data["sub_type"] = json!(tail);
    }
    if data.get("guild_id").map(|v| !v.is_null()).unwrap_or(false)
        && data.get("channel_id").map(|v| !v.is_null()).unwrap_or(false)
    {
        data["group_id"] = json!(format!(
            "{}-{}",
            util::string(&data["guild_id"]),
            util::string(&data["channel_id"])
        ));
    }
    let name = format!(
        "{}.{}.{}",
        data["post_type"].as_str().unwrap_or("notice"),
        data["notice_type"].as_str().unwrap_or(""),
        data["sub_type"].as_str().unwrap_or("")
    );
    bot.em(&bot, &name, data).await;
}

/// ≈ makeRequest — 请求事件
async fn make_request(bot: Arc<Bot>, mut data: Value) {
    let self_id = util::string(&data["self_id"]);
    let request_type = data.get("request_type").map(util::string).unwrap_or_default();
    match request_type.as_str() {
        "friend" => {
            util::make_log(
                Level::Info,
                Some(&format!("{} <= {}", self_id, util::string(&data["user_id"]))),
                true,
                vec![format!(
                    "加好友请求：{}({})",
                    util::string(&data["comment"]),
                    util::string(&data["flag"])
                )],
            );
            data["sub_type"] = json!("add");
        }
        "group" => {
            util::make_log(
                Level::Info,
                Some(&format!("{} <= {}, {}", self_id, util::string(&data["group_id"]), util::string(&data["user_id"]))),
                true,
                vec![format!(
                    "加群请求：{} {}({})",
                    util::string(&data["sub_type"]),
                    util::string(&data["comment"]),
                    util::string(&data["flag"])
                )],
            );
        }
        _ => {
            util::make_log1(
                Level::Warn,
                Some(&self_id),
                format!("未知请求：{}", logger::magenta(&data["raw"].as_str().unwrap_or(""))),
            );
        }
    }
    if let Some(instance) = bot.get_bot(&self_id) {
        instance.request_list.write().unwrap().push(data.clone());
    }
    let name = format!(
        "{}.{}.{}",
        data["post_type"].as_str().unwrap_or("request"),
        data["request_type"].as_str().unwrap_or(""),
        data["sub_type"].as_str().unwrap_or("")
    );
    bot.em(&bot, &name, data).await;
}

/// 刷新单个群信息（≈ group.getInfo(true,true)）
async fn refresh_single(bot: Arc<Bot>, self_id: &str, group_id: &Value) {
    let instance = match bot.get_bot(self_id) {
        Some(i) => i,
        None => return,
    };
    if let Ok(info) = instance
        .protocol
        .send_api("get_group_info", json!({ "group_id": group_id }))
        .await
    {
        if info.is_object() {
            instance.gl.write().unwrap().insert(util::string(group_id), info);
        }
    }
}
