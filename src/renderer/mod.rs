//! 自研 HTML/CSS 渲染器（洁净室）：html5ever 解析 → 样式匹配 → taffy 布局 → tiny-skia 光栅化
//!
//! 阶段A支持：div/span/p/b 等通用盒、display:flex（row/column/gap/justify/align/wrap）、
//! background-color/linear-gradient、border(-radius)、padding/margin、width/height(px/%/auto)、
//! color/font-size/font-weight/text-align/line-height、img(本地路径/base64)。
//! 入口：[`render`] — HTML 字符串 → PNG 字节。

pub mod css;
pub mod dom;
pub mod layout;
pub mod paint;
pub mod text;

use serde_json::Value;

/// 渲染 HTML → PNG。宽度默认 720，高度按内容自适应（上限 4096）。
pub fn render(html: &str, width: u32, font_dirs: &[String]) -> Result<Vec<u8>, String> {
    let width = width.clamp(64, 2048) as f32;

    // 1. 解析 DOM + 收集样式（<style> 块 + inline style + 选择器匹配）
    let (root_node, rules) = dom::parse(html)?;
    let styled = css::apply_styles(root_node, &rules);

    // 2. 字体系统
    let fonts = text::FontSystem::load(font_dirs)?;

    // 3. 布局（先按宽度约束算内容高度）
    let tree = layout::build_tree(&styled, width, &fonts)?;
    let (_, total_h) = layout::compute(&tree, width, f32::MAX)?;
    let height = (total_h.ceil() as u32).clamp(1, 4096);

    // 4. 光栅化
    paint::paint(&tree, width, height as f32, &fonts)
}

/// op 层入口：JSON 参数 { html, width?, fontDirs? } → { data: base64, width, height }
pub fn render_op(args: &Value) -> Value {
    let html = args.get("html").and_then(Value::as_str).unwrap_or("");
    let width = args.get("width").and_then(Value::as_u64).unwrap_or(720) as u32;
    let font_dirs: Vec<String> = args
        .get("fontDirs")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    match render(html, width, &font_dirs) {
        Ok(png) => serde_json::json!({
            "data": crate::util::bytes_to_base64(&png),
        }),
        Err(e) => serde_json::json!({ "error": e }),
    }
}
