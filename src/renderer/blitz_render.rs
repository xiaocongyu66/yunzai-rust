//! Blitz 引擎渲染：html5ever 解析 → stylo 样式（Servo 真实级联/选择器/继承）→
//! taffy 布局 → blitz-paint 遍历绘制 → vello_cpu 纯 CPU 光栅化（无 GPU 依赖）。
//!
//! 资源（img src / background url / @font-face src）由 blitz-net Provider 统一加载：
//! data: / file: / https 三类源全覆盖；相对路径按 base_url（file://<base_dir>/）解析。

use std::sync::Arc;

use anyrender::PaintScene as _;
use blitz_dom::{DocumentConfig, util::Color};
use blitz_html::HtmlDocument;
use blitz_net::Provider;
use blitz_traits::shell::{ColorScheme, Viewport};

/// HTML → PNG 字节。宽度为视口宽，高度按根元素内容自适应（≈TRSS 截图行为）。
pub fn render(html: &str, width: u32, font_dirs: &[String], base_dir: &str) -> Result<Vec<u8>, String> {
    let width = width.clamp(64, 4096);
    let base_url = format!("file://{}/", base_dir.trim_end_matches('/'));
    let net = Arc::new(Provider::new(None));

    let mut document = HtmlDocument::from_html(
        html,
        DocumentConfig {
            base_url: Some(base_url),
            net_provider: Some(Arc::clone(&net) as _),
            viewport: Some(Viewport::new(width, 800, 1.0, ColorScheme::Light)),
            font_ctx: Some(build_font_ctx(font_dirs)),
            ..Default::default()
        },
    );

    // 驱动资源拉取（图片/字体异步）；net.is_empty() = 无在途请求即完成。上限防死循环
    for _ in 0..600 {
        document.resolve(0.0);
        if net.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    document.resolve(0.0);

    let content_height = document.root_element().final_layout().size.height;
    let render_height = (content_height.ceil() as u32).clamp(1, width * 4);

    // 白底 + 文档 → RGBA（vello_cpu 纯 CPU 光栅化）
    let rgba = anyrender::render_to_buffer::<anyrender_vello_cpu::VelloCpuImageRenderer, _>(
        |scene| {
            use peniko::kurbo::Rect;
            scene.fill(
                peniko::Fill::NonZero,
                Default::default(),
                Color::WHITE,
                Default::default(),
                &Rect::new(0.0, 0.0, width as f64, render_height as f64),
            );
            blitz_paint::paint_scene(scene, &mut *document, 1.0, width, render_height, 0, 0);
        },
        width,
        render_height,
    );

    encode_png(&rgba, width, render_height)
}

/// 字体目录注册进 Parley FontContext（模板 @font-face 由 stylo 经 net provider 自动拉取注册）
fn build_font_ctx(font_dirs: &[String]) -> blitz_dom::FontContext {
    use parley::fontique::{Blob, Collection, CollectionOptions, SourceCache};
    let mut ctx = blitz_dom::FontContext {
        source_cache: SourceCache::new_shared(),
        collection: Collection::new(CollectionOptions {
            shared: false,
            system_fonts: false,
        }),
    };
    for dir in font_dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_font_file(&path) {
                continue;
            }
            if let Ok(data) = std::fs::read(&path) {
                ctx.collection
                    .register_fonts(Blob::new(Arc::new(data) as _), None);
            }
        }
    }
    ctx
}

fn is_font_file(path: &std::path::Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|s| s.to_ascii_lowercase()).as_deref(),
        Some("ttf" | "otf" | "ttc" | "woff" | "woff2")
    )
}

fn encode_png(rgba: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}
