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

/// HTML → PNG 字节。width=视口宽；scale=输出像素密度（内容宽高 × scale，直接出大图，
/// 不降采样——等价 TRSS puppeteer 的 deviceScaleFactor 语义）。
pub fn render(html: &str, width: u32, scale: f64, font_dirs: &[String], base_dir: &str) -> Result<Vec<u8>, String> {
    // ps-blitz-net 的 Provider::new() 要求 tokio runtime 上下文；
    // 渲染入口是同步线程，这里套一个局部 current-thread runtime
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    rt.block_on(render_inner(html, width, scale, font_dirs, base_dir))
}

async fn render_inner(
    html: &str,
    width: u32,
    scale: f64,
    font_dirs: &[String],
    base_dir: &str,
) -> Result<Vec<u8>, String> {
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

    // 驱动资源拉取（图片/字体异步）；net.is_empty() = 无在途请求即完成。上限 30s 防死循环。
    // 两个关键时序：
    // 1. resolve 触发 fetch spawn 后任务尚未调度、计数未增——立刻查 is_empty 会误判；
    // 2. fetch 回调经 channel 发 DocumentEvent::ResourceLoad，必须 handle_messages()
    //    消费事件才会回填 DOM（图片/字体数据落地），否则永远空白。
    document.resolve(0.0);
    for _ in 0..600 {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        document.handle_messages();
        document.resolve(0.0);
        if net.is_empty() {
            break;
        }
    }
    document.handle_messages();
    document.resolve(0.0);

    // ≈ TRSS element.screenshot() 语义：html/body 撑满视口不可作边界，
    // 取 body 子树的几何并集（递归累加偏移，含溢出子元素），四周再加
    // 12px 容忍 box-shadow 等非布局绘制；scale 乘成输出像素。
    let html = document.root_element();
    let html_id = html.id;
    let body_id = html
        .children
        .iter()
        .find(|id| {
            document
                .get_node(**id)
                .and_then(|n| n.element_data())
                .map(|el| el.name.local.as_ref() == "body")
                .unwrap_or(false)
        })
        .copied()
        .unwrap_or(html_id);
    let scale = scale.clamp(0.5, 4.0);
    let mut union = peniko::kurbo::Rect::ZERO;
    subtree_union(&document, body_id, 0.0, 0.0, &mut union);
    let content_w = (union.width().ceil() as u32).clamp(64, 4096) + 12;
    let content_h = (union.height().ceil() as u32).clamp(1, content_w * 4) + 12;
    let out_w = ((content_w as f64 * scale).ceil() as u32).clamp(64, 8192);
    let render_height = ((content_h as f64 * scale).ceil() as u32).clamp(1, out_w * 4);

    // 白底 + 文档 → RGBA（vello_cpu 纯 CPU 光栅化，scale 直接作为绘制密度）
    let rgba = anyrender::render_to_buffer::<anyrender_vello_cpu::VelloCpuImageRenderer, _>(
        |scene| {
            use peniko::kurbo::Rect;
            scene.fill(
                peniko::Fill::NonZero,
                Default::default(),
                Color::WHITE,
                Default::default(),
                &Rect::new(0.0, 0.0, out_w as f64, render_height as f64),
            );
            blitz_paint::paint_scene(scene, &mut *document, scale, out_w, render_height, 0, 0);
        },
        out_w,
        render_height,
    );

    encode_png(&rgba, out_w, render_height)
}

/// body 子树几何并集（taffy location 相对父，递归累加偏移），用于内容边界裁取
fn subtree_union(
    doc: &blitz_dom::BaseDocument,
    id: blitz_dom::NodeId,
    ox: f64,
    oy: f64,
    rect: &mut peniko::kurbo::Rect,
) {
    let Some(n) = doc.get_node(id) else { return };
    if n.element_data().is_none() { return }  // 非元素节点无 final_layout
    let l = n.final_layout();
    let x = ox + l.location.x as f64;
    let y = oy + l.location.y as f64;
    *rect = rect.union(peniko::kurbo::Rect::new(
        x,
        y,
        x + l.size.width as f64,
        y + l.size.height as f64,
    ));
    for c in &n.children {
        subtree_union(doc, *c, x, y, rect);
    }
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
