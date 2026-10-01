//! 布局结果 → tiny-skia 光栅化：纯几何绘制层（无 CSS 字符串解析）
//!
//! 所有绘制参数（颜色/渐变/阴影/边框/圆角/变换/背景 size-position-repeat/
//! 文本属性/透明度）均来自结构化样式 [`super::style::Resolved`]：绘制入口对每个
//! 结构化样式由 layout 侧 `style::resolve` 产出并回填到 `PaintNode.style`，
//! 本模块只做几何计算与光栅化（路径构造 / 渐变 shader / 阴影分层近似 /
//! 平铺循环 / 像素混合）。
//!
//! 绘制顺序（CSS 标准）：box-shadow（最底）→ background-color → background
//! layers（第一层最上）→ <img> 内容 → border → 文本 → 子节点（z-index 排序）。
//!
//! 与 style.rs 的结构化契约：
//! - `transform: Option<[f32; 6]>` 为 tiny-skia `Transform { sx, kx, ky, sy, tx, ty }`
//!   字段序；CSS `matrix(a,b,c,d,e,f)` 需折算为 `[a, c, b, d, e, f]`
//! - `transform_origin: (f32, f32)` 分量 0..=1 视为相对节点宽/高的比例
//!   （默认 0.5/0.5 = 中心）；>1 或 <0 视为 px 偏移
//! - `BgPaint::Url` 内为裸 URL/路径（url() 包装与 base_dir 解析由 style.rs 侧完成）
//! - `BgLayer.paint` 渐变 stops 的位置已归一到 0..1（无位置色标均匀分布）
//! - border 的 style none/hidden 折叠为 width 0；线型统一按 solid 绘制
//!
//! 唯一保留的字符串解析是 [`parse_color`]：layout.rs 文本测量侧依赖（非绘制路径）。

use super::layout::PaintNode;
use super::style::{
    BgLayer, BgPaint, BgPosComp, BgRepeat, BgSize, CornerRadiusP, LengthOrPct, LineH, Resolved,
    Rgba, ShadowP,
};
use super::text::TextAlign;
use taffy::style::Overflow;
use tiny_skia::*;

/// CSS 颜色解析：#rgb/#rrggbb/#rrggbbaa/rgb()/rgba()/命名色子集
/// （供 layout.rs 文本测量回退使用；绘制层颜色一律来自 Resolved 结构化字段）
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
        ("violet", [238, 130, 238, 255]),
        ("brown", [165, 42, 42, 255]),
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

/// 圆角矩形路径：四角独立半径 [tl, tr, br, bl]（顺时针），每角一段三次贝塞尔近似圆弧
fn rounded_rect_path4(x: f32, y: f32, w: f32, h: f32, radii: [f32; 4]) -> Path {
    // 半径非负且不超过边长一半（防相邻圆角交叠 / 退化路径）
    let cap = (w.max(0.0).min(h.max(0.0)) / 2.0).max(0.0);
    let [tl, tr, br, bl] = radii.map(|v| v.max(0.0).min(cap));
    if tl <= 0.01 && tr <= 0.01 && br <= 0.01 && bl <= 0.01 {
        return rect_path(x, y, w, h);
    }
    const K: f32 = 0.55; // 圆弧贝塞尔近似系数
    let (x1, y1) = (x + w, y + h);
    let mut pb = PathBuilder::new();
    pb.move_to(x + tl, y);
    pb.line_to(x1 - tr, y); // 顶边
    if tr > 0.01 {
        pb.cubic_to(x1 - tr * K, y, x1, y + tr * K, x1, y + tr);
    }
    pb.line_to(x1, y1 - br); // 右边
    if br > 0.01 {
        pb.cubic_to(x1, y1 - br * K, x1 - br * K, y1, x1 - br, y1);
    }
    pb.line_to(x + bl, y1); // 底边
    if bl > 0.01 {
        pb.cubic_to(x + bl * K, y1, x, y1 - bl * K, x, y1 - bl);
    }
    pb.line_to(x, y + tl); // 左边
    if tl > 0.01 {
        pb.cubic_to(x, y + tl * K, x + tl * K, y, x + tl, y);
    }
    pb.close();
    pb.finish().unwrap_or_else(|| rect_path(x, y, w, h))
}

/// CornerRadiusP → [tl, tr, br, bl]（顺时针）
fn corner_radii(rad: &CornerRadiusP) -> [f32; 4] {
    [rad.top_left, rad.top_right, rad.bottom_right, rad.bottom_left]
}

/// 渐变 shader（角度：0=向上，顺时针；CSS 语义）
fn linear_shader(deg: f32, stops: &[(f32, Rgba)], w: f32, h: f32) -> Option<Shader> {
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

fn grad_stops(stops: &[(f32, Rgba)]) -> Vec<GradientStop> {
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

/// 单节点绘制：结构化样式（layout 侧回填到 PaintNode.style）→ 纯几何绘制
fn draw_node(pixmap: &mut Pixmap, n: &PaintNode, fonts: &mut super::text::TextEngine) {
    let r = &n.style;
    draw_node_r(pixmap, n, &r, fonts);
}

/// 绘制顺序（CSS）：box-shadow（最底）→ background-color → background layers
/// （第一层最上）→ <img> 内容 → border → 文本 → 子节点（z-index 稳定排序）
fn draw_node_r(pixmap: &mut Pixmap, n: &PaintNode, r: &Resolved, fonts: &mut super::text::TextEngine) {
    if n.w <= 0.0 || n.h <= 0.0 {
        return;
    }
    let radii = corner_radii(&r.radius);
    let opacity = r.opacity.clamp(0.0, 1.0);

    // 1. box-shadow（多重分层近似；先于内容绘制）
    if !r.box_shadows.is_empty() {
        draw_box_shadows(pixmap, n, &r.box_shadows, radii, opacity);
    }

    // transform（矩阵 [sx, kx, ky, sy, tx, ty]，tiny-skia 字段序）：
    // 路径类绘制（背景/边框）套变换；origin 以节点尺寸定位
    let transform = r.transform
        .map(|m| Transform { sx: m[0], kx: m[1], ky: m[2], sy: m[3], tx: m[4], ty: m[5] });
    let tx = transform.map(|m| {
        let (ox, oy) = (
            origin_px(r.transform_origin.0, n.w),
            origin_px(r.transform_origin.1, n.h),
        );
        Transform::from_translate(n.x + ox, n.y + oy)
            .post_concat(m)
            .post_concat(Transform::from_translate(-(n.x + ox), -(n.y + oy)))
    });
    let ident = Transform::identity();
    let draw_tx = tx.as_ref().unwrap_or(&ident);

    // 2. 背景色（CSS：位于全部背景图层之下）
    if let Some(c) = r.bg_color {
        fill_solid(pixmap, n, mul_alpha(c, opacity), radii, draw_tx);
    }

    // 3. 背景图层（第一层最上 → 逆序绘制）；<img> 节点不画背景图（图即内容）
    if n.tag != "img" {
        draw_bg_layers(pixmap, n, &r.bg_layers, draw_tx, opacity, radii);
    }

    // <img> 内容：按节点矩形绘制 src 图
    if n.tag == "img" {
        if let Some(src) = &n.src {
            if let Some(img) = crate::renderer::media::load(src, crate::renderer::BASE_DIR.get().map(String::as_str).unwrap_or(".")) {
                blit_r(pixmap, &img, n.x, n.y, n.w, n.h, (n.x, n.y, n.w, n.h), radii);
            }
        }
    }

    // 4. 边框（等宽同色走圆角 stroke，否则每侧梯形）
    draw_borders(pixmap, n, r, draw_tx, opacity, radii);

    // 5. 文本（仅叶子画：容器的文本已作为附加叶子单独布局，这里再画会重复）
    if !n.text.is_empty() && n.children.is_empty() {
        draw_text(pixmap, n, r, opacity, fonts);
    }

    // 6. 子节点：z-index 稳定排序（负值在下、正值在上，同值保持 DOM 序）
    let mut ordered: Vec<(&PaintNode, Resolved)> = n
        .children
        .iter()
        .map(|c| (c, &c.style))
        .collect();
    ordered.sort_by_key(|(_, cr)| cr.z_index);

    // overflow: hidden/clip/scroll → 整棵子树画到临时 Pixmap（尺寸=节点矩形，圆角 mask 贴回）
    // 临时 Pixmap 上限 2048×2048（= 16MB RGBA）防内存；超限（如超宽节点）退回直接绘制
    let (tw, th) = (n.w.ceil().max(1.0), n.h.ceil().max(1.0));
    let fits = tw <= 2048.0 && th <= 2048.0 && tw * th <= 16.0 * 1024.0 * 1024.0 / 4.0;
    if is_overflow_clip(r) && fits && !ordered.is_empty() {
        if let Some(mut tmp) = Pixmap::new(tw as u32, th as u32) {
            tmp.fill(Color::from_rgba8(0, 0, 0, 0));
            // 画布原点 = 节点左上：子树坐标整体平移 -x/-y 后绘制
            // （*c 模式解出 &PaintNode 再 clone，避免 &&PaintNode 上 clone 出引用）
            for (c, _) in ordered.iter() {
                let mut cc = (*c).clone();
                shift_xy(&mut cc, -n.x, -n.y);
                draw_node(&mut tmp, &cc, fonts);
            }
            blit_r(pixmap, &tmp, n.x, n.y, n.w, n.h, (n.x, n.y, n.w, n.h), radii);
            return;
        }
    }
    for (c, cr) in ordered {
        draw_node_r(pixmap, c, &cr, fonts);
    }
}

/// 纯色填充节点矩形（四角圆角；opacity 已预乘进颜色）
fn fill_solid(pixmap: &mut Pixmap, n: &PaintNode, c: Rgba, radii: [f32; 4], draw_tx: &Transform) {
    let path = rounded_rect_path4(n.x, n.y, n.w, n.h, radii);
    let mut p = Paint::default();
    p.set_color_rgba8(c[0], c[1], c[2], c[3]);
    p.anti_alias = true;
    pixmap.fill_path(&path, &p, FillRule::Winding, *draw_tx, None);
}

/// overflow 裁剪判定：任一轴 hidden/clip/scroll 即裁剪
/// （scroll 的滚动位移未实现，滚动内容按可视区裁剪近似）
fn is_overflow_clip(r: &Resolved) -> bool {
    let clip = |o: Overflow| matches!(o, Overflow::Hidden | Overflow::Clip | Overflow::Scroll);
    clip(r.overflow.x) || clip(r.overflow.y)
}

/// transform-origin 分量 → 像素：0..=1 视为节点尺寸比例（默认 0.5 = 中心），
/// >1 或 <0 视为 px 偏移（兼容 style.rs 直接透传 px 的形态）
fn origin_px(v: f32, base: f32) -> f32 {
    if (0.0..=1.0).contains(&v) {
        v * base
    } else {
        v
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

/// box-shadow 多层近似绘制（输入结构化 ShadowP）
fn draw_box_shadows(pixmap: &mut Pixmap, n: &PaintNode, shadows: &[ShadowP], radii: [f32; 4], opacity: f32) {
    for s in shadows {
        let c = mul_alpha(s.color, opacity);
        let mut p = Paint::default();
        p.anti_alias = true;
        if s.inset {
            // inset：内部描边近似（沿内边界 stroke 两圈，alpha 递减）
            let bw = (s.blur.max(1.0) * 0.6 + s.spread).max(1.0);
            for (i, a) in [0.35f32, 0.2].iter().enumerate() {
                let off = bw * (i as f32 + 1.0) / 3.0;
                let r4 = radii.map(|v| (v - off).max(0.0));
                let path = rounded_rect_path4(
                    n.x + off,
                    n.y + off,
                    (n.w - off * 2.0).max(0.0),
                    (n.h - off * 2.0).max(0.0),
                    r4,
                );
                p.set_color_rgba8(c[0], c[1], c[2], (c[3] as f32 * a) as u8);
                let stroke = Stroke { width: bw / 2.0, ..Stroke::default() };
                pixmap.stroke_path(&path, &p, &stroke, Transform::identity(), None);
            }
        } else {
            // 外阴影：偏移+扩散矩形，blur 用 5 层近似（pad 由 blur*0.5 → blur*0.1 均匀过渡）
            // 每层 alpha 基准 0.24（递减系数 0.5），5 层叠加总覆盖 ≈0.65，与旧 3 层 0.35 基准密度一致；
            // 无 blur 单层保持基准 0.35
            let layers = if s.blur > 0.5 { 5 } else { 1 };
            let base = if layers > 1 { 0.24f32 } else { 0.35 };
            for i in 0..layers {
                let k = i as f32 / layers as f32;
                let pad = s.spread + s.blur * (1.0 - k) * 0.5;
                let r4 = radii.map(|v| v + pad);
                let path = rounded_rect_path4(
                    n.x + s.x - pad,
                    n.y + s.y - pad,
                    n.w + pad * 2.0,
                    n.h + pad * 2.0,
                    r4,
                );
                let a = 1.0 - k * 0.5;
                p.set_color_rgba8(c[0], c[1], c[2], (c[3] as f32 * a * base) as u8);
                pixmap.fill_path(&path, &p, FillRule::Winding, Transform::identity(), None);
            }
        }
    }
}

/// 边框绘制：等宽同色走圆角矩形 stroke（保留 border-radius 外观，四角半径随中线内缩）；
/// 不等宽/异色走每侧梯形填充（外边=节点矩形、内边=内容矩形、四角 45° 斜切衔接）。
/// 输入为结构化 r.border 四侧（style.rs 已折叠简写回落链：per-side > border-width/color/style > border；
/// style none/hidden → width 0；仅声明 style 未声明宽度 → medium(3px)；线型统一按 solid）。
/// 注意：dashed/dotted/double 等线型分段绘制 TODO；
/// 不等宽梯形模式下边框形状暂不跟随 border-radius（角部为斜切直线）。
fn draw_borders(pixmap: &mut Pixmap, n: &PaintNode, r: &Resolved, draw_tx: &Transform, opacity: f32, radii: [f32; 4]) {
    let b = &r.border;
    let widths = [b.top.width, b.right.width, b.bottom.width, b.left.width];
    let colors = [b.top.color, b.right.color, b.bottom.color, b.left.color];
    let [wt, wr, wb, wl] = widths;
    if wt <= 0.0 && wr <= 0.0 && wb <= 0.0 && wl <= 0.0 {
        return;
    }
    let same_color = colors.windows(2).all(|w| w[0] == w[1]);
    if wt == wr && wr == wb && wb == wl && same_color {
        let bw = wt;
        let c = mul_alpha(colors[0], opacity);
        // stroke 沿中线走：矩形内缩 bw/2，四角半径同步内缩
        let inner = radii.map(|v| (v - bw / 2.0).max(0.0));
        let path = rounded_rect_path4(n.x + bw / 2.0, n.y + bw / 2.0, (n.w - bw).max(0.0), (n.h - bw).max(0.0), inner);
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

/// 文本绘制：字形来自 cosmic-text 排版，属性（字号/字重/颜色/行高/字族/字距/
/// 对齐/缩进/阴影/透明度）全部来自结构化样式。
/// 对齐用 PaintNode.text_align：layout 侧已按 CSS 继承链解析（decls 级 resolve
/// 无父上下文，无法表达 text-align 继承），非 decls 字符串读取。
fn draw_text(pixmap: &mut Pixmap, n: &PaintNode, r: &Resolved, opacity: f32, fonts: &mut super::text::TextEngine) {
    let lh = line_height_px(r);
    let family = r.font_family.as_deref();
    let main_color = mul_alpha(r.color, opacity);

    // text-shadow（全部层，先于正文；CSS 中阴影绘制在文字之下）
    for s in &r.text_shadows {
        let sc = mul_alpha(s.color, opacity);
        let buffer_s = super::text::layout_buffer(
            fonts,
            &n.text,
            r.font_size,
            r.font_weight,
            sc,
            Some(n.w),
            lh,
            n.text_align,
            family,
        );
        let bx = (n.x + s.x).round() as i32;
        let by = (n.y + s.y).round() as i32;
        super::text::draw_with_letter_spacing(&buffer_s, fonts, cosmic_text_color(sc), r.letter_spacing, |gx, gy, gw, gh, col| {
            let px = bx + gx;
            let py = by + gy;
            for yy in 0..gh {
                for xx in 0..gw {
                    blend_glyph(pixmap, px + xx as i32, py + yy as i32, col);
                }
            }
        });
    }

    let buffer = super::text::layout_buffer(
        fonts,
        &n.text,
        r.font_size,
        r.font_weight,
        main_color,
        Some(n.w),
        lh,
        n.text_align,
        family,
    );
    // 水平对齐偏移：逐行用本行 line_w（用整段最宽行会让短行错位）；
    // 绘制走全局 buffer.draw（LayoutRun 无 draw 方法），以最宽行近似
    let runs_info: Vec<(f32, f32)> = buffer
        .layout_runs()
        .map(|run| (run.line_w, run.line_top))
        .collect();
    let max_w = runs_info.iter().map(|(w, _)| *w).fold(0f32, f32::max);
    let align_off = |lw: f32| -> f32 {
        match n.text_align {
            TextAlign::Center => ((n.w - lw) / 2.0).max(0.0),
            TextAlign::Right => (n.w - lw).max(0.0),
            TextAlign::Left => 0.0,
        }
    };
    // text-indent：首行缩进（多行段落的首行定位需逐行偏移，暂在单行场景生效）
    let mut dx = align_off(max_w);
    if runs_info.len() <= 1 {
        dx += r.text_indent;
    }
    let base_x = (n.x + dx).round() as i32;
    let base_y = n.y.round() as i32;

    super::text::draw_with_letter_spacing(&buffer, fonts, cosmic_text_color(main_color), r.letter_spacing, |gx, gy, gw, gh, color| {
        // 回调给的是设备像素矩形 + 颜色（alpha 已混合）
        let px = base_x + gx;
        let py = base_y + gy;
        for yy in 0..gh {
            for xx in 0..gw {
                blend_glyph(pixmap, px + xx as i32, py + yy as i32, color);
            }
        }
    });
}

/// 行高 px：LineH::Px 直取；Mult × font-size；未声明 = font-size × 1.5
fn line_height_px(r: &Resolved) -> f32 {
    match r.line_height {
        Some(LineH::Px(v)) => v,
        Some(LineH::Mult(m)) => r.font_size * m,
        None => r.font_size * 1.5,
    }
}

/// glyph 像素写入（premul src-over，越界忽略）——正文与 text-shadow 共用
fn blend_glyph(pixmap: &mut Pixmap, x: i32, y: i32, c: cosmic_text::Color) {
    if x < 0 || y < 0 {
        return;
    }
    let (x, y) = (x as u32, y as u32);
    if x >= pixmap.width() || y >= pixmap.height() {
        return;
    }
    let sa = c.a() as u32;
    if sa == 0 {
        return;
    }
    let data = pixmap.data_mut();
    let di = ((y * pixmap.width() + x) * 4) as usize;
    // 源色预乘后与目标（premul）做 src-over：out = src + dst × (1 - sa)
    let sr = (c.r() as u32 * sa + 127) / 255;
    let sg = (c.g() as u32 * sa + 127) / 255;
    let sb = (c.b() as u32 * sa + 127) / 255;
    let dr = data[di] as u32;
    let dg = data[di + 1] as u32;
    let db = data[di + 2] as u32;
    let da = data[di + 3] as u32;
    data[di] = (sr + dr * (255 - sa) / 255) as u8;
    data[di + 1] = (sg + dg * (255 - sa) / 255) as u8;
    data[di + 2] = (sb + db * (255 - sa) / 255) as u8;
    data[di + 3] = (sa + da * (255 - sa) / 255) as u8;
}

fn cosmic_text_color(c: [u8; 4]) -> cosmic_text::Color {
    cosmic_text::Color::rgba(c[0], c[1], c[2], c[3])
}

/// 带四角圆角裁剪的 blit（图片/背景图不溢出 border-radius；nearest 采样 + 目标区域裁剪）
fn blit_r(pixmap: &mut Pixmap, img: &Pixmap, dx: f32, dy: f32, dw: f32, dh: f32, clip: (f32, f32, f32, f32), radii: [f32; 4]) {
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
    // 圆角半径钳到边长一半（防半径超过边长导致四角区域交叠）
    let cap = (cw.max(0.0).min(ch.max(0.0)) / 2.0).max(0.0);
    let [rtl, rtr, rbr, rbl] = radii.map(|v| v.max(0.0).min(cap));
    for py in y0..y1 {
        for px in x0..x1 {
            let (fx, fy) = (px as f32, py as f32);
            // 四角圆角内判断：落入某角象限用该角半径，像素到该角圆心的距离超界则剔除
            let corner = if fx < cx + rtl && fy < cy + rtl {
                Some((rtl, cx + rtl, cy + rtl))
            } else if fx > cx + cw - rtr && fy < cy + rtr {
                Some((rtr, cx + cw - rtr, cy + rtr))
            } else if fx > cx + cw - rbr && fy > cy + ch - rbr {
                Some((rbr, cx + cw - rbr, cy + ch - rbr))
            } else if fx < cx + rbl && fy > cy + ch - rbl {
                Some((rbl, cx + rbl, cy + ch - rbl))
            } else {
                None
            };
            if let Some((rr, qx, qy)) = corner {
                if rr > 0.0 {
                    let ddx = fx - qx;
                    let ddy = fy - qy;
                    if ddx * ddx + ddy * ddy > rr * rr {
                        continue;
                    }
                }
            }
            let sx = ((fx - dx) / dw * iw).floor().clamp(0.0, iw - 1.0) as u32;
            let sy = ((fy - dy) / dh * ih).floor().clamp(0.0, ih - 1.0) as u32;
            let c = match img.pixel(sx, sy) {
                Some(c) => c,
                None => continue,
            };
            if c.alpha() == 0 {
                continue;
            }
            // 负坐标经 u32 回绕后被越界检查拦下（与旧实现一致）
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

/// 多层背景：CSS 第一层最上 → 逆序绘制。
/// 渐变/纯色层铺满节点矩形（圆角裁剪，size/position 不作用于渐变）；
/// url 层走 size → position → repeat 几何。
fn draw_bg_layers(
    pixmap: &mut Pixmap,
    n: &PaintNode,
    layers: &[BgLayer],
    draw_tx: &Transform,
    opacity: f32,
    radii: [f32; 4],
) {
    let base = crate::renderer::BASE_DIR.get().map(String::as_str).unwrap_or(".");
    for layer in layers.iter().rev() {
        match &layer.paint {
            BgPaint::Color(c) => fill_solid(pixmap, n, mul_alpha(*c, opacity), radii, draw_tx),
            BgPaint::Linear { .. } | BgPaint::Radial { .. } => {
                fill_gradient(pixmap, n, &layer.paint, draw_tx, opacity, radii);
            }
            BgPaint::Url(path) => {
                if let Some(img) = crate::renderer::media::load(path, base) {
                    draw_bg_layer(pixmap, n, &img, layer, radii);
                }
            }
        }
    }
}

/// 渐变背景填充（linear/radial，铺满节点矩形，圆角裁剪，opacity 预乘到色标）
fn fill_gradient(pixmap: &mut Pixmap, n: &PaintNode, g: &BgPaint, draw_tx: &Transform, opacity: f32, radii: [f32; 4]) {
    let fade = |stops: &[(f32, Rgba)]| -> Vec<(f32, Rgba)> {
        stops.iter().map(|(p, c)| (*p, mul_alpha(*c, opacity))).collect()
    };
    match g {
        BgPaint::Linear { deg, stops } => {
            if let Some(shader) = linear_shader(*deg, &fade(stops), n.w, n.h) {
                let path = rounded_rect_path4(n.x, n.y, n.w, n.h, radii);
                let mut p = Paint::default();
                p.shader = shader;
                p.anti_alias = true;
                pixmap.fill_path(&path, &p, FillRule::Winding, *draw_tx, None);
            }
        }
        BgPaint::Radial { stops } => {
            if stops.len() >= 2 {
                let center = Point::from_xy(n.x + n.w / 2.0, n.y + n.h / 2.0);
                let grad_r = n.w.min(n.h).max(1.0) / 2.0;
                if let Some(gd) = RadialGradient::new(
                    center,
                    Point::from_xy(center.x + 1.0, center.y),
                    grad_r,
                    grad_stops(&fade(stops)),
                    SpreadMode::Pad,
                    Transform::identity(),
                ) {
                    let path = rounded_rect_path4(n.x, n.y, n.w, n.h, radii);
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

/// LengthOrPct → 像素（百分比相对基准 = background 定位区宽/高）
fn len_or_pct(v: LengthOrPct, base: f32) -> f32 {
    match v {
        LengthOrPct::Px(v) => v,
        LengthOrPct::Pct(p) => base * p / 100.0,
    }
}

/// background-position 单分量 → 像素偏移（百分比 = 剩余空间 (容器-图) × p：
/// "图片 p% 点对齐容器 p% 点" 的等价形式）
fn pos_comp_px(c: BgPosComp, container: f32, img: f32) -> f32 {
    match c {
        BgPosComp::Px(v) => v,
        BgPosComp::Pct(p) => (container - img) * p / 100.0,
        BgPosComp::Left | BgPosComp::Top => 0.0,
        BgPosComp::Center => (container - img) * 0.5,
        BgPosComp::Right | BgPosComp::Bottom => container - img,
    }
}

/// 单层背景图：size 缩放（contain/cover/px/%）→ position 定位 → repeat 平铺，
/// 裁剪到节点矩形（四角圆角 mask）。
/// 注意：BgSize::Val 单值形态的等比缩放（"200px" → h 按图源比例）需 style.rs
/// 侧折算（绘制侧无图源尺寸，Val 两轴按字面解析）。
fn draw_bg_layer(pixmap: &mut Pixmap, n: &PaintNode, img: &Pixmap, layer: &BgLayer, radii: [f32; 4]) {
    let (mut dw, mut dh) = (img.width() as f32, img.height() as f32);
    match layer.size {
        BgSize::Auto => {}
        BgSize::Contain => {
            // 整图含入：取较小缩放系数
            let k = (n.w / dw).min(n.h / dh);
            if k > 0.0 && k.is_finite() {
                dw *= k;
                dh *= k;
            }
        }
        BgSize::Cover => {
            // 覆盖裁剪：取较大缩放系数
            let k = (n.w / dw).max(n.h / dh);
            if k > 0.0 && k.is_finite() {
                dw *= k;
                dh *= k;
            }
        }
        BgSize::Val { w, h } => {
            let (w2, h2) = (len_or_pct(w, n.w), len_or_pct(h, n.h));
            if w2 > 0.0 && h2 > 0.0 {
                dw = w2;
                dh = h2;
            }
        }
    }
    // 退化尺寸防护（避免平铺死循环）
    if dw < 0.5 || dh < 0.5 {
        return;
    }
    let (ox, oy) = (
        pos_comp_px(layer.position.0, n.w, dw),
        pos_comp_px(layer.position.1, n.h, dh),
    );
    let (tile_x, tile_y) = match layer.repeat {
        BgRepeat::Repeat => (true, true),
        BgRepeat::RepeatX => (true, false),
        BgRepeat::RepeatY => (false, true),
        BgRepeat::NoRepeat => (false, false),
    };
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
            blit_r(pixmap, img, tx, ty, dw, dh, (n.x, n.y, n.w, n.h), radii);
            kx += 1;
        }
        ky += 1;
    }
}
