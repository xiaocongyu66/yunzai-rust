//! yz-bridge — Node.js napi addon：yunzai-rust 主进程 ↔ Node 插件沙箱的双向桥
//!
//! 状态归属：bridge 零业务。全部 yunzai 能力经 `yz_init` 注入的宿主函数指针
//! （主 crate 与本 cdylib 各持一份 rlib 静态区，跨边界状态必须走指针交换）。

use napi::bindgen_prelude::*;
use napi::{Env, JsDeferred, JsFunction, JsObject, Result};
use std::collections::HashMap;
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::Mutex;

/// 宿主注入的函数指针组（Rust 主 crate 提供）
#[repr(C)]
pub struct YzHostFns {
    /// 同步 op：name + argsJson → resultJson（返回值需 host 提供 yz_free 释放？——约定：返回值由 host 内 static 缓冲，node 侧立即拷贝）
    pub op: unsafe extern "C" fn(name: *const c_char, args: *const c_char) -> *const c_char,
    /// 异步 op 提交：host 在自己的 tokio 运行时执行，完成后调 yz_complete_async(id, json)
    pub op_async_submit: unsafe extern "C" fn(id: u64, name: *const c_char, args: *const c_char),
    /// JS dispatcher 结果回传（Rust→JS 调用的应答）
    pub resolve: unsafe extern "C" fn(id: u64, result: *const c_char),
    /// host.mjs 就绪
    pub on_ready: unsafe extern "C" fn(),
    /// 日志桥（level: 0=debug 1=info 2=warn 3=error）
    pub log: unsafe extern "C" fn(level: c_int, msg: *const c_char),
}

/// 本 addon 暴露给宿主的函数指针组
#[repr(C)]
pub struct BridgeFns {
    /// 宿主 → tsfn 投递 JSON 指令（非阻塞）
    pub dispatch_cmd: unsafe extern "C" fn(cmd: *const c_char) -> c_int,
    /// 宿主完成异步 op → resolve JS deferred
    pub complete_async: unsafe extern "C" fn(id: u64, json: *const c_char),
}

static HOST: Mutex<Option<*const YzHostFns>> = Mutex::new(None);
static JS_DEFS: Mutex<Option<HashMap<u64, JsDeferred<String>>>> = Mutex::new(None);

fn host() -> Option<&'static YzHostFns> {
    HOST.lock().ok().and_then(|g| g.as_ref().map(|p| unsafe { &**p }))
}

// ============================ C ABI（宿主调用） ============================

/// 指针交换入口：宿主传 YzHostFns，本 addon 返回 BridgeFns
#[no_mangle]
pub extern "C" fn yz_init(host_fns: *const YzHostFns) -> *const BridgeFns {
    if let Ok(mut g) = HOST.lock() {
        *g = Some(host_fns);
    }
    &BRIDGE_FNS
}

static BRIDGE_FNS: BridgeFns = BridgeFns {
    dispatch_cmd,
    complete_async,
};

/// 宿主 → tsfn 投递指令
unsafe extern "C" fn dispatch_cmd(cmd: *const c_char) -> c_int {
    let json = CStr::from_ptr(cmd).to_string_lossy().into_owned();
    CMD_TSFN.with(|cell| match cell.get() {
        Some(tsfn) => match tsfn.call(Ok(json)) {
            Ok(()) => 0,
            Err(_) => -1,
        },
        None => -2,
    })
}

thread_local! {
    static CMD_TSFN: std::cell::OnceCell<ThreadsafeFunction<String, ErrorStrategy::CalleeHandled>> =
        const { std::cell::OnceCell::new() };
}

/// 宿主完成异步 op → resolve JS deferred
unsafe extern "C" fn complete_async(id: u64, json: *const c_char) {
    let val = CStr::from_ptr(json).to_string_lossy().into_owned();
    if let Ok(mut g) = JS_DEFS.lock() {
        if let Some(map) = g.as_mut() {
            if let Some(deferred) = map.remove(&id) {
                deferred.resolve(val);
            }
        }
    }
}

// ============================ napi 导出（JS 调用） ============================

/// host.mjs 启动时注册 dispatcher：Rust 指令由此泵入 JS
#[napi]
pub fn ready(env: Env, dispatcher: JsFunction) -> Result<()> {
    let _ = env;
    let tsfn: ThreadsafeFunction<String, ErrorStrategy::CalleeHandled> =
        dispatcher.create_threadsafe_function(0, |ctx| {
            // 把 Rust 指令 JSON 作为第一个参数传给 dispatcher
            let arg = ctx.env.create_string(&ctx.value)?.to_unknown();
            Ok(vec![arg])
        })?;
    CMD_TSFN.with(|cell| {
        let _ = cell.set(tsfn);
    });
    if let Some(h) = host() {
        unsafe { (h.on_ready)() };
    } else {
        return Err(Error::new(Status::GenericFailure, "yz-bridge: host not initialized"));
    }
    Ok(())
}

/// 同步 op（node 线程直调宿主）
#[napi]
pub fn op(name: String, args: String) -> Result<String> {
    match host() {
        Some(h) => {
            let c_name = CString::new(name)?;
            let c_args = CString::new(args)?;
            let ret = unsafe { (h.op)(c_name.as_ptr(), c_args.as_ptr()) };
            let s = unsafe { CStr::from_ptr(ret).to_string_lossy().into_owned() };
            Ok(s)
        }
        None => Ok(r#"{"error":"bridge host not initialized"}"#.to_string()),
    }
}

/// 异步 op：返回 Promise，宿主 tokio 执行后 resolve
#[napi]
pub fn op_async(env: Env, name: String, args: String) -> Result<JsObject> {
    let (deferred, promise) = env.create_deferred::<String, ()>()?;
    let id = next_def_id();
    if let Ok(mut g) = JS_DEFS.lock() {
        let map = g.get_or_insert_with(HashMap::new);
        map.insert(id, deferred);
    }
    match host() {
        Some(h) => {
            let c_name = CString::new(name)?;
            let c_args = CString::new(args)?;
            unsafe { (h.op_async_submit)(id, c_name.as_ptr(), c_args.as_ptr()) };
        }
        None => {
            if let Ok(mut g) = JS_DEFS.lock() {
                if let Some(map) = g.as_mut() {
                    if let Some(d) = map.remove(&id) {
                        d.resolve(r#"{"error":"bridge host not initialized"}"#.to_string());
                    }
                }
            }
        }
    }
    Ok(promise)
}

/// JS dispatcher 执行完成 → 宿主 oneshot 唤醒
#[napi]
pub fn resolve(id: f64, result: String) -> Result<()> {
    if let Some(h) = host() {
        let c = CString::new(result)?;
        unsafe { (h.resolve)(id as u64, c.as_ptr()) };
    }
    Ok(())
}

/// JS 调宿主日志
#[napi]
pub fn log(level: i32, msg: String) -> Result<()> {
    if let Some(h) = host() {
        let c = CString::new(msg)?;
        unsafe { (h.log)(level, c.as_ptr()) };
    }
    Ok(())
}

static DEF_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn next_def_id() -> u64 {
    DEF_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
