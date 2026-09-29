//! ≈ lib/util.js — 通用工具
use crate::logger::{self, Level};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use std::time::Duration;

/// ≈ util.String — 字符串化：字符串原样，其余紧凑 JSON
pub fn string(data: &Value) -> String {
    match data {
        Value::String(s) => s.clone(),
        Value::Null => "[object null]".to_string(),
        _ => serde_json::to_string(data).unwrap_or_else(|_| "[object null]".to_string()),
    }
}

/// ≈ util.Loging — 超长截断
pub fn loging(data: String) -> String {
    let len = logger::LOG_LENGTH.load(Ordering::Relaxed);
    if data.chars().count() > len {
        let cut: String = data.chars().take(len).collect();
        format!("{}{}", cut, logger::gray(format!("... {} more characters", data.chars().count() - len)))
    } else {
        data
    }
}

/// ≈ util.makeLogID — id 对齐
pub fn make_log_id(id: &str) -> String {
    let align = logger::LOG_ALIGN.read().unwrap().clone();
    if align.is_empty() {
        return id.to_string();
    }
    let align_len = align.chars().count();
    let id_len = id.chars().count();
    if align_len >= id_len {
        let length = (align_len - id_len) as f64 / 2.0;
        format!(
            "{}{}{}",
            " ".repeat((length.ceil()) as usize),
            id,
            " ".repeat((length.floor()) as usize)
        )
    } else {
        let cut: String = id.chars().take(align_len - 1).collect();
        format!("{}.", cut)
    }
}

/// ≈ util.makeLog(level, msg, id, force)
pub fn make_log(level: Level, id: Option<&str>, force: bool, parts: Vec<String>) {
    let id_str = match id {
        None => None,
        Some(i) if force => Some(i.to_string()),
        Some(i) => Some(make_log_id(i)),
    };
    let parts: Vec<String> = parts.into_iter().map(loging).collect();
    logger::log(level, id_str.as_deref(), &parts);
}

pub fn make_log1(level: Level, id: Option<&str>, part: String) {
    make_log(level, id, false, vec![part]);
}

/// ≈ util.sleep — 毫秒
pub async fn sleep(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// ≈ util.getTimeDiff
pub fn get_time_diff(start_ms: u64, end_ms: u64) -> String {
    let mut time = end_ms.saturating_sub(start_ms);
    let ms = time % 1000;
    time /= 1000;
    let sec = time % 60;
    time /= 60;
    let min = time % 60;
    time /= 60;
    let hour = time % 24;
    let day = time / 24;
    let mut ret = String::new();
    if day > 0 {
        ret += &format!("{}天", day);
    }
    if hour > 0 {
        ret += &format!("{}时", hour);
    }
    if min > 0 {
        ret += &format!("{}分", min);
    }
    if sec > 0 {
        ret += &format!("{}秒", sec);
    }
    if ms > 0 {
        ret += &ms.to_string();
    }
    if ret.is_empty() {
        "0秒".to_string()
    } else {
        ret
    }
}

/// ≈ util.fsStat
pub async fn fs_stat(path: &str) -> Option<std::fs::Metadata> {
    tokio::fs::metadata(path).await.ok()
}

/// ≈ util.mkdir
pub async fn mkdir(dir: &str) -> bool {
    match tokio::fs::create_dir_all(dir).await {
        Ok(_) => true,
        Err(err) => {
            make_log(
                Level::Error,
                None,
                false,
                vec!["创建".into(), dir.into(), "错误".into(), format!("{}", err)],
            );
            false
        }
    }
}

/// ≈ util.rm
pub async fn rm(file: &str) -> bool {
    match tokio::fs::remove_dir_all(file).await.or(tokio::fs::remove_file(file).await) {
        Ok(_) => true,
        Err(err) => {
            make_log(
                Level::Error,
                None,
                false,
                vec!["删除".into(), file.into(), "错误".into(), format!("{}", err)],
            );
            false
        }
    }
}

/// ≈ util.exec — shell 执行并记录日志
pub async fn exec(cmd: &str) -> (Option<i32>, String, String) {
    let name = logger::cyan(cmd);
    make_log1(Level::Mark, Some("Command"), name.clone());
    let start_time = now_ms();
    let out = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .output()
        .await;
    let (code, stdout, stderr) = match out {
        Ok(o) => (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).trim().to_string(),
            String::from_utf8_lossy(&o.stderr).trim().to_string(),
        ),
        Err(err) => (Some(-1), String::new(), format!("{}", err)),
    };
    make_log1(
        Level::Mark,
        Some("Command"),
        format!(
            "{} {} {}{}",
            name,
            logger::green(format!("[完成{}]", get_time_diff(start_time, now_ms()))),
            if stdout.is_empty() { String::new() } else { format!("\n{}", stdout) },
            if stderr.is_empty() { String::new() } else { logger::red(format!("\n{}", stderr)) }
        ),
    );
    (code, stdout, stderr)
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// ≈ util.Buffer — base64:// / file:// / http(s):// → Buffer 或 file:// 引用
/// opts: { http: bool, file: bool, size: usize }
pub async fn buffer(data: Value, opts: &Value) -> Value {
    let s = match &data {
        Value::String(_) => string(&data),
        _ => string(&data),
    };
    if s.starts_with("base64://") {
        return json!(s);
    }
    if s.starts_with("http://") || s.starts_with("https://") {
        if opts.get("http").and_then(Value::as_bool).unwrap_or(false) {
            return json!(s);
        }
        // 阶段2接入 reqwest 后实现下载
        return json!(s);
    }
    let file = s.trim_start_matches("file://");
    if fs_stat(file).await.is_some() {
        if opts.get("file").and_then(Value::as_bool).unwrap_or(false) {
            let abs = std::fs::canonicalize(file)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| file.to_string());
            return json!(format!("file://{}", abs));
        }
        match tokio::fs::read(file).await {
            Ok(bytes) => {
                if let Some(size) = opts.get("size").and_then(Value::as_u64) {
                    if bytes.len() as u64 > size {
                        return json!(format!("file://{}", file));
                    }
                }
                return Value::String(bytes_to_base64(&bytes));
            }
            Err(_) => return json!(s),
        }
    }
    json!(s)
}

pub fn bytes_to_base64(bytes: &[u8]) -> String {
    use base64::Engine;
    format!("base64://{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// ≈ Number(value) || value — 数字化
pub fn to_number(v: &Value) -> Value {
    match v {
        Value::String(s) => match s.parse::<f64>() {
            Ok(n) if s.trim() != "" => json!(n),
            _ => v.clone(),
        },
        _ => v.clone(),
    }
}

/// 数值/字符串宽松相等（≈ JS ==）
pub fn loose_eq(a: &Value, b: &Value) -> bool {
    let an = to_number(a);
    let bn = to_number(b);
    if an.is_number() && bn.is_number() {
        return an.as_f64() == bn.as_f64();
    }
    string(&an) == string(&bn)
}

/// ≈ Bot.fs[name] — 文件外链缓冲项
pub struct FileEntry {
    pub buffer: std::sync::Arc<Vec<u8>>,
    pub content_type: String,
    /// 剩余可下载次数（None=不限）
    pub times: std::sync::Mutex<Option<u32>>,
}
