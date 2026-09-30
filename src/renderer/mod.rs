//! 自研 HTML/CSS 渲染器（洁净室）：html5ever 解析 → 样式匹配 → taffy 布局 → tiny-skia 光栅化
//!
//! 阶段A支持：div/span/p/b 等通用盒、display:flex（row/column/gap/justify/align/wrap）、
//! background-color/linear-gradient、border(-radius)、padding/margin、width/height(px/%/auto)、
//! color/font-size/font-weight/text-align/line-height、img(本地路径/base64)。
//! 入口：[`render`] — HTML 字符串 → PNG 字节。

pub mod css;
pub mod dom;
pub mod layout;
pub mod media;
pub mod paint;
pub mod style;
pub mod text;

use serde_json::Value;

/// 渲染 HTML → PNG。宽度默认 720，高度按内容自适应（上限 4096）。
/// HTML 相对资源的基准目录（img/background url 解析）
pub static BASE_DIR: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn render(html: &str, width: u32, font_dirs: &[String], base_dir: &str) -> Result<Vec<u8>, String> {
    let _ = BASE_DIR.set(base_dir.to_string());
    // 支持到 4K（3840）：宽度上限 4096；高度按内容自适应，保护上限 = 宽×4（防内存爆）
    let width = width.clamp(64, 4096) as f32;

    // 1. 解析 DOM + 收集样式（<style> 块 + inline style + 选择器匹配）
    let (root_node, css_rules, font_faces, pseudo_rules) = dom::parse_with_base(html, base_dir)?;
    let mut styled = css::apply_styles(root_node, &css_rules);
    css::apply_pseudo(&mut styled, &pseudo_rules);

    // 诊断：样式收集统计（link 读取 / faces 解析）
    crate::util::make_log1(
        crate::logger::Level::Info,
        Some("Renderer"),
        format!(
            "样式收集：rules={} faces={}",
            css_rules.len(),
            font_faces.len()
        ),
    );

    // 2. 字体系统（先加载插件字体，再注册 @font-face 别名）
    let mut fonts = text::TextEngine::load(font_dirs)?;
    for (fam, url) in &font_faces {
        let path = crate::renderer::media::resolve(url, base_dir);
        fonts.load_face_file(&path, fam);
    }
    crate::util::make_log1(
        crate::logger::Level::Info,
        Some("Renderer"),
        format!(
            "font-face 加载完成：faces={:?} 别名={:?}",
            font_faces.iter().map(|(f, u)| format!("{f}←{u}")).collect::<Vec<_>>(),
            fonts.font_aliases
        ),
    );
    // font-family 别名替换（@font-face 引用名 → 字体内部真实名）
    let styled = css::apply_font_aliases(styled, &fonts);

    // 3. 布局（按宽度约束算内容高度）
    let tree = layout::build_tree(&styled, width, &mut fonts, base_dir)?;
    let (root_paint, total_h) = layout::compute(tree, width, &mut fonts)?;
    // ≈ TRSS 按根元素实际尺寸截图：内容宽（body 显式 width）而非 viewport 宽
    let out_w = (root_paint.w.ceil() as u32).clamp(64, 4096);
    let height = (total_h.ceil() as u32).clamp(1, out_w * 4);

    // 4. 光栅化
    paint::paint(&root_paint, out_w as f32, height as f32, &mut fonts)
}

/// op 层入口：JSON 参数 { html, width?, fontDirs? } → { data: base64, width, height }
pub fn render_op(args: &Value) -> Value {
    let html = args.get("html").and_then(Value::as_str).unwrap_or("");
    let base_dir = args
        .get("baseDir")
        .and_then(Value::as_str)
        .unwrap_or(".");
    let width = args.get("width").and_then(Value::as_u64).unwrap_or(720) as u32;
    let font_dirs: Vec<String> = args
        .get("fontDirs")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    match render(html, width, &font_dirs, base_dir) {
        Ok(png) => {
            // 直接落盘返回路径：大 base64 过 bridge 传输曾被污染（PNG 头前混入垃圾字节）
            let dir = std::path::Path::new("data/render");
            let _ = std::fs::create_dir_all(dir);
            let file = dir.join(format!(
                "{}_{}.png",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis())
                    .unwrap_or(0),
                ulid_part(),
            ));
            match std::fs::write(&file, &png) {
                Ok(_) => {
                    let abs = std::fs::canonicalize(&file)
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|_| file.to_string_lossy().to_string());
                    serde_json::json!({ "file": abs, "size": png.len() })
                }
                Err(e) => serde_json::json!({ "error": format!("写盘失败: {}", e) }),
            }
        }
        Err(e) => serde_json::json!({ "error": e }),
    }
}

/// 轻量随机后缀（无 ulid 依赖）
fn ulid_part() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{:x}", n)
}
