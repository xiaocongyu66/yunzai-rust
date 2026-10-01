//! StyleNode → taffy 布局树：样式映射 + 文本测量 + 布局求解

use super::css::split_commas;
use super::dom::StyleNode;
use super::style::Resolved;
use super::text::{TextAlign, TextEngine};
use std::collections::BTreeMap;
use taffy::prelude::*;
use taffy::util::MaybeResolve;

/// 布局完成的绘制节点（绝对坐标 + 原始声明 + 结构化绘制样式）
#[derive(Clone)]
pub struct PaintNode {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub decls: BTreeMap<String, String>,
    pub text: String,
    pub text_align: TextAlign,
    pub children: Vec<PaintNode>,
    pub src: Option<String>,
    pub tag: String,
    /// letter-spacing（px）：绘制回调逐 glyph 加累计 x 偏移 i * letter_spacing
    /// （见 text::draw_with_letter_spacing）
    pub letter_spacing: f32,
    /// 结构化绘制样式（style::Resolved，背景/阴影/边框/圆角/变换/字体文本）：
    /// paint 层直接消费，不再解析 decls 字符串
    pub style: Resolved,
}

/// taffy 节点上下文（所有节点）：样式声明 + 文本信息
#[derive(Clone)]
pub struct NodeCtx {
    pub decls: BTreeMap<String, String>,
    pub text: String,
    pub font_size: f32,
    pub weight: u16,
    pub color: [u8; 4],
    pub line_height: f32,
    pub align: TextAlign,
    pub src: Option<String>,
    pub family: Option<String>,
    pub tag: String,
    /// letter-spacing（px）
    pub letter_spacing: f32,
    /// ellipsis 截断的定宽约束（measure 间传递：min-content 等无定宽 pass 复用）
    pub ellip_w: Option<f32>,
    /// 结构化绘制样式（style::resolve 产出，回填到 PaintNode.style）
    pub resolved: Resolved,
}

pub struct Tree {
    pub taffy: TaffyTree<NodeCtx>,
    pub root: taffy::NodeId,
}

/// 解析长度：px / % / auto → taffy Dimension
pub fn dim(v: &str, basis: f32) -> Dimension {
    let v = v.trim();
    if v == "auto" || v.is_empty() {
        return Dimension::Auto;
    }
    // calc(百分比 ± 像素)：解析百分比主体与像素偏移
    if v.starts_with("calc(") && v.ends_with(')') {
        if let Some((pct, px)) = parse_calc_pct_px(&v[5..v.len() - 1]) {
            let base = basis * pct;
            return length(base + px);
        }
        return Dimension::Auto;
    }
    if let Some(p) = v.strip_suffix('%') {
        return p.trim().parse::<f32>().map(|n| Dimension::Percent(n / 100.0)).unwrap_or(Dimension::Auto);
    }
    let n = v.trim().trim_end_matches("px").trim();
    if let Ok(f) = n.parse::<f32>() {
        return length(f);
    }
    Dimension::Auto
}

/// calc 表达式 → (百分比 0..1, 像素偏移)。支持 "100% - 400px" / "50% + 20px" 等二元形态
pub fn parse_calc_pct_px(expr: &str) -> Option<(f32, f32)> {
    let mut pct = 0.0f32;
    let mut px = 0.0f32;
    let mut matched = false;
    for tok in expr.split_whitespace() {
        let (neg, t) = match tok {
            "-" => continue,
            "+" => continue,
            t if t.starts_with('-') => (true, &t[1..]),
            t => (false, t),
        };
        if let Some(p) = t.strip_suffix('%') {
            let v = p.parse::<f32>().ok()? / 100.0;
            pct = if neg { -v } else { v };
            matched = true;
        } else if let Some(v) = t.trim_end_matches("px").parse::<f32>().ok() {
            px += if neg { -v } else { v };
            matched = true;
        }
    }
    matched.then_some((pct, px))
}

fn lpa(v: &str, basis: f32) -> LengthPercentageAuto {
    let v = v.trim();
    if v == "auto" || v.is_empty() {
        return LengthPercentageAuto::Auto;
    }
    match lp(v, basis) {
        LengthPercentage::Length(l) => LengthPercentageAuto::Length(l),
        LengthPercentage::Percent(p) => LengthPercentageAuto::Percent(p),
    }
}

fn lp(v: &str, basis: f32) -> LengthPercentage {
    match dim(v, basis) {
        Dimension::Length(l) => LengthPercentage::Length(l),
        Dimension::Percent(p) => LengthPercentage::Percent(p),
        _ => LengthPercentage::Length(0.0),
    }
}

fn style_of(n: &StyleNode, width: f32) -> (Style, Resolved) {
    // 全量映射走 style::resolve（Lightning CSS 结构化），此处只补充 taffy 特有字段；
    // Resolved 原样透出（paint 层消费其绘制字段）
    let d = |k: &str| n.decl(k).map(String::from);
    let r = super::style::resolve(n, width);
    let display_decl = d("display");
    // display 决策：CSS 显式声明优先；未声明时按标签默认（dom::tag_inline → inline，其余 block）
    let inline_like = match display_decl.as_deref() {
        Some(v) => matches!(
            v.trim(),
            "inline" | "inline-block" | "inline-flex" | "inline-table" | "run-in"
        ),
        None => super::dom::tag_inline(&n.tag),
    };
    // table 现有近似优先：CSS 声明的 table-row/table-cell 走原逻辑；裸 table 系标签
    // 同样豁免块流改写（保持既有横排 flex 近似不被破坏）
    let table_decl = display_decl.as_deref().map(str::trim).unwrap_or("");
    let table_native = display_decl.is_none()
        && matches!(
            n.tag.as_str(),
            "table" | "thead" | "tbody" | "tfoot" | "caption" | "colgroup" | "col" | "tr" | "td" | "th"
        );
    // table-cell：均分父行宽度
    let cell_grow = if table_decl == "table-cell" { 1.0 } else { 0.0 };
    // 块流近似：块级容器纵向排布（taffy 无 block 流，用 column flex 表达）；
    // inline 子节点间的块级换行由父容器 column 方向自然产生
    let block_flow = !inline_like
        && !table_native
        && table_decl != "table-row"
        && table_decl != "table-cell"
        && (display_decl.is_none() || matches!(table_decl, "block" | "flow" | "flow-root" | "list-item"));

    // box-sizing: border-box → taffy 是 content-box 语义：显式尺寸先扣掉 padding+border
    let mut w_d = r.width;
    let mut h_d = r.height;
    let mut minw_d = r.min_width;
    let mut minh_d = r.min_height;
    let mut maxw_d = r.max_width;
    let mut maxh_d = r.max_height;
    if r.border_box {
        let pxlp = |v: LengthPercentage| match v {
            LengthPercentage::Length(l) => l,
            _ => 0.0,
        };
        let pad_h = pxlp(r.padding.left) + pxlp(r.padding.right);
        let pad_v = pxlp(r.padding.top) + pxlp(r.padding.bottom);
        let bw = d("border-width")
            .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
            .unwrap_or(0.0)
            * 2.0;
        let sub_w = pad_h + bw;
        let sub_h = pad_v + bw;
        let shrink = |dim: Dimension, sub: f32| match dim {
            Dimension::Length(l) => Dimension::Length((l - sub).max(0.0)),
            other => other,
        };
        w_d = shrink(w_d, sub_w);
        h_d = shrink(h_d, sub_h);
        minw_d = shrink(minw_d, sub_w);
        minh_d = shrink(minh_d, sub_h);
        maxw_d = shrink(maxw_d, sub_w);
        maxh_d = shrink(maxh_d, sub_h);
    }
    let mut style = Style {
        display: r.display,
        position: r.position,
        inset: r.inset,
        size: Size { width: w_d, height: h_d },
        min_size: Size { width: minw_d, height: minh_d },
        max_size: Size { width: maxw_d, height: maxh_d },
        margin: r.margin,
        padding: r.padding,
        gap: r.gap,
        flex_direction: r.flex_direction,
        flex_wrap: r.flex_wrap,
        flex_basis: r.flex_basis,
        flex_grow: if cell_grow > 0.0 { cell_grow } else { r.flex_grow },
        flex_shrink: r.flex_shrink,
        justify_content: r.justify_content,
        align_items: r.align_items,
        align_content: r.align_content,
        overflow: r.overflow,
        ..Default::default()
    };
    if inline_like && r.display != Display::None {
        // inline 近似：行内收缩盒 → 横向 flex 排列内容，flex_grow 0 收缩内容宽
        style.display = Display::Flex;
        style.flex_direction = FlexDirection::Row;
        style.align_items = Some(AlignItems::FlexStart);
        style.flex_grow = 0.0;
        // 防父级交叉轴 stretch 拉满宽度：自身起点对齐 → 收缩内容宽
        style.align_self = Some(AlignItems::FlexStart);
    } else if block_flow {
        style.flex_direction = FlexDirection::Column;
    }
    // table 容器（CSS display:table 或裸 table/thead/tbody 标签）：纵向排列行（行内 cell 横排由 table-row 的 Row 方向保证）
    let table_container = table_decl == "table"
        || (display_decl.is_none() && matches!(n.tag.as_str(), "table" | "thead" | "tbody" | "tfoot"));
    if table_container {
        style.display = Display::Flex;
        style.flex_direction = FlexDirection::Column;
        style.flex_grow = if style.flex_grow > 0.0 { style.flex_grow } else { 0.0 };
    }
    (style, r)
}

fn text_info(
    n: &StyleNode,
    default_size: f32,
    parent_align: TextAlign,
) -> (f32, u16, [u8; 4], f32, TextAlign, Option<String>) {
    let font_size = n
        .decl("font-size")
        .and_then(|v| v.trim().trim_end_matches("px").parse().ok())
        .unwrap_or(default_size);
    let weight = n
        .decl("font-weight")
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(400);
    let color = n
        .decl("color")
        .and_then(super::paint::parse_color)
        .unwrap_or([26, 26, 26, 255]);
    // line-height 三形态：无单位倍数（× font-size）、px 固定值、百分比（× font-size）
    let lh = n
        .decl("line-height")
        .and_then(|v| {
            let t = v.trim();
            if let Some(p) = t.strip_suffix('%') {
                p.trim().parse::<f32>().ok().map(|m| m / 100.0 * font_size)
            } else if let Some(px) = t.strip_suffix("px") {
                px.trim().parse::<f32>().ok()
            } else if let Some(em) = t.strip_suffix("em") {
                em.trim().parse::<f32>().ok().map(|m| m * font_size)
            } else if let Some(rem) = t.strip_suffix("rem") {
                rem.trim().parse::<f32>().ok().map(|m| m * 16.0)
            } else {
                // 无单位数值：CSS 语义为 font-size 的倍数
                t.parse::<f32>().ok().map(|m| m * font_size)
            }
        })
        .filter(|v| *v > 0.0)
        .unwrap_or(font_size * 1.5);
    // font-family：取逗号分隔的第一项（去引号）；排除通用族关键字
    let family = n.decl("font-family").and_then(|v| {
        v.split(',')
            .map(|s| s.trim().trim_matches(['"', '\'']))
            .find(|s| !s.is_empty() && !matches!(*s, "sans-serif" | "serif" | "monospace" | "system-ui"))
            .map(String::from)
    });
    // text-align 可继承：自身未声明时用父级
    let align = match n.decl("text-align") {
        Some(v) => TextAlign::parse(v),
        None => parent_align,
    };
    (font_size, weight, color, lh, align, family)
}

pub fn build_tree(root: &StyleNode, width: f32, fonts: &mut TextEngine, base_dir: &str) -> Result<Tree, String> {
    let mut taffy = TaffyTree::new();

    fn add(
        taffy: &mut TaffyTree<NodeCtx>,
        fonts: &mut TextEngine,
        n: &StyleNode,
        width: f32,
        parent_font: f32,
        parent_align: TextAlign,
        base_dir: &str,
    ) -> Result<taffy::NodeId, String> {
        let (st, rslv) = style_of(n, width);
        let (font_size, weight, color, lh, align, family) = text_info(n, parent_font, parent_align);
        // white-space / text-overflow / letter-spacing（decls 透传，测量与绘制共用判定）
        let nowrap = super::text::white_space_nowrap(&n.decls);
        let ellipsis = nowrap && super::text::text_overflow_ellipsis(&n.decls);
        let base_ctx = NodeCtx {
            decls: n.decls.clone(),
            text: n.text.clone(),
            font_size,
            weight,
            color,
            line_height: lh,
            align,
            src: n.src.clone(),
            family,
            tag: n.tag.clone(),
            letter_spacing: super::text::letter_spacing_px(&n.decls),
            ellip_w: None,
            resolved: rslv,
        };

        // 文本叶子：无子节点但有文本（尺寸由 compute_layout_with_measure 按约束宽度动态换行）
        if n.children.is_empty() && !n.text.is_empty() {
            let mut st = st;
            // white-space:nowrap（无 ellipsis）：禁止收缩与交叉轴拉伸，保持单行自然宽度
            if nowrap && !ellipsis {
                st.flex_shrink = 0.0;
                st.align_self = Some(AlignItems::FlexStart);
            }
            let id = taffy
                .new_leaf(st)
                .map_err(|e| e.to_string())?;
            taffy.set_node_context(id, Some(base_ctx)).map_err(|e| e.to_string())?;
            return Ok(id);
        }

        // 容器：递归子节点时传"实际可用宽"（CSS 显式宽优先，扣除自身 padding/margin）
        let cw = {
            let mut w = match st.size.width {
                Dimension::Length(l) => l,
                _ => width,
            };
            let ppx = |v: LengthPercentage| match v {
                LengthPercentage::Length(l) => l,
                _ => 0.0,
            };
            w -= ppx(st.padding.left) + ppx(st.padding.right);
            w -= st.margin.left.maybe_resolve(16.0).unwrap_or(0.0) + st.margin.right.maybe_resolve(16.0).unwrap_or(0.0);
            // maybe_resolve 是 Resolve trait 方法，需引入
            w.max(0.0)
        };
        let mut child_ids = Vec::new();
        for c in &n.children {
            child_ids.push(add(taffy, fonts, c, cw, font_size, align, base_dir)?);
        }
        // 混合节点：自身文本作为附加叶子（尺寸由 measure 决定）
        if !n.text.is_empty() {
            let mut leaf_st = Style::default();
            if nowrap && !ellipsis {
                leaf_st.flex_shrink = 0.0;
                leaf_st.align_self = Some(AlignItems::FlexStart);
            }
            let id = taffy
                .new_leaf(leaf_st)
                .map_err(|e| e.to_string())?;
            taffy.set_node_context(id, Some(base_ctx.clone())).map_err(|e| e.to_string())?;
            child_ids.push(id);
        }

        if child_ids.is_empty() {
            let mut st = st;
            if n.tag == "img" {
                if let Some(src) = &n.src {
                    if let Some(pm) = crate::renderer::media::load(src, base_dir) {
                        let has_w = n.decl("width").is_some();
                        let has_h = n.decl("height").is_some();
                        let mut iw = n
                            .decl("width")
                            .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
                            .unwrap_or(pm.width() as f32);
                        let mut ih = n
                            .decl("height")
                            .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
                            .unwrap_or(pm.height() as f32 * iw / pm.width() as f32);
                        // 自然尺寸超出容器宽：等比缩到容器宽（img 不会主动溢出普通容器）
                        if !has_w && width > 0.0 && iw > width {
                            ih *= width / iw;
                            iw = width;
                        }
                        // 有 CSS width/height 折算值时保留（style_of 已算好），不覆盖
                        if !has_w {
                            st.size.width = taffy::style_helpers::length(iw);
                        }
                        if !has_h {
                            st.size.height = taffy::style_helpers::length(ih);
                        }
                    }
                }
            }
            let id = taffy.new_leaf(st).map_err(|e| e.to_string())?;
            taffy.set_node_context(id, Some(base_ctx)).map_err(|e| e.to_string())?;
            return Ok(id);
        }
        let id = taffy.new_with_children(st, &child_ids).map_err(|e| e.to_string())?;
        taffy.set_node_context(id, Some(base_ctx)).map_err(|e| e.to_string())?;
        Ok(id)
    }

    let root_id = add(&mut taffy, fonts, root, width, 16.0, TextAlign::Left, base_dir)?;
    Ok(Tree { taffy, root: root_id })
}

/// 求布局并回填 PaintNode 树
pub fn compute(mut tree: Tree, width: f32, fonts: &mut TextEngine) -> Result<(PaintNode, f32), String> {
    let space = Size { width: AvailableSpace::Definite(width), height: AvailableSpace::MaxContent };
    tree.taffy
        .compute_layout_with_measure(tree.root, space, |known, avail, _node, ctx, style| {
            if let Some(ctx) = ctx {
                if !ctx.text.is_empty() {
                    // 显式声明宽（Length）作为约束之一
                    let declared = match style.size.width {
                        Dimension::Length(l) => Some(l),
                        _ => None,
                    };
                    // white-space:nowrap / pre：单行排版，测量不做换行约束
                    if super::text::white_space_nowrap(&ctx.decls) {
                        // text-overflow:ellipsis → 定宽内截断加省略号（单行高度）
                        if super::text::text_overflow_ellipsis(&ctx.decls) {
                            let ew = known
                                .width
                                .or(declared)
                                .filter(|w| *w > 0.0)
                                .or_else(|| match avail.width {
                                    AvailableSpace::Definite(w) if w > 0.0 => Some(w),
                                    _ => None,
                                });
                            // 定宽约束在多次 measure 间传递（min-content 等 pass 复用）
                            if ew.is_some() {
                                ctx.ellip_w = ew;
                            }
                            if let Some(ew) = ew.or(ctx.ellip_w).filter(|w| *w > 0.0) {
                                if let Some(ellipsized) = fonts.ellipsize(
                                    &ctx.text,
                                    ctx.font_size,
                                    ctx.weight,
                                    ctx.line_height,
                                    ctx.family.as_deref(),
                                    ew,
                                    ctx.letter_spacing,
                                ) {
                                    let (tw, th) = fonts.measure(
                                        &ellipsized,
                                        ctx.font_size,
                                        ctx.weight,
                                        ctx.color,
                                        None,
                                        ctx.line_height,
                                        ctx.family.as_deref(),
                                        ctx.letter_spacing,
                                    );
                                    return Size { width: tw.min(ew), height: th };
                                }
                            }
                        }
                        // 无换行约束：单行自然宽度
                        let (tw, th) = fonts.measure(
                            &ctx.text,
                            ctx.font_size,
                            ctx.weight,
                            ctx.color,
                            None,
                            ctx.line_height,
                            ctx.family.as_deref(),
                            ctx.letter_spacing,
                        );
                        return Size { width: tw, height: th };
                    }
                    // 约束宽度内动态换行；无约束时单行
                    let maxw = known.width.or(declared).filter(|w| *w > 0.0);
                    let (tw, th) = fonts.measure(
                        &ctx.text,
                        ctx.font_size,
                        ctx.weight,
                        ctx.color,
                        maxw,
                        ctx.line_height,
                        ctx.family.as_deref(),
                        ctx.letter_spacing,
                    );
                    return Size { width: tw, height: th };
                }
            }
            Size {
                width: known.width.unwrap_or(0.0),
                height: known.height.unwrap_or(0.0),
            }
        })
        .map_err(|e| e.to_string())?;
    let total = tree.taffy.layout(tree.root).map_err(|e| e.to_string())?.size.height;
    let painted = collect(&mut tree.taffy, tree.root, 0.0, 0.0, fonts)?;
    Ok((painted, total))
}

fn collect(
    taffy: &mut TaffyTree<NodeCtx>,
    id: taffy::NodeId,
    ox: f32,
    oy: f32,
    fonts: &mut TextEngine,
) -> Result<PaintNode, String> {
    let (nx, ny, nw, nh) = {
        let lay = taffy.layout(id).map_err(|e| e.to_string())?;
        (ox + lay.location.x, oy + lay.location.y, lay.size.width, lay.size.height)
    };
    let x = nx;
    let y = ny;
    let ctx = taffy.get_node_context(id).cloned().unwrap_or(NodeCtx {
        decls: BTreeMap::new(),
        text: String::new(),
        font_size: 16.0,
        weight: 400,
        color: [0, 0, 0, 255],
        line_height: 24.0,
        align: TextAlign::Left,
        src: None,
        family: None,
        tag: String::new(),
        letter_spacing: 0.0,
        ellip_w: None,
        resolved: Resolved::default(),
    });
    let mut children = Vec::new();
    let kids = taffy.children(id).map_err(|e| e.to_string())?;
    for c in kids {
        children.push(collect(taffy, c, x, y, fonts)?);
    }
    let resolved = ctx.resolved.clone();
    let mut node = PaintNode {
        x,
        y,
        w: nw,
        h: nh,
        decls: ctx.decls,
        text: ctx.text,
        text_align: ctx.align,
        children,
        src: ctx.src,
        tag: ctx.tag,
        letter_spacing: ctx.letter_spacing,
        style: resolved,
    };

    // 计算值归一化：绘制层从 decls 重新解析 font-size/line-height（仅认 px），
    // 将继承/倍数/百分比形态的计算值落成 px，保证绘制与布局一致
    node.decls.insert("font-size".to_string(), format!("{}px", ctx.font_size));
    if node.decls.contains_key("line-height") {
        node.decls.insert("line-height".to_string(), format!("{}px", ctx.line_height));
    }

    let is_text_leaf = node.children.is_empty() && !node.text.is_empty();
    if is_text_leaf && super::text::white_space_nowrap(&node.decls) {
        if super::text::text_overflow_ellipsis(&node.decls) {
            // text-overflow:ellipsis：按实际盒宽截断（含省略号），替换绘制文本
            if node.w > 0.0 {
                if let Some(ellipsized) = fonts.ellipsize(
                    &node.text,
                    ctx.font_size,
                    ctx.weight,
                    ctx.line_height,
                    ctx.family.as_deref(),
                    node.w,
                    ctx.letter_spacing,
                ) {
                    node.text = ellipsized;
                }
            }
        } else {
            // 纯 nowrap：绘制层按盒宽排版，盒宽不足单行（如 max-width 钳制）时放宽到文本宽
            let (tw, _) = fonts.measure(
                &node.text,
                ctx.font_size,
                ctx.weight,
                ctx.color,
                None,
                ctx.line_height,
                ctx.family.as_deref(),
                ctx.letter_spacing,
            );
            if tw > node.w {
                node.w = tw;
            }
        }
    }
    Ok(node)
}
