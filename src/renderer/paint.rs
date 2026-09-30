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

/// 矩形路径（点线构造，零 Rect 依赖）
fn rect_path(x: f32, y: f32, w: f32, h: f32) -> Path {
    let mut pb = PathBuilder::new();
    pb.move_to(x, y);
    pb.line_to(x + w, y);
    pb.line_to(x + w, y + h);
    pb.line_to(x, y + h);
    pb.close();
    pb.finish().unwrap_or_else(|| PathBuilder::new().finish().unwrap())
}

/// 圆角矩形路径
fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Path {
    let r = r.clamp(0.0, w.min(h) / 2.0);
    let mut pb = PathBuilder::new();
    let (x, y) = (x as f32, y as f32);
    if r <= 0.01 {
        return rect_path(x, y, w, h);
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
    pb.finish().unwrap_or_else(|| rect_path(x, y, w, h))
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
    if v.contains("url(") {
        if let Some(raw) = super::media::extract_url(v) {
            let base = super::BASE_DIR.get().map(String::as_str).unwrap_or(".");
            return Some(Bg::Image(super::media::resolve(&raw, base)));
        }
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
        let all_auto = out.iter().enumerate().all(|(i, (p, _))| *p == f32::MAX || (i == n - 1 && *p == f32::MAX));
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
    LinearGradient::new(start, end, grad_stops(stops), SpreadMode::Pad, Transform::identity())
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
    // border-radius 多值（四角独立，取逗号分隔后的组内值）
    let radius = n
        .decls
        .get("border-radius")
        .and_then(|v| split_commas(v).first().cloned())
        .map(|g| {
            let vals: Vec<f32> = g
                .split_whitespace()
                .filter_map(|t| t.trim().trim_end_matches("px").parse::<f32>().ok())
                .collect();
            vals.first().copied().unwrap_or(0.0)
        })
        .unwrap_or(0.0);
    // opacity（0..1）：预乘到背景/文本颜色
    let opacity = n
        .decls
        .get("opacity")
        .and_then(|v| v.trim().trim_end_matches(';').parse::<f32>().ok())
        .filter(|v| *v >= 0.0 && *v <= 1.0)
        .unwrap_or(1.0);

    // box-shadow（多重，先画外层阴影再画内容）
    if let Some(bs) = n.decls.get("box-shadow") {
        draw_box_shadows(pixmap, n, bs, radius);
    }

    // 背景图（Lightning CSS 已把 background 简写展开为 background-image 等长属性）
    if n.tag != "img" {
        if let Some(bi) = n.decls.get("background-image") {
            if let Some(url) = crate::renderer::media::extract_url(bi) {
                let base = crate::renderer::BASE_DIR.get().map(String::as_str).unwrap_or(".");
                let abs = crate::renderer::media::resolve(&url, base);
                if let Some(img) = crate::renderer::media::load(&abs, base) {
                    draw_bg_image(pixmap, n, &img);
                }
            }
        }
    }

    // <img> 标签：按节点矩形绘制 src 图
    if n.tag == "img" {
        if let Some(src) = &n.src {
            if let Some(img) = crate::renderer::media::load(src, crate::renderer::BASE_DIR.get().map(String::as_str).unwrap_or(".")) {
                blit_r(pixmap, &img, n.x, n.y, n.w, n.h, (n.x, n.y, n.w, n.h), radius);
            }
        }
    }

    // 背景
    if let Some(bg) = n.decls.get("background").or_else(|| n.decls.get("background-color")).cloned() {
        match parse_bg(&bg) {
            Some(Bg::Color(c)) => {
                let path = rounded_rect_path(n.x, n.y, n.w, n.h, radius);
                let mut p = Paint::default();
                p.set_color_rgba8(c[0], c[1], c[2], (c[3] as f32 * opacity) as u8);
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
                    let center = Point::from_xy(n.x + n.w / 2.0, n.y + n.h / 2.0);
                    let radius = n.w.min(n.h).max(1.0) / 2.0;
                    if let Some(g) = RadialGradient::new(
                        center,
                        Point::from_xy(center.x + 1.0, center.y),
                        radius,
                        grad_stops(&[(0.0, fg.1), (1.0, lg.1)]),
                        SpreadMode::Pad,
                        Transform::identity(),
                    ) {
                        let path = rounded_rect_path(n.x, n.y, n.w, n.h, radius);
                        let mut p = Paint::default();
                        p.shader = g;
                        p.anti_alias = true;
                        pixmap.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
                    }
                }
            }
            Some(Bg::Image(path)) => {
                if let Some(img) = crate::renderer::media::load(&path, crate::renderer::BASE_DIR.get().map(String::as_str).unwrap_or(".")) {
                    draw_bg_image(pixmap, n, &img);
                }
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
        p.set_color_rgba8(bc[0], bc[1], bc[2], (bc[3] as f32 * opacity) as u8);
        p.anti_alias = true;
        let stroke = Stroke { width: bw, ..Stroke::default() };
        pixmap.stroke_path(&path, &p, &stroke, Transform::identity(), None);
    }

    // 文本（仅叶子画：容器的文本已作为附加叶子单独布局，这里再画会重复）
    if !n.text.is_empty() && n.children.is_empty() {
        draw_text(pixmap, n, fonts);
    }

    // z-index 稳定排序（负值在下、正值在上，同值保持 DOM 序）
    let mut ordered: Vec<&PaintNode> = n.children.iter().collect();
    ordered.sort_by_key(|c| {
        c.decls
            .get("z-index")
            .and_then(|v| v.trim().parse::<i32>().ok())
            .unwrap_or(0)
    });
    for c in ordered {
        draw_node(pixmap, c, fonts);
    }
}

/// box-shadow 解析与绘制：多重 "x y blur spread color [inset]"
fn draw_box_shadows(pixmap: &mut Pixmap, n: &PaintNode, decl: &str, radius: f32) {
    for part in split_commas(decl) {
        let mut nums: Vec<f32> = Vec::new();
        let mut color: Option<[u8; 4]> = None;
        let mut inset = false;
        for tok in part.split_whitespace() {
            if tok == "inset" {
                inset = true;
                continue;
            }
            if let Some(c) = parse_color(tok) {
                color = Some(c);
                continue;
            }
            if let Ok(v) = tok.trim_end_matches("px").parse::<f32>() {
                nums.push(v);
            }
        }
        if nums.len() < 2 {
            continue;
        }
        let (dx, dy) = (nums[0], nums[1]);
        let blur = nums.get(2).copied().unwrap_or(0.0);
        let spread = nums.get(3).copied().unwrap_or(0.0);
        let Some(c) = color else { continue };
        let mut p = Paint::default();
        p.anti_alias = true;
        if inset {
            // inset：内部描边近似（沿内边界 stroke 两圈，alpha 递减）
            let bw = (blur.max(1.0) * 0.6 + spread).max(1.0);
            for (i, a) in [0.35f32, 0.2].iter().enumerate() {
                let off = bw * (i as f32 + 1.0) / 3.0;
                let path = rounded_rect_path(
                    n.x + off,
                    n.y + off,
                    (n.w - off * 2.0).max(0.0),
                    (n.h - off * 2.0).max(0.0),
                    (radius - off).max(0.0),
                );
                p.set_color_rgba8(c[0], c[1], c[2], (c[3] as f32 * a) as u8);
                let stroke = Stroke { width: bw / 2.0, ..Stroke::default() };
                pixmap.stroke_path(&path, &p, &stroke, Transform::identity(), None);
            }
        } else {
            // 外阴影：偏移+扩散矩形，blur 用多层近似
            let layers = if blur > 0.5 { 3 } else { 1 };
            for i in 0..layers {
                let k = i as f32 / layers as f32;
                let pad = spread + blur * (1.0 - k) * 0.5;
                let path = rounded_rect_path(
                    n.x + dx - pad,
                    n.y + dy - pad,
                    n.w + pad * 2.0,
                    n.h + pad * 2.0,
                    radius + pad,
                );
                let a = 1.0 - k * 0.6;
                p.set_color_rgba8(c[0], c[1], c[2], (c[3] as f32 * a * 0.35) as u8);
                pixmap.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
            }
        }
    }
}


fn draw_text(pixmap: &mut Pixmap, n: &PaintNode, fonts: &mut super::text::TextEngine) {
    // text-shadow（取第一重）：先画一层偏移阴影，再画正文
    if let Some(ts) = n.decls.get("text-shadow").cloned() {
        let mut nums: Vec<f32> = Vec::new();
        let mut color: Option<[u8; 4]> = None;
        for tok in split_commas(&ts).first().map(String::as_str).unwrap_or("").split_whitespace() {
            if let Some(c) = parse_color(tok) {
                color = Some(c);
                continue;
            }
            if let Ok(v) = tok.trim_end_matches("px").parse::<f32>() {
                nums.push(v);
            }
        }
        if let Some(c) = color {
            let dx = nums.first().copied().unwrap_or(0.0);
            let dy = nums.get(1).copied().unwrap_or(0.0);
            let mut sn = n.clone();
            sn.x += dx;
            sn.y += dy;
            let sc = cosmic_text_color(c);
            let buffer_s = super::text::layout_buffer(
                fonts,
                &sn.text,
                font_size_of(&sn),
                weight_of(&sn),
                c,
                Some(sn.w),
                line_height_of(&sn),
                sn.text_align,
                sn.decls.get("font-family").and_then(|v| {
                    v.split(',').map(|s2| s2.trim().trim_matches(['"', '\''])).find(|s2| {
                        !s2.is_empty() && !matches!(*s2, "sans-serif" | "serif" | "monospace" | "system-ui")
                    })
                }),
            );
            let bx = sn.x.round() as i32;
            let by = sn.y.round() as i32;
            buffer_s.draw(&mut fonts.font_system, &mut fonts.swash, sc, |gx, gy, gw, gh, col| {
                let px = bx + gx;
                let py = by + gy;
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
                        let sa = col.a() as u32;
                        if sa == 0 {
                            continue;
                        }
                        let pw = pixmap.width();
                        let data = pixmap.data_mut();
                        let di = ((y * pw + x) * 4) as usize;
                        let da = data[di + 3] as u32;
                        let out_a = (sa + da * (255 - sa) / 255) as u8;
                        let mix = |fg: u32, bg: u32| ((fg * sa + bg * da * (255 - sa) / 255) / out_a.max(1) as u32) as u8;
                        data[di] = mix(col.r() as u32, data[di] as u32);
                        data[di + 1] = mix(col.g() as u32, data[di + 1] as u32);
                        data[di + 2] = mix(col.b() as u32, data[di + 2] as u32);
                        data[di + 3] = out_a;
                    }
                }
            });
        }
    }
    let buffer = super::text::layout_buffer(
        fonts,
        &n.text,
        font_size_of(n),
        weight_of(n),
        color_of(n),
        Some(n.w),
        line_height_of(n),
        n.text_align,
        n.decls.get("font-family").and_then(|v| {
            v.split(',')
                .map(|s| s.trim().trim_matches(['"', '\'']))
                .find(|s| !s.is_empty() && !matches!(*s, "sans-serif" | "serif" | "monospace" | "system-ui"))
        }),
    );
    // 水平对齐偏移：逐行用本行 line_w（用整段最宽行会让短行错位）
    let color = cosmic_text_color(color_of(n));
    // 逐行对齐：直接遍历 run 计算，但绘制走全局 buffer.draw（LayoutRun 无 draw 方法）
    let runs_info: Vec<(f32, f32)> = buffer
        .layout_runs()
        .map(|r| (r.line_w, r.line_top))
        .collect();
    let max_w = runs_info.iter().map(|(w, _)| *w).fold(0f32, f32::max);
    let align_off = |lw: f32| -> f32 {
        match n.text_align {
            TextAlign::Center => ((n.w - lw) / 2.0).max(0.0),
            TextAlign::Right => (n.w - lw).max(0.0),
            TextAlign::Left => 0.0,
        }
    };
    let _ = runs_info;
    let dx = align_off(max_w);
    let base_x = (n.x + dx).round() as i32;
    let base_y = n.y.round() as i32;

    buffer.draw(&mut fonts.font_system, &mut fonts.swash, color, |gx, gy, gw, gh, color| {
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

/// 绘制图片（nearest 采样 + 目标区域裁剪）：精灵/平铺/拉伸通用
fn blit(pixmap: &mut Pixmap, img: &Pixmap, dx: f32, dy: f32, dw: f32, dh: f32, clip: (f32, f32, f32, f32)) {
    blit_r(pixmap, img, dx, dy, dw, dh, clip, 0.0)
}

/// 带圆角裁剪的 blit（背景图不溢出 border-radius）
fn blit_r(pixmap: &mut Pixmap, img: &Pixmap, dx: f32, dy: f32, dw: f32, dh: f32, clip: (f32, f32, f32, f32), radius: f32) {
    let (cx, cy, cw, ch) = clip;
    let iw = img.width() as f32;
    let ih = img.height() as f32;
    if iw <= 0.0 || ih <= 0.0 || dw <= 0.0 || dh <= 0.0 {
        return;
    }
    let x0 = dx.max(cx).floor() as i32;
    let y0 = dy.max(cy).floor() as i32;
    let x1 = (dx + dw).min(cx + cw).ceil() as i32;
    let y1 = (dy + dh).min(cy + ch).ceil() as i32;
    for py in y0..y1 {
        for px in x0..x1 {
            let sx = ((px as f32 - dx) / dw * iw).floor().clamp(0.0, iw - 1.0) as u32;
            let sy = ((py as f32 - dy) / dh * ih).floor().clamp(0.0, ih - 1.0) as u32;
            if radius > 0.0 {
                // 圆角内判断：像素点到圆角矩形内切盒的钳制点距离
                let (cx0, cy0, cw0, ch0) = clip;
                let qx = (px as f32).clamp(cx0 + radius, cx0 + cw0 - radius);
                let qy = (py as f32).clamp(cy0 + radius, cy0 + ch0 - radius);
                let ddx = px as f32 - qx;
                let ddy = py as f32 - qy;
                if ddx * ddx + ddy * ddy > radius * radius {
                    continue;
                }
            }
            let c = match img.pixel(sx, sy) {
                Some(c) => c,
                None => continue,
            };
            if c.alpha() == 0 {
                continue;
            }
            let (ux, uy) = (px as u32, py as u32);
            if ux >= pixmap.width() || uy >= pixmap.height() {
                continue;
            }
            // 源（premul）直接与目标（premul）做 src-over
            let idx = (uy * pixmap.width() + ux) as usize;
            let data = pixmap.data_mut();
            let di = idx * 4;
            let (sr, sg, sb, sa) = (
                c.red() as u32,
                c.green() as u32,
                c.blue() as u32,
                c.alpha() as u32,
            );
            let dr = data[di] as u32;
            let dg = data[di + 1] as u32;
            let db = data[di + 2] as u32;
            let da = data[di + 3] as u32;
            data[di] = (sr + dr * (255 - sa) / 255) as u8;
            data[di + 1] = (sg + dg * (255 - sa) / 255) as u8;
            data[di + 2] = (sb + db * (255 - sa) / 255) as u8;
            data[di + 3] = (sa + da * (255 - sa) / 255) as u8;
        }
    }
}

fn draw_image(pixmap: &mut Pixmap, path: &str, x: f32, y: f32, w: f32, h: f32, _r: f32) {
    let img = match crate::renderer::media::load(path, crate::renderer::BASE_DIR.get().map(String::as_str).unwrap_or(".")) {
        Some(p) => p,
        None => return,
    };
    // 简写里的 repeat 语义：含 no-repeat 或未知 → 单次绘制
    let draw_one = |pixmap: &mut Pixmap, img: &Pixmap, x: f32, y: f32, w: f32, h: f32| {
        blit(pixmap, img, x, y, w, h, (x, y, w, h));
    };
    // 默认：拉伸铺满节点
    draw_one(pixmap, &img, x, y, w, h);
}

/// 背景图绘制（含 background-position 精灵取片 / background-size 缩放）
fn draw_bg_image(pixmap: &mut Pixmap, n: &PaintNode, img: &Pixmap) {
    let base = crate::renderer::BASE_DIR.get().map(String::as_str).unwrap_or(".");
    let _ = base;
    let (mut dw, mut dh) = (img.width() as f32, img.height() as f32);
    if let Some(sz) = n.decls.get("background-size").or_else(|| {
        n.decls.get("background").filter(|v| v.contains("background-size"))
    }) {
        (dw, dh) = crate::renderer::media::parse_size(sz, n.w, n.h, img.width(), img.height());
    } else if let Some(bg) = n.decls.get("background") {
        // 简写里可能带 "500px auto" 等尺寸？CSS 简写不含 size；保持固有
        let _ = bg;
    }
    let (ox, oy) = n
        .decls
        .get("background-position")
        .map(|p| crate::renderer::media::parse_position(p))
        .unwrap_or((0.0, 0.0));
    // repeat 语义：CSS 默认 repeat（未写 no-repeat 时平铺铺满节点）
    let bg = n.decls.get("background").cloned().unwrap_or_default();
    let no_repeat = bg.contains("no-repeat")
        || n.decls
            .get("background-repeat")
            .map(|v| v.contains("no-repeat"))
            .unwrap_or(false);
    let radius = n
        .decls
        .get("border-radius")
        .and_then(|v| split_commas(v).first().and_then(|r| r.trim().trim_end_matches("px").parse::<f32>().ok()))
        .unwrap_or(0.0);
    if no_repeat {
        // 精灵取片：背景图按 size 缩放后偏移 ox,oy，裁到节点矩形
        blit_r(pixmap, img, n.x + ox, n.y + oy, dw, dh, (n.x, n.y, n.w, n.h), radius);
    } else {
        let mut ty = n.y + oy;
        while ty < n.y + n.h {
            let mut tx = n.x + ox;
            while tx < n.x + n.w {
                blit_r(pixmap, img, tx, ty, dw, dh, (n.x, n.y, n.w, n.h), radius);
                if dw <= 0.0 { break; }
                tx += dw;
            }
            if dh <= 0.0 { break; }
            ty += dh;
        }
    }
}
