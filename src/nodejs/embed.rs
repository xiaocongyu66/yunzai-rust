//! 进程内嵌入 Node.js：dlopen(libnode) → yz_init 指针交换 → node::Start(host.mjs)
//!
//! node::Start 阻塞直至 node 退出——在专用线程运行；
//! host.mjs 拦截 process.exit 保持 event loop 永驻。

use std::sync::OnceLock;
use std::ffi::{c_char, CString};
use std::path::PathBuf;

/// libnode 动态库句柄（故意泄漏：node 注册 atexit 钩子依赖它存活）
pub static LIBNODE_LIB: OnceLock<libloading::Library> = OnceLock::new();
static STARTED: OnceLock<()> = OnceLock::new();

/// 启动嵌入（幂等）：返回 Ok(()) 表示已触发启动
pub fn start(libnode_path: PathBuf, bridge_path: PathBuf, host_path: PathBuf, max_old_space: u32) -> anyhow::Result<()> {
    if STARTED.get().is_some() {
        return Ok(());
    }
    let lib = unsafe { libloading::Library::new(&libnode_path) }?;
    // 指针交换：宿主函数 → bridge，换回 dispatch_cmd/complete_async
    super::host::exchange_fns(&lib)?;

    let host_s = host_path.to_string_lossy().into_owned();
    let bridge_s = bridge_path.to_string_lossy().into_owned();
    let mos = format!("--max-old-space-size={max_old_space}");

    // argv：[0]程序名 + V8 标志 + 入口脚本 + bridge 路径（host.mjs 从 argv[1] 读）
    let argv: Vec<CString> = vec![
        CString::new("yunzai-node")?,
        CString::new("--stack-size=8000")?,
        CString::new(mos)?,
        CString::new("--experimental-strip-types")?,
        CString::new(host_s)?,
        CString::new(bridge_s)?,
    ];
    let argc = argv.len() as i32;

    STARTED.set(()).ok();
    let _ = LIBNODE_LIB.set(lib);

    std::thread::Builder::new()
        .name("node-main".into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            let lib = LIBNODE_LIB.get().expect("libnode");
            // 裸指针不可跨线程，闭包内从 CString 重建（argv 为 move 所有权）
            let mut argv_ptrs: Vec<*const c_char> = argv.iter().map(|a| a.as_ptr() as *const c_char).collect();
            argv_ptrs.push(std::ptr::null());
            // node::Start C++ 修饰名（node.h NODE_EXTERN，shared 构建导出）
            let sym: Result<libloading::Symbol<unsafe extern "C" fn(i32, *mut *const c_char) -> i32>, _> =
                unsafe { lib.get(b"_ZN4node5StartEiPPc\0") };
            match sym {
                Ok(start_fn) => {
                    let ret = unsafe { start_fn(argc, argv_ptrs.as_mut_ptr() as *mut *const c_char) };
                    if ret != 0 {
                        crate::util::make_log1(
                            crate::logger::Level::Warn,
                            Some("Node"),
                            format!("node::Start 返回 {ret}"),
                        );
                    }
                }
                Err(e) => {
                    crate::util::make_log1(
                        crate::logger::Level::Error,
                        Some("Node"),
                        format!("未找到 node::Start 导出符号（需 CI 启用 node_embedding 补丁或导出清单）: {e}"),
                    );
                }
            }
        })?;
    Ok(())
}

