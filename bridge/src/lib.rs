//! yz-bridge — Node.js napi addon：yunzai-rust 主进程 ↔ Node 插件沙箱的双向桥
//!
//! 状态归属：bridge 零业务。全部 yunzai 能力经 `yz_init` 注入的宿主函数指针
//! （主 crate 与本 cdylib 各持一份 rlib 静态区，跨边界状态必须走指针交换）。

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{ErrorStrategy, ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Env, JsFunction, Result, Status};
use napi_derive::napi;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::Mutex;

/// 宿主注入的函数指针组（Rust 主 crate 提供）
#[repr(C)]
pub struct YzHostFns {
    /// 同步 op：name + argsJson → resultJson（返回值指向宿主进程级缓冲，node 侧立即拷贝）
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
    /// 宿主完成异步 op → 唤醒 JS 侧 await
    pub complete_async: unsafe extern "C" fn(id: u64, json: *const c_char),
}

/// Sender 指针非 Sync，static 手工包装；访问均经内部 Mutex 串行化
struct DefSync<T>(Mutex<T>);
unsafe impl<T> Sync for DefSync<T> {}

impl<T> DefSync<T> {
    fn lock(&self) -> std::sync::MutexGuard<'_, T> {
        self.0.lock().unwrap()
    }
}

static HOST: DefSync<Option<*const YzHostFns>> = DefSync(Mutex::new(None));
/// 异步 op 等待表：id → sender（futures oneshot）
static PENDING_OP: once_cell::sync::Lazy<DefSync<HashMap<u64, futures::channel::oneshot::Sender<String>>>> =
    once_cell::sync::Lazy::new(|| DefSync(Mutex::new(HashMap::new())));

fn host() -> Option<&'static YzHostFns> {
    HOST.lock().as_ref().map(|p| unsafe { &**p })
}

fn read_cstr(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
}

fn call_on_ready(host: &YzHostFns) {
    unsafe { (host.on_ready)() }
}

fn call_op(host: &YzHostFns, name: &CString, args: &CString) -> String {
    let ret = unsafe { (host.op)(name.as_ptr(), args.as_ptr()) };
    read_cstr(ret)
}

fn submit_async(host: &YzHostFns, id: u64, name: &CString, args: &CString) {
    unsafe { (host.op_async_submit)(id, name.as_ptr(), args.as_ptr()) }
}

fn resolve(host: &YzHostFns, id: u64, result: &CString) {
    unsafe { (host.resolve)(id, result.as_ptr()) }
}

fn log_message(host: &YzHostFns, level: i32, message: &CString) {
    unsafe { (host.log)(level, message.as_ptr()) }
}

/// tsfn 在 node JS 线程创建（ready），但 dispatch_cmd 在宿主 tokio 线程调用——必须全局共享
static CMD_TSFN: once_cell::sync::Lazy<
    DefSync<Option<ThreadsafeFunction<String, ErrorStrategy::CalleeHandled>>>,
> = once_cell::sync::Lazy::new(|| DefSync(Mutex::new(None)));

static DEF_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn next_def_id() -> u64 {
    DEF_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

// ============================ C ABI（宿主调用） ============================

/// 指针交换入口：宿主传 YzHostFns，本 addon 返回 BridgeFns
#[no_mangle]
pub extern "C" fn yz_init(host_fns: *const YzHostFns) -> *const BridgeFns {
    {
        let mut g = HOST.lock();
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
    let json = read_cstr(cmd);
    let guard = CMD_TSFN.lock();
    match guard.as_ref() {
        Some(tsfn) => {
            let st = tsfn.call(Ok(json), ThreadsafeFunctionCallMode::NonBlocking);
            if st == Status::Ok {
                0
            } else {
                -1
            }
        }
        None => -2,
    }
}

/// 宿主完成异步 op → 唤醒 op_async 的 await
unsafe extern "C" fn complete_async(id: u64, json: *const c_char) {
    let val = read_cstr(json);
    if let Some(tx) = PENDING_OP.lock().remove(&id) {
        let _ = tx.send(val);
    }
}

// ============================ napi 导出（JS 调用） ============================

/// host.mjs 启动时注册 dispatcher：Rust 指令由此泵入 JS
#[napi]
pub fn ready(env: Env, dispatcher: JsFunction) -> Result<()> {
    let _ = env;
    let tsfn: ThreadsafeFunction<String, ErrorStrategy::CalleeHandled> =
        dispatcher.create_threadsafe_function(
            0,
            |ctx: napi::threadsafe_function::ThreadSafeCallContext<String>| -> Result<Vec<napi::JsUnknown>> {
                let arg = ctx.env.create_string(ctx.value.as_str())?.into_unknown();
                Ok(vec![arg])
            },
        )?;
    {
        let mut g = CMD_TSFN.lock();
        *g = Some(tsfn);
    }
    match host() {
        Some(h) => {
            call_on_ready(h);
            Ok(())
        }
        None => Err(Error::new(Status::GenericFailure, "yz-bridge: host not initialized")),
    }
}

/// 同步 op（node 线程直调宿主）
#[napi]
pub fn op(name: String, args: String) -> Result<String> {
    match host() {
        Some(h) => {
            let c_name = CString::new(name)?;
            let c_args = CString::new(args)?;
            let s = call_op(h, &c_name, &c_args);
            Ok(s)
        }
        None => Ok(r#"{"error":"bridge host not initialized"}"#.to_string()),
    }
}

/// 异步 op：宿主 tokio 执行后 complete_async 唤醒此 await（napi 自动转 Promise）
/// 注意：napi 默认把 snake_case 转成 camelCase（op_async → opAsync），js_name 锁定原名
#[napi(js_name = "op_async")]
pub async fn op_async(name: String, args: String) -> Result<String> {
    let (tx, rx) = futures::channel::oneshot::channel::<String>();
    let id = next_def_id();
    PENDING_OP.lock().insert(id, tx);
    match host() {
        Some(h) => {
            let c_name = match CString::new(name) {
                Ok(s) => s,
                Err(e) => return Err(Error::new(Status::GenericFailure, e.to_string())),
            };
            let c_args = match CString::new(args) {
                Ok(s) => s,
                Err(e) => return Err(Error::new(Status::GenericFailure, e.to_string())),
            };
            submit_async(h, id, &c_name, &c_args);
        }
        None => {
            if let Some(tx) = PENDING_OP.lock().remove(&id) {
                let _ = tx.send(r#"{"error":"bridge host not initialized"}"#.to_string());
            }
        }
    }
    rx.await.map_err(|_| Error::new(Status::GenericFailure, "op_async cancelled"))
}

/// JS dispatcher 执行完成 → 宿主 oneshot 唤醒
#[napi]
pub fn resolve(id: f64, result: String) -> Result<()> {
    if let Some(h) = host() {
        let c = CString::new(result)?;
        resolve(h, id as u64, &c);
    }
    Ok(())
}

/// JS 调宿主日志
#[napi]
pub fn log(level: i32, msg: String) -> Result<()> {
    if let Some(h) = host() {
        let c = CString::new(msg)?;
        log_message(h, level, &c);
    }
    Ok(())
}
