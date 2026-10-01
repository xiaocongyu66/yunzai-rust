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


/// transform 声明 → tiny_skia 变换矩阵（按函数顺序从左到右组合）
fn parse_transform(decl: &str, w: f32, h: f32) -> Option<Transform> {
    let mut m = Transform::identity();
    let mut found = false;
    let bytes: Vec<char> = decl.chars().collect();
    let text: String = bytes.iter().collect();
    // 逐个 function(arg...) 扫描
    let mut rest = text.as_str();
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().trim_start_matches(',').trim().to_lowercase();
        let after = &rest[open + 1..];
        let Some(close) = after.find(')') else { break };
        let args_s = after[..close].trim();
        rest = &after[close + 1..];
        let mut args: Vec<f32> = Vec::new();
        for a in args_s.split(',') {
            let t = a.trim();
            let v = if t.ends_with("deg") {
                t.trim_end_matches("deg").parse::<f32>().unwrap_or(0.0)
            } else if t.ends_with("rad") {
                t.trim_end_matches("rad").parse::<f32>().unwrap_or(0.0).to_degrees()
            } else if t.ends_with("px") {
                t.trim_end_matches("px").parse::<f32>().unwrap_or(0.0)
            } else if t.ends_with('%') {
                let pct = t.trim_end_matches('%').parse::<f32>().unwrap_or(0.0);
                0.0 // 百分比在函数内按需展开，此处占位
            } else {
                t.parse::<f32>().unwrap_or(0.0)
            };
            args.push(v);
        }
        let one = match name.as_str() {
            "scale" => {
                let sx = args.first().copied().unwrap_or(1.0);
                let sy = args.get(1).copied().unwrap_or(sx);
                Transform::from_scale(sx, sy)
            }
            "scalex" => Transform::from_scale(args.first().copied().unwrap_or(1.0), 1.0),
            "scaley" => Transform::from_scale(1.0, args.first().copied().unwrap_or(1.0)),
            "rotate" => { let a = args.first().copied().unwrap_or(0.0).to_radians(); Transform { sx: a.cos(), kx: -a.sin(), ky: a.sin(), sy: a.cos(), tx: 0.0, ty: 0.0 } }
            "translate" => {
                let tx = args.first().copied().unwrap_or(0.0);
                let ty = args.get(1).copied().unwrap_or(0.0);
                Transform::from_translate(tx, ty)
            }
            "translatex" => Transform::from_translate(args.first().copied().unwrap_or(0.0), 0.0),
            "translatey" => Transform::from_translate(0.0, args.first().copied().unwrap_or(0.0)),
            "matrix" => {
                let g = |i: usize| args.get(i).copied().unwrap_or(0.0);
                Transform {
                    sx: g(0),
                    kx: g(2),
                    ky: g(1),
                    sy: g(3),
                    tx: g(4),
                    ty: g(5),
                }
            }
            "skew" | "skewx" | "skewy" => {
                let ang = args.first().copied().unwrap_or(0.0).to_radians();
                if name == "skewy" {
                    Transform { sx: 1.0, kx: 0.0, ky: ang.tan(), sy: 1.0, tx: 0.0, ty: 0.0 }
                } else if name == "skewx" {
                    Transform { sx: 1.0, kx: ang.tan(), ky: 0.0, sy: 1.0, tx: 0.0, ty: 0.0 }
                } else {
                    Transform { sx: 1.0, kx: ang.tan(), ky: 0.0, sy: 1.0, tx: 0.0, ty: 0.0 }
                }
            }
            _ => continue,
        };
        // 从左到右：m = m * one（CSS 语义：右边的先作用于点）
        m = m.post_concat(one);
        found = true;
    }
    found.then_some(m)
}

/// transform-origin 声明 → (ox, oy) 像素
fn parse_origin(decl: Option<&String>, w: f32, h: f32) -> (f32, f32) {
    let Some(d) = decl else { return (w / 2.0, h / 2.0) };
    let parts: Vec<&str> = d.split_whitespace().collect();
    let px = |t: &str, base: f32| -> f32 {
        if let Some(p) = t.strip_suffix('%') {
            base * p.parse::<f32>().unwrap_or(50.0) / 100.0
        } else if t == "center" {
            base / 2.0
        } else if t == "left" || t == "top" {
            0.0
        } else if t == "right" || t == "bottom" {
            base
        } else {
            t.trim_end_matches("px").parse::<f32>().unwrap_or(base / 2.0)
        }
    };
    match parts.len() {
        0 => (w / 2.0, h / 2.0),
        1 => (px(parts[0], w), h / 2.0),
        _ => (px(parts[0], w), px(parts[1], h)),
    }
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

    // transform：路径类绘制（背景/边框）套变换；transform-origin 以节点中心偏移
    let transform = n.decls.get("transform").and_then(|t| parse_transform(t, n.w, n.h));
    let tx = transform.map(|m| {
        let (ox, oy) = parse_origin(n.decls.get("transform-origin"), n.w, n.h);
        Transform::from_translate(n.x + ox, n.y + oy)
            .post_concat(m)
            .post_concat(Transform::from_translate(-(n.x + ox), -(n.y + oy)))
    });
    let ident = Transform::identity();
    let draw_tx = tx.as_ref().unwrap_or(&ident);

    // 背景图层（Lightning CSS 已把 background 简写展开为 background-image 等长属性）：
    // 多层背景逐层绘制，第一层最上；CSS 绘制顺序为背景色在所有图层之下
    if n.tag != "img" {
        match n.decls.get("background-image") {
            Some(bi) => draw_background_layers(pixmap, n, bi, false, draw_tx, opacity, radius),
            // 兜底：background 键（正常不会出现，简写已展开）；渐变已在背景色块处理，此处只画 url 图层
            None => {
                if let Some(bg) = n.decls.get("background") {
                    draw_background_layers(pixmap, n, bg, true, draw_tx, opacity, radius);
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

    // 背景色（CSS 顺序：颜色位于全部背景图层之下；纯色取 background-color，兜底 background 键）
    // 渐变按标准属于 background-image，此处兼容旧数据（background-color 直接写渐变）
    if let Some(bg) = n.decls.get("background").or_else(|| n.decls.get("background-color")) {
        match parse_bg(bg) {
            Some(Bg::Color(c)) => {
                let path = rounded_rect_path(n.x, n.y, n.w, n.h, radius);
                let mut p = Paint::default();
                p.set_color_rgba8(c[0], c[1], c[2], (c[3] as f32 * opacity) as u8);
                p.anti_alias = true;
                pixmap.fill_path(&path, &p, FillRule::Winding, *draw_tx, None);
            }
            Some(other @ (Bg::Linear { .. } | Bg::Radial { .. })) => {
                fill_gradient(pixmap, n, &other, draw_tx, opacity, radius);
            }
            _ => {}
        }
    }

    // 边框（四侧独立：width/color 逐侧回落解析；等宽同色走圆角 stroke，否则每侧梯形）
    draw_borders(pixmap, n, draw_tx, opacity, radius);

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
    // overflow: hidden/clip → 整棵子树画到临时 Pixmap（尺寸=节点矩形，圆角 mask 贴回）
    // 临时 Pixmap 上限 2048×2048（= 16MB RGBA）防内存；超限（如超宽节点）退回直接绘制
    let clipped = ["overflow", "overflow-x", "overflow-y"].iter().any(|k| {
        n.decls
            .get(*k)
            .map(|v| v.to_lowercase().split_whitespace().any(|w| w == "hidden" || w == "clip"))
            .unwrap_or(false)
    });
    let (tw, th) = (n.w.ceil().max(1.0), n.h.ceil().max(1.0));
    let fits = tw <= 2048.0 && th <= 2048.0 && tw * th <= 16.0 * 1024.0 * 1024.0 / 4.0;
    if clipped && fits && !n.children.is_empty() {
        if let Some(mut tmp) = Pixmap::new(tw as u32, th as u32) {
            tmp.fill(Color::from_rgba8(0, 0, 0, 0));
            // 画布原点 = 节点左上：子树坐标整体平移 -x/-y 后绘制
            // （&c 模式解出 &PaintNode 再 clone，避免 &&PaintNode 上 clone 出引用）
            for &c in &ordered {
                let mut cc = c.clone();
                shift_xy(&mut cc, -n.x, -n.y);
                draw_node(&mut tmp, &cc, fonts);
            }
            blit_r(pixmap, &tmp, n.x, n.y, n.w, n.h, (n.x, n.y, n.w, n.h), radius);
            return;
        }
    }
    for c in ordered {
        draw_node(pixmap, c, fonts);
    }
}

/// 子树坐标平移（临时画布原点对齐）
fn shift_xy(n: &mut PaintNode, dx: f32, dy: f32) {
    n.x += dx;
    n.y += dy;
    for c in &mut n.children {
        shift_xy(c, dx, dy);
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
            // 外阴影：偏移+扩散矩形，blur 用 5 层近似（pad 由 blur*0.5 → blur*0.1 均匀过渡）
            // 每层 alpha 基准 0.24（递减系数 0.5），5 层叠加总覆盖 ≈0.65，与旧 3 层 0.35 基准密度一致；
            // 无 blur 单层保持旧基准 0.35
            let layers = if blur > 0.5 { 5 } else { 1 };
            let base = if layers > 1 { 0.24f32 } else { 0.35 };
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
                let a = 1.0 - k * 0.5;
                p.set_color_rgba8(c[0], c[1], c[2], (c[3] as f32 * a * base) as u8);
                pixmap.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
            }
        }
    }
}


/// 边框绘制：等宽同色走圆角矩形 stroke（保留 border-radius 外观）；
/// 不等宽/异色走每侧梯形填充（外边=节点矩形、内边=内容矩形、四角 45° 斜切衔接）。
/// 注意：dashed/dotted/double 等线型先按 solid 绘制（TODO: 线型分段绘制）；
/// 不等宽梯形模式下边框形状暂不跟随 border-radius（角部为斜切直线）。
fn draw_borders(pixmap: &mut Pixmap, n: &PaintNode, draw_tx: &Transform, opacity: f32, radius: f32) {
    let (widths, colors) = border_sides(n);
    let [wt, wr, wb, wl] = widths;
    if wt <= 0.0 && wr <= 0.0 && wb <= 0.0 && wl <= 0.0 {
        return;
    }
    let same_color = colors.windows(2).all(|w| w[0] == w[1]);
    if wt == wr && wr == wb && wb == wl && same_color {
        let bw = wt;
        let c = mul_alpha(colors[0], opacity);
        let path = rounded_rect_path(n.x + bw / 2.0, n.y + bw / 2.0, (n.w - bw).max(0.0), (n.h - bw).max(0.0), radius);
        let mut p = Paint::default();
        p.set_color_rgba8(c[0], c[1], c[2], c[3]);
        p.anti_alias = true;
        let stroke = Stroke { width: bw, ..Stroke::default() };
        pixmap.stroke_path(&path, &p, &stroke, *draw_tx, None);
        return;
    }
    // 单侧宽度和超过盒尺寸时按 CSS 规则等比缩小
    let sw = if wl + wr > n.w && wl + wr > 0.0 { n.w / (wl + wr) } else { 1.0 };
    let sh = if wt + wb > n.h && wt + wb > 0.0 { n.h / (wt + wb) } else { 1.0 };
    let (wt, wr, wb, wl) = (wt * sh, wr * sw, wb * sh, wl * sw);
    let (x, y, w, h) = (n.x, n.y, n.w, n.h);
    // 四侧梯形（外角 → 内角斜切）：top / right / bottom / left
    let quads: [[(f32, f32); 4]; 4] = [
        [(x, y), (x + w, y), (x + w - wr, y + wt), (x + wl, y + wt)],
        [(x + w, y), (x + w, y + h), (x + w - wr, y + h - wb), (x + w - wr, y + wt)],
        [(x, y + h), (x + w, y + h), (x + w - wr, y + h - wb), (x + wl, y + h - wb)],
        [(x, y), (x, y + h), (x + wl, y + h - wb), (x + wl, y + wt)],
    ];
    let ws = [wt, wr, wb, wl];
    for (i, quad) in quads.iter().enumerate() {
        if ws[i] <= 0.0 {
            continue;
        }
        let c = mul_alpha(colors[i], opacity);
        let mut p = Paint::default();
        p.set_color_rgba8(c[0], c[1], c[2], c[3]);
        p.anti_alias = true;
        pixmap.fill_path(&quad_path(*quad), &p, FillRule::Winding, *draw_tx, None);
    }
}

/// 边框四侧解析 → ([宽 top,right,bottom,left], [色 top,right,bottom,left])
/// 回落链（每侧）：border-{side}-width/color/style > border-{side} 简写 > border-width/color/style（四值展开）> border 简写；
/// 颜色缺省 = currentColor（节点 color）；样式 none/hidden → 该侧不画；
/// 只声明样式未声明宽度 → CSS medium(3px)；只声明宽度未声明样式 → 按 solid 处理（兼容旧数据）
fn border_sides(n: &PaintNode) -> ([f32; 4], [[u8; 4]; 4]) {
    #[derive(PartialEq, Clone, Copy)]
    enum BStyle {
        None,
        Solid,
    }
    let bstyle = |t: &str| -> Option<BStyle> {
        match t.trim().to_lowercase().as_str() {
            "none" | "hidden" => Some(BStyle::None),
            // dashed/dotted/double 等先统一按 solid 绘制
            "solid" | "double" | "dashed" | "dotted" | "groove" | "ridge" | "inset" | "outset" => Some(BStyle::Solid),
            _ => None,
        }
    };
    let len = |t: &str| -> Option<f32> {
        let t = t.trim();
        match t.to_lowercase().as_str() {
            "thin" => return Some(1.0),
            "medium" => return Some(3.0),
            "thick" => return Some(5.0),
            _ => {}
        }
        t.trim_end_matches("px").trim().parse::<f32>().ok()
    };
    // 简写值 "1px solid rgb(0, 0, 0)" → (宽, 色, 线型)
    let short = |v: &str| -> (Option<f32>, Option<[u8; 4]>, Option<BStyle>) {
        let (mut w, mut c, mut st) = (None, None, None);
        for tok in split_ws_groups(v) {
            if st.is_none() {
                if let Some(s) = bstyle(&tok) {
                    st = Some(s);
                    continue;
                }
            }
            if c.is_none() {
                if let Some(col) = parse_color(&tok) {
                    c = Some(col);
                    continue;
                }
            }
            if w.is_none() {
                if let Some(l) = len(&tok) {
                    w = Some(l);
                }
            }
        }
        (w, c, st)
    };
    let d = |k: &str| n.decls.get(k).map(String::as_str);
    let bw4 = d("border-width").map(|v| expand4(&split_ws_groups(v).iter().filter_map(|t| len(t)).collect::<Vec<f32>>()));
    let bc4 = d("border-color").map(|v| expand4(&split_ws_groups(v).iter().filter_map(|t| parse_color(t)).collect::<Vec<[u8; 4]>>()));
    let bs4 = d("border-style").map(|v| expand4(&split_ws_groups(v).iter().filter_map(|t| bstyle(t)).collect::<Vec<BStyle>>()));
    let bshort = d("border").map(short);

    let mut widths = [0f32; 4];
    let mut colors = [[0u8, 0, 0, 255]; 4];
    for (i, side) in ["top", "right", "bottom", "left"].iter().enumerate() {
        let s_short = d(&format!("border-{side}")).map(short);
        let w = d(&format!("border-{side}-width"))
            .and_then(len)
            .or_else(|| s_short.as_ref().and_then(|s| s.0))
            .or_else(|| bw4.as_ref().and_then(|a| a[i]))
            .or_else(|| bshort.as_ref().and_then(|s| s.0));
        let c = d(&format!("border-{side}-color"))
            .and_then(parse_color)
            .or_else(|| s_short.as_ref().and_then(|s| s.1))
            .or_else(|| bc4.as_ref().and_then(|a| a[i]))
            .or_else(|| bshort.as_ref().and_then(|s| s.1))
            .unwrap_or_else(|| color_of(n)); // CSS: border-color 初始值 currentColor
        let st = d(&format!("border-{side}-style"))
            .and_then(bstyle)
            .or_else(|| s_short.as_ref().and_then(|s| s.2))
            .or_else(|| bs4.as_ref().and_then(|a| a[i]))
            .or_else(|| bshort.as_ref().and_then(|s| s.2));
        widths[i] = match (w, st) {
            (_, Some(BStyle::None)) => 0.0, // none/hidden → 该侧不画
            (Some(v), _) => v.max(0.0),
            (None, Some(_)) => 3.0, // 仅声明样式：CSS medium
            (None, None) => 0.0,
        };
        colors[i] = c;
    }
    (widths, colors)
}

/// 四值 CSS 简写展开（top/right/bottom/left；1/2/3 值按 CSS 规则补全）
fn expand4<T: Copy>(p: &[T]) -> [Option<T>; 4] {
    match p.len() {
        1 => [Some(p[0]); 4],
        2 => [Some(p[0]), Some(p[1]), Some(p[0]), Some(p[1])],
        3 => [Some(p[0]), Some(p[1]), Some(p[2]), Some(p[1])],
        4.. => [Some(p[0]), Some(p[1]), Some(p[2]), Some(p[3])],
        _ => [None, None, None, None],
    }
}

/// 按空白分割（括号内空格不切）："1px rgb(0, 0, 0)" → ["1px", "rgb(0, 0, 0)"]
fn split_ws_groups(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            c if c.is_whitespace() && depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// 四边形路径
fn quad_path(pts: [(f32, f32); 4]) -> Path {
    let mut pb = PathBuilder::new();
    pb.move_to(pts[0].0, pts[0].1);
    for p in &pts[1..] {
        pb.line_to(p.0, p.1);
    }
    pb.close();
    pb.finish().unwrap_or_else(|| PathBuilder::new().finish().unwrap())
}

/// alpha 预乘（opacity 等淡出系数）
fn mul_alpha(c: [u8; 4], k: f32) -> [u8; 4] {
    [c[0], c[1], c[2], (c[3] as f32 * k).clamp(0.0, 255.0) as u8]
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

    super::text::draw_with_letter_spacing(&buffer, fonts, color, n.letter_spacing, |gx, gy, gw, gh, color| {
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
    // 圆角半径钳到边长一半（防 clamp(min>max) panic：radius 超过边长/2 时）
    let r = if radius > 0.0 {
        radius.max(0.0).min(cw.max(0.0).min(ch.max(0.0)) / 2.0)
    } else {
        0.0
    };
    for py in y0..y1 {
        for px in x0..x1 {
            let sx = ((px as f32 - dx) / dw * iw).floor().clamp(0.0, iw - 1.0) as u32;
            let sy = ((py as f32 - dy) / dh * ih).floor().clamp(0.0, ih - 1.0) as u32;
            if r > 0.0 {
                // 圆角内判断：像素点到圆角矩形内切盒的钳制点距离
                let qx = (px as f32).clamp(cx + r, cx + cw - r);
                let qy = (py as f32).clamp(cy + r, cy + ch - r);
                let ddx = px as f32 - qx;
                let ddy = py as f32 - qy;
                if ddx * ddx + ddy * ddy > r * r {
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

/// 多层背景绘制：background-image: url(a), url(b) 逐层绘制（CSS 第一层最上，故从后往前画）。
/// size/position/repeat 按逗号分层与图层循环对应（层数不足时循环使用）。
/// images_only=true 时跳过渐变层（background 键兜底路径用：渐变已在背景色块处理）。
fn draw_background_layers(
    pixmap: &mut Pixmap,
    n: &PaintNode,
    decl: &str,
    images_only: bool,
    draw_tx: &Transform,
    opacity: f32,
    radius: f32,
) {
    let layers = split_commas(decl);
    if layers.is_empty() {
        return;
    }
    let d = |k: &str| n.decls.get(k).cloned().unwrap_or_default();
    let sizes = split_commas(&d("background-size"));
    let poss = split_commas(&d("background-position"));
    let reps = split_commas(&d("background-repeat"));
    let pick = |list: &[String], def: &str, i: usize| -> String {
        if list.is_empty() {
            def.to_string()
        } else {
            list[i % list.len()].clone()
        }
    };
    let base = crate::renderer::BASE_DIR.get().map(String::as_str).unwrap_or(".");
    for (i, ly) in layers.iter().enumerate().rev() {
        let layer = ly.trim();
        if layer.is_empty() || layer.eq_ignore_ascii_case("none") {
            continue;
        }
        match parse_bg(layer) {
            Some(Bg::Image(path)) => {
                if let Some(img) = crate::renderer::media::load(&path, base) {
                    draw_bg_layer(
                        pixmap,
                        n,
                        &img,
                        &pick(&sizes, "auto", i),
                        &pick(&poss, "0% 0%", i),
                        &pick(&reps, "repeat", i),
                        radius,
                    );
                }
            }
            Some(other @ (Bg::Linear { .. } | Bg::Radial { .. })) => {
                // 渐变层：铺满节点矩形（渐变的 size/position 暂不生效）
                if !images_only {
                    fill_gradient(pixmap, n, &other, draw_tx, opacity, radius);
                }
            }
            _ => {}
        }
    }
}

/// 渐变背景填充（linear/radial，铺满节点矩形，圆角裁剪，opacity 预乘到色标）
fn fill_gradient(pixmap: &mut Pixmap, n: &PaintNode, g: &Bg, draw_tx: &Transform, opacity: f32, radius: f32) {
    let fade = |stops: &[(f32, [u8; 4])]| -> Vec<(f32, [u8; 4])> {
        stops.iter().map(|(p, c)| (*p, mul_alpha(*c, opacity))).collect()
    };
    match g {
        Bg::Linear { deg, stops } => {
            if let Some(shader) = linear_shader(*deg, &fade(stops), n.w, n.h) {
                let path = rounded_rect_path(n.x, n.y, n.w, n.h, radius);
                let mut p = Paint::default();
                p.shader = shader;
                p.anti_alias = true;
                pixmap.fill_path(&path, &p, FillRule::Winding, *draw_tx, None);
            }
        }
        Bg::Radial { stops } => {
            if let (Some(fg), Some(lg)) = (stops.first(), stops.last()) {
                let center = Point::from_xy(n.x + n.w / 2.0, n.y + n.h / 2.0);
                let grad_r = n.w.min(n.h).max(1.0) / 2.0;
                if let Some(gd) = RadialGradient::new(
                    center,
                    Point::from_xy(center.x + 1.0, center.y),
                    grad_r,
                    grad_stops(&[(0.0, fg.1), (1.0, lg.1)]),
                    SpreadMode::Pad,
                    Transform::identity(),
                ) {
                    let path = rounded_rect_path(n.x, n.y, n.w, n.h, radius);
                    let mut p = Paint::default();
                    p.shader = gd;
                    p.anti_alias = true;
                    pixmap.fill_path(&path, &p, FillRule::Winding, *draw_tx, None);
                }
            }
        }
        _ => {}
    }
}

/// 单层背景图：size 缩放 → position 定位 → repeat 平铺，裁剪到节点矩形（圆角 mask）
fn draw_bg_layer(pixmap: &mut Pixmap, n: &PaintNode, img: &Pixmap, size_s: &str, pos_s: &str, rep_s: &str, radius: f32) {
    let (mut dw, mut dh) = (img.width() as f32, img.height() as f32);
    let s = size_s.trim();
    // contain（整图含入）/cover（覆盖裁剪）/像素/百分比（含 "100% auto" 双值、单值按比例）由 parse_size 处理
    if !s.is_empty() && s != "auto" && s != "auto auto" {
        let (w2, h2) = crate::renderer::media::parse_size(s, n.w, n.h, img.width(), img.height());
        if w2 > 0.0 && h2 > 0.0 {
            dw = w2;
            dh = h2;
        }
    }
    // 退化尺寸防护（避免平铺死循环）
    if dw < 0.5 || dh < 0.5 {
        return;
    }
    let (ox, oy) = parse_bg_pos(pos_s, n.w, n.h, dw, dh);
    let (tile_x, tile_y) = parse_repeat(rep_s);
    let (ax, ay) = (n.x + ox, n.y + oy);
    let (x1, y1) = (n.x + n.w, n.y + n.h);
    // repeat 平铺从锚点向两侧扩展（CSS 双向平铺；no-repeat 只画锚点一次）
    let kx0 = if tile_x { ((n.x - ax) / dw).floor() as i32 } else { 0 };
    let ky0 = if tile_y { ((n.y - ay) / dh).floor() as i32 } else { 0 };
    let mut ky = ky0;
    loop {
        let ty = ay + ky as f32 * dh;
        if ty >= y1 || (!tile_y && ky > ky0) {
            break;
        }
        let mut kx = kx0;
        loop {
            let tx = ax + kx as f32 * dw;
            if tx >= x1 || (!tile_x && kx > kx0) {
                break;
            }
            blit_r(pixmap, img, tx, ty, dw, dh, (n.x, n.y, n.w, n.h), radius);
            kx += 1;
        }
        ky += 1;
    }
}

/// 单分量 background-position → 像素偏移（百分比 = 剩余空间 (容器-图) × p：
/// "图片 p% 点对齐容器 p% 点" 的等价形式）
fn bg_pos_comp(t: &str, container: f32, img: f32) -> Option<f32> {
    match t {
        "left" | "top" => Some(0.0),
        "center" => Some((container - img) * 0.5),
        "right" | "bottom" => Some(container - img),
        _ => {
            if let Some(p) = t.strip_suffix('%') {
                p.trim().parse::<f32>().ok().map(|n| (container - img) * n / 100.0)
            } else {
                t.trim_end_matches("px").trim().parse::<f32>().ok()
            }
        }
    }
}

/// background-position 全形态解析 → (ox, oy) 像素偏移：
/// 关键字（top/center/bottom/left/right）、百分比（"50% 40%"）、像素（可负，精灵图）、
/// 混用（"left 40%"）、四值形态（"left 10px top 20px"）；单值时缺省轴 = center
fn parse_bg_pos(v: &str, cw: f32, ch: f32, iw: f32, ih: f32) -> (f32, f32) {
    let toks: Vec<String> = v.split_whitespace().map(|s| s.trim().to_lowercase()).collect();
    // 四值形态："left 10px top 20px" / "right 20% bottom 5px"（关键字+偏移成对，right/bottom 从对边内推）
    if toks.len() >= 4 {
        let pair = |kw: &str, off: &str, container: f32, img: f32, from_end: bool| -> f32 {
            let o = if let Some(p) = off.trim().strip_suffix('%') {
                p.trim().parse::<f32>().unwrap_or(0.0) / 100.0 * (container - img)
            } else {
                off.trim().trim_end_matches("px").trim().parse::<f32>().unwrap_or(0.0)
            };
            let base = match kw {
                "center" => (container - img) * 0.5,
                "right" | "bottom" => container - img,
                _ => 0.0,
            };
            if from_end { base - o } else { base + o }
        };
        let (mut x, mut y) = (0f32, 0f32);
        let mut i = 0;
        while i + 1 < toks.len() {
            match toks[i].as_str() {
                "left" => x = pair("left", &toks[i + 1], cw, iw, false),
                "right" => x = pair("right", &toks[i + 1], cw, iw, true),
                "top" => y = pair("top", &toks[i + 1], ch, ih, false),
                "bottom" => y = pair("bottom", &toks[i + 1], ch, ih, true),
                _ => {}
            }
            i += 2;
        }
        return (x, y);
    }
    // 1-2 值形态：关键字/百分比/像素混用；先出现的数值归 x，其次 y
    let (mut x, mut y) = (None, None);
    for t in &toks {
        match t.as_str() {
            "left" => x = Some(0.0),
            "right" => x = Some(cw - iw),
            "top" => y = Some(0.0),
            "bottom" => y = Some(ch - ih),
            "center" => {
                if x.is_none() {
                    x = Some((cw - iw) * 0.5);
                } else if y.is_none() {
                    y = Some((ch - ih) * 0.5);
                }
            }
            _ => {
                if x.is_none() {
                    if let Some(v) = bg_pos_comp(t, cw, iw) {
                        x = Some(v);
                        continue;
                    }
                }
                if y.is_none() {
                    if let Some(v) = bg_pos_comp(t, ch, ih) {
                        y = Some(v);
                    }
                }
            }
        }
    }
    match (x, y) {
        (Some(a), Some(b)) => (a, b),
        (Some(a), None) => (a, (ch - ih) * 0.5), // 单值：缺省轴 = center
        (None, Some(b)) => ((cw - iw) * 0.5, b),
        (None, None) => (0.0, 0.0),
    }
}

/// background-repeat → (平铺 x, 平铺 y)；支持 repeat-x / repeat-y / 双轴形式
fn parse_repeat(v: &str) -> (bool, bool) {
    let t = v.trim().to_lowercase();
    match t.as_str() {
        "repeat-x" => (true, false),
        "repeat-y" => (false, true),
        "no-repeat" => (false, false),
        "" => (true, true),
        _ => {
            let toks: Vec<&str> = t.split_whitespace().collect();
            let ax = |s: &str| !s.contains("no-repeat");
            match toks.len() {
                1 => (ax(toks[0]), ax(toks[0])),
                _ => (ax(toks[0]), ax(toks[1])),
            }
        }
    }
}
