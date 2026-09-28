//! ≈ lib/plugins/loader.js — 插件加载与消息分发管线
use crate::bot::Bot;
use crate::logger::{self, Level};
use crate::plugins::plugin::{Accept, E, Permission, Plugin, Rule};
use crate::util;
use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

/// 双轨插件：原生 trait / JS 脚本
pub enum AnyPlugin {
    Native(Arc<dyn Plugin>),
    Js(crate::jsrt::JsPluginData),
}

pub struct PluginEntry {
    pub key: String,
    pub name: String,
    pub dsc: String,
    pub event: String,
    pub priority: i64,
    pub namespace: String,
    pub plugin: AnyPlugin,
}

pub struct TaskJob {
    pub name: String,
    pub cron: String,
    pub fnc: String,
    pub log: bool,
    pub plugin_key: String,
}

pub struct PluginsLoader {
    pub priority: RwLock<Vec<Arc<PluginEntry>>>,
    pub task: RwLock<Vec<TaskJob>>,
    pub engine: RwLock<Option<Arc<crate::jsrt::JsEngine>>>,
    group_cd: RwLock<HashMap<String, std::time::Instant>>,
    single_cd: RwLock<HashMap<String, std::time::Instant>>,
    msg_throttle: RwLock<HashSet<String>>,
}

/// 星铁/绝区零前缀（≈ loader.js srReg/zzzReg）
static SR_REG: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^#?(\*|星铁|星轨|穹轨|星穹|崩铁|星穹铁道|崩坏星穹铁道|铁道)+").unwrap()
});
static ZZZ_REG: Lazy<Regex> = Lazy::new(|| Regex::new(r"^#?(%|％|绝区零|绝区)+").unwrap());

impl PluginsLoader {
    pub fn new() -> PluginsLoader {
        PluginsLoader {
            priority: RwLock::new(Vec::new()),
            task: RwLock::new(Vec::new()),
            engine: RwLock::new(None),
            group_cd: RwLock::new(HashMap::new()),
            single_cd: RwLock::new(HashMap::new()),
            msg_throttle: RwLock::new(HashSet::new()),
        }
    }

    /// ≈ load — 注册插件并按优先级排序（阶段3接入 JS 插件扫描）
    pub async fn load(self: &Arc<Self>, bot: &Arc<Bot>) {
        util::make_log1(Level::Info, Some("Plugin"), "-----------".into());
        util::make_log1(Level::Info, Some("Plugin"), "加载插件中...".into());
        let mut count = 0;
        let mut tasks: Vec<TaskJob> = vec![];
        for plugin in crate::plugins::builtin::builtins() {
            let key = format!("builtin/{}", plugin.name());
            let name = plugin.name().to_string();
            for t in plugin.tasks() {
                tasks.push(TaskJob {
                    name: if t.name.is_empty() { name.clone() } else { t.name },
                    cron: t.cron.clone(),
                    fnc: t.fnc.clone(),
                    log: t.log,
                    plugin_key: key.clone(),
                });
            }
            let entry = Arc::new(PluginEntry {
                key,
                name,
                dsc: plugin.dsc().to_string(),
                event: plugin.event().to_string(),
                priority: plugin.priority(),
                namespace: format!("builtin.{}", plugin.name()),
                plugin: AnyPlugin::Native(plugin),
            });
            util::make_log1(
                Level::Debug,
                Some("Plugin"),
                format!("加载插件 [{}][{}]", entry.key, entry.name),
            );
            count += 1;
            self.priority.write().unwrap().push(entry);
        }
        // JS 插件扫描（≈ getPlugins + importPlugin）
        let engine = match self.engine.read().unwrap().clone() {
            Some(e) => Some(e),
            None => match crate::jsrt::JsEngine::new().await {
                Ok(e) => {
                    let e = Arc::new(e);
                    *self.engine.write().unwrap() = Some(e.clone());
                    Some(e)
                }
                Err(err) => {
                    util::make_log1(Level::Error, Some("Plugin"), format!("JS 引擎初始化失败 {}", err));
                    None
                }
            },
        };
        if let Some(engine) = engine {
            for (rel, abs) in crate::jsrt::scan_plugin_files("plugins") {
                let datas = engine.load_plugin(&abs, &rel).await;
                for data in datas {
                    if data.name.is_empty() {
                        continue;
                    }
                    util::make_log1(
                        Level::Debug,
                        Some("Plugin"),
                        format!("加载插件 [{}][{}]", rel, data.name),
                    );
                    count += 1;
                    self.priority.write().unwrap().push(Arc::new(PluginEntry {
                        key: rel.clone(),
                        name: data.name.clone(),
                        dsc: data.dsc.clone(),
                        event: data.event.clone(),
                        priority: data.priority,
                        namespace: data.reg_key.clone(),
                        plugin: AnyPlugin::Js(data),
                    }));
                }
            }
        }

        self.priority.write().unwrap().sort_by_key(|e| e.priority);
        util::make_log1(Level::Info, Some("Plugin"), format!("加载定时任务[{}个]", tasks.len()));
        util::make_log1(Level::Info, Some("Plugin"), format!("加载插件[{}个]", count));
        *self.task.write().unwrap() = tasks;
        let _ = bot;
    }

    /// JS 插件派发：实例化 + 调用，返回 false 表示 handler 返回 false（继续下一条 rule）
    async fn js_call(
        &self,
        engine: &Arc<crate::jsrt::JsEngine>,
        data: &crate::jsrt::JsPluginData,
        e: &E,
        fnc: &str,
    ) -> bool {
        crate::jsrt::EventGuard::set(e.bot.clone(), e.data.clone());
        engine.instantiate(&data.reg_key, &e.data).await;
        let ret = engine.call(&data.reg_key, fnc, &e.data).await;
        crate::jsrt::EventGuard::clear();
        ret != "false"
    }

    /// ≈ deal — 消息分发主管线
    pub async fn deal(self: &Arc<Self>, bot: Arc<Bot>, data: Value) {
        let mut e = E::new(bot.clone(), data);
        self.count(&e, "receive", &e.message()).await;
        if !self.check_black(&e) {
            return;
        }
        let group_cfg = CFG.get_group(&e.self_id(), &util::string(&e.group_id()));
        if !self.check_limit(&e, &group_cfg) {
            return;
        }
        self.deal_event(&bot, &mut e, &group_cfg);
        // ≈ 设置冷却
        if truthy(&e.get("only_reply_at")) {
            self.set_limit(&e, &group_cfg);
        }
        // ≈ Runtime.init — miao/genshin 缺失时降级为空操作
        let priority: Vec<Arc<PluginEntry>> = self.priority.read().unwrap().clone();
        let mut filtered = vec![];
        for p in priority {
            if self.check_disable(&p.name, &group_cfg) && filt_event(&e.data, &p.event) {
                filtered.push(p);
            }
        }
        // 上下文 hook
        let hook = self.context_hook(&filtered, &e).await;
        if hook {
            return;
        }
        // only_reply_at 门
        if !truthy(&e.get("only_reply_at")) {
            return;
        }
        // 星铁/绝区零命令标准化
        let msg = e.msg();
        if SR_REG.is_match(&msg) {
            e.data["game"] = json!("sr");
            e.data["msg"] = json!(SR_REG.replace(&msg, "#星铁").to_string());
        } else if ZZZ_REG.is_match(&msg) {
            e.data["game"] = json!("zzz");
            e.data["msg"] = json!(ZZZ_REG.replace(&msg, "#绝区零").to_string());
        }
        // accept 链
        for p in &filtered {
            match &p.plugin {
                AnyPlugin::Native(native) => {
                    let mut pe = with_plugin_name(&e, &p.name);
                    match native.accept(&mut pe).await {
                        Accept::Return => return,
                        Accept::Break => break,
                        Accept::Next => {}
                    }
                }
                AnyPlugin::Js(data) => {
                    if let Some(engine) = self.engine.read().unwrap().clone() {
                        crate::jsrt::EventGuard::set(e.bot.clone(), e.data.clone());
                        engine.instantiate(&data.reg_key, &e.data).await;
                        let r = engine.accept(&data.reg_key, &e.data).await;
                        crate::jsrt::EventGuard::clear();
                        if r == "return" {
                            return;
                        }
                        if r == "true" {
                            break;
                        }
                    }
                }
            }
        }
        // rule 匹配
        for p in &filtered {
            let rules: Vec<crate::jsrt::JsRule> = match &p.plugin {
                AnyPlugin::Native(native) => native.rules().iter().map(|r| crate::jsrt::JsRule {
                    reg_src: String::new(),
                    rust: Some(r.reg.clone()),
                    fnc: r.fnc.clone(),
                    log: r.log,
                    permission: r.permission.clone(),
                    event: r.event.clone(),
                }).collect(),
                AnyPlugin::Js(data) => data.rules.clone(),
            };
            for (ri, rule) in rules.iter().enumerate() {
                if let Some(ev) = &rule.event {
                    if !filt_event(&e.data, ev) {
                        continue;
                    }
                }
                let matched = match &rule.rust {
                    Some(re) => re.is_match(&e.msg()),
                    None => {
                        // JS RegExp 兜底
                        if let (AnyPlugin::Js(data), Some(engine)) = (&p.plugin, self.engine.read().unwrap().clone()) {
                            engine.regex_test(&data.reg_key, ri, &e.msg()).await
                        } else {
                            false
                        }
                    }
                };
                if !matched {
                    continue;
                }
                let log_fnc = logger::blue(format!("[{}({})]", p.name, rule.fnc));
                util::make_log(
                    if rule.log { Level::Info } else { Level::Debug },
                    None,
                    false,
                    vec![format!(
                        "{}{}{}",
                        util::string(&e.get("logText")),
                        log_fnc,
                        logger::yellow("[开始处理]")
                    )],
                );
                if self.filt_permission(&e, rule).await {
                    let start_time = util::now_ms();
                    let res = match &p.plugin {
                        AnyPlugin::Native(native) => {
                            let mut pe = with_plugin_name(&e, &p.name);
                            native.handle(&mut pe, &rule.fnc).await
                        }
                        AnyPlugin::Js(data) => {
                            if let Some(engine) = self.engine.read().unwrap().clone() {
                                self.js_call(&engine, data, &e, &rule.fnc).await
                            } else {
                                false
                            }
                        }
                    };
                    if !res {
                        continue;
                    }
                    util::make_log(
                        if rule.log { Level::Mark } else { Level::Debug },
                        None,
                        false,
                        vec![format!(
                            "{}{}{}",
                            util::string(&e.get("logText")),
                            log_fnc,
                            logger::green(format!("[完成{}]", util::get_time_diff(start_time, util::now_ms())))
                        )],
                    );
                }
                return;
            }
        }
        util::make_log(
            Level::Debug,
            None,
            false,
            vec![format!(
                "{}{}",
                util::string(&e.get("logText")),
                logger::blue("[暂无插件处理]")
            )],
        );
    }

    /// ≈ 上下文 hook — 有待续对话时优先派发；返回 true 表示事件已消费
    async fn context_hook(&self, filtered: &[Arc<PluginEntry>], e: &E) -> bool {
        for p in filtered {
            let contexts = collect_context(&p.name, e);
            if contexts.is_empty() {
                continue;
            }
            for (fnc, _ctx_data) in contexts {
                let mut pe = with_plugin_name(e, &p.name);
                if expired_context(&p.name, &pe, &fnc) {
                    continue;
                }
                let res = match &p.plugin {
                    AnyPlugin::Native(native) => native.handle(&mut pe, &fnc).await,
                    AnyPlugin::Js(data) => {
                        if let Some(engine) = self.engine.read().unwrap().clone() {
                            self.js_call(&engine, data, e, &fnc).await
                        } else {
                            false
                        }
                    }
                };
                if !res {
                    continue; // ≈ 返回 "continue"
                }
                return true;
            }
            return true; // 有上下文即消费
        }
        false
    }

    /// ≈ dealEvent — 段加工与事件字段补全
    fn deal_event(&self, bot: &Arc<Bot>, e: &mut E, group_cfg: &Value) {
        if let Some(message) = e.data.get("message").cloned() {
            let mut msg = String::new();
            let mut img: Vec<Value> = vec![];
            let mut at: Option<Value> = None;
            let mut at_bot = false;
            for seg in message.as_array().cloned().unwrap_or_default() {
                let typ = seg.get("type").and_then(Value::as_str).unwrap_or("");
                match typ {
                    "text" => {
                        msg += &deal_text(&seg.get("text").map(util::string).unwrap_or_default(), bot);
                    }
                    "image" => {
                        img.push(seg.get("url").cloned().unwrap_or(Value::Null));
                    }
                    "at" => {
                        let qq = seg.get("qq").cloned().unwrap_or(Value::Null);
                        if util::loose_eq(&qq, &json!(e.self_id())) {
                            at_bot = true;
                        } else {
                            at = Some(qq);
                        }
                    }
                    "reply" => {
                        e.data["reply_id"] = seg.get("id").cloned().unwrap_or(Value::Null);
                    }
                    "file" => {
                        e.data["file"] = seg.clone();
                    }
                    "xml" | "json" => {
                        let data = seg.get("data").cloned().unwrap_or(Value::Null);
                        msg += &match data {
                            Value::String(s) => s,
                            other => util::string(&other),
                        };
                    }
                    _ => {}
                }
            }
            e.data["msg"] = json!(msg);
            if !img.is_empty() {
                e.data["img"] = json!(img);
            }
            if let Some(a) = at {
                e.data["at"] = a;
            }
            e.data["atBot"] = json!(at_bot);
        }
        // isPrivate / isGroup / logText
        let message_type = e.data.get("message_type").and_then(Value::as_str).unwrap_or("");
        let notice_type = e.data.get("notice_type").and_then(Value::as_str).unwrap_or("");
        let sender = e.get("sender");
        let nickname = sender.get("nickname").map(util::string).unwrap_or_default();
        let uid = util::string(&e.user_id());
        let gid = util::string(&e.group_id());
        if message_type == "private" || notice_type == "friend" {
            e.data["isPrivate"] = json!(true);
            e.data["logText"] = json!(format!(
                "[{}]",
                if nickname.is_empty() { uid.clone() } else { format!("{}({})", nickname, uid) }
            ));
        } else if message_type == "group" || notice_type == "group" {
            e.data["isGroup"] = json!(true);
            let group_name = e.data.get("group_name").and_then(Value::as_str).unwrap_or("").to_string();
            let card = sender.get("card").map(util::string).unwrap_or_default();
            let who = if card.is_empty() { uid.clone() } else { format!("{}({})", card, uid) };
            e.data["logText"] = json!(format!(
                "[{}, {}]",
                if group_name.is_empty() { gid.clone() } else { format!("{}({})", group_name, gid) },
                who
            ));
        }
        // isMaster
        let masters = bot.cfg.master();
        if masters
            .get(&e.self_id())
            .map(|m| m.iter().any(|u| util::loose_eq(&json!(u), &e.user_id())))
            .unwrap_or(false)
        {
            e.data["isMaster"] = json!(true);
        }
        // botAlias 前缀
        let mut has_alias = false;
        let msg = e.msg();
        if !msg.is_empty() && e.is_group() && !e.at_bot() {
            let alias = group_cfg.get("botAlias").cloned().unwrap_or(Value::Null);
            let alias_list = alias.as_array().cloned().unwrap_or_else(|| vec![alias.clone()]);
            for a in alias_list {
                let alias_str = util::string(&a);
                if alias_str.is_empty() {
                    continue;
                }
                if msg.starts_with(&alias_str) {
                    e.data["msg"] = json!(msg[alias_str.len()..].to_string());
                    has_alias = true;
                    break;
                }
            }
        }
        e.data["hasAlias"] = json!(has_alias);
        e.data["only_reply_at"] = json!(self.only_reply_at(e, group_cfg, has_alias));
    }

    /// ≈ onlyReplyAt — 0 未开启 / 1 开启 / 2 非主人
    fn only_reply_at(&self, e: &E, group_cfg: &Value, has_alias: bool) -> bool {
        if e.get("message").is_null() || e.is_private() {
            return true;
        }
        let mode = group_cfg.get("onlyReplyAt").and_then(Value::as_i64).unwrap_or(0);
        let alias = group_cfg.get("botAlias").cloned().unwrap_or(Value::Null);
        if mode == 0 || alias.is_null() {
            return true;
        }
        if mode == 2 && e.is_master() {
            return true;
        }
        if e.at_bot() {
            return true;
        }
        if has_alias {
            return true;
        }
        false
    }

    /// ≈ checkLimit — 群 CD / 个人 CD / 1s 消息去重
    fn check_limit(self: &Arc<Self>, e: &E, config: &Value) -> bool {
        if e.get("message").is_null() || e.is_private() {
            return true;
        }
        let group_cd = config.get("groupCD").and_then(Value::as_i64).unwrap_or(0);
        let single_cd = config.get("singleCD").and_then(Value::as_i64).unwrap_or(0);
        let now = std::time::Instant::now();
        let gid = util::string(&e.group_id());
        let uid = util::string(&e.user_id());
        if group_cd > 0 {
            if let Some(t) = self.group_cd.read().unwrap().get(&gid) {
                if now - *t < std::time::Duration::from_millis(group_cd as u64) {
                    return false;
                }
            }
        }
        let key = format!("{}.{}", gid, uid);
        if single_cd > 0 {
            if let Some(t) = self.single_cd.read().unwrap().get(&key) {
                if now - *t < std::time::Duration::from_millis(single_cd as u64) {
                    return false;
                }
            }
        }
        let msg_id = format!("{}:{}:{}", e.self_id(), uid, util::string(&e.get("raw_message")));
        {
            let mut throttle = self.msg_throttle.write().unwrap();
            if throttle.contains(&msg_id) {
                return false;
            }
            throttle.insert(msg_id.clone());
        }
        let this = self.clone();
        tokio::spawn(async move {
            util::sleep(1000).await;
            this.msg_throttle.write().unwrap().remove(&msg_id);
        });
        true
    }

    /// ≈ setLimit — 命中后设置 CD
    fn set_limit(self: &Arc<Self>, e: &E, config: &Value) {
        if e.is_private() {
            return;
        }
        let now = std::time::Instant::now();
        let gid = util::string(&e.group_id());
        let uid = util::string(&e.user_id());
        if let Some(ms) = config.get("groupCD").and_then(Value::as_u64) {
            if ms > 0 {
                self.group_cd.write().unwrap().insert(gid.clone(), now);
                let this = self.clone();
                let gid2 = gid.clone();
                tokio::spawn(async move {
                    util::sleep(ms).await;
                    this.group_cd.write().unwrap().remove(&gid2);
                });
            }
        }
        if let Some(ms) = config.get("singleCD").and_then(Value::as_u64) {
            if ms > 0 {
                let key = format!("{}.{}", gid, uid);
                self.single_cd.write().unwrap().insert(key.clone(), now);
                let this = self.clone();
                tokio::spawn(async move {
                    util::sleep(ms).await;
                    this.single_cd.write().unwrap().remove(&key);
                });
            }
        }
    }

    /// ≈ checkBlack — 黑白名单
    fn check_black(&self, e: &E) -> bool {
        let other = CFG.get_other();
        let contains = |list: &Value, v: &Value| -> bool {
            list.as_array()
                .map(|a| a.iter().any(|i| util::loose_eq(i, v)))
                .unwrap_or(false)
        };
        let white_user = other.get("whiteUser").cloned().unwrap_or(Value::Null);
        if let Value::Array(ref a) = white_user {
            if !a.is_empty() && !contains(&white_user, &e.user_id()) {
                return false;
            }
        }
        let black_user = other.get("blackUser").cloned().unwrap_or(Value::Null);
        if contains(&black_user, &e.user_id()) {
            return false;
        }
        if !e.group_id().is_null() {
            let white_group = other.get("whiteGroup").cloned().unwrap_or(Value::Null);
            if let Value::Array(ref a) = white_group {
                if !a.is_empty() && !contains(&white_group, &e.group_id()) {
                    return false;
                }
            }
            let black_group = other.get("blackGroup").cloned().unwrap_or(Value::Null);
            if contains(&black_group, &e.group_id()) {
                return false;
            }
        }
        true
    }

    /// ≈ checkDisable — 群功能启停
    fn check_disable(&self, name: &str, group_cfg: &Value) -> bool {
        let disable = group_cfg.get("disable").cloned().unwrap_or(Value::Null);
        if let Value::Array(ref a) = disable {
            if a.iter().any(|i| util::loose_eq(i, &json!(name))) {
                return false;
            }
        }
        let enable = group_cfg.get("enable").cloned().unwrap_or(Value::Null);
        if let Value::Array(ref a) = enable {
            if !a.is_empty() && !a.iter().any(|i| util::loose_eq(i, &json!(name))) {
                return false;
            }
        }
        true
    }

    /// ≈ filtPermission — master/owner/admin
    async fn filt_permission(&self, e: &E, rule: &crate::jsrt::JsRule) -> bool {
        if e.is_master() {
            return true;
        }
        match rule.permission {
            Permission::All => true,
            Permission::Master => {
                let _ = e.reply(json!("暂无权限，只有主人才能操作")).await;
                false
            }
            Permission::Owner => {
                let member = e.member_info();
                let role = member.get("role").and_then(Value::as_str).unwrap_or("");
                if role == "owner" {
                    true
                } else {
                    let _ = e.reply(json!("暂无权限，只有群主才能操作")).await;
                    false
                }
            }
            Permission::Admin => {
                let member = e.member_info();
                let role = member.get("role").and_then(Value::as_str).unwrap_or("");
                if role == "owner" || role == "admin" {
                    true
                } else {
                    let _ = e.reply(json!("暂无权限，只有管理员才能操作")).await;
                    false
                }
            }
        }
    }

    /// ≈ count/saveCounts — Redis 消息统计
    async fn count(&self, e: &E, typ: &str, msg: &Value) {
        let bot_cfg = CFG.bot();
        let mut count: Vec<(String, i64)> = vec![(format!("{}:msg", typ), 1)];
        if bot_cfg.get("msg_type_count").and_then(Value::as_bool).unwrap_or(false) {
            for seg in msg.as_array().cloned().unwrap_or_else(|| vec![msg.clone()]) {
                let seg_type = seg.get("type").map(util::string).unwrap_or_else(|| "text".into());
                count.push((format!("{}:{}", typ, seg_type), 1));
            }
        }
        self.save_counts(e, count).await;
    }

    pub async fn count_send(&self, e: &E, msg: &Value) {
        self.count(e, "send", msg).await;
    }

    async fn save_counts(&self, e: &E, count: Vec<(String, i64)>) {
        let redis = match e.bot.redis_arc() {
            Some(r) => r,
            None => return,
        };
        use chrono::{Datelike, Utc};
        let now = Utc::now().with_timezone(&*logger::TZ);
        let day = format!("{}:{:02}:{:02}", now.year(), now.month(), now.day());
        let month = format!("{}:{:02}", now.year(), now.month());
        let year = format!("{}", now.year());
        let mut keys: Vec<String> = vec![];
        for period in [day, month, year, "total".to_string()] {
            keys.push(format!("total:{}", period));
            let sid = e.self_id();
            if !sid.is_empty() {
                keys.push(format!("bot:{}:{}", sid, period));
            }
            let uid = util::string(&e.user_id());
            if !uid.is_empty() {
                keys.push(format!("user:{}:{}", uid, period));
            }
            let gid = util::string(&e.group_id());
            if !gid.is_empty() {
                keys.push(format!("group:{}:{}", gid, period));
            }
        }
        for (typ, value) in count {
            for key in &keys {
                redis.incr_by(&format!("Yz:count:{}:{}", typ, key), value).await;
            }
        }
    }
}

static CFG: Lazy<Arc<crate::config::Cfg>> = Lazy::new(|| {
    crate::GLOBAL_CFG
        .get()
        .cloned()
        .expect("Cfg 未初始化")
});

/// 事件匹配 ≈ filtEvent
pub fn filt_event(e: &Value, event: &str) -> bool {
    if event.is_empty() {
        return false;
    }
    let post_type = e.get("post_type").and_then(Value::as_str).unwrap_or("");
    let fields: &[&str] = match post_type {
        "message" => &["post_type", "message_type", "sub_type"],
        "notice" => &["post_type", "notice_type", "sub_type"],
        "request" => &["post_type", "request_type", "sub_type"],
        _ => &[],
    };
    let mapped: Vec<String> = event
        .split('.')
        .enumerate()
        .map(|(i, part)| {
            if part == "*" {
                part.to_string()
            } else {
                e.get(fields.get(i).copied().unwrap_or(""))
                    .map(util::string)
                    .unwrap_or_default()
            }
        })
        .collect();
    event == mapped.join(".")
}

fn with_plugin_name(e: &E, name: &str) -> E {
    let mut pe = E::new(e.bot.clone(), e.data.clone());
    pe.data["plugin_name"] = json!(name);
    pe
}

fn deal_text(text: &str, bot: &Arc<Bot>) -> String {
    let mut t = text.to_string();
    let to_hash = bot.cfg.bot().get("/→#").and_then(Value::as_bool).unwrap_or(false);
    if to_hash {
        if let Ok(re) = Regex::new(r"^\s*/\s*") {
            t = re.replace(&t, "#").to_string();
        }
    }
    if let Ok(re) = Regex::new(r"^\s*[＃井]\s*") {
        t = re.replace(&t, "#").to_string();
    }
    if let Ok(re) = Regex::new(r"^\s*[＊※]\s*") {
        t = re.replace(&t, "*").to_string();
    }
    t.trim().to_string()
}

fn collect_context(plugin_name: &str, e: &E) -> Vec<(String, Value)> {
    let mut ret = vec![];
    for is_group in [false, true] {
        let key = context_key(plugin_name, e, is_group);
        let store = crate::plugins::plugin::CONTEXTS.read().unwrap();
        if let Some(entry) = store.get(&key) {
            for (t, ctx) in entry.iter() {
                if !ctx.expired() {
                    ret.push((t.clone(), ctx.data.clone()));
                }
            }
        }
    }
    ret
}

/// 过期上下文处理：移除并回复超时提示
fn expired_context(plugin_name: &str, e: &E, typ: &str) -> bool {
    for is_group in [false, true] {
        let key = context_key(plugin_name, e, is_group);
        let reply_msg = {
            let mut store = crate::plugins::plugin::CONTEXTS.write().unwrap();
            if let Some(entry) = store.get_mut(&key) {
                if let Some(ctx) = entry.get(typ) {
                    if ctx.expired() {
                        let reply = ctx.timeout_reply.clone();
                        entry.remove(typ);
                        Some(reply)
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some(reply) = reply_msg {
            let e2 = E::new(e.bot.clone(), e.data.clone());
            tokio::spawn(async move {
                let _ = e2.reply(json!(reply)).await;
            });
            return true;
        }
    }
    false
}

fn context_key(plugin_name: &str, e: &E, is_group: bool) -> String {
    format!(
        "{}.{}.{}",
        plugin_name,
        e.self_id(),
        if is_group { util::string(&e.group_id()) } else { util::string(&e.user_id()) }
    )
}

pub use crate::plugins::plugin::truthy;
