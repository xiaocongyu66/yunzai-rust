//! StyleNode → taffy 布局树：样式映射 + 文本测量 + 布局求解

use super::css::split_commas;
use super::dom::StyleNode;
use super::text::{TextAlign, TextEngine};
use std::collections::BTreeMap;
use taffy::prelude::*;

/// 布局完成的绘制节点（绝对坐标 + 原始声明）
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
fn parse_calc_pct_px(expr: &str) -> Option<(f32, f32)> {
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

fn style_of(n: &StyleNode, width: f32) -> Style {
    // 全量映射走 style::resolve（Lightning CSS 结构化），此处只补充 taffy 特有字段
    let d = |k: &str| n.decl(k).map(String::from);
    let r = super::style::resolve(n);
    let display = d("display");
    let is_flex = display.as_deref() == Some("flex") || display.as_deref() == Some("inline-flex");
    // table-cell：均分父行宽度
    let cell_grow = if display.as_deref() == Some("table-cell") { 1.0 } else { 0.0 };

    Style {
        display: r.display,
        position: r.position,
        inset: r.inset,
        size: Size { width: r.width, height: r.height },
        min_size: Size { width: r.min_width, height: r.min_height },
        max_size: Size { width: r.max_width, height: r.max_height },
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
    }
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
    let lh = n
        .decl("line-height")
        .and_then(|v| {
            let t = v.trim().trim_end_matches("px").trim();
            t.parse::<f32>().ok().or_else(|| t.parse::<f32>().ok().map(|m| m * font_size))
        })
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
        let st = style_of(n, width);
        let (font_size, weight, color, lh, align, family) = text_info(n, parent_font, parent_align);
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
        };

        // 文本叶子：无子节点但有文本（尺寸由 compute_layout_with_measure 按约束宽度动态换行）
        if n.children.is_empty() && !n.text.is_empty() {
            let id = taffy
                .new_leaf(st)
                .map_err(|e| e.to_string())?;
            taffy.set_node_context(id, Some(base_ctx)).map_err(|e| e.to_string())?;
            return Ok(id);
        }

        // 容器
        let mut child_ids = Vec::new();
        for c in &n.children {
            child_ids.push(add(taffy, fonts, c, width, font_size, align, base_dir)?);
        }
        // 混合节点：自身文本作为附加叶子（尺寸由 measure 决定）
        if !n.text.is_empty() {
            let id = taffy
                .new_leaf(Style::default())
                .map_err(|e| e.to_string())?;
            taffy.set_node_context(id, Some(base_ctx.clone())).map_err(|e| e.to_string())?;
            child_ids.push(id);
        }

        if child_ids.is_empty() {
            let mut st = st;
            if n.tag == "img" {
                if let Some(src) = &n.src {
                    if let Some(pm) = crate::renderer::media::load(src, base_dir) {
                        let iw = n
                            .decl("width")
                            .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
                            .unwrap_or(pm.width() as f32);
                        let ih = n
                            .decl("height")
                            .and_then(|v| v.trim().trim_end_matches("px").parse::<f32>().ok())
                            .unwrap_or(pm.height() as f32 * iw / pm.width() as f32);
                        st.size = taffy::Size {
                            width: taffy::style_helpers::length(iw),
                            height: taffy::style_helpers::length(ih),
                        };
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
        .compute_layout_with_measure(tree.root, space, |known, _avail, _node, ctx, style| {
            if let Some(ctx) = ctx {
                if !ctx.text.is_empty() {
                    // 约束宽度内动态换行；无约束时单行
                    let declared = match style.size.width {
                        Dimension::Length(l) => Some(l),
                        _ => None,
                    };
                    let maxw = known.width.or(declared).filter(|w| *w > 0.0);
                    let (tw, th) = fonts.measure(
                        &ctx.text,
                        ctx.font_size,
                        ctx.weight,
                        ctx.color,
                        maxw,
                        ctx.line_height,
                        ctx.family.as_deref(),
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
    let painted = collect(&mut tree.taffy, tree.root, 0.0, 0.0)?;
    Ok((painted, total))
}

fn collect(taffy: &mut TaffyTree<NodeCtx>, id: taffy::NodeId, ox: f32, oy: f32) -> Result<PaintNode, String> {
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
    });
    let mut children = Vec::new();
    let kids = taffy.children(id).map_err(|e| e.to_string())?;
    for c in kids {
        children.push(collect(taffy, c, x, y)?);
    }
    Ok(PaintNode {
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
    })
}
