//! ≈ lib/config/log.js — 日志系统：default/command/error 三通道 + 文件日志
use chrono::{FixedOffset, Utc};
use once_cell::sync::Lazy;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::RwLock;

pub static TZ: Lazy<FixedOffset> = Lazy::new(|| FixedOffset::east_opt(8 * 3600).unwrap());

pub static COLOR: AtomicBool = AtomicBool::new(true);
pub static LOG_LEVEL: AtomicUsize = AtomicUsize::new(2); // info
pub static LOG_ALIGN: Lazy<RwLock<String>> = Lazy::new(|| RwLock::new("  TRSSYz  ".to_string()));
pub static LOG_LENGTH: AtomicUsize = AtomicUsize::new(10000);
pub static LOG_OBJECT: AtomicBool = AtomicBool::new(true);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Mark = 4,
    Error = 5,
    Fatal = 6,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Mark => "mark",
            Level::Error => "error",
            Level::Fatal => "fatal",
        }
    }
    /// log4js %4.4p：default 通道 trace/debug/info，command 通道 warn/mark，error 通道 error/fatal
    fn channel(&self) -> &'static str {
        match self {
            Level::Trace | Level::Debug | Level::Info => "default",
            Level::Warn | Level::Mark => "command",
            Level::Error | Level::Fatal => "error",
        }
    }
    pub fn parse(s: &str) -> Level {
        match s {
            "trace" => Level::Trace,
            "debug" => Level::Debug,
            "warn" => Level::Warn,
            "mark" => Level::Mark,
            "error" => Level::Error,
            "fatal" => Level::Fatal,
            _ => Level::Info,
        }
    }
}

pub fn color(code: &str, s: impl AsRef<str>) -> String {
    if COLOR.load(Ordering::Relaxed) {
        format!("\x1b[{}m{}\x1b[0m", code, s.as_ref())
    } else {
        s.as_ref().to_string()
    }
}
pub fn blue(s: impl AsRef<str>) -> String {
    color("34", s)
}
pub fn green(s: impl AsRef<str>) -> String {
    color("32", s)
}
pub fn cyan(s: impl AsRef<str>) -> String {
    color("36", s)
}
pub fn yellow(s: impl AsRef<str>) -> String {
    color("33", s)
}
pub fn red(s: impl AsRef<str>) -> String {
    color("31", s)
}
pub fn magenta(s: impl AsRef<str>) -> String {
    color("35", s)
}
pub fn gray(s: impl AsRef<str>) -> String {
    color("90", s)
}

fn now_str() -> String {
    Utc::now().with_timezone(&*TZ).format("%H:%M:%S%.3f").to_string()
}

fn today() -> String {
    Utc::now().with_timezone(&*TZ).format("%Y-%m-%d").to_string()
}

fn append_file(path: &str, line: &str) {
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{}", line);
    }
}

fn enabled(level: Level) -> bool {
    match level.channel() {
        "default" => level as usize >= LOG_LEVEL.load(Ordering::Relaxed),
        _ => true,
    }
}

pub fn set_level(level: &str) {
    LOG_LEVEL.store(Level::parse(level) as usize, Ordering::Relaxed);
}

/// ≈ logger.logger[level](...) — 输出一行日志并写入文件
pub fn log(level: Level, id: Option<&str>, parts: &[String]) {
    if !enabled(level) {
        return;
    }
    let lvl = match level {
        Level::Trace => gray("TRACE"),
        Level::Debug => cyan("DEBUG"),
        Level::Info => green("INFO "),
        Level::Warn => yellow("WARN "),
        Level::Mark => magenta("MARK "),
        Level::Error => red("ERROR"),
        Level::Fatal => color("1;35", "FATAL"),
    };
    let id_str = blue(format!("[{}]", id.unwrap_or("")));
    let body = parts.join(" ");
    let line = format!("[{}][{}]{} {}", now_str(), lvl, id_str, body);
    println!("{}", line);
    // command 通道文件日志（warn/mark/error/fatal）
    if matches!(level, Level::Warn | Level::Mark | Level::Error | Level::Fatal) {
        let plain = format!(
            "[{}][{}]{} {}",
            now_str(),
            level.as_str().to_uppercase(),
            format!("[{}]", id.unwrap_or("")),
            body
        );
        append_file(&format!("logs/command.{}.log", today()), &plain);
        if matches!(level, Level::Error | Level::Fatal) {
            append_file("logs/error.log", &plain);
        }
    }
}
