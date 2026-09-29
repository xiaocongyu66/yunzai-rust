//! 最小 5 字段 cron 匹配器 + 定时任务调度循环
//!
//! 字段：分 时 日 月 周（支持 `*/n`、`n-m`、列表、`*`）
//! ≈ node-schedule 的 `scheduleJob({rule: cron}, fnc)` 语义（loader.task 消费点）

use chrono::{Datelike, Timelike};
use once_cell::sync::Lazy;
use std::sync::Mutex;

fn parse_field(field: &str, min: u32, max: u32) -> Option<Vec<u32>> {
    let mut set = std::collections::BTreeSet::new();
    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().ok().filter(|n| *n > 0).unwrap_or(1)),
            None => (part, 1),
        };
        let (lo, hi) = if range == "*" {
            (min, max)
        } else if let Some((a, b)) = range.split_once('-') {
            (a.parse().ok()?, b.parse().ok()?)
        } else {
            let n = range.parse().ok()?;
            (n, n)
        };
        if !(min..=max).contains(&lo) || !(min..=max).contains(&hi) || hi < lo {
            return None;
        }
        let mut v = lo;
        while v <= hi {
            set.insert(v);
            v += step;
        }
    }
    Some(set.into_iter().collect())
}

/// cron 表达式（分 时 日 月 周）是否匹配给定时刻（本地时间）
pub fn cron_match(expr: &str, t: chrono::DateTime<chrono::Local>) -> bool {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return false;
    }
    let (Some(m), Some(h), Some(dom), Some(mon), Some(dow)) = (
        parse_field(parts[0], 0, 59),
        parse_field(parts[1], 0, 23),
        parse_field(parts[2], 1, 31),
        parse_field(parts[3], 1, 12),
        parse_field(parts[4], 0, 7),
    ) else {
        return false;
    };
    let dow: Vec<u32> = dow.iter().map(|d| if *d == 7 { 0 } else { *d }).collect();
    m.contains(&t.minute())
        && h.contains(&t.hour())
        && mon.contains(&t.month())
        && dow.contains(&(t.weekday().num_days_from_sunday()))
        // 日/周都非 * 时按 or 语义（cron 标准）；任一为 * 则按 and
        && match (parts[2] == "*", parts[4] == "*") {
            (true, true) => true,
            (true, false) => dow.contains(&t.weekday().num_days_from_sunday()),
            (false, true) => dom.contains(&t.day()),
            (false, false) => dom.contains(&t.day()) || dow.contains(&t.weekday().num_days_from_sunday()),
        }
}

/// 上次触发标记（分钟精度，进程内）
static LAST_FIRE: Lazy<Mutex<std::collections::HashSet<String>>> =
    Lazy::new(|| Mutex::new(std::collections::HashSet::new()));

fn minute_key(expr: &str, t: &chrono::DateTime<chrono::Local>) -> String {
    format!("{expr}@{}", t.format("%Y%m%d%H%M"))
}

/// 每分钟驱动一次：返回本分钟应触发的 cron 表达式集合（去重）
pub fn tick(exprs: &[String]) -> Vec<String> {
    let now = chrono::Local::now();
    let mut fired = LAST_FIRE.lock().unwrap();
    // 清理上一分钟的标记（防集合无限增长）
    fired.retain(|k| {
        let Some((_, ts)) = k.rsplit_once('@') else { return false };
        ts != now.format("%Y%m%d%H%M").to_string()
    });
    let mut out = vec![];
    for expr in exprs {
        if !cron_match(expr, now) {
            continue;
        }
        let key = minute_key(expr, &now);
        if fired.contains(&key) {
            continue;
        }
        fired.insert(key);
        out.push(expr.clone());
    }
    out
}

/// 调度循环：每 30s 扫描任务表，cron 命中即调 JS 插件方法
pub async fn spawn_scheduler(loader: std::sync::Arc<crate::plugins::loader::PluginsLoader>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let jobs: Vec<crate::plugins::loader::TaskJob> = loader.task.read().unwrap().clone();
            if jobs.is_empty() {
                continue;
            }
            let exprs: Vec<String> = jobs.iter().map(|j| j.cron.clone()).collect();
            let due: std::collections::HashSet<String> = tick(&exprs).into_iter().collect();
            if due.is_empty() {
                continue;
            }
            let engine = loader.engine.read().unwrap().clone();
            let Some(engine) = engine else { continue };
            for job in jobs {
                if !due.contains(&job.cron) {
                    continue;
                }
                if job.log {
                    crate::util::make_log1(
                        crate::logger::Level::Mark,
                        Some("Task"),
                        format!("[{}][定时任务] {}", job.name, job.cron),
                    );
                }
                let engine = engine.clone();
                let job = job;
                tokio::spawn(async move {
                    engine.call(&job.plugin_key, &job.fnc, &serde_json::json!({})).await;
                });
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> chrono::DateTime<chrono::Local> {
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
            .unwrap()
            .and_local_timezone(chrono::Local).unwrap()
    }

    #[test]
    fn test_cron_match() {
        assert!(cron_match("* * * * *", t("2026-09-29 10:30:15")));
        assert!(cron_match("30 10 * * *", t("2026-09-29 10:30:00")));
        assert!(!cron_match("30 10 * * *", t("2026-09-29 10:31:00")));
        assert!(cron_match("*/15 * * * *", t("2026-09-29 10:45:00")));
        assert!(!cron_match("*/15 * * * *", t("2026-09-29 10:50:00")));
        assert!(cron_match("0 0 1 1 *", t("2026-01-01 00:00:00")));
        // 周一（2026-09-28 是周一）
        assert!(cron_match("0 0 * * 1", t("2026-09-28 00:00:30")));
        assert!(!cron_match("0 0 * * 1", t("2026-09-29 00:00:30")));
    }
}

// ============================ 插件热重载（notify watch plugins/） ============================

/// ≈ lib/plugins/loader.js 的 watch() — chokidar 等价：debounce 5s
/// change → 重新加载该文件；unlink → 卸载其注册项；add → 加载新文件
pub fn spawn_watcher(loader: std::sync::Arc<crate::plugins::loader::PluginsLoader>) {
    use notify::{RecursiveMode, Watcher};
    use std::path::Path;

    let plugins_dir = Path::new("plugins");
    if !plugins_dir.is_dir() {
        return;
    }

    let loader_c = loader.clone();
    // 去抖窗口：同文件 5s 内合并事件（编辑器原子保存/连续写）
    std::thread::Builder::new()
        .name("plugin-watch".into())
        .spawn(move || {
            let (tx, rx) = std::sync::mpsc::channel::<notify::Result<notify::Event>>();
            let mut watcher = match notify::RecommendedWatcher::new(tx, notify::Config::default()) {
                Ok(w) => w,
                Err(e) => {
                    crate::util::make_log1(crate::logger::Level::Warn, Some("Plugin"), format!("文件监听初始化失败 {e}"));
                    return;
                }
            };
            if watcher.watch(plugins_dir, RecursiveMode::Recursive).is_err() {
                return;
            }
            // 主循环：收到事件就进 debounce 攒批，5s 无新事件才 flush
            let mut pending: std::collections::HashMap<std::path::PathBuf, notify::EventKind> = std::collections::HashMap::new();
            let mut last = std::time::Instant::now();
            loop {
                match rx.recv_timeout(std::time::Duration::from_millis(500)) {
                    Ok(Ok(ev)) => {
                        for p in ev.paths {
                            // 只关心 .js
                            if p.extension().and_then(|e| e.to_str()) != Some("js") {
                                continue;
                            }
                            pending.insert(p, ev.kind);
                        }
                        last = std::time::Instant::now();
                    }
                    _ => {
                        if !pending.is_empty() && last.elapsed() >= std::time::Duration::from_secs(5) {
                            let batch: Vec<(std::path::PathBuf, notify::EventKind)> = pending.drain().collect();
                            for (path, kind) in batch {
                                handle_watch_event(&loader_c, &path, kind);
                            }
                        }
                    }
                }
            }
        })
        .ok();
}

fn handle_watch_event(
    loader: &std::sync::Arc<crate::plugins::loader::PluginsLoader>,
    path: &std::path::Path,
    kind: notify::EventKind,
) {
    use notify::EventKind;
    let rel = path
        .strip_prefix("plugins")
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    if rel.is_empty() {
        return;
    }
    let is_rm = matches!(kind, EventKind::Remove(_) | EventKind::Modify(notify::event::ModifyKind::Name(notify::event::RenameMode::From)));
    let is_add = matches!(kind, EventKind::Create(_) | EventKind::Modify(notify::event::ModifyKind::Name(notify::event::RenameMode::To)));

    crate::util::make_log1(
        crate::logger::Level::Mark,
        Some("Plugin"),
        format!("[{}{}][{}]", if is_rm { "卸载插件" } else { "热更新" }, if is_add { "新增" } else { "修改" }, rel),
    );

    // 卸载旧 entry（按 rel 前缀）
    loader.unload_key(&rel);

    if !is_rm && path.is_file() {
        // 重新 import（新线程里 block_on——watch 线程非 tokio）
        let loader = loader.clone();
        let abs = std::fs::canonicalize(path).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_string_lossy().into_owned());
        let rel2 = rel.clone();
        std::thread::spawn(move || {
            if let Some(handle) = crate::nodejs::host::MAIN_HANDLE.get() {
                handle.spawn(async move {
                    loader.reload_plugin(&rel2, &abs).await;
                });
            }
        });
    }
}
