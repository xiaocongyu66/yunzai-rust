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
    // ps-blitz-net 的 Provider::new() 要求 tokio runtime 上下文；
    // 渲染入口是同步线程，这里套一个局部 current-thread runtime
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("tokio runtime: {e}"))?;
    rt.block_on(render_inner(html, width, font_dirs, base_dir))
}

async fn render_inner(
    html: &str,
    width: u32,
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

    // ≈ TRSS 按根元素实际尺寸截图：输出宽高取内容边界（防视口留白）
    let layout = document.root_element().final_layout().size;
    let out_w = (layout.width.ceil() as u32).clamp(64, 4096);
    let render_height = (layout.height.ceil() as u32).clamp(1, out_w * 4);

    // 2× 超采样：vello_cpu 字形/边缘 AA 为单采样，直接 1x 渲染锯齿明显；
    // 按 2x 画完后 2×2 盒滤波降回 1x，视觉上逼近 Chrome(Skia) 的平滑度。
    const SS: u32 = 1; // 超采样实测观感劣化（盒滤波钝化），回退 1x 直出
    let ss_w = out_w * SS;
    let ss_h = render_height * SS;
    let rgba2 = anyrender::render_to_buffer::<anyrender_vello_cpu::VelloCpuImageRenderer, _>(
        |scene| {
            use peniko::kurbo::Rect;
            scene.fill(
                peniko::Fill::NonZero,
                Default::default(),
                Color::WHITE,
                Default::default(),
                &Rect::new(0.0, 0.0, ss_w as f64, ss_h as f64),
            );
            blitz_paint::paint_scene(scene, &mut *document, SS as f64, ss_w, ss_h, 0, 0);
        },
        ss_w,
        ss_h,
    );

    // 2×2 盒滤波降采样到输出尺寸
    let mut rgba = vec![0u8; (out_w * render_height * 4) as usize];
    for y in 0..render_height {
        for x in 0..out_w {
            let (mut r, mut g, mut b, mut a) = (0u32, 0u32, 0u32, 0u32);
            for dy in 0..SS {
                for dx in 0..SS {
                    let i = (((y * SS + dy) * ss_w + x * SS + dx) * 4) as usize;
                    r += rgba2[i] as u32;
                    g += rgba2[i + 1] as u32;
                    b += rgba2[i + 2] as u32;
                    a += rgba2[i + 3] as u32;
                }
            }
            let n = (SS * SS) as u32;
            let o = ((y * out_w + x) * 4) as usize;
            rgba[o] = (r / n) as u8;
            rgba[o + 1] = (g / n) as u8;
            rgba[o + 2] = (b / n) as u8;
            rgba[o + 3] = (a / n) as u8;
        }
    }

    encode_png(&rgba, out_w, render_height)
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
