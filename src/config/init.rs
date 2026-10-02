//! ≈ lib/config/init.js — 启动初始化：时区、信号、panic 钩子、横幅
use crate::logger::{self, Level};
use crate::util;

/// ≈ init.js 模块级初始化 + init() 函数
pub fn pre_init() {
    // 时区（edition 2024 起 set_var 为 unsafe；pre_init 在多线程启动前执行，安全）
    unsafe {
        std::env::set_var("TZ", "Asia/Shanghai");
    }
    // panic 钩子 → 错误日志（≈ uncaughtException/unhandledRejection）
    std::panic::set_hook(Box::new(|info| {
        util::make_log1(Level::Error, None, format!("uncaughtException: {}", info));
    }));

    logger::log(Level::Mark, None, &["----^_^----".to_string()]);
    util::make_log1(
        Level::Mark,
        None,
        logger::yellow(format!(
            "TRSS-Yunzai {} (Rust) 启动中...",
            crate::bot::VERSION.clone()
        )),
    );
    util::make_log1(
        Level::Mark,
        None,
        logger::cyan("https://git.trss.me/Yunzai".to_string()),
    );
}

/// ≈ process.on("exit") 退出日志
pub fn exit_log(runtime_ms: u64, code: i32) {
    util::make_log(
        Level::Mark,
        Some("exit"),
        false,
        vec![format!(
            "TRSS-Yunzai 已停止运行，本次运行时长：{} ({})",
            util::get_time_diff(runtime_ms, util::now_ms()),
            code
        )],
    );
}
