//! libnode 运行时管理器：平台映射、多源 2MB 实测选路下载、xz 解压、缓存清单
//!
//! 产物契约（.github/workflows/bin-pack.yml）：
//!   bin/libnode-<os>-<arch>.<so|dylib|dll>.xz   （xz -9 单文件）
//! 源格式：
//!   https://<jsdelivr-host>/gh/xiaocongyu66/yunzai-rust@main/bin/<file>
//!   https://raw.githubusercontent.com/xiaocongyu66/yunzai-rust/main/bin/<file>

use once_cell::sync::Lazy;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const GH_USER: &str = "xiaocongyu66";
const GH_REPO: &str = "yunzai-rust";
const GH_BRANCH: &str = "main";
const PROBE_BYTES: u64 = 2 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(600);

/// jsDelivr 系镜像（20MB 单文件上限：超限文件该源必然 4xx 自动淘汰）+ GitHub raw 兜底
const DL_HOSTS: &[&str] = &[
    "https://cdn.jsdmirror.com",
    "https://gcore.jsdelivr.com",
    "https://cdn.jsdelivr.net",
    "https://cloudflare.jsdelivr.net",
    "https://raw.githubusercontent.com",
];

static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .user_agent("yunzai-rust")
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .expect("reqwest client")
});

/// Rust target → CI 产物命名（os=macos 非 darwin；linux 32 位为 i386）
pub fn platform_name() -> (&'static str, &'static str, &'static str) {
    // (os, arch, 扩展名)
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => ("linux", "x86_64", "so"),
        ("linux", "aarch64") => ("linux", "arm64", "so"),
        ("linux", "x86") => ("linux", "i386", "so"),
        ("macos", "aarch64") => ("macos", "arm64", "dylib"),
        ("macos", "x86_64") => ("macos", "x86_64", "dylib"),
        ("windows", "x86_64") => ("windows", "x86_64", "dll"),
        ("windows", "aarch64") => ("windows", "arm64", "dll"),
        (os, arch) => (os, arch, "so"),
    }
}

/// 产物文件名（不含 .xz），如 libnode-linux-arm64.so
pub fn libnode_file_name() -> String {
    let (os, arch, ext) = platform_name();
    format!("libnode-{os}-{arch}.{ext}")
}

/// 运行时缓存目录 data/libnode/
pub fn cache_dir() -> PathBuf {
    PathBuf::from("data").join("libnode")
}

/// 本地 libnode 动态库完整路径
pub fn libnode_local_path() -> PathBuf {
    cache_dir().join(libnode_file_name())
}

/// 解析 libnode 路径：LIBNODE_PATH → node.yaml node_path → 本地缓存 → 自动下载
/// 返回 (动态库路径, 是否为本次新下载)
pub async fn ensure_libnode(cfg: Option<&crate::config::Cfg>) -> anyhow::Result<(PathBuf, bool)> {
    // 1. 环境变量直接指定（跳过一切管理）
    if let Ok(p) = std::env::var("LIBNODE_PATH") {
        let path = PathBuf::from(&p);
        if path.is_file() {
            crate::util::make_log1(crate::logger::Level::Info, Some("Libnode"), format!("使用 LIBNODE_PATH: {}", path.display()));
            return Ok((path, false));
        }
        crate::util::make_log1(crate::logger::Level::Warn, Some("Libnode"), format!("LIBNODE_PATH 不存在: {p}，回落自动管理"));
    }
    // LIBNODE_SKIP=1：禁用自动下载（无网环境不告警重试）
    let autodownload = std::env::var("LIBNODE_SKIP").map(|v| v != "1").unwrap_or(true);

    // 2. node.yaml node_path
    if let Some(cfg) = cfg {
        let p = match cfg.get("node").get("node_path") {
            Some(v) if !v.is_null() => crate::util::string(v),
            _ => String::new(),
        };
        if !p.is_empty() {
            let path = PathBuf::from(&p);
            if path.is_file() {
                crate::util::make_log1(crate::logger::Level::Info, Some("Libnode"), format!("使用配置路径: {}", path.display()));
                return Ok((path, false));
            }
            crate::util::make_log1(crate::logger::Level::Warn, Some("Libnode"), format!("node_path 不存在: {p}"));
        }
        let ad = cfg.get("node").get("autodownload").map(|v| v.as_bool().unwrap_or(true)).unwrap_or(true);
        if !ad {
            return Err(anyhow::anyhow!("[libnode] 本地库缺失且 autodownload=false"));
        }
    }
    if !autodownload {
        return Err(anyhow::anyhow!("[libnode] 本地库缺失且 LIBNODE_SKIP=1"));
    }

    // 3. 本地缓存
    let local = libnode_local_path();
    if local.is_file() && std::fs::metadata(&local).map(|m| m.len() > 0).unwrap_or(false) {
        return Ok((local, false));
    }

    // 4. 自动下载
    crate::util::make_log1(crate::logger::Level::Info, Some("Libnode"), "本地运行时缺失，开始自动下载…".to_string());
    let path = download_and_extract().await?;
    Ok((path, true))
}

fn url_for(host: &str, file: &str) -> String {
    if host.contains("raw.githubusercontent.com") {
        format!("{host}/{GH_USER}/{GH_REPO}/{GH_BRANCH}/bin/{file}")
    } else {
        format!("{host}/gh/{GH_USER}/{GH_REPO}@{GH_BRANCH}/bin/{file}")
    }
}

/// 并行探测各源：Range 下载前 2MB 计时，成功的按耗时升序返回
async fn probe_sources(file: &str) -> Vec<(String, Duration)> {
    let mut futs = Vec::new();
    for host in DL_HOSTS {
        let url = url_for(host, file);
        futs.push(tokio::spawn(async move {
            let start = Instant::now();
            let client = reqwest::Client::builder()
                .user_agent("yunzai-rust")
                .timeout(PROBE_TIMEOUT)
                .build();
            let ok = match client {
                Ok(c) => c
                    .get(&url)
                    .header("Range", &format!("bytes=0-{}", PROBE_BYTES - 1))
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false),
                Err(_) => false,
            };
            if ok {
                Some((url, start.elapsed()))
            } else {
                None
            }
        }));
    }
    let mut results: Vec<(String, Duration)> = Vec::new();
    for f in futs {
        if let Ok(Some(pair)) = f.await {
            results.push(pair);
        }
    }
    results.sort_by_key(|(_, d)| *d);
    results
}

/// 全量下载（带 .part 临时文件）
async fn download_to(url: &str, dest: &Path) -> anyhow::Result<()> {
    let part = dest.with_extension("part");
    let mut resp = CLIENT.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("[libnode] 下载失败 {} → {}", url, resp.status());
    }
    use std::io::Write;
    let mut file = std::fs::File::create(&part)?;
    while let Some(chunk) = resp.chunk().await? {
        file.write_all(&chunk)?;
    }
    file.flush()?;
    drop(file);
    std::fs::rename(&part, dest)?;
    Ok(())
}

/// xz 解压（单文件流）
fn xz_decompress(src: &Path, dest: &Path) -> anyhow::Result<()> {
    let input = std::fs::read(src)?;
    let mut out = Vec::with_capacity(input.len() * 4);
    lzma_rs::xz_decompress(&mut std::io::BufReader::new(&input[..]), &mut out)?;
    std::fs::write(dest, out)?;
    Ok(())
}

async fn download_and_extract() -> anyhow::Result<PathBuf> {
    let file = format!("{}.xz", libnode_file_name());
    crate::util::mkdir(cache_dir().to_string_lossy().as_ref()).await;

    // 实测选路
    let sources = probe_sources(&file).await;
    if sources.is_empty() {
        return Err(anyhow::anyhow!(
            "[libnode] 所有下载源不可达（jsdelivr×4 + github raw）。可设 LIBNODE_PATH 指向本地 libnode"
        ));
    }
    for (i, (url, d)) in sources.iter().enumerate() {
        crate::util::make_log1(crate::logger::Level::Info, Some("Libnode"), format!("源{}: {} (探测 {}ms)", i + 1, url, d.as_millis()));
    }

    let xz_path = cache_dir().join(&file);
    let lib_path = libnode_local_path();
    // 按测速排序逐源尝试
    for (url, _) in &sources {
        match download_to(url, &xz_path).await {
            Ok(()) => {
                crate::util::make_log1(crate::logger::Level::Info, Some("Libnode"), format!("下载完成: {}", url));
                match xz_decompress(&xz_path, &lib_path) {
                    Ok(()) => {
                        let _ = std::fs::remove_file(&xz_path);
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let _ = std::fs::set_permissions(&lib_path, std::fs::Permissions::from_mode(0o755));
                        }
                        write_manifest(url).await;
                        return Ok(lib_path);
                    }
                    Err(e) => {
                        // xz 损坏（截断等）→ 删除换下一源
                        crate::util::make_log1(crate::logger::Level::Warn, Some("Libnode"), format!("xz 解压失败（文件损坏？）: {e}，换源重试"));
                        let _ = std::fs::remove_file(&xz_path);
                    }
                }
            }
            Err(e) => {
                crate::util::make_log1(crate::logger::Level::Warn, Some("Libnode"), format!("源下载失败: {e}，换下一源"));
            }
        }
    }
    Err(anyhow::anyhow!("[libnode] 全部源下载/解压失败"))
}

async fn write_manifest(source: &str) {
    let path = libnode_local_path();
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let manifest = serde_json::json!({
        "file": libnode_file_name(),
        "source": source,
        "size": size,
        "ts": chrono::Local::now().to_rfc3339(),
    });
    let mp = cache_dir().join("manifest.json");
    if let Ok(s) = serde_json::to_string_pretty(&manifest) {
        let _ = std::fs::write(mp, s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_platform_name() {
        let (os, arch, ext) = platform_name();
        assert!(matches!(os, "linux" | "macos" | "windows"));
        assert!(matches!(arch, "x86_64" | "arm64" | "i386"));
        assert!(matches!(ext, "so" | "dylib" | "dll"));
        let f = libnode_file_name();
        assert!(f.starts_with("libnode-"));
        assert!(f.contains(os));
    }
}
