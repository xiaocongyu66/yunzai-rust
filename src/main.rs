//! ≈ app.js — 入口：stop / daemon / start 模式
mod adapters;
mod bot;
mod config;
mod events;
// mod jsrt; // 已由 nodejs（dlopen libnode）取代
mod listener;
mod logger;
mod nodejs;
mod plugins;
mod renderer;
mod segment;
mod util;

use bot::Bot;
use config::Cfg;
use once_cell::sync::Lazy;
use serde_json::Value;
use std::sync::Arc;

/// 全局配置（loader 等模块引用）
pub static GLOBAL_CFG: std::sync::OnceLock<Arc<Cfg>> = std::sync::OnceLock::new();

/// ≈ process.start_type
pub static START_TYPE: Lazy<String> = Lazy::new(|| {
    match std::env::var("YZ_START_TYPE") {
        Ok(v) => v,
        Err(_) => "internal".to_string(),
    }
});

fn main() {
    // CWD 自愈：config/ 必须可达（后台 shell 的 cwd 漂移会让相对路径全炸）
    if !std::path::Path::new("config/default_config").exists() {
        if let Ok(exe) = std::env::current_exe() {
            // 约定：bin/<binary> → 项目根为 exe 父目录的父级
            if let Some(root) = exe.parent().and_then(|p| p.parent()) {
                if root.join("config/default_config").exists() {
                    let _ = std::env::set_current_dir(root);
                }
            }
        }
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or("");

    // 渲染测试：yunzai --render-test <in.html> [out.png] [width]
    if mode == "--render-test" {
        let file = args.get(1).map(String::as_str).unwrap_or("/tmp/test.html");
        let out = args.get(2).map(String::as_str).unwrap_or("/tmp/test.png");
        let width: u32 = args.get(3).and_then(|a| a.parse().ok()).unwrap_or(720);
        match std::fs::read_to_string(file) {
            Ok(html) => match crate::renderer::render(&html, width, &[]) {
                Ok(png) => {
                    std::fs::write(out, &png).ok();
                    println!("OK {} -> {} ({} bytes)", file, out, png.len());
                }
                Err(e) => {
                    eprintln!("render error: {}", e);
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!("read error: {}", e);
                std::process::exit(1);
            }
        }
        return;
    }

    // ≈ app.js stop — 通知运行中的实例退出
    if mode == "stop" {
        let cfg = Cfg::new();
        let port = cfg
            .get("server")
            .get("port")
            .and_then(Value::as_u64)
            .unwrap_or(2536);
        let auth = cfg.get("server").get("auth").cloned().unwrap_or(Value::Null);
        tokio::runtime::Runtime::new().unwrap().block_on(async move {
            let _ = http_exit(port, &auth).await;
        });
        return;
    }

    // ≈ app.js daemon — 守护进程：子进程退出码非 255 时重启
    if mode == "daemon" {
        println!("守护进程正在启动主进程");
        loop {
            let exe = std::env::current_exe().unwrap_or_else(|_| "yunzai".into());
            let status = std::process::Command::new(exe)
                .arg("start")
                .status()
                .map(|s| s.code().unwrap_or(0));
            match status {
                Ok(255) => break,
                Ok(_) => println!("守护进程正在重启主进程"),
                Err(err) => {
                    eprintln!("守护进程启动失败: {}", err);
                    break;
                }
            }
        }
        println!("守护进程已停止");
        return;
    }

    // ≈ internal / external start
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(run());
}

async fn http_exit(port: u64, auth: &Value) -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port as u16))
        .await
        .map_err(|e| e.to_string())?;
    let mut req = format!("GET /exit HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n", port);
    if let Value::Object(map) = auth {
        for (k, v) in map {
            req += &format!("{}: {}\r\n", k, util::string(v));
        }
    }
    req += "\r\n";
    stream.write_all(req.as_bytes()).await.map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 256];
    let _ = stream.read(&mut buf).await;
    Ok(())
}

/// ≈ lib/config/init.js — 启动初始化
async fn run() {
    // 时区、panic 钩子、横幅
    config::init::pre_init();

    // 信号处理：SIGHUP/SIGTERM → 退出
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        for kind in [SignalKind::hangup(), SignalKind::terminate()] {
            let mut sig = match signal(kind) {
                Ok(s) => s,
                Err(_) => continue,
            };
            tokio::spawn(async move {
                sig.recv().await;
                std::process::exit(0);
            });
        }
    }

    let cfg = Cfg::new();
    let _ = GLOBAL_CFG.set(cfg.clone());

    // ≈ global.Bot = new Yunzai(); Bot.run()
    let bot = Bot::new(cfg);
    bot.run().await;

    // ctrl-c → exit
    tokio::signal::ctrl_c().await.ok();
    bot.exit(0).await;
}
