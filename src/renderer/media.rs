//! 媒体加载：png/jpg/webp/svg（+ http(s) 远程），统一解码为 tiny-skia Pixmap（RGBA premul）

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tiny_skia::Pixmap;

static CACHE: Lazy<Mutex<HashMap<String, Option<Pixmap>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// 解析资源路径：http(s) 原样；file:// 剥前缀；绝对路径原样；相对 → 相对 base_dir 规范化
pub fn resolve(src: &str, base_dir: &str) -> String {
    let s = src.trim().trim_matches(['"', '\'']);
    if s.starts_with("http://") || s.starts_with("https://") || s.starts_with("data:") {
        return s.to_string();
    }
    let s = s.strip_prefix("file://").unwrap_or(s);
    let p = Path::new(s);
    if p.is_absolute() {
        return s.to_string();
    }
    let mut buf = PathBuf::from(base_dir);
    for part in s.split('/') {
        match part {
            "." => {}
            ".." => {
                buf.pop();
            }
            "" => {}
            other => buf.push(other),
        }
    }
    buf.to_string_lossy().to_string()
}

/// 加载并解码图片（带缓存）。失败返回 None。
pub fn load(src: &str, base_dir: &str) -> Option<Pixmap> {
    let key = resolve(src, base_dir);
    if let Some(hit) = CACHE.lock().unwrap().get(&key) {
        return hit.clone();
    }
    let decoded = decode_any(&key);
    CACHE.lock().unwrap().insert(key, decoded.clone());
    decoded
}

fn decode_any(key: &str) -> Option<Pixmap> {
    let bytes: Vec<u8> = if key.starts_with("http://") || key.starts_with("https://") {
        reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(8))
            .build()
            .ok()?
            .get(key)
            .send()
            .ok()?
            .bytes()
            .ok()?
            .to_vec()
    } else if let Some(data) = key.strip_prefix("data:") {
        let (_, b64) = data.split_once(',')?;
        crate::util::base64_to_bytes(b64)?
    } else {
        std::fs::read(key).ok()?
    };

    // svg → resvg 光栅化
    if key.ends_with(".svg") || bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml") {
        return decode_svg(&bytes);
    }
    // png/jpg/webp → image crate
    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img.to_rgba8();
    let (w, h) = rgba.dimensions();
    let mut pm = Pixmap::new(w, h)?;
    for (i, px) in rgba.pixels().enumerate() {
        if let Some(c) = tiny_skia::PremultipliedColorU8::from_rgba(px[0], px[1], px[2], px[3]) {
            pm.pixels_mut()[i] = c;
        }
    }
    Some(pm)
}

fn decode_svg(bytes: &[u8]) -> Option<Pixmap> {
    let opt = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(bytes, &opt).ok()?;
    let size = tree.size();
    let w = (size.width() as u32).clamp(1, 4096);
    let h = (size.height() as u32).clamp(1, 4096);
    let mut pm = Pixmap::new(w, h)?;
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut pm.as_mut());
    Some(pm)
}

/// 解析 background 简写 / background-image：提取 url(...)
pub fn extract_url(v: &str) -> Option<String> {
    let idx = v.find("url(")?;
    let rest = &v[idx + 4..];
    let end = rest.find(')')?;
    Some(rest[..end].trim().trim_matches(['"', '\'']).to_string())
}

/// 解析 background-position：像素偏移（精灵图为负值）；top/left/bottom/right/center 关键字按 0 处理
pub fn parse_position(v: &str) -> (f32, f32) {
    let parts: Vec<&str> = v.split_whitespace().collect();
    let px = |s: &str| -> f32 {
        s.trim()
            .trim_end_matches("px")
            .parse::<f32>()
            .unwrap_or(0.0)
    };
    let mut x = 0f32;
    let mut y = 0f32;
    for (i, p) in parts.iter().take(4).enumerate() {
        match *p {
            "left" | "top" => {}
            "right" | "bottom" | "center" => {}
            _ => {
                if i == 0 || x == 0.0 {
                    if x == 0.0 {
                        x = px(p);
                    }
                }
                if y == 0.0 && (i == 1 || parts.len() == 1 && i == 0) {
                    y = px(p);
                }
            }
        }
    }
    // 典型精灵："-{x}px -{y}px"：第一个为 x，第二个为 y
    if parts.len() >= 2 {
        x = px(parts[0]);
        y = px(parts[1]);
    } else if parts.len() == 1 {
        x = px(parts[0]);
    }
    (x, y)
}

/// 解析 background-size：返回 (目标宽, 目标高)，None 表示 auto
pub fn parse_size(v: &str, node_w: f32, node_h: f32, img_w: u32, img_h: u32) -> (f32, f32) {
    // contain：完整放入节点（保持比例，可留白）；cover：铺满节点（保持比例，裁剪溢出）
    let head = v.split_whitespace().next().unwrap_or("");
    let iw = img_w as f32;
    let ih = img_h as f32;
    if head == "contain" {
        let scale = (node_w / iw).min(node_h / ih);
        if scale.is_finite() && scale > 0.0 {
            return (iw * scale, ih * scale);
        }
        return (iw, ih);
    }
    if head == "cover" {
        let scale = (node_w / iw).max(node_h / ih);
        if scale.is_finite() && scale > 0.0 {
            return (iw * scale, ih * scale);
        }
        return (iw, ih);
    }
    let parts: Vec<&str> = v.split_whitespace().collect();
    let dim = |s: &str, base: f32, natural: f32| -> Option<f32> {
        let s = s.trim();
        if s == "auto" || s.is_empty() {
            return None;
        }
        if let Some(p) = s.strip_suffix('%') {
            return p.trim().parse::<f32>().ok().map(|n| base * n / 100.0);
        }
        s.trim_end_matches("px").parse::<f32>().ok()
    };
    let w = parts
        .first()
        .and_then(|s| dim(s, node_w, img_w as f32));
    let h = parts
        .get(1)
        .and_then(|s| dim(s, node_h, img_h as f32));
    match (w, h) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => {
            let scale = w / img_w as f32;
            (w, img_h as f32 * scale)
        }
        (None, Some(h)) => {
            let scale = h / img_h as f32;
            (img_w as f32 * scale, h)
        }
        _ => (img_w as f32, img_h as f32),
    }
}
