//! 跨语言状态机（宿主侧）：PENDING 表 + oneshot 回传 + 指令投递
//!
//! Rust(tokio) → bridge yz_dispatch_cmd → node tsfn → host.mjs dispatcher
//!   → bridge __resolve → resolve_impl → oneshot 唤醒
//! JS → bridge op/op_async → host op / op_async_submit → tokio 执行
//!   → bridge yz_complete_async → JS deferred

use once_cell::sync::Lazy;
use serde_json::{json, Value as J};
use std::collections::HashMap;
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::oneshot;

pub type OpFn = unsafe extern "C" fn(name: *const c_char, args: *const c_char) -> *const c_char;
pub type OpAsyncSubmitFn = unsafe extern "C" fn(id: u64, name: *const c_char, args: *const c_char);
pub type ResolveFn = unsafe extern "C" fn(id: u64, result: *const c_char);
pub type OnReadyFn = unsafe extern "C" fn();
pub type LogFn = unsafe extern "C" fn(level: c_int, msg: *const c_char);

/// bridge ↔ host 的指针交换结构（bridge/src/lib.rs YzHostFns 的镜像）
#[repr(C)]
pub struct YzHostFns {
    pub op: OpFn,
    pub op_async_submit: OpAsyncSubmitFn,
    pub resolve: ResolveFn,
    pub on_ready: OnReadyFn,
    pub log: LogFn,
}

/// bridge 返回的函数组（bridge/src/lib.rs BridgeFns 的镜像）
#[repr(C)]
pub struct BridgeFns {
    pub dispatch_cmd: unsafe extern "C" fn(cmd: *const c_char) -> c_int,
    pub complete_async: unsafe extern "C" fn(id: u64, json: *const c_char),
}

// ============================ 全局状态 ============================

static PENDING: Lazy<Mutex<HashMap<u64, oneshot::Sender<String>>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static SEQ: AtomicU64 = AtomicU64::new(1);
pub static MAIN_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();
pub static READY: OnceLock<tokio::sync::Notify> = OnceLock::new();
/// bridge 函数组（embed 阶段 yz_init 换回）
pub static BRIDGE: OnceLock<&'static BridgeFns> = OnceLock::new();
static HOST_FNS: OnceLock<&'static YzHostFns> = OnceLock::new();

/// 稳定 C ABI 边界：host.mjs 到宿主的 op 结果需要跨越同步调用返回
/// ——约定：宿主返回的 *const c_char 指向一个进程级常量区（本实现：临时 static 缓冲，
/// node 侧 op() 是同步调用，返回值在 bridge 侧立即 to_string_lossy 拷贝）
const RET_BUF_COUNT: usize = 64;
static RET_BUF: Lazy<Vec<Mutex<String>>> = Lazy::new(|| (0..RET_BUF_COUNT).map(|_| Mutex::new(String::new())).collect());
static RET_SEQ: AtomicU64 = AtomicU64::new(0);

fn ret_buf_put(s: String) -> *const c_char {
    let idx = (RET_SEQ.fetch_add(1, Ordering::Relaxed) % RET_BUF_COUNT as u64) as usize;
    let mut g = RET_BUF[idx].lock().unwrap();
    // String 无 NUL 终止符，CStr 读取会越界带上旧残留字节——内嵌 NUL 结尾
    *g = s + "\0";
    g.as_ptr() as *const c_char
}

// ============================ 宿主函数实现 ============================

fn read_cstr(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

fn complete_async(id: u64, result: &str) {
    if let Some(b) = BRIDGE.get() {
        if let Ok(c) = CString::new(result) {
            unsafe { (b.complete_async)(id, c.as_ptr()) };
        }
    }
}

/// 同步 op（node 线程直调）
unsafe extern "C" fn op_impl(name: *const c_char, args: *const c_char) -> *const c_char {
    let name = read_cstr(name);
    let args_s = read_cstr(args);
    let args: J = serde_json::from_str(&args_s).unwrap_or(json!({}));
    let ret = super::ops::op_sync(&name, &args);
    ret_buf_put(serde_json::to_string(&ret).unwrap_or_else(|_| "null".to_string()))
}

/// 异步 op 提交：回到主 tokio 运行时执行
unsafe extern "C" fn op_async_submit_impl(id: u64, name: *const c_char, args: *const c_char) {
    let name = read_cstr(name);
    let args_s = read_cstr(args);
    if let Some(handle) = MAIN_HANDLE.get() {
        let fut = async move {
            let args: J = serde_json::from_str(&args_s).unwrap_or(json!({}));
            let ret = super::ops::op_async(&name, args).await;
            let s = serde_json::to_string(&ret).unwrap_or_else(|_| "null".to_string());
            complete_async(id, &s);
        };
        handle.spawn(fut);
    } else {
        complete_async(id, r#"{"error":"no tokio handle"}"#);
    }
}

/// JS dispatcher 结果回传
unsafe extern "C" fn resolve_impl(id: u64, result: *const c_char) {
    let res = read_cstr(result);
    let tx = PENDING.lock().unwrap().remove(&id);
    if let Some(tx) = tx {
        let _ = tx.send(res);
    }
    // 无匹配（超时弃单）→ 静默丢弃
}

/// host.mjs 就绪
extern "C" fn on_ready_impl() {
    if let Some(n) = READY.get() {
        n.notify_waiters();
    }
}

/// 日志桥：level 0=debug 1=info 2=warn 3=error
unsafe extern "C" fn log_impl(level: c_int, msg: *const c_char) {
    let s = read_cstr(msg);
    let lvl = match level {
        0 => crate::logger::Level::Debug,
        1 => crate::logger::Level::Info,
        2 => crate::logger::Level::Warn,
        _ => crate::logger::Level::Error,
    };
    crate::util::make_log1(lvl, Some("Node"), s);
}

// ============================ 对 embed / 门面的接口 ============================

/// 构造并注册宿主函数组，换回 bridge 函数组（embed::start 中调用；yz_init 在 bridge.node 里）
pub fn exchange_fns(bridge_lib: &libloading::os::unix::Library) -> anyhow::Result<()> {
    let host_fns: &'static YzHostFns = Box::leak(Box::new(YzHostFns {
        op: op_impl,
        op_async_submit: op_async_submit_impl,
        resolve: resolve_impl,
        on_ready: on_ready_impl,
        log: log_impl,
    }));
    let init: libloading::os::unix::Symbol<unsafe extern "C" fn(*const YzHostFns) -> *const BridgeFns> =
        unsafe { bridge_lib.get(b"yz_init\0") }?;
    let bridge_fns: *const BridgeFns = unsafe { init(host_fns as *const YzHostFns) };
    let _ = HOST_FNS.set(host_fns);
    let _ = BRIDGE.set(unsafe { &*bridge_fns });
    Ok(())
}

/// Rust → JS 投递指令（非阻塞）
pub fn dispatch_cmd(cmd: &J) -> bool {
    let s = serde_json::to_string(cmd).unwrap_or_default();
    match BRIDGE.get() {
        Some(b) => {
            let c = CString::new(s).unwrap_or_default();
            unsafe { (b.dispatch_cmd)(c.as_ptr()) == 0 }
        }
        None => false,
    }
}

/// Rust → JS 一次调用（等待 dispatcher 回传）
pub async fn call_js(cmd: J, timeout: Duration) -> Option<String> {
    let id = SEQ.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = oneshot::channel();
    let mut cmd = cmd;
    if let Some(obj) = cmd.as_object_mut() {
        obj.insert("__id".into(), json!(id));
    }
    {
        let mut p = PENDING.lock().unwrap();
        if p.len() >= 512 {
            crate::util::make_log1(crate::logger::Level::Warn, Some("Node"), "指令队列积压(512)，拒绝新指令".to_string());
            return None;
        }
        p.insert(id, tx);
    }
    if !dispatch_cmd(&cmd) {
        PENDING.lock().unwrap().remove(&id);
        return None;
    }
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(res)) => Some(res),
        _ => {
            // 超时/弃单
            PENDING.lock().unwrap().remove(&id);
            None
        }
    }
}

/// 等待 host.mjs 就绪
pub async fn wait_ready(timeout: Duration) -> bool {
    let notify = READY.get_or_init(tokio::sync::Notify::new);
    let notified = notify.notified();
    tokio::select! {
        _ = notified => true,
        _ = tokio::time::sleep(timeout) => false,
    }
}
