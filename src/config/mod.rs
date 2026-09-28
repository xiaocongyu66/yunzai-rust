//! ≈ lib/config/config.js — 配置系统：default_config + config 双层合并
pub mod init;
pub mod redis;

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

pub struct Cfg {
    cache: RwLock<HashMap<String, Value>>,
}

impl Cfg {
    /// ≈ initCfg — config/config 缺失时从 default_config 拷贝
    pub fn new() -> std::sync::Arc<Cfg> {
        let cfg = std::sync::Arc::new(Cfg { cache: RwLock::new(HashMap::new()) });
        let def_dir = Path::new("config/default_config");
        let cfg_dir = Path::new("config/config");
        if let Ok(files) = std::fs::read_dir(def_dir) {
            if cfg_dir.exists() {
                let have: Vec<String> = std::fs::read_dir(cfg_dir)
                    .map(|rd| {
                        rd.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
                            .collect()
                    })
                    .unwrap_or_default();
                for f in files.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string())) {
                    if !have.contains(&f) {
                        let _ = std::fs::copy(def_dir.join(&f), cfg_dir.join(&f));
                    }
                }
            } else {
                let _ = std::fs::create_dir_all(cfg_dir);
                for f in files.filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string())) {
                    let _ = std::fs::copy(def_dir.join(&f), cfg_dir.join(&f));
                }
            }
        }
        // 日志参数预热（logger 全局变量）
        let bot = cfg.get_all_cfg("bot");
        if let Some(l) = bot.get("log_level").and_then(Value::as_str) {
            crate::logger::set_level(l);
        }
        if let Some(l) = bot.get("log_align").and_then(Value::as_str) {
            *crate::logger::LOG_ALIGN.write().unwrap() = l.to_string();
        }
        if let Some(l) = bot.get("log_length").and_then(Value::as_u64) {
            crate::logger::LOG_LENGTH.store(l as usize, std::sync::atomic::Ordering::Relaxed);
        }
        cfg
    }

    fn read_yaml(dir: &str, name: &str) -> Value {
        let file = format!("config/{}/{}.yaml", dir, name);
        match std::fs::read_to_string(&file) {
            Ok(s) => serde_yaml::from_str::<Value>(&s).unwrap_or(Value::Null),
            Err(_) => Value::Null,
        }
    }

    /// ≈ getdefSet
    pub fn get_def_set(&self, name: &str) -> Value {
        let key = format!("default_config.{}", name);
        if let Some(v) = self.cache.read().unwrap().get(&key) {
            return v.clone();
        }
        let v = Self::read_yaml("default_config", name);
        self.cache.write().unwrap().insert(key, v.clone());
        v
    }

    /// ≈ getConfig
    pub fn get_config(&self, name: &str) -> Value {
        let key = format!("config.{}", name);
        if let Some(v) = self.cache.read().unwrap().get(&key) {
            return v.clone();
        }
        let v = Self::read_yaml("config", name);
        self.cache.write().unwrap().insert(key, v.clone());
        v
    }

    /// ≈ getAllCfg — { ...default, ...user } 浅合并
    pub fn get_all_cfg(&self, name: &str) -> Value {
        let def = self.get_def_set(name);
        let user = self.get_config(name);
        merge_shallow(&def, &user)
    }

    /// ≈ cfgProxy.get — cfg.bot / cfg.group ...
    pub fn get(&self, name: &str) -> Value {
        self.get_all_cfg(name)
    }

    pub fn bot(&self) -> Value {
        self.get_all_cfg("bot")
    }

    /// ≈ getOther
    pub fn get_other(&self) -> Value {
        self.get_all_cfg("other")
    }

    /// ≈ masterQQ
    pub fn master_qq(&self) -> Vec<Value> {
        let other = self.get_other();
        if let Some(m) = other.get("masterQQs") {
            return to_array_owned(m);
        }
        to_array_owned(other.get("masterQQ").unwrap_or(&Value::Null))
    }

    /// ≈ master — Bot账号:[主人帐号]，"bot:user" 解析
    pub fn master(&self) -> HashMap<String, Vec<String>> {
        let other = self.get_other();
        if let Some(Value::Object(m)) = other.get("masters") {
            let mut ret = HashMap::new();
            for (k, v) in m {
                ret.insert(k.clone(), to_string_array(v));
            }
            return ret;
        }
        let mut masters: HashMap<String, Vec<String>> = HashMap::new();
        for i in to_array_owned(other.get("master").unwrap_or(&Value::Null)) {
            let s = crate::util::string(&i);
            let mut parts = s.splitn(2, ':');
            let bot_id = parts.next().unwrap_or("").to_string();
            let user_id = parts.next().unwrap_or("").to_string();
            masters.entry(bot_id).or_default().push(user_id);
        }
        masters
    }

    /// ≈ getGroup — default → {bot}:default → {group} → {bot}:{group} 四层合并
    pub fn get_group(&self, bot_id: &str, group_id: &str) -> Value {
        let config = self.get_all_cfg("group");
        let mut ret = config.get("default").cloned().unwrap_or(Value::Null);
        ret = merge_shallow(&ret, &config.get(format!("{}:default", bot_id)).cloned().unwrap_or(Value::Null));
        ret = merge_shallow(&ret, &config.get(group_id).cloned().unwrap_or(Value::Null));
        merge_shallow(&ret, &config.get(format!("{}:{}", bot_id, group_id)).cloned().unwrap_or(Value::Null))
    }
}

fn merge_shallow(def: &Value, user: &Value) -> Value {
    match (def, user) {
        (Value::Object(a), Value::Object(b)) => {
            let mut m: Map<String, Value> = a.clone();
            for (k, v) in b {
                m.insert(k.clone(), v.clone());
            }
            Value::Object(m)
        }
        (_, Value::Null) => def.clone(),
        (Value::Null, _) => user.clone(),
        (_, u) => u.clone(),
    }
}

fn to_array_owned(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Null => vec![],
        other => vec![other.clone()],
    }
}

fn to_string_array(v: &Value) -> Vec<String> {
    match v {
        Value::Array(a) => a.iter().map(crate::util::string).collect(),
        Value::Null => vec![],
        other => vec![crate::util::string(other)],
    }
}
