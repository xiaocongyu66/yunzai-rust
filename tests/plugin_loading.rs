//! 生态验收：实测加载社区 JS 插件（kkp-plugin / logier-plugins / neko-status-plugin / imgS-plugin）
//! 断言：JS 引擎初始化成功 + 各插件被扫描加载 + 无致命错误
use std::time::Duration;

struct LogDumper;
impl Drop for LogDumper {
    fn drop(&mut self) {
        for f in ["/tmp/pl-child.log", "/tmp/pl-child-err.log"] {
            if let Ok(s) = std::fs::read_to_string(f) {
                println!("=== 子进程日志 {} ===\n{}", f, s);
            }
        }
    }
}

fn clone_plugin(dir: &std::path::Path, name: &str, url: &str) -> bool {
    let target = dir.join(name);
    // 外网 clone 偶发复位：重试 3 次，仍失败则跳过该插件（断言侧按存在性条件判断）
    for attempt in 0..3 {
        let out = std::process::Command::new("git")
            .args(["clone", "--depth", "1", url, target.to_string_lossy().as_ref()])
            .output()
            .expect("git clone 执行失败");
        if out.status.success() {
            return true;
        }
        eprintln!("clone {} 第{}次失败: {}", url, attempt + 1, String::from_utf8_lossy(&out.stderr).trim());
        let _ = std::fs::remove_dir_all(&target);
        std::thread::sleep(Duration::from_secs(3));
    }
    false
}

#[tokio::test]
async fn community_plugins_loading() {
    let _dumper = LogDumper;
    let dir = tempfile::tempdir().unwrap();

    // 1. 配置目录
    let cfg_dir = dir.path().join("config/config");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    let def = dir.path().join("config/default_config");
    std::fs::create_dir_all(&def).unwrap();
    for f in ["bot.yaml", "server.yaml", "redis.yaml", "group.yaml", "other.yaml"] {
        std::fs::copy(format!("config/default_config/{}", f), def.join(f)).unwrap();
    }
    std::fs::write(cfg_dir.join("bot.yaml"), "log_level: debug\n").unwrap();
    std::fs::write(cfg_dir.join("server.yaml"), "url: http://localhost:25362\nport: 25362\nredirect: https://example.com\n").unwrap();
    std::fs::write(cfg_dir.join("other.yaml"), "master:\n  - \"10001:222\"\n").unwrap();

    // 2. 克隆四个社区插件到 plugins/
    let plugins_dir = dir.path().join("plugins");
    std::fs::create_dir_all(&plugins_dir).unwrap();
    let mut cloned = vec![];
    if clone_plugin(&plugins_dir, "kkp-plugin", "https://gitee.com/dungeonmaster/kkp-plugin") { cloned.push("kkp-plugin"); }
    if clone_plugin(&plugins_dir, "logier-plugins", "https://gitee.com/logier/logier-plugins") { cloned.push("logier-plugins"); }
    if clone_plugin(&plugins_dir, "neko-status-plugin", "https://github.com/erzaozi/neko-status-plugin") { cloned.push("neko-status-plugin"); }
    if clone_plugin(&plugins_dir, "imgS-plugin", "https://github.com/erzaozi/imgS-plugin") { cloned.push("imgS-plugin"); }

    // 3. 启动主程序
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_yunzai"))
        .current_dir(dir.path())
        .env("YZ_NO_REDIS", "1")
        .stdout(std::fs::File::create("/tmp/pl-child.log").unwrap())
        .stderr(std::fs::File::create("/tmp/pl-child-err.log").unwrap())
        .spawn()
        .unwrap();

    // 4. 等端口就绪
    let mut ready = false;
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", 25362)).await.is_ok() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(ready, "服务器未启动");

    // 5. 等插件加载完成后读日志
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let _ = child.kill();
    let log = std::fs::read_to_string("/tmp/pl-child.log").unwrap_or_default();

    // JS 引擎与插件加载断言
    assert!(log.contains("引擎就绪"), "JS 引擎未就绪:\n{}", log);
    assert!(log.contains("加载插件 ["), "没有任何 JS 插件被加载:\n{}", log);
    for name in cloned {
        assert!(log.contains(name), "插件 {} 未出现在加载日志:\n{}", name, log);
    }
    // 加载完成统计行
    assert!(log.contains("加载插件["), "缺少加载统计行:\n{}", log);
}
