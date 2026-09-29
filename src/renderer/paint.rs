//! 布局结果 → tiny-skia 光栅化：背景/渐变/圆角边框/文本/图片

use super::css::split_commas;
use super::layout::PaintNode;
use super::text::TextAlign;
use tiny_skia::*;

/// CSS 颜色解析：#rgb/#rrggbb/#rrggbbaa/rgb()/rgba()/命名色子集
pub fn parse_color(s: &str) -> Option<[u8; 4]> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        let h = hex.as_bytes();
        return match h.len() {
            3 => Some([hex2(h[0], h[0]), hex2(h[1], h[1]), hex2(h[2], h[2]), 255]),
            6 => Some([hex2(h[0], h[1]), hex2(h[2], h[3]), hex2(h[4], h[5]), 255]),
            8 => Some([hex2(h[0], h[1]), hex2(h[2], h[3]), hex2(h[4], h[5]), hex2(h[6], h[7])]),
            _ => None,
        };
    }
    let lower = s.to_lowercase();
    if let Some(inner) = lower.strip_prefix("rgba(").or_else(|| lower.strip_prefix("rgb(")) {
        let inner = inner.trim_end_matches(')');
        let parts: Vec<f32> = inner.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        if parts.len() >= 3 {
            return Some([
                parts[0].clamp(0.0, 255.0) as u8,
                parts[1].clamp(0.0, 255.0) as u8,
                parts[2].clamp(0.0, 255.0) as u8,
                if parts.len() > 3 { (parts[3] * 255.0).clamp(0.0, 255.0) as u8 } else { 255 },
            ]);
        }
    }
    let named: &[(&str, [u8; 4])] = &[
        ("white", [255, 255, 255, 255]),
        ("black", [0, 0, 0, 255]),
        ("transparent", [0, 0, 0, 0]),
        ("red", [255, 0, 0, 255]),
        ("green", [0, 128, 0, 255]),
        ("blue", [0, 0, 255, 255]),
        ("yellow", [255, 255, 0, 255]),
        ("orange", [255, 165, 0, 255]),
        ("gray", [128, 128, 128, 255]),
        ("grey", [128, 128, 128, 255]),
        ("silver", [192, 192, 192, 255]),
        ("gold", [255, 215, 0, 255]),
        ("purple", [128, 0, 128, 255]),
        ("pink", [255, 192, 203, 255]),
    ];
    named.iter().find(|(n, _)| *n == lower).map(|(_, c)| *c)
}

fn hex2(a: u8, b: u8) -> u8 {
    let d = |c: u8| (c as char).to_digit(16).unwrap_or(0) as u8;
    (d(a) << 4) | d(b)
}

/// 圆角矩形路径
fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    let r = r.clamp(0.0, w.min(h) / 2.0);
    let mut pb = PathBuilder::new();
    let (x, y) = (x as f32, y as f32);
    if r <= 0.01 {
        pb.push_rect(Rect::from_xywh(x, y, w, h));
        return pb.finish().unwrap();
    }
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r * 0.55, y, x + w, y + r * 0.55, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r * 0.55, x + w - r * 0.55, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r * 0.55, y + h, x, y + h - r * 0.55, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r * 0.55, x + r * 0.55, y, x + r, y);
    pb.close();
    pb.finish().unwrap_or_else(|| PathBuilder::from_rect(Rect::from_xywh(x, y, w, h)).finish().unwrap())
}

/// 解析 background：纯色 / linear-gradient(...) / radial-gradient(...)
enum Bg {
    Color([u8; 4]),
    Linear { deg: f32, stops: Vec<(f32, [u8; 4])> },
    Radial { stops: Vec<(f32, [u8; 4])> },
    Image(String),
}

fn parse_bg(v: &str) -> Option<Bg> {
    let v = v.trim();
    if let Some(grad) = v.strip_prefix("linear-gradient(") {
        let inner = grad.strip_suffix(')')?;
        let parts = split_commas(inner);
        let mut deg = 180.0f32;
        let mut stops_raw: &[String] = &parts;
        if let Some(first) = parts.first() {
            if first.ends_with("deg") {
                deg = first.trim().trim_end_matches("deg").trim().parse().unwrap_or(180.0);
                stops_raw = &parts[1..];
            }
        }
        let stops = parse_stops(stops_raw);
        return Some(Bg::Linear { deg, stops });
    }
    if let Some(grad) = v.strip_prefix("radial-gradient(") {
        let inner = grad.strip_suffix(')')?;
        let parts = split_commas(inner);
        // 跳过形状/尺寸描述（circle、ellipse、at x y 等）
        let stops_raw: Vec<String> = parts
            .iter()
            .filter(|p| p.contains('#') || p.contains("rgb") || p.parse::<f32>().is_ok() || p.ends_with('%'))
            .filter(|p| !p.contains("at "))
            .cloned()
            .collect();
        return Some(Bg::Radial { stops: parse_stops(&stops_raw) });
    }
    if let Some(u) = v.strip_prefix("url(") {
        return Some(Bg::Image(u.trim_end_matches(')').trim().trim_matches(|c| c == '"' || c == '\'').to_string()));
    }
    parse_color(v).map(Bg::Color)
}

fn parse_stops(parts: &[String]) -> Vec<(f32, [u8; 4])> {
    let mut out = Vec::new();
    for p in parts {
        let segs = p.split_whitespace().collect::<Vec<_>>();
        let (cpart, pos) = if segs.len() >= 2 {
            (segs[0], segs[1].trim_end_matches('%').parse::<f32>().ok().map(|n| n / 100.0))
        } else {
            (p.trim(), None)
        };
        if let Some(c) = parse_color(cpart) {
            out.push((pos.unwrap_or(f32::MAX), c));
        }
    }
    // 均匀分布无位置色标
    if out.len() >= 2 {
        let n = out.len();
        let all_auto = out.iter().enumerate().all(|(i, (p, _))| p == f32::MAX || (i == n - 1 && *p == f32::MAX));
        if all_auto {
            let step = 1.0 / (n as f32 - 1.0);
            for (i, e) in out.iter_mut().enumerate() {
                e.0 = i as f32 * step;
            }
        }
    }
    out
}

/// 渐变 shader（角度：0=向上，顺时针；CSS 语义）
fn linear_shader(deg: f32, stops: &[(f32, [u8; 4])], w: f32, h: f32) -> Option<Shader> {
    if stops.len() < 2 {
        return None;
    }
    let rad = (deg - 90.0).to_radians();
    let (dx, dy) = (rad.cos(), rad.sin());
    // 线段端点穿过中心并与边界相交
    let half = ((w * dx.abs() + h * dy.abs()) / 2.0).max(1.0);
    let cx = w / 2.0;
    let cy = h / 2.0;
    let start = Point::from_xy(cx - dx * half, cy - dy * half);
    let end = Point::from_xy(cx + dx * half, cy + dy * half);
    let g = LinearGradient::new(start, end, grad_stops(stops), SpreadMethod::Pad, Transform::identity())?;
    Some(Shader::LinearGradient(g))
}

fn grad_stops(stops: &[(f32, [u8; 4])]) -> Vec<GradientStop> {
    stops
        .iter()
        .map(|(p, c)| GradientStop::new(*p, Color::from_rgba8(c[0], c[1], c[2], c[3])))
        .collect()
}

/// 绘制主入口
pub fn paint(root: &PaintNode, width: f32, height: f32, fonts: &mut super::text::TextEngine) -> Result<Vec<u8>, String> {
    let mut pixmap = Pixmap::new(width as u32, height as u32).ok_or("pixmap create fail")?;
    pixmap.fill(Color::from_rgba8(255, 255, 255, 255));
    draw_node(&mut pixmap, root, fonts);
    pixmap.encode_png().map_err(|e| e.to_string())
}

fn draw_node(pixmap: &mut Pixmap, n: &PaintNode, fonts: &mut super::text::TextEngine) {
    if n.w <= 0.0 || n.h <= 0.0 {
        return;
    }
    let radius = n
        .decls
        .get("border-radius")
        .and_then(|v| split_commas(v).first().and_then(|r| r.trim().trim_end_matches("px").parse::<f32>().ok()))
        .unwrap_or(0.0);

    // 背景
    if let Some(bg) = n.decls.get("background").or_else(|| n.decls.get("background-color")).cloned() {
        match parse_bg(&bg) {
            Some(Bg::Color(c)) => {
                let path = rounded_rect_path(n.x, n.y, n.w, n.h, radius);
                let mut p = Paint::default();
                p.set_color_rgba8(c[0], c[1], c[2], c[3]);
                p.anti_alias = true;
                pixmap.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
            }
            Some(Bg::Linear { deg, stops }) => {
                if let Some(shader) = linear_shader(deg, &stops, n.w, n.h) {
                    let path = rounded_rect_path(n.x, n.y, n.w, n.h, radius);
                    let mut p = Paint::default();
                    p.shader = shader;
                    p.anti_alias = true;
                    pixmap.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
                }
            }
            Some(Bg::Radial { stops }) => {
                if let (Some(fg), Some(lg)) = (stops.first(), stops.last()) {
                    let g = RadialGradient::new(
                        Point::from_xy(n.x + n.w / 2.0, n.y + n.h / 2.0),
                        n.w.min(n.h).max(1.0) / 2.0,
                        grad_stops(&[(0.0, fg.1), (1.0, lg.1)]),
                        SpreadMethod::Pad,
                        Transform::identity(),
                    );
                    let path = rounded_rect_path(n.x, n.y, n.w, n.h, radius);
                    let mut p = Paint::default();
                    p.shader = g;
                    p.anti_alias = true;
                    pixmap.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
                }
            }
            Some(Bg::Image(path)) => {
                draw_image(pixmap, &path, n.x, n.y, n.w, n.h, radius);
            }
            None => {}
        }
    }

    // 边框
    if let Some(bw) = n
        .decls
        .get("border-width")
        .or_else(|| n.decls.get("border"))
        .and_then(|v| split_commas(v).first().cloned())
        .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
        .filter(|v| *v > 0.0)
    {
        let bc = n
            .decls
            .get("border-color")
            .or_else(|| n.decls.get("border"))
            .and_then(|v| {
                split_commas(v)
                    .iter()
                    .rev()
                    .find_map(|p| parse_color(p))
            })
            .unwrap_or([0, 0, 0, 255]);
        let path = rounded_rect_path(n.x + bw / 2.0, n.y + bw / 2.0, (n.w - bw).max(0.0), (n.h - bw).max(0.0), radius);
        let mut p = Paint::default();
        p.set_color_rgba8(bc[0], bc[1], bc[2], bc[3]);
        p.anti_alias = true;
        p.stroke_width = bw;
        p.stroke = true;
        pixmap.stroke_path(&path, &p, Stroke { width: bw, ..Stroke::default() }, Transform::identity(), None);
    }

    // 文本
    if !n.text.is_empty() {
        draw_text(pixmap, n, fonts);
    }

    for c in &n.children {
        draw_node(pixmap, c, fonts);
    }
}

fn draw_text(pixmap: &mut Pixmap, n: &PaintNode, fonts: &mut super::text::TextEngine) {
    let buffer = super::text::layout_buffer(
        fonts,
        &n.text,
        font_size_of(n),
        weight_of(n),
        color_of(n),
        Some(n.w),
        line_height_of(n),
        n.text_align,
    );
    // 水平对齐偏移
    let line_w = buffer.layout_runs().map(|r| r.line_w).fold(0f32, f32::max);
    let dx = match n.text_align {
        TextAlign::Center => (n.w - line_w) / 2.0,
        TextAlign::Right => n.w - line_w,
        TextAlign::Left => 0.0,
    }
    .max(0.0);
    let base_x = (n.x + dx).round();
    let base_y = n.y.round();

    buffer.draw(fonts, &mut fonts.swash, cosmic_text_color(color_of(n)), |gx, gy, gw, gh, color| {
        // 回调给的是设备像素矩形 + 颜色（alpha 已混合）
        let px = (base_x as i32) + gx;
        let py = (base_y as i32) + gy;
        for yy in 0..gh {
            for xx in 0..gw {
                let (x, y) = (px + xx as i32, py + yy as i32);
                if x < 0 || y < 0 {
                    continue;
                }
                let (x, y) = (x as u32, y as u32);
                if x >= pixmap.width() || y >= pixmap.height() {
                    continue;
                }
                let idx = (y * pixmap.width() + x) as usize;
                let c = color;
                // 源 alpha
                let sa = c.a() as u32;
                if sa == 0 {
                    continue;
                }
                let data = pixmap.data_mut();
                let di = idx * 4;
                // premultiplied 目标（tiny-skia data 是 premul RGBA）
                let dr = data[di] as u32;
                let dg = data[di + 1] as u32;
                let db = data[di + 2] as u32;
                let da = data[di + 3] as u32;
                let sr = (c.r() as u32 * sa + 127) / 255;
                let sg = (c.g() as u32 * sa + 127) / 255;
                let sb = (c.b() as u32 * sa + 127) / 255;
                // out = src + dst*(1-sa)
                data[di] = (sr + dr * (255 - sa) / 255) as u8;
                data[di + 1] = (sg + dg * (255 - sa) / 255) as u8;
                data[di + 2] = (sb + db * (255 - sa) / 255) as u8;
                data[di + 3] = (sa + da * (255 - sa) / 255) as u8;
            }
        }
    });
}

fn cosmic_text_color(c: [u8; 4]) -> cosmic_text::Color {
    cosmic_text::Color::rgba(c[0], c[1], c[2], c[3])
}

fn font_size_of(n: &PaintNode) -> f32 {
    n.decls.get("font-size").and_then(|v| v.trim().trim_end_matches("px").parse().ok()).unwrap_or(16.0)
}
fn weight_of(n: &PaintNode) -> u16 {
    n.decls.get("font-weight").and_then(|v| v.trim().parse().ok()).unwrap_or(400)
}
fn color_of(n: &PaintNode) -> [u8; 4] {
    n.decls.get("color").and_then(|v| parse_color(v)).unwrap_or([26, 26, 26, 255])
}
fn line_height_of(n: &PaintNode) -> f32 {
    n.decls
        .get("line-height")
        .and_then(|v| v.trim().trim_end_matches("px").parse().ok())
        .unwrap_or(font_size_of(n) * 1.5)
}

fn draw_image(pixmap: &mut Pixmap, path: &str, x: f32, y: f32, w: f32, h: f32, _r: f32) {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(_) => return,
    };
    let img = match image::load_from_memory(&bytes) {
        Ok(i) => i.to_rgba8(),
        Err(_) => return,
    };
    let iw = img.width().min(4096);
    let ih = img.height().min(4096);
    let mut pix = match tiny_skia::Pixmap::from_vec(
        img.to_vec(),
        tiny_skia::IntSize::from_wh(iw, ih).unwrap(),
    ) {
        Some(p) => p,
        None => return,
    };
    let sx = if iw > 0 { w / iw as f32 } else { 1.0 };
    let sy = if ih > 0 { h / ih as f32 } else { 1.0 };
    let tr = Transform::from_translate(x, y).post_scale(sx, sy);
    pixmap.draw_pixmap(0, 0, pix.as_ref(), &PixmapPaint::default(), tr, None);
}
