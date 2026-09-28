//! ≈ lib/config/redis.js — Redis/Valkey 初始化，127.0.0.1 失败时自动拉起 redis-server
use crate::logger::{self, Level};
use crate::util;
use redis::aio::MultiplexedConnection;
use redis::ConnectionInfo;
use serde_json::Value;

pub struct RedisHandle {
    pub conn: MultiplexedConnection,
    pub process: Option<tokio::process::Child>,
}

impl RedisHandle {
    pub async fn get(&self, key: &str) -> Option<String> {
        let mut c = self.conn.clone();
        use redis::AsyncCommands;
        c.get(key).await.ok()
    }

    pub async fn set(&self, key: &str, val: &str) {
        let mut c = self.conn.clone();
        use redis::AsyncCommands;
        let _: Result<(), _> = c.set(key, val).await;
    }

    pub async fn set_ex(&self, key: &str, val: &str, secs: i64) {
        let mut c = self.conn.clone();
        use redis::AsyncCommands;
        let _: Result<(), _> = c.set_ex(key, val, secs as u64).await;
    }

    pub async fn del(&self, key: &str) {
        let mut c = self.conn.clone();
        use redis::AsyncCommands;
        let _: Result<i64, _> = c.del(key).await;
    }

    pub async fn incr_by(&self, key: &str, n: i64) {
        let mut c = self.conn.clone();
        use redis::AsyncCommands;
        let _: Result<i64, _> = c.incr(key, n).await;
    }

    pub async fn keys(&self, pattern: &str) -> Vec<String> {
        let mut c = self.conn.clone();
        use redis::AsyncCommands;
        c.keys(pattern).await.unwrap_or_default()
    }
}

pub async fn redis_init(cfg: &crate::config::Cfg) -> Option<RedisHandle> {
    if std::env::var("YZ_NO_REDIS").map(|v| v == "1").unwrap_or(false) {
        util::make_log1(Level::Warn, Some("Redis"), "YZ_NO_REDIS=1，跳过 Redis 初始化".into());
        return None;
    }
    let rc = cfg.get("redis");
    let host = rc.get("host").and_then(Value::as_str).unwrap_or("127.0.0.1").to_string();
    let port = rc.get("port").and_then(Value::as_u64).unwrap_or(6379) as u16;
    let username = rc.get("username").and_then(Value::as_str).map(String::from);
    let password = rc.get("password").and_then(Value::as_str).map(String::from);
    let db = rc.get("db").and_then(Value::as_i64).unwrap_or(0) as i64;

    util::make_log1(
        Level::Info,
        Some("Redis"),
        logger::cyan(format!("redis://{}:{}/{}", host, port, db)),
    );

    let addr = redis::ConnectionAddr::Tcp(host.clone(), port);
    let info = ConnectionInfo {
        addr,
        redis: redis::RedisConnectionInfo {
            db,
            username,
            password,
            protocol: redis::ProtocolVersion::RESP2,
        },
    };

    match connect(&info).await {
        Some(conn) => Some(RedisHandle { conn, process: None }),
        None => {
            if host != "127.0.0.1" && host != "localhost" {
                util::make_log1(Level::Error, Some("Redis"), "连接错误，请确认连接地址正确".into());
                return None;
            }
            // ≈ Redis.start — 自动启动本地 redis-server
            let path = rc.get("path").and_then(Value::as_str).unwrap_or("redis-server").to_string();
            let mut args = vec!["--port".to_string(), port.to_string()];
            args.extend(aarch64_flags(&path).await);
            util::make_log1(
                Level::Info,
                Some("Redis"),
                format!("正在启动 {}", logger::cyan(args.join(" "))),
            );
            let child = tokio::process::Command::new(&path)
                .args(&args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn();
            let mut child = match child {
                Ok(c) => c,
                Err(err) => {
                    util::make_log1(
                        Level::Error,
                        Some("Redis"),
                        format!("启动错误 {}（未安装 redis-server？可用 YZ_NO_REDIS=1 跳过）", err),
                    );
                    return None;
                }
            };
            for _ in 0..15 {
                util::sleep(1000).await;
                if let Ok(Some(_)) = child.try_wait() {
                    break;
                }
                if let Some(conn) = connect(&info).await {
                    util::make_log1(Level::Mark, Some("Redis"), "redis-server 已启动并连接".into());
                    return Some(RedisHandle { conn, process: Some(child) });
                }
            }
            let _ = child.kill().await;
            util::make_log1(Level::Error, Some("Redis"), "连接错误".into());
            None
        }
    }
}

async fn connect(info: &ConnectionInfo) -> Option<MultiplexedConnection> {
    let client = match redis::Client::open(info.clone()) {
        Ok(c) => c,
        Err(_) => return None,
    };
    match client.get_multiplexed_tokio_connection().await {
        Ok(conn) => {
            // ≈ ping 校验
            let mut c = conn.clone();
            match redis::cmd("PING").query_async::<String>(&mut c).await {
                Ok(_) => Some(conn),
                Err(_) => None,
            }
        }
        Err(_) => None,
    }
}

/// ≈ Redis.aarch64 — ARM64 COW bug 规避参数
async fn aarch64_flags(path: &str) -> Vec<String> {
    if cfg!(target_arch = "aarch64") {
        let (_, stdout, _) = util::exec(&format!("\"{}\" -v", path)).await;
        if let Some(v) = regex::Regex::new(r"v=(\d)\.")
            .ok()
            .and_then(|re| re.captures(&stdout))
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<u32>().ok())
        {
            if v >= 6 {
                return vec!["--ignore-warnings".into(), "ARM64-COW-BUG".into()];
            }
        }
    }
    vec![]
}
