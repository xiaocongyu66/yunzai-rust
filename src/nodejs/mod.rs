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

/// 编译期内嵌的 napi 桥（build.rs 从 YZ_BRIDGE_BIN 拷入 OUT_DIR；本地开发为空占位）
pub const EMBED_BRIDGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/yz_bridge.node"));


/// Linux memfd 匿名内存桥 → /dev/fd/<fd>
#[cfg(target_os = "linux")]
fn memfd_bridge() -> Option<PathBuf> {
    use std::io::Write;
    use std::os::fd::FromRawFd;
    extern "C" {
        fn memfd_create(name: *const u8, flags: u32) -> i32;
        fn ftruncate(fd: i32, length: i64) -> i32;
    }
    let fd = unsafe { memfd_create(b"yz_bridge.node\0".as_ptr(), 0x0001) };
    if fd < 3 {
        return None;
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(EMBED_BRIDGE).ok()?;
    file.flush().ok()?;
    if unsafe { ftruncate(fd, EMBED_BRIDGE.len() as i64) } != 0 {
        return None;
    }
    std::mem::forget(file); // fd 保活（node require 与 dlopen 都要读它）
    let p = PathBuf::from(format!("/dev/fd/{fd}"));
    if probe_bridge(&p) {
        Some(p)
    } else {
        None
    }
}

/// macOS shm 匿名内存桥（open+unlink → /dev/fd/<fd>）
#[cfg(target_os = "macos")]
fn shm_bridge() -> Option<PathBuf> {
    use std::io::Write;
    use std::os::fd::FromRawFd;
    extern "C" {
        fn shm_open(name: *const u8, oflag: i32, mode: u32) -> i32;
        fn shm_unlink(name: *const u8) -> i32;
        fn ftruncate(fd: i32, length: i64) -> i32;
    }
    // O_CREAT|O_RDWR = 0x0002|0x0002 = 0x0002 (O_CREAT=0x0400 on mac? 实际
    // macOS: O_CREAT=0x0400, O_RDWR=0x0002) → 0x0402；mode 0600
    let fd = unsafe { shm_open(b"/yz_bridge\0".as_ptr(), 0x0402, 0o600) };
    if fd < 3 {
        return None;
    }
    unsafe { shm_unlink(b"/yz_bridge\0".as_ptr()) }; // 名字立即消失 → 匿名
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.write_all(EMBED_BRIDGE).ok()?;
    file.flush().ok()?;
    if unsafe { ftruncate(fd, EMBED_BRIDGE.len() as i64) } != 0 {
        return None;
    }
    std::mem::forget(file);
    let p = PathBuf::from(format!("/dev/fd/{fd}"));
    if probe_bridge(&p) {
        Some(p)
    } else {
        None
    }
}

/// dlopen 试探（成功即保留句柄：embed.rs 再次 open 同路径得同一镜像）
fn probe_bridge(path: &Path) -> bool {
    use libloading::os::unix::{Library as UnixLib, RTLD_LAZY};
    match unsafe { UnixLib::open(Some(path), RTLD_LAZY) } {
        Ok(lib) => {
            std::mem::forget(lib);
            true
        }
        Err(_) => false,
    }
}

/// 内嵌桥三级内存加载（照搬 ccb embedded-stage 实证策略，按"尽可能不落盘"排序）：
///
/// 1. memfd_create → /dev/fd/<fd>——内核匿名内存，零文件对象。仅正常 Linux
///    可用；dlopen 内存加载只有 /dev/fd 路径可用（/proc/self/fd → "file too
///    short"，Buffer 形式 → ERR_DLOPEN_FAILED，bun 1.4.2 实测）。memfd 须先
///    ftruncate 定长（初始 size=0）；fd 不 close（dlopen 以路径字符串为缓存
///    键，close 后 fd 复用会撞缓存）。
/// 2. /dev/shm tmpfs——介质是 RAM，磁盘零写入；proot 可用。固定文件名 +
///    启动覆盖写（tmpfs 重启即清，天然免清理）。
/// 3. std::env::temp_dir() 兜底——多数设备上它本身也是 tmpfs。
///
/// 每级产出都过一次 dlopen(RTLD_LAZY) 试探，失败即降级下一级；全失败再回
/// 退发行包 lib/ 文件（find_bundled_bridge）。
#[cfg(unix)]
fn load_embedded_bridge() -> anyhow::Result<PathBuf> {
    use libloading::os::unix::{Library as UnixLib, RTLD_LAZY};

    // 1. Linux：memfd_create → /dev/fd/<fd>（内核匿名内存，零文件对象）
    #[cfg(target_os = "linux")]
    if let Some(p) = memfd_bridge() {
        return Ok(p);
    }
    #[cfg(target_os = "linux")]
    crate::util::make_log1(crate::logger::Level::Info, Some("Node"), "memfd 不可用（proot/老内核？）→ shm 降级".to_string());

    // 1'. macOS：shm_open + 立即 shm_unlink = 匿名内存对象（名字已消失，
    //     对象随 fd 生命周期，语义等价 memfd），/dev/fd/<fd> 喂 dlopen
    #[cfg(target_os = "macos")]
    if let Some(p) = shm_bridge() {
        return Ok(p);
    }
    #[cfg(target_os = "macos")]
    crate::util::make_log1(crate::logger::Level::Info, Some("Node"), "shm 匿名加载不可用 → tmpfs 降级".to_string());

    // 2./3. tmpfs / tmpdir（磁盘零写路径：/dev/shm 与多数 tmpdir 是 RAM 介质；
    //    Windows 无内存 dlopen 路径（LoadLibrary 架构性只收路径），保留 temp 兜底）
    let mut bases: Vec<PathBuf> = vec![];
    if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
        bases.push("/dev/shm".into());
    }
    bases.push(std::env::temp_dir());

    // 2./3. tmpfs / tmpdir（写固定名文件，覆盖式）
    let mut bases: Vec<PathBuf> = vec!["/dev/shm".into()];
    bases.push(std::env::temp_dir());
    for base in bases {
        let p = base.join("yz_bridge.node");
        if std::fs::write(&p, EMBED_BRIDGE).is_ok() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700));
            }
            if probe_bridge(&p) {
                crate::util::make_log1(crate::logger::Level::Info, Some("Node"), format!("内嵌桥经 tmpfs 加载: {}", p.display()));
                return Ok(p);
            }
            let _ = std::fs::remove_file(&p);
        }
    }
    anyhow::bail!("内嵌桥内存加载三级全失败（memfd//dev/shm/tmpdir）")
}

pub struct JsEngine;

impl JsEngine {
    /// 初始化：确保 libnode → dlopen 嵌入 → 等待 host.mjs 就绪
    pub async fn new() -> anyhow::Result<JsEngine> {
        let cfg = crate::GLOBAL_CFG.get();
        let (lib_path, _fresh) = manager::ensure_libnode(cfg.map(|c| &**c)).await?;
        // 先 dlopen libnode（RTLD_GLOBAL）：桥的 napi_* 未定义符号必须能解析到它，
        // 否则内存加载的 probe 必失败（内嵌桥 probe 先于 embed::start）
        {
            use libloading::os::unix::{Library as UnixLib, RTLD_GLOBAL, RTLD_LAZY};
            match unsafe { UnixLib::open(Some(&lib_path), RTLD_LAZY | RTLD_GLOBAL) } {
                Ok(lib) => std::mem::forget(lib), // 常驻；embed::start 再次 open 同镜像
                Err(e) => anyhow::bail!("libnode 预加载失败: {e}"),
            }
        }
        // bridge.node / host.mjs 运行时文件（先建缓存目录，tempdir 场景不预置）
        let _ = std::fs::create_dir_all(manager::cache_dir());
        // 桥优先 memfd 内存直载（不落盘）；空内嵌回退发行包文件。
        // node 侧 require 相对 host.mjs 解析 → 路径必须绝对
        let bridge_path = if !EMBED_BRIDGE.is_empty() {
            #[cfg(unix)]
            {
                load_embedded_bridge()?
            }
            #[cfg(not(unix))]
            {
                let p = manager::cache_dir().join("yz_bridge.node");
                std::fs::write(&p, EMBED_BRIDGE).map_err(|e| anyhow::anyhow!("写出内嵌 bridge 失败: {e}"))?;
                p
            }
        } else {
            let p = manager::cache_dir().join("yz_bridge.node");
            match find_bundled_bridge() {
                Some(b) => {
                    std::fs::copy(&b, &p).map_err(|e| anyhow::anyhow!("拷贝 bridge 失败: {e}"))?;
                    std::env::current_dir().map(|c| c.join(&p)).unwrap_or(p)
                }
                None => anyhow::bail!("yz_bridge.node 缺失（内嵌为空且发行包 lib/ 无此文件）"),
            }
        };
        let host_path = manager::cache_dir().join("host.mjs");
        std::fs::write(&host_path, HOST_MJS)?;
        // 生态路径映射：社区插件普遍 `import cfg from "../../lib/config/config.js"`——磁盘写真实模块
        write_eco_shims(Path::new("."), &bridge_path.to_string_lossy())?;

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

/// 写生态映射模块（lib/config/config.js 等；bridge 路径字面量化，兼容 memfd fd 路径）
fn write_eco_shims(root: &Path, bridge: &str) -> anyhow::Result<()> {
    let cfg_dir = root.join("lib/config");
    std::fs::create_dir_all(&cfg_dir)?;
    let cfg_js = r#"// 生态映射：TRSS-Yunzai 的 cfg 门面（经 yz-bridge 与 Rust 主进程通信）
import { createRequire } from 'node:module'
const require = createRequire(import.meta.url)
let bridge
{
  const p = {bridge}
  if (p.startsWith('/dev/fd/') || p.startsWith('/proc/self/fd/')) {
    const m = { exports: {} }
    process.dlopen(m, p)
    bridge = m.exports
  } else {
    bridge = require(p)
  }
}
const parse = (s) => { try { return JSON.parse(s) } catch { return null } }
const cfgProxy = new Proxy({}, {
  get(_, prop) {
    if (prop === 'then') return undefined
    if (prop === 'getGroup') return async (botId, gid) => parse(bridge.op('cfg_get_group', JSON.stringify({ bot_id: botId ?? '', group_id: gid ?? '' })))
    if (prop === 'getOther') return async () => parse(bridge.op('cfg_get', JSON.stringify({ name: 'other' })))
    if (prop === 'getdefSet') return async (n) => parse(bridge.op('cfg_get_config', JSON.stringify({ name: n })))
    if (prop === 'getConfig') return async (n) => parse(bridge.op('cfg_get', JSON.stringify({ name: n })))
    return parse(bridge.op('cfg_get', JSON.stringify({ name: String(prop) })))
  },
})
export default cfgProxy
export { cfgProxy as config, cfgProxy as cfg }
"#;
    let bridge_lit = format!("\"{}\"", bridge.replace('\\', "\\\\"));
    std::fs::write(cfg_dir.join("config.js"), cfg_js.replace("{bridge}", &bridge_lit))?;
    let plg_dir = root.join("lib/plugins");
    std::fs::create_dir_all(&plg_dir)?;
    std::fs::write(plg_dir.join("plugin.js"), "export default globalThis.plugin
")?;
    let cmn_dir = root.join("lib/common");
    std::fs::create_dir_all(&cmn_dir)?;
    std::fs::write(cmn_dir.join("common.js"), "export default {}
")?;
    Ok(())
}

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
