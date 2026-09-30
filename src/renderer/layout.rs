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
    if let Some(p) = v.strip_suffix('%') {
        return p.trim().parse::<f32>().map(|n| Dimension::Percent(n / 100.0)).unwrap_or(Dimension::Auto);
    }
    let n = v.trim().trim_end_matches("px").trim();
    if let Ok(f) = n.parse::<f32>() {
        return length(f);
    }
    Dimension::Auto
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
    let d = |k: &str| n.decl(k).map(String::from);
    let display = d("display");
    let is_flex = display.as_deref() == Some("flex") || display.as_deref() == Some("inline-flex");
    let flex_direction = match d("flex-direction").as_deref() {
        Some("column") => FlexDirection::Column,
        Some("column-reverse") => FlexDirection::ColumnReverse,
        Some("row-reverse") => FlexDirection::RowReverse,
        _ => FlexDirection::Row,
    };
    // 非 flex 的容器：近似 block = flex column；table-row 近似 flex row（table-cell 横排）
    let is_table_row = display.as_deref() == Some("table-row");
    let dir = if is_table_row {
        FlexDirection::Row
    } else if is_flex {
        flex_direction
    } else {
        FlexDirection::Column
    };
    // table-cell：均分父行宽度
    let cell_grow = if display.as_deref() == Some("table-cell") { 1.0 } else { 0.0 };

    let justify = match d("justify-content").as_deref().unwrap_or("") {
        "center" => JustifyContent::Center,
        "flex-end" | "end" => JustifyContent::FlexEnd,
        "space-between" => JustifyContent::SpaceBetween,
        "space-around" => JustifyContent::SpaceAround,
        "space-evenly" => JustifyContent::SpaceEvenly,
        _ => JustifyContent::FlexStart,
    };
    let align_items = match d("align-items").as_deref().unwrap_or("") {
        "center" => AlignItems::Center,
        "flex-end" | "end" => AlignItems::FlexEnd,
        "stretch" => AlignItems::Stretch,
        "baseline" => AlignItems::Baseline,
        // block 容器（近似 flex column）默认 stretch：块级子元素撑满父宽
        _ => if is_flex { AlignItems::FlexStart } else { AlignItems::Stretch },
    };
    let wrap = match d("flex-wrap").as_deref().unwrap_or("") {
        "wrap" => FlexWrap::Wrap,
        _ => FlexWrap::NoWrap,
    };
    let gap_parts = d("gap").map(|s| split_commas(&s)).unwrap_or_default();
    let (gx, gy) = match gap_parts.len() {
        0 => ("0".into(), "0".into()),
        1 => (gap_parts[0].clone(), gap_parts[0].clone()),
        _ => (gap_parts[0].clone(), gap_parts[1].clone()),
    };

    // padding/margin 简写展开
    let sides = |name: &str| -> Vec<String> {
        d(name).map(|s| split_commas(&s)).unwrap_or_default()
    };
    let expand = |v: &[String], def: &str| -> (String, String, String, String) {
        match v.len() {
            0 => (def.into(), def.into(), def.into(), def.into()),
            1 => (v[0].clone(), v[0].clone(), v[0].clone(), v[0].clone()),
            2 => (v[0].clone(), v[1].clone(), v[0].clone(), v[1].clone()),
            3 => (v[0].clone(), v[1].clone(), v[2].clone(), v[1].clone()),
            _ => (v[0].clone(), v[1].clone(), v[2].clone(), v[3].clone()),
        }
    };
    let (pt, pr, pb, pl) = expand(&sides("padding"), "0");
    let (mt, mr, mb, ml) = expand(&sides("margin"), "0");

    Style {
        display: match display.as_deref() {
            Some("none") => Display::None,
            _ => Display::Flex,
        },
        flex_direction: dir,
        justify_content: Some(justify),
        align_items: Some(align_items),
        align_self: match d("align-self").as_deref().unwrap_or("") {
            "center" => Some(AlignSelf::Center),
            "flex-end" => Some(AlignSelf::FlexEnd),
            "stretch" => Some(AlignSelf::Stretch),
            _ => None,
        },
        flex_wrap: wrap,
        gap: Size { width: lp(&gx, width), height: lp(&gy, width) },
        padding: Rect {
            top: lp(&pt, width),
            right: lp(&pr, width),
            bottom: lp(&pb, width),
            left: lp(&pl, width),
        },
        margin: Rect {
            top: lp(&mt, width).into(),
            right: lp(&mr, width).into(),
            bottom: lp(&mb, width).into(),
            left: lp(&ml, width).into(),
        },
        size: Size {
            width: dim(d("width").as_deref().unwrap_or("auto"), width),
            height: dim(d("height").as_deref().unwrap_or("auto"), width),
        },
        min_size: Size {
            width: dim(d("min-width").as_deref().unwrap_or("auto"), width),
            height: dim(d("min-height").as_deref().unwrap_or("auto"), width),
        },
        max_size: Size {
            width: dim(d("max-width").as_deref().unwrap_or("auto"), width),
            height: dim(d("max-height").as_deref().unwrap_or("auto"), width),
        },
        flex_grow: d("flex-grow")
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(cell_grow),
        flex_shrink: d("flex-shrink").and_then(|v| v.trim().parse().ok()).unwrap_or(1.0),
        position: match d("position").as_deref().unwrap_or("") {
            "absolute" => Position::Absolute,
            _ => Position::Relative,
        },
        inset: Rect {
            top: lpa(d("top").as_deref().unwrap_or("auto"), width),
            right: lpa(d("right").as_deref().unwrap_or("auto"), width),
            bottom: lpa(d("bottom").as_deref().unwrap_or("auto"), width),
            left: lpa(d("left").as_deref().unwrap_or("auto"), width),
        },
        ..Default::default()
    }
}

fn text_info(n: &StyleNode, default_size: f32) -> (f32, u16, [u8; 4], f32, TextAlign, Option<String>) {
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
    let align = TextAlign::parse(n.decl("text-align").unwrap_or("left"));
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
        base_dir: &str,
    ) -> Result<taffy::NodeId, String> {
        let st = style_of(n, width);
        let (font_size, weight, color, lh, align, family) = text_info(n, parent_font);
        let base_ctx = NodeCtx {
            decls: n.decls.clone(),
            text: n.text.clone(),
            font_size,
            weight,
            color,
            line_height: lh,
            align,
            src: n.src.clone(),
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
            child_ids.push(add(taffy, fonts, c, width, font_size, base_dir)?);
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

    let root_id = add(&mut taffy, fonts, root, width, 16.0, base_dir)?;
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
    })
}
