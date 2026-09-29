//! 进程内嵌入 Node.js（dlopen libnode）真加载 TS/JS 插件
//!
//! 门面方法与旧 JsEngine(rquickjs) 签名一致——loader.rs 无感切换。

pub mod embed;
pub mod host;
pub mod manager;
pub mod ops;

use host::call_js;
use std::sync::OnceLock;
use serde_json::{json, Value as J};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub static GLOBAL_BOT: OnceLock<std::sync::Arc<crate::bot::Bot>> = OnceLock::new();

/// libnode 管理配置
fn node_cfg(cfg: Option<&crate::config::Cfg>) -> (bool, u32, u64) {
    let (autodl, mos, timeout) = cfg
        .map(|c| {
            let n = c.get("node");
            (
                n.get("autodownload").and_then(J::as_bool).unwrap_or(true),
                n.get("max_old_space_size").and_then(J::as_u64).unwrap_or(512) as u32,
                n.get("call_timeout_s").and_then(J::as_u64).unwrap_or(30),
            )
        })
        .unwrap_or((true, 512, 30));
    (autodl, mos, timeout)
}

pub struct JsEngine;

impl JsEngine {
    /// 初始化：确保 libnode → dlopen 嵌入 → 等待 host.mjs 就绪
    pub async fn new() -> anyhow::Result<JsEngine> {
        let cfg = crate::GLOBAL_CFG.get();
        let (lib_path, _fresh) = manager::ensure_libnode(cfg.map(|c| &**c)).await?;
        // bridge.node / host.mjs 运行时文件（先建缓存目录，tempdir 场景不预置）
        let _ = std::fs::create_dir_all(manager::cache_dir());
        let bridge_path = manager::cache_dir().join("yz_bridge.node");
        if let Some(bundled) = find_bundled_bridge() {
            std::fs::copy(&bundled, &bridge_path).map_err(|e| anyhow::anyhow!("拷贝 bridge 失败: {e}"))?;
        } else {
            return Err(anyhow::anyhow!(
                "yz_bridge.node 不存在（随发行包附带的 napi 桥缺失）。可从发行包 lib/ 目录恢复"
            ));
        }
        let host_path = manager::cache_dir().join("host.mjs");
        std::fs::write(&host_path, HOST_MJS)?;

        let (_, mos, _timeout_s) = node_cfg(cfg.map(|c| &**c));
        embed::start(lib_path, bridge_path, host_path, mos)?;
        if !host::wait_ready(Duration::from_secs(30)).await {
            return Err(anyhow::anyhow!("[Node] host.mjs 30s 内未就绪（嵌入失败，详见日志）"));
        }
        crate::util::make_log1(crate::logger::Level::Mark, Some("Node"), "嵌入 Node 插件引擎就绪".to_string());
        Ok(JsEngine)
    }

    pub async fn load_plugin(&self, path: &str, key: &str) -> Vec<JsPluginData> {
        let abs = std::fs::canonicalize(path)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string());
        let res = timeout_call(
            json!({ "cmd": "load", "path": abs, "key": key }),
            Duration::from_secs(120),
        )
        .await;
        let metas: Vec<J> = res.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or_default();
        metas.into_iter().map(js_plugin_data_from).collect()
    }

    pub async fn instantiate(&self, reg_key: &str, e_data: &J) -> Option<J> {
        let res = timeout_call(
            json!({ "cmd": "instantiate", "key": reg_key, "e": e_data }),
            DEFAULT_TIMEOUT,
        )
        .await?;
        let v: J = serde_json::from_str(&res).ok()?;
        v.get("rules").and_then(J::as_array).map(|arr| {
            json!({ "rules": arr })
        })
    }

    pub async fn call(&self, reg_key: &str, fnc: &str, e_data: &J) -> String {
        timeout_call(json!({ "cmd": "call", "key": reg_key, "fnc": fnc, "e": e_data }), DEFAULT_TIMEOUT)
            .await
            .unwrap_or_else(|| "null".into())
    }

    pub async fn accept(&self, reg_key: &str, e_data: &J) -> String {
        timeout_call(json!({ "cmd": "accept", "key": reg_key, "e": e_data }), DEFAULT_TIMEOUT)
            .await
            .unwrap_or_else(|| "null".into())
    }

    pub async fn regex_test(&self, reg_key: &str, idx: usize, msg: &str) -> bool {
        timeout_call(
            json!({ "cmd": "regex_test", "key": reg_key, "idx": idx, "msg": msg }),
            DEFAULT_TIMEOUT,
        )
        .await
        .and_then(|r| serde_json::from_str::<bool>(&r).ok())
        .unwrap_or(false)
    }
}

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

async fn timeout_call(cmd: J, timeout: Duration) -> Option<String> {
    call_js(cmd, timeout).await
}

// ============ 数据结构（与旧 jsrt 兼容） ============

#[derive(Clone, Debug)]
pub struct JsRule {
    pub reg_src: String,
    pub rust: Option<regex::Regex>,
    pub fnc: String,
    pub log: bool,
    pub permission: crate::plugins::plugin::Permission,
    pub event: Option<String>,
}

#[derive(Clone, Debug)]
pub struct JsTask {
    pub name: String,
    pub cron: String,
    pub fnc: String,
    pub log: bool,
}

#[derive(Clone, Debug)]
pub struct JsPluginData {
    pub reg_key: String,
    pub sub: String,
    pub name: String,
    pub dsc: String,
    pub event: String,
    pub priority: i64,
    pub rules: Vec<JsRule>,
    pub tasks: Vec<JsTask>,
}

fn js_rule_from(v: &J) -> JsRule {
    JsRule {
        reg_src: v.get("reg_src").map(crate::util::string).unwrap_or_default(),
        rust: None,
        fnc: v.get("fnc").map(crate::util::string).unwrap_or_default(),
        log: v.get("log").and_then(J::as_bool).unwrap_or(false),
        permission: crate::plugins::plugin::Permission::parse(
            v.get("permission").map(crate::util::string).unwrap_or_else(|| "all".into()).as_str(),
        ),
        event: v.get("event").and_then(|e| e.as_str()).map(String::from),
    }
}

fn js_task_from(v: &J) -> JsTask {
    JsTask {
        name: v.get("name").map(crate::util::string).unwrap_or_default(),
        cron: v.get("cron").map(crate::util::string).unwrap_or_default(),
        fnc: v.get("fnc").map(crate::util::string).unwrap_or_default(),
        log: v.get("log").and_then(J::as_bool).unwrap_or(false),
    }
}

fn js_plugin_data_from(v: J) -> JsPluginData {
    JsPluginData {
        reg_key: v.get("reg_key").map(crate::util::string).unwrap_or_default(),
        sub: v.get("sub").map(crate::util::string).unwrap_or_default(),
        name: v.get("name").map(crate::util::string).unwrap_or_default(),
        dsc: v.get("dsc").map(crate::util::string).unwrap_or_default(),
        event: v.get("event").map(crate::util::string).unwrap_or_else(|| "message".into()),
        priority: v.get("priority").and_then(J::as_i64).unwrap_or(5000),
        rules: v.get("rules").and_then(J::as_array).map(|a| a.iter().map(js_rule_from).collect()).unwrap_or_default(),
        tasks: v.get("tasks").and_then(J::as_array).map(|a| a.iter().map(js_task_from).collect()).unwrap_or_default(),
    }
}

/// 插件目录扫描（≈ loader.js getPlugins；含 .ts/.mts 支持）
pub fn scan_plugin_files(dir: &str) -> Vec<(String, String)> {
    let mut ret = vec![];
    let Ok(rd) = std::fs::read_dir(dir) else { return ret };
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        // index.js / index.ts 优先
        let index = ["index.js", "index.ts"]
            .iter()
            .map(|f| path.join(f))
            .find(|p| p.exists());
        if let Some(idx) = index {
            ret.push((format!("{name}/index.{}", idx.extension().map(|e| e.to_string_lossy()).unwrap_or_default()), idx.to_string_lossy().to_string()));
            continue;
        }
        if let Ok(apps) = std::fs::read_dir(&path) {
            for app in apps.filter_map(|e| e.ok()) {
                let ap = app.path();
                let is_js = ap.extension().map(|e| e == "js" || e == "ts").unwrap_or(false);
                if is_js {
                    let fname = app.file_name().to_string_lossy().to_string();
                    ret.push((format!("{name}/{fname}"), ap.to_string_lossy().to_string()));
                }
            }
        }
    }
    ret
}

/// 当前事件上下文守卫（同进程直接访问 ops::EventGuard）
pub struct EventGuard;
impl EventGuard {
    pub fn set(key: &str, bot: std::sync::Arc<crate::bot::Bot>, data: J) {
        ops::EventGuard::set(key, bot, data);
    }
    pub fn clear(key: &str) {
        ops::EventGuard::clear(key);
    }
    pub fn get(key: &str) -> Option<(std::sync::Arc<crate::bot::Bot>, J)> {
        ops::EventGuard::get(key)
    }
}

use std::collections::HashMap;
use std::sync::Mutex;

/// 内嵌 host.mjs
pub const HOST_MJS: &str = include_str!("host.mjs");

/// 在二进制旁/发行包 lib/ 下找随包 bridge
fn find_bundled_bridge() -> Option<PathBuf> {
    // 环境变量直接指定（测试/特殊部署场景）
    if let Ok(p) = std::env::var("YZ_BRIDGE_PATH") {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let candidates = [
        exe.parent()?.join("lib").join("yz_bridge.node"),
        exe.parent()?.join("yz_bridge.node"),
        PathBuf::from("lib").join("yz_bridge.node"),
    ];
    candidates.into_iter().find(|p| p.is_file())
}
