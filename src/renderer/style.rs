//! 全量 CSS → taffy Style 映射 + 绘制结构产出
//!
//! 值解析统一走 Lightning CSS 的 Property::parse_string（结构化），
//! 属性名→PropertyId 用白名单（alpha.72 的 from_name_and_prefix 为私有 API）。
//! calc() 经序列化后用 parse_calc_pct_px 按 basis 折算。
//!
//! 绘制字段（背景/阴影/边框/圆角/变换/字体文本）同样从 Property 枚举直接读取，
//! 落成 Resolved 的绘制结构供 paint 层直接消费 —— paint 层不做任何字符串解析。
//! 允许的最小胶水：FontFamily 名字经 ToCss 序列化（lightningcss 内部字段私有）。

use super::dom::StyleNode;
use super::layout::parse_calc_pct_px;
use lightningcss::properties::Property;
use lightningcss::stylesheet::ParserOptions;
use lightningcss::printer::PrinterOptions;
use lightningcss::traits::ToCss;
use lightningcss::vendor_prefix::VendorPrefix;
use lightningcss::values::color::CssColor;
use lightningcss::values::gradient::{Gradient, GradientItem, LineDirection};
use lightningcss::values::image::Image;
use lightningcss::values::length::{Length, LengthPercentageOrAuto};
use lightningcss::values::percentage::NumberOrPercentage;
use lightningcss::values::position::{HorizontalPositionKeyword, VerticalPositionKeyword};
use taffy::geometry::Point;
use taffy::prelude::*;
use taffy::style::Overflow;

/// lightningcss 的 <length-percentage>（与 taffy 的同名类型区分，用别名引用）
type CssLp = lightningcss::values::length::LengthPercentage;

/// 属性名 → lightningcss PropertyId（白名单）
fn property_id_of(name: &str) -> Option<lightningcss::properties::PropertyId<'static>> {
    use lightningcss::properties::PropertyId;
    Some(match name {
        "width" => PropertyId::Width,
        "height" => PropertyId::Height,
        "min-width" => PropertyId::MinWidth,
        "min-height" => PropertyId::MinHeight,
        "max-width" => PropertyId::MaxWidth,
        "max-height" => PropertyId::MaxHeight,
        "margin" => PropertyId::Margin,
        "margin-top" => PropertyId::MarginTop,
        "margin-right" => PropertyId::MarginRight,
        "margin-bottom" => PropertyId::MarginBottom,
        "margin-left" => PropertyId::MarginLeft,
        "padding" => PropertyId::Padding,
        "padding-top" => PropertyId::PaddingTop,
        "padding-right" => PropertyId::PaddingRight,
        "padding-bottom" => PropertyId::PaddingBottom,
        "padding-left" => PropertyId::PaddingLeft,
        "top" => PropertyId::Top,
        "right" => PropertyId::Right,
        "bottom" => PropertyId::Bottom,
        "left" => PropertyId::Left,
        "display" => PropertyId::Display,
        "position" => PropertyId::Position,
        "overflow" => PropertyId::Overflow,
        "gap" => PropertyId::Gap,
        "row-gap" => PropertyId::RowGap,
        "column-gap" => PropertyId::ColumnGap,
        "z-index" => PropertyId::ZIndex,
        "flex-grow" => PropertyId::FlexGrow(VendorPrefix::None),
        "flex-shrink" => PropertyId::FlexShrink(VendorPrefix::None),
        "flex-basis" => PropertyId::FlexBasis(VendorPrefix::None),
        "flex-direction" => PropertyId::FlexDirection(VendorPrefix::None),
        "flex-wrap" => PropertyId::FlexWrap(VendorPrefix::None),
        "justify-content" => PropertyId::JustifyContent(VendorPrefix::None),
        "align-items" => PropertyId::AlignItems(VendorPrefix::None),
        "align-content" => PropertyId::AlignContent(VendorPrefix::None),
        "box-sizing" => PropertyId::BoxSizing(VendorPrefix::None),
        // ---- 背景 ----
        "background" => PropertyId::Background,
        "background-color" => PropertyId::BackgroundColor,
        "background-image" => PropertyId::BackgroundImage,
        "background-size" => PropertyId::BackgroundSize,
        "background-position" => PropertyId::BackgroundPosition,
        "background-repeat" => PropertyId::BackgroundRepeat,
        // ---- 阴影 ----
        "box-shadow" => PropertyId::BoxShadow(VendorPrefix::None),
        "text-shadow" => PropertyId::TextShadow,
        // ---- 边框 ----
        "border" => PropertyId::Border,
        "border-top" => PropertyId::BorderTop,
        "border-right" => PropertyId::BorderRight,
        "border-bottom" => PropertyId::BorderBottom,
        "border-left" => PropertyId::BorderLeft,
        "border-width" => PropertyId::BorderWidth,
        "border-style" => PropertyId::BorderStyle,
        "border-color" => PropertyId::BorderColor,
        "border-top-width" => PropertyId::BorderTopWidth,
        "border-right-width" => PropertyId::BorderRightWidth,
        "border-bottom-width" => PropertyId::BorderBottomWidth,
        "border-left-width" => PropertyId::BorderLeftWidth,
        "border-top-style" => PropertyId::BorderTopStyle,
        "border-right-style" => PropertyId::BorderRightStyle,
        "border-bottom-style" => PropertyId::BorderBottomStyle,
        "border-left-style" => PropertyId::BorderLeftStyle,
        "border-top-color" => PropertyId::BorderTopColor,
        "border-right-color" => PropertyId::BorderRightColor,
        "border-bottom-color" => PropertyId::BorderBottomColor,
        "border-left-color" => PropertyId::BorderLeftColor,
        "border-radius" => PropertyId::BorderRadius(VendorPrefix::None),
        "border-top-left-radius" => PropertyId::BorderTopLeftRadius(VendorPrefix::None),
        "border-top-right-radius" => PropertyId::BorderTopRightRadius(VendorPrefix::None),
        "border-bottom-right-radius" => PropertyId::BorderBottomRightRadius(VendorPrefix::None),
        "border-bottom-left-radius" => PropertyId::BorderBottomLeftRadius(VendorPrefix::None),
        // ---- 变换 ----
        "transform" => PropertyId::Transform(VendorPrefix::None),
        "transform-origin" => PropertyId::TransformOrigin(VendorPrefix::None),
        // ---- 颜色 / 字体 / 文本 ----
        "opacity" => PropertyId::Opacity,
        "color" => PropertyId::Color,
        "font-size" => PropertyId::FontSize,
        "font-weight" => PropertyId::FontWeight,
        "font-family" => PropertyId::FontFamily,
        "line-height" => PropertyId::LineHeight,
        "letter-spacing" => PropertyId::LetterSpacing,
        "text-indent" => PropertyId::TextIndent,
        "text-align" => PropertyId::TextAlign,
        "white-space" => PropertyId::WhiteSpace,
        "text-overflow" => PropertyId::TextOverflow(VendorPrefix::None),
        _ => return None,
    })
}

/// 解析单个 CSS 属性声明为 lightningcss Property（结构化值）
pub fn parse_prop<'a>(name: &str, value: &'a str) -> Option<Property<'a>> {
    let pid = property_id_of(name)?;
    Property::parse_string(pid, value, ParserOptions::default()).ok()
}

// ==================== 绘制结构（paint 层消费的冻结接口） ====================

pub type Rgba = [u8; 4];

/// 单条阴影（box-shadow / text-shadow 通用）
#[derive(Clone, Debug)]
pub struct ShadowP {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub spread: f32,
    pub color: Rgba,
    pub inset: bool,
}

/// 背景绘制：纯色 / 线性渐变 / 径向渐变 / 图片
#[derive(Clone, Debug)]
pub enum BgPaint {
    Color(Rgba),
    /// deg：0 = 向上、顺时针（CSS 线性渐变语义）；stops 位置为 0..1
    Linear { deg: f32, stops: Vec<(f32, Rgba)> },
    Radial { stops: Vec<(f32, Rgba)> },
    /// 原始 url（paint 层负责加载）
    Url(String),
}

/// 单个背景图层（CSS 多层背景，声明顺序即绘制顺序，第一层最上）
#[derive(Clone, Debug)]
pub struct BgLayer {
    pub paint: BgPaint,
    /// 尺寸
    pub size: BgSize,
    /// 水平/垂直位置
    pub position: (BgPosComp, BgPosComp),
    pub repeat: BgRepeat,
}

#[derive(Clone, Copy, Debug)]
pub enum BgSize {
    Auto,
    Contain,
    Cover,
    Val { w: LengthOrPct, h: LengthOrPct },
}

#[derive(Clone, Copy, Debug)]
pub enum LengthOrPct {
    Px(f32),
    Pct(f32),
}

#[derive(Clone, Copy, Debug)]
pub enum BgPosComp {
    Px(f32),
    Pct(f32),
    Left,
    Center,
    Right,
    Top,
    Bottom,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BgRepeat {
    Repeat,
    RepeatX,
    RepeatY,
    NoRepeat,
}

#[derive(Clone, Copy, Debug)]
pub struct BorderSideP {
    pub width: f32,
    pub color: Rgba,
}

impl Default for BorderSideP {
    fn default() -> Self {
        Self { width: 0.0, color: [0, 0, 0, 0] }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct BorderSidesP {
    pub top: BorderSideP,
    pub right: BorderSideP,
    pub bottom: BorderSideP,
    pub left: BorderSideP,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CornerRadiusP {
    pub top_left: f32,
    pub top_right: f32,
    pub bottom_right: f32,
    pub bottom_left: f32,
}

#[derive(Clone, Copy, Debug)]
pub enum TextAlignP {
    Left,
    Center,
    Right,
}

/// line-height：Px 固定像素 / Mult 相对自身 font-size 的倍数
#[derive(Clone, Copy, Debug)]
pub enum LineH {
    Px(f32),
    Mult(f32),
}

// ==================== Resolved ====================

/// 解析后的属性集（basis 为百分比参照宽，一般为父容器宽）
#[derive(Clone)]
pub struct Resolved {
    pub width: Dimension,
    pub height: Dimension,
    pub min_width: Dimension,
    pub min_height: Dimension,
    pub max_width: Dimension,
    pub max_height: Dimension,
    pub margin: Rect<LengthPercentageAuto>,
    pub padding: Rect<LengthPercentage>,
    pub inset: Rect<LengthPercentageAuto>,
    pub display: Display,
    pub position: Position,
    pub overflow: Point<Overflow>,
    pub gap: Size<LengthPercentage>,
    pub z_index: i32,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Dimension,
    pub flex_direction: FlexDirection,
    pub flex_wrap: FlexWrap,
    pub justify_content: Option<JustifyContent>,
    pub align_items: Option<AlignItems>,
    pub align_content: Option<AlignContent>,
    /// box-sizing: border-box（width/height 含 padding+border，layout 侧据此做内容盒补偿）
    pub border_box: bool,
    // ---- 绘制字段（paint 层直接消费） ----
    /// background-color（纯色底）
    pub bg_color: Option<Rgba>,
    /// 背景图层（CSS 顺序，第一层最上；纯色由 bg_color 承载，不入层）
    pub bg_layers: Vec<BgLayer>,
    pub box_shadows: Vec<ShadowP>,
    pub text_shadows: Vec<ShadowP>,
    /// 四侧边框（style none/hidden 的侧宽度按 0，默认全 0/透明）
    pub border: BorderSidesP,
    pub radius: CornerRadiusP,
    /// 2D 仿射矩阵 [sx, kx, ky, sy, tx, ty]（transform 序列按序组合；origin 平移由 paint 层按 transform_origin 组合）
    pub transform: Option<[f32; 6]>,
    /// transform-origin（0..1 比例，默认 (0.5, 0.5)）
    pub transform_origin: (f32, f32),
    /// 文本色（沿用现有近黑默认 #1a1a1a）
    pub color: Rgba,
    pub font_size: f32,
    pub font_weight: u16,
    /// 第一个非通用字体名
    pub font_family: Option<String>,
    pub line_height: Option<LineH>,
    pub letter_spacing: f32,
    pub text_align: TextAlignP,
    pub text_indent: f32,
    /// white-space: nowrap / pre
    pub nowrap: bool,
    /// text-overflow: ellipsis
    pub ellipsis: bool,
    pub opacity: f32,
    // ---- 私有中间态（对 paint/layout 不可见） ----
    /// 四侧 border-style 是否 none/hidden（边框宽度在 resolve 末尾按此归零）
    border_hidden: [bool; 4],
    /// 四侧原始 border-width（未按 style 归零）
    border_raw: [f32; 4],
}

impl Default for Resolved {
    fn default() -> Self {
        Self {
            width: Dimension::Auto,
            height: Dimension::Auto,
            min_width: Dimension::Auto,
            min_height: Dimension::Auto,
            max_width: Dimension::Auto,
            max_height: Dimension::Auto,
            margin: Rect::<LengthPercentageAuto>::auto(),
            padding: Rect::<LengthPercentage>::zero(),
            inset: Rect {
                top: LengthPercentageAuto::Auto,
                right: LengthPercentageAuto::Auto,
                bottom: LengthPercentageAuto::Auto,
                left: LengthPercentageAuto::Auto,
            },
            display: Display::Flex,
            position: Position::Relative,
            overflow: Point { x: Overflow::Visible, y: Overflow::Visible },
            gap: Size::zero(),
            z_index: 0,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Dimension::Auto,
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::NoWrap,
            justify_content: None,
            align_items: None,
            align_content: None,
            border_box: false,
            bg_color: None,
            bg_layers: Vec::new(),
            box_shadows: Vec::new(),
            text_shadows: Vec::new(),
            border: BorderSidesP::default(),
            radius: CornerRadiusP::default(),
            transform: None,
            transform_origin: (0.5, 0.5),
            color: [26, 26, 26, 255],
            font_size: 16.0,
            font_weight: 400,
            font_family: None,
            line_height: None,
            letter_spacing: 0.0,
            text_align: TextAlignP::Left,
            text_indent: 0.0,
            nowrap: false,
            ellipsis: false,
            opacity: 1.0,
            border_hidden: [false; 4],
            border_raw: [0.0; 4],
        }
    }
}

pub fn resolve(n: &StyleNode, width: f32) -> Resolved {
    let mut r = Resolved::default();
    for (k, v) in n.decls.iter() {
        if let Some(p) = parse_prop(k, v) {
            apply(&p, width, &mut r);
        }
    }
    // 边框宽度收口：style none/hidden 的侧边按 0（声明字典为字母序，style 恒先于 width 出现）
    for i in 0..4 {
        let w = if r.border_hidden[i] { 0.0 } else { r.border_raw[i] };
        set_side_width(&mut r, i, w);
    }
    r
}

fn apply(p: &Property, basis: f32, r: &mut Resolved) {
    use lightningcss::properties::Property as P;
    match p {
        P::Width(v) => r.width = size_dim(v, basis),
        P::Height(v) => r.height = size_dim(v, basis),
        P::MinWidth(v) => r.min_width = size_dim(v, basis),
        P::MinHeight(v) => r.min_height = size_dim(v, basis),
        P::MaxWidth(v) => r.max_width = maxsize_dim(v, basis),
        P::MaxHeight(v) => r.max_height = maxsize_dim(v, basis),
        P::Margin(m) => {
            r.margin.top = tlpa(&m.top, basis);
            r.margin.right = tlpa(&m.right, basis);
            r.margin.bottom = tlpa(&m.bottom, basis);
            r.margin.left = tlpa(&m.left, basis);
        }
        P::Padding(pd) => {
            r.padding.top = tlpa(&pd.top, basis).into_lp();
            r.padding.right = tlpa(&pd.right, basis).into_lp();
            r.padding.bottom = tlpa(&pd.bottom, basis).into_lp();
            r.padding.left = tlpa(&pd.left, basis).into_lp();
        }
        P::Top(v) => r.inset.top = tlpa(v, basis),
        P::Right(v) => r.inset.right = tlpa(v, basis),
        P::Bottom(v) => r.inset.bottom = tlpa(v, basis),
        P::Left(v) => r.inset.left = tlpa(v, basis),
        P::Display(d) => {
            use lightningcss::properties::display::{Display as LD, DisplayInside, DisplayKeyword};
            r.display = match d {
                LD::Keyword(kw) => match kw {
                    DisplayKeyword::None => Display::None,
                    // contents 近似为参与布局（Flex）；table 系关键字（table-cell/row/row-group 等）统一 Flex，
                    // 方向与均分由 layout::style_of 的 table_decl 特判处理
                    _ => Display::Flex,
                },
                LD::Pair(p) => match p.inside {
                    DisplayInside::Flex(_) => Display::Flex,
                    DisplayInside::Grid => Display::Grid,
                    // Flow/FlowRoot/Table/Box/Ruby → Flex（table 容器纵排由 style_of 处理）
                    _ => Display::Flex,
                },
            };
        }
        P::Position(pos) => {
            use lightningcss::properties::position::Position as LP2;
            r.position = match pos {
                LP2::Absolute | LP2::Fixed => Position::Absolute,
                _ => Position::Relative,
            };
        }
        P::Overflow(o) => {
            // Overflow 字段私有 → 序列化后判断关键字
            if let Ok(s) = o.to_css_string(PrinterOptions::default()) {
                r.overflow.x = ov_str(&s);
                r.overflow.y = s.split_whitespace().nth(1).map(ov_str).unwrap_or(r.overflow.x);
            }
        }
        P::Gap(g) => {
            r.gap.width = gap_val(&g.row, basis);
            r.gap.height = gap_val(&g.column, basis);
        }
        P::RowGap(v) => r.gap.width = gap_val(v, basis),
        P::ColumnGap(v) => r.gap.height = gap_val(v, basis),
        P::ZIndex(z) => {
            if let lightningcss::properties::position::ZIndex::Integer(i) = z {
                r.z_index = *i;
            }
        }
        P::FlexGrow(v, _) => r.flex_grow = *v,
        P::FlexShrink(v, _) => r.flex_shrink = *v,
        P::FlexBasis(v, _) => r.flex_basis = tlpa(v, basis).into_dim(),
        P::FlexDirection(d, _) => {
            use lightningcss::properties::flex::FlexDirection::*;
            r.flex_direction = match d {
                Row => FlexDirection::Row,
                RowReverse => FlexDirection::RowReverse,
                Column => FlexDirection::Column,
                ColumnReverse => FlexDirection::ColumnReverse,
            };
        }
        P::FlexWrap(w, _) => {
            use lightningcss::properties::flex::FlexWrap::*;
            r.flex_wrap = match w {
                Wrap => FlexWrap::Wrap,
                WrapReverse => FlexWrap::WrapReverse,
                NoWrap => FlexWrap::NoWrap,
            };
        }
        P::JustifyContent(j, _) => {
            use lightningcss::properties::align::{JustifyContent as LJ, ContentPosition, ContentDistribution};
            r.justify_content = match j {
                LJ::ContentPosition { value: ContentPosition::FlexStart, .. } => Some(JustifyContent::FlexStart),
                LJ::ContentPosition { value: ContentPosition::Center, .. } => Some(JustifyContent::Center),
                LJ::ContentPosition { value: ContentPosition::Start, .. } => Some(JustifyContent::FlexStart),
                LJ::ContentPosition { value: ContentPosition::End, .. } => Some(JustifyContent::FlexEnd),
                LJ::ContentPosition { value: ContentPosition::FlexEnd, .. } => Some(JustifyContent::FlexEnd),
                LJ::ContentDistribution(ContentDistribution::SpaceBetween) => Some(JustifyContent::SpaceBetween),
                LJ::ContentDistribution(ContentDistribution::SpaceAround) => Some(JustifyContent::SpaceAround),
                LJ::ContentDistribution(ContentDistribution::SpaceEvenly) => Some(JustifyContent::SpaceEvenly),
                _ => None,
            };
        }
        P::AlignItems(a, _) => {
            use lightningcss::properties::align::{AlignItems as LA, SelfPosition};
            r.align_items = match a {
                LA::SelfPosition { value: SelfPosition::FlexStart, .. } => Some(AlignItems::FlexStart),
                LA::SelfPosition { value: SelfPosition::FlexEnd, .. } => Some(AlignItems::FlexEnd),
                LA::SelfPosition { value: SelfPosition::Center, .. } => Some(AlignItems::Center),
                LA::SelfPosition { value: SelfPosition::Start, .. } => Some(AlignItems::FlexStart),
                LA::SelfPosition { value: SelfPosition::End, .. } => Some(AlignItems::FlexEnd),
                LA::BaselinePosition(_) => Some(AlignItems::Baseline),
                LA::Stretch => Some(AlignItems::Stretch),
                _ => None,
            };
        }
        P::AlignContent(a, _) => {
            use lightningcss::properties::align::{AlignContent as LC, ContentPosition, ContentDistribution};
            r.align_content = match a {
                LC::ContentPosition { value: ContentPosition::FlexStart, .. } => Some(AlignContent::FlexStart),
                LC::ContentPosition { value: ContentPosition::Center, .. } => Some(AlignContent::Center),
                LC::ContentPosition { value: ContentPosition::End, .. } => Some(AlignContent::FlexEnd),
                LC::ContentPosition { value: ContentPosition::FlexEnd, .. } => Some(AlignContent::FlexEnd),
                LC::ContentDistribution(ContentDistribution::SpaceBetween) => Some(AlignContent::SpaceBetween),
                LC::ContentDistribution(ContentDistribution::SpaceAround) => Some(AlignContent::SpaceAround),
                LC::ContentDistribution(ContentDistribution::SpaceEvenly) => Some(AlignContent::SpaceEvenly),
                _ => None,
            };
        }
        P::BoxSizing(b, _) => {
            use lightningcss::properties::size::BoxSizing as LB;
            r.border_box = matches!(b, LB::BorderBox);
        }

        // ==================== 背景 ====================
        P::BackgroundColor(c) => r.bg_color = css_color(c, r.color),
        P::Background(list) => {
            // 简写：逐层读取 image/color/position/repeat/size（纯色由 bg_color 承载）
            for bg in list.iter() {
                if let Some(c) = bg_shorthand_color(&bg.color, r.color) {
                    r.bg_color = Some(c);
                }
                if let Some(paint) = image_paint(&bg.image, r.color) {
                    r.bg_layers.push(BgLayer {
                        paint,
                        size: bg_size(&bg.size),
                        position: (bg_pos_h(&bg.position.x), bg_pos_v(&bg.position.y)),
                        repeat: bg_repeat(&bg.repeat),
                    });
                }
            }
        }
        P::BackgroundImage(list) => {
            // 长写在字母序上后于简写 → 清空重建（覆盖简写的 image 部分）
            r.bg_layers.clear();
            for img in list.iter() {
                if let Some(paint) = image_paint(img, r.color) {
                    r.bg_layers.push(BgLayer {
                        paint,
                        size: BgSize::Auto,
                        position: (BgPosComp::Center, BgPosComp::Center),
                        repeat: BgRepeat::Repeat,
                    });
                }
            }
        }
        P::BackgroundSize(list) => {
            for (i, sz) in list.iter().enumerate() {
                if i < r.bg_layers.len() {
                    r.bg_layers[i].size = bg_size(sz);
                }
            }
        }
        P::BackgroundPosition(list) => {
            for (i, pos) in list.iter().enumerate() {
                if i < r.bg_layers.len() {
                    r.bg_layers[i].position = (bg_pos_h(&pos.x), bg_pos_v(&pos.y));
                }
            }
        }
        P::BackgroundRepeat(list) => {
            for (i, rp) in list.iter().enumerate() {
                if i < r.bg_layers.len() {
                    r.bg_layers[i].repeat = bg_repeat(rp);
                }
            }
        }

        // ==================== 阴影 ====================
        P::BoxShadow(list, _) => {
            r.box_shadows = list
                .iter()
                .map(|s| shadow_from(&s.color, &s.x_offset, &s.y_offset, &s.blur, &s.spread, s.inset, r.color))
                .collect();
        }
        P::TextShadow(list) => {
            r.text_shadows = list
                .iter()
                .map(|s| shadow_from(&s.color, &s.x_offset, &s.y_offset, &s.blur, &s.spread, false, r.color))
                .collect();
        }

        // ==================== 边框 ====================
        P::Border(b) => {
            let hidden = border_style_hidden(&b.style);
            let w = border_side_width(&b.width);
            let c = css_color(&b.color, r.color).unwrap_or([0, 0, 0, 0]);
            r.border_hidden = [hidden; 4];
            r.border_raw = [w; 4];
            set_side_color(r, 0, c);
            set_side_color(r, 1, c);
            set_side_color(r, 2, c);
            set_side_color(r, 3, c);
        }
        P::BorderTop(b) => border_side(r, 0, b),
        P::BorderRight(b) => border_side(r, 1, b),
        P::BorderBottom(b) => border_side(r, 2, b),
        P::BorderLeft(b) => border_side(r, 3, b),
        P::BorderWidth(bw) => {
            r.border_raw[0] = border_side_width(&bw.top);
            r.border_raw[1] = border_side_width(&bw.right);
            r.border_raw[2] = border_side_width(&bw.bottom);
            r.border_raw[3] = border_side_width(&bw.left);
        }
        P::BorderStyle(bs) => {
            r.border_hidden[0] = border_style_hidden(&bs.top);
            r.border_hidden[1] = border_style_hidden(&bs.right);
            r.border_hidden[2] = border_style_hidden(&bs.bottom);
            r.border_hidden[3] = border_style_hidden(&bs.left);
        }
        P::BorderColor(bc) => {
            set_side_color(r, 0, css_color(&bc.top, r.color).unwrap_or([0, 0, 0, 0]));
            set_side_color(r, 1, css_color(&bc.right, r.color).unwrap_or([0, 0, 0, 0]));
            set_side_color(r, 2, css_color(&bc.bottom, r.color).unwrap_or([0, 0, 0, 0]));
            set_side_color(r, 3, css_color(&bc.left, r.color).unwrap_or([0, 0, 0, 0]));
        }
        P::BorderTopWidth(v) => r.border_raw[0] = border_side_width(v),
        P::BorderRightWidth(v) => r.border_raw[1] = border_side_width(v),
        P::BorderBottomWidth(v) => r.border_raw[2] = border_side_width(v),
        P::BorderLeftWidth(v) => r.border_raw[3] = border_side_width(v),
        P::BorderTopStyle(s) => r.border_hidden[0] = border_style_hidden(s),
        P::BorderRightStyle(s) => r.border_hidden[1] = border_style_hidden(s),
        P::BorderBottomStyle(s) => r.border_hidden[2] = border_style_hidden(s),
        P::BorderLeftStyle(s) => r.border_hidden[3] = border_style_hidden(s),
        P::BorderTopColor(c) => set_side_color(r, 0, css_color(c, r.color).unwrap_or(r.border.top.color)),
        P::BorderRightColor(c) => set_side_color(r, 1, css_color(c, r.color).unwrap_or(r.border.right.color)),
        P::BorderBottomColor(c) => set_side_color(r, 2, css_color(c, r.color).unwrap_or(r.border.bottom.color)),
        P::BorderLeftColor(c) => set_side_color(r, 3, css_color(c, r.color).unwrap_or(r.border.left.color)),

        // ==================== 圆角 ====================
        P::BorderRadius(br, _) => {
            r.radius.top_left = radius_px(&br.top_left.0, basis);
            r.radius.top_right = radius_px(&br.top_right.0, basis);
            r.radius.bottom_right = radius_px(&br.bottom_right.0, basis);
            r.radius.bottom_left = radius_px(&br.bottom_left.0, basis);
        }
        P::BorderTopLeftRadius(sz, _) => r.radius.top_left = radius_px(&sz.0, basis),
        P::BorderTopRightRadius(sz, _) => r.radius.top_right = radius_px(&sz.0, basis),
        P::BorderBottomRightRadius(sz, _) => r.radius.bottom_right = radius_px(&sz.0, basis),
        P::BorderBottomLeftRadius(sz, _) => r.radius.bottom_left = radius_px(&sz.0, basis),

        // ==================== 变换 ====================
        P::Transform(list, _) => {
            let mut acc = [1.0f32, 0.0, 0.0, 1.0, 0.0, 0.0];
            for t in list.0.iter() {
                acc = mat_mul(&acc, &mat_of(t));
            }
            r.transform = Some(acc);
        }
        P::TransformOrigin(pos, _) => {
            r.transform_origin = (origin_comp_h(&pos.x), origin_comp_v(&pos.y));
        }

        // ==================== 颜色 / 字体 / 文本 ====================
        P::Opacity(v) => r.opacity = v.0.clamp(0.0, 1.0),
        P::Color(c) => {
            if let Some(rgba) = css_color(c, r.color) {
                r.color = rgba;
            }
        }
        P::FontSize(fs) => {
            use lightningcss::properties::font::{AbsoluteFontSize, FontSize as LFS};
            r.font_size = match fs {
                // 百分比/em 参照父级字号（resolve 阶段不可知），按 16px 近似折算
                LFS::Length(lp) => lp_px(lp, 16.0).max(0.0),
                LFS::Absolute(a) => match a {
                    AbsoluteFontSize::XXSmall => 9.0,
                    AbsoluteFontSize::XSmall => 10.0,
                    AbsoluteFontSize::Small => 13.0,
                    AbsoluteFontSize::Medium => 16.0,
                    AbsoluteFontSize::Large => 18.0,
                    AbsoluteFontSize::XLarge => 24.0,
                    AbsoluteFontSize::XXLarge => 32.0,
                    AbsoluteFontSize::XXXLarge => 48.0,
                },
                LFS::Relative(_) => r.font_size,
            };
        }
        P::FontWeight(w) => {
            use lightningcss::properties::font::{AbsoluteFontWeight, FontWeight as LFW};
            // bolder/lighter 需父级字重，忽略
            if let LFW::Absolute(a) = w {
                r.font_weight = match a {
                    AbsoluteFontWeight::Weight(n) => (*n as u16).clamp(100, 900),
                    AbsoluteFontWeight::Normal => 400,
                    AbsoluteFontWeight::Bold => 700,
                };
            }
        }
        P::FontFamily(list) => {
            // 取第一个非通用族名（通用关键字为 Generic 变体，跳过）；
            // FamilyName 内部字段私有 → 经 ToCss 序列化取名字符串（标准胶水）
            r.font_family = list.iter().find_map(|f| match f {
                lightningcss::properties::font::FontFamily::FamilyName(n) => n
                    .to_css_string(PrinterOptions::default())
                    .ok()
                    .map(|s| s.trim_matches('"').trim_matches('\'').to_string())
                    .filter(|s| !s.is_empty()),
                _ => None,
            });
        }
        P::LineHeight(lh) => {
            use lightningcss::properties::font::LineHeight as LLH;
            use lightningcss::values::length::LengthValue as LV;
            r.line_height = match lh {
                LLH::Number(n) => Some(LineH::Mult(*n)),
                LLH::Length(lp) => match lp {
                    // em 与无单位数值同义（相对自身 font-size）；rem 按根 16px 折算
                    CssLp::Dimension(LV::Em(v)) => Some(LineH::Mult(*v)),
                    CssLp::Dimension(LV::Rem(v)) => Some(LineH::Px(v * 16.0)),
                    CssLp::Dimension(other) => Some(LineH::Px(conv_len(other))),
                    CssLp::Percentage(p) => Some(LineH::Mult(p.0)),
                    // calc 百分比参照自身 font-size（字母序保证 font-size 先行解析）
                    CssLp::Calc(c) => calc_dim(c, r.font_size)
                        .map(|d| match d {
                            Dimension::Length(l) => LineH::Px(l),
                            _ => LineH::Mult(1.0),
                        }),
                },
                LLH::Normal => None,
            };
        }
        P::LetterSpacing(sp) => {
            use lightningcss::properties::text::Spacing as LSP;
            if let LSP::Length(l) = sp {
                r.letter_spacing = len_px(l);
            }
        }
        P::TextIndent(ti) => {
            // 百分比参照包含块宽度（basis = width）
            r.text_indent = lp_px(&ti.value, basis);
        }
        P::TextAlign(a) => {
            use lightningcss::properties::text::TextAlign as LTA;
            r.text_align = match a {
                LTA::Center => TextAlignP::Center,
                LTA::Right | LTA::End => TextAlignP::Right,
                LTA::Left | LTA::Start | LTA::Justify | LTA::MatchParent | LTA::JustifyAll => TextAlignP::Left,
            };
        }
        P::WhiteSpace(ws) => {
            use lightningcss::properties::text::WhiteSpace as LWS;
            r.nowrap = matches!(ws, LWS::NoWrap | LWS::Pre);
        }
        P::TextOverflow(to, _) => {
            use lightningcss::properties::overflow::TextOverflow as LTO;
            if matches!(to, LTO::Ellipsis) {
                r.ellipsis = true;
            }
        }
        _ => {}
    }
}

// ==================== 取值辅助（Property 枚举 → 绘制结构） ====================

/// lightningcss CssColor → RGBA（CurrentColor 回退 currentColor = 文本色；
/// 非 RGB 色域经 to_rgb 标准折算）
fn css_color(c: &CssColor, current: Rgba) -> Option<Rgba> {
    match c {
        CssColor::RGBA(c) => Some([c.red, c.green, c.blue, c.alpha]),
        CssColor::CurrentColor => Some(current),
        other => match other.to_rgb() {
            Ok(CssColor::RGBA(c)) => Some([c.red, c.green, c.blue, c.alpha]),
            _ => None,
        },
    }
}

/// background 简写里的颜色：缺省为透明，不记录（避免覆盖显式 background-color）
fn bg_shorthand_color(c: &CssColor, current: Rgba) -> Option<Rgba> {
    let is_transparent = |c: &lightningcss::values::color::RGBA| {
        c.red == 0 && c.green == 0 && c.blue == 0 && c.alpha == 0
    };
    match c {
        CssColor::RGBA(c) if !is_transparent(c) => Some([c.red, c.green, c.blue, c.alpha]),
        CssColor::CurrentColor => Some(current),
        other => match other.to_rgb() {
            Ok(CssColor::RGBA(c)) if !is_transparent(&c) => Some([c.red, c.green, c.blue, c.alpha]),
            _ => None,
        },
    }
}

/// shadow（box-shadow / text-shadow 共用形态）→ ShadowP
fn shadow_from(
    color: &CssColor,
    x: &Length,
    y: &Length,
    blur: &Length,
    spread: &Length,
    inset: bool,
    current: Rgba,
) -> ShadowP {
    ShadowP {
        x: len_px(x),
        y: len_px(y),
        blur: len_px(blur),
        spread: len_px(spread),
        color: css_color(color, current).unwrap_or([0, 0, 0, 0]),
        inset,
    }
}

/// 背景图 → BgPaint（none / image-set / conic 等不支持返回 None）
fn image_paint(img: &Image<'_>, current: Rgba) -> Option<BgPaint> {
    match img {
        Image::Url(u) => Some(BgPaint::Url(u.url.to_string())),
        Image::Gradient(g) => match &**g {
            Gradient::Linear(lin) | Gradient::RepeatingLinear(lin) => Some(BgPaint::Linear {
                deg: line_dir_deg(&lin.direction),
                stops: grad_stops(&lin.items, current),
            }),
            Gradient::Radial(rad) | Gradient::RepeatingRadial(rad) => {
                Some(BgPaint::Radial { stops: grad_stops(&rad.items, current) })
            }
            _ => None,
        },
        _ => None,
    }
}

/// LineDirection → 角度（deg，0 = 向上、顺时针；与 CSS 线性渐变语义一致）
fn line_dir_deg(d: &LineDirection) -> f32 {
    match d {
        LineDirection::Angle(a) => angle_deg(a),
        LineDirection::Horizontal(HorizontalPositionKeyword::Left) => 270.0,
        LineDirection::Horizontal(HorizontalPositionKeyword::Right) => 90.0,
        LineDirection::Vertical(VerticalPositionKeyword::Top) => 0.0,
        LineDirection::Vertical(VerticalPositionKeyword::Bottom) => 180.0,
        LineDirection::Corner { horizontal, vertical } => {
            // 角点方向按单位向量近似（CSS 语义与盒尺寸相关，此处取方形近似）
            let dx = match horizontal {
                HorizontalPositionKeyword::Left => -1.0,
                HorizontalPositionKeyword::Right => 1.0,
            };
            let dy = match vertical {
                VerticalPositionKeyword::Top => -1.0,
                VerticalPositionKeyword::Bottom => 1.0,
            };
            f32::atan2(dx, -dy).to_degrees()
        }
    }
}

fn angle_deg(a: &lightningcss::values::angle::Angle) -> f32 {
    use lightningcss::values::angle::Angle as A;
    const PI: f32 = std::f32::consts::PI;
    match a {
        A::Deg(v) => *v,
        A::Rad(v) => v * 180.0 / PI,
        A::Grad(v) => v * 360.0 / 400.0,
        A::Turn(v) => v * 360.0,
    }
}

fn angle_rad(a: &lightningcss::values::angle::Angle) -> f32 {
    angle_deg(a).to_radians()
}

enum StopPos {
    Pct(f32),
    Px(f32),
}

/// 渐变 stops → (0..1 位置, RGBA)；未指定位置按相邻锚点均分（首尾补 0/1）
fn grad_stops(
    items: &[GradientItem<CssLp>],
    current: Rgba,
) -> Vec<(f32, Rgba)> {
    let mut raw: Vec<(Option<StopPos>, Rgba)> = Vec::new();
    for it in items {
        // Hint（过渡提示）不产生 stop
        if let GradientItem::ColorStop(st) = it {
            let color = css_color(&st.color, current).unwrap_or([0, 0, 0, 0]);
            let pos = match &st.position {
                Some(CssLp::Percentage(p)) => Some(StopPos::Pct(p.0)),
                Some(CssLp::Dimension(lv)) => Some(StopPos::Px(conv_len(lv))),
                Some(CssLp::Calc(c)) => match calc_dim(c, 0.0) {
                    Some(Dimension::Length(l)) => Some(StopPos::Px(l)),
                    _ => None,
                },
                None => None,
            };
            raw.push((pos, color));
        }
    }
    // 像素位置归一：渐变总长在 resolve 阶段不可知，按最大像素位置缩放
    let span = raw
        .iter()
        .filter_map(|(p, _)| match p {
            Some(StopPos::Px(v)) => Some(*v),
            _ => None,
        })
        .fold(0.0f32, f32::max);
    let stops: Vec<(Option<f32>, Rgba)> = raw
        .into_iter()
        .map(|(p, c)| {
            let f = match p {
                Some(StopPos::Pct(v)) => Some(v.clamp(0.0, 1.0)),
                Some(StopPos::Px(v)) if span > 0.0 => Some((v / span).clamp(0.0, 1.0)),
                _ => None,
            };
            (f, c)
        })
        .collect();
    fill_stop_positions(stops)
}

/// 未指定位置的 stop 在相邻已知锚点间线性均分；首段前从 0 起、末段后补 1（CSS 渐变语义）
fn fill_stop_positions(stops: Vec<(Option<f32>, Rgba)>) -> Vec<(f32, Rgba)> {
    let n = stops.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![(0.0, stops.into_iter().next().unwrap().1)];
    }
    let known: Vec<(usize, f32)> = stops
        .iter()
        .enumerate()
        .filter_map(|(i, (p, _))| p.map(|v| (i, v)))
        .collect();
    let mut pos: Vec<f32> = vec![0.0; n];
    if known.is_empty() {
        for i in 0..n {
            pos[i] = i as f32 / (n - 1) as f32;
        }
    } else {
        for (i, v) in &known {
            pos[*i] = *v;
        }
        // 虚拟锚点：首锚点前 (-1, 0)、末锚点后 (n-1, 1)，各未知点在相邻锚点间线性插值
        let mut anchors: Vec<(f32, f32)> = known.iter().map(|(i, v)| (*i as f32, *v)).collect();
        if anchors[0].0 > 0.0 {
            anchors.insert(0, (-1.0, 0.0));
        }
        if (anchors[anchors.len() - 1].0 as usize) < n - 1 {
            anchors.push(((n - 1) as f32, 1.0));
        }
        for w in anchors.windows(2) {
            let (i0, p0) = (w[0].0, w[0].1);
            let (i1, p1) = (w[1].0, w[1].1);
            if i1 - i0 > 1.0 {
                for k in (i0 as usize + 1)..(i1 as usize) {
                    let t = (k as f32 - i0) / (i1 - i0);
                    pos[k] = (p0 + (p1 - p0) * t).clamp(0.0, 1.0);
                }
            }
        }
    }
    stops.into_iter().zip(pos).map(|((_, c), p)| (p, c)).collect()
}

/// lightningcss BackgroundSize → BgSize（单侧 auto 无法在接口内表达比例自适应，退化为 Auto）
fn bg_size(v: &lightningcss::properties::background::BackgroundSize) -> BgSize {
    use lightningcss::properties::background::BackgroundSize as BS;
    match v {
        BS::Contain => BgSize::Contain,
        BS::Cover => BgSize::Cover,
        BS::Explicit { width, height } => match (lpa_lenorpat(width), lpa_lenorpat(height)) {
            (Some(w), Some(h)) => BgSize::Val { w, h },
            _ => BgSize::Auto,
        },
    }
}

fn lpa_lenorpat(v: &LengthPercentageOrAuto) -> Option<LengthOrPct> {
    match v {
        LengthPercentageOrAuto::Auto => None,
        LengthPercentageOrAuto::LengthPercentage(lp) => Some(match lp {
            CssLp::Dimension(lv) => LengthOrPct::Px(conv_len(lv)),
            CssLp::Percentage(p) => LengthOrPct::Pct(p.0),
            // calc 百分比参照盒尺寸不可知，取像素部分
            CssLp::Calc(c) => match calc_dim(c, 0.0) {
                Some(Dimension::Length(l)) => LengthOrPct::Px(l),
                Some(Dimension::Percent(p)) => LengthOrPct::Pct(p),
                _ => LengthOrPct::Px(0.0),
            },
        }),
    }
}

fn lp_bgpos(lp: &CssLp) -> BgPosComp {
    match lp {
        CssLp::Dimension(lv) => BgPosComp::Px(conv_len(lv)),
        CssLp::Percentage(p) => BgPosComp::Pct(p.0),
        CssLp::Calc(c) => match calc_dim(c, 0.0) {
            Some(Dimension::Length(l)) => BgPosComp::Px(l),
            Some(Dimension::Percent(p)) => BgPosComp::Pct(p),
            _ => BgPosComp::Px(0.0),
        },
    }
}

/// 背景水平位置（Right 带偏移的语义接口无法表达，按偏移值近似）
fn bg_pos_h(pc: &lightningcss::values::position::HorizontalPosition) -> BgPosComp {
    use lightningcss::values::position::{HorizontalPositionKeyword as H, PositionComponent as PC};
    match pc {
        PC::Center => BgPosComp::Center,
        PC::Length(lp) => lp_bgpos(lp),
        PC::Side { side: H::Left, offset } => offset.as_ref().map(lp_bgpos).unwrap_or(BgPosComp::Left),
        PC::Side { side: H::Right, offset } => offset.as_ref().map(lp_bgpos).unwrap_or(BgPosComp::Right),
    }
}

/// 背景垂直位置（Bottom 带偏移的语义接口无法表达，按偏移值近似）
fn bg_pos_v(pc: &lightningcss::values::position::VerticalPosition) -> BgPosComp {
    use lightningcss::values::position::{VerticalPositionKeyword as V, PositionComponent as PC};
    match pc {
        PC::Center => BgPosComp::Center,
        PC::Length(lp) => lp_bgpos(lp),
        PC::Side { side: V::Top, offset } => offset.as_ref().map(lp_bgpos).unwrap_or(BgPosComp::Top),
        PC::Side { side: V::Bottom, offset } => offset.as_ref().map(lp_bgpos).unwrap_or(BgPosComp::Bottom),
    }
}

fn bg_repeat(rp: &lightningcss::properties::background::BackgroundRepeat) -> BgRepeat {
    use lightningcss::properties::background::BackgroundRepeatKeyword as K;
    match (&rp.x, &rp.y) {
        (K::Repeat, K::Repeat) => BgRepeat::Repeat,
        (K::Repeat, K::NoRepeat) => BgRepeat::RepeatX,
        (K::NoRepeat, K::Repeat) => BgRepeat::RepeatY,
        (K::NoRepeat, K::NoRepeat) => BgRepeat::NoRepeat,
        // space / round 近似为 repeat
        _ => BgRepeat::Repeat,
    }
}

/// 边框宽度关键字 → px（CSS 约定 thin=1 / medium=3 / thick=5）
fn border_side_width(v: &lightningcss::properties::border::BorderSideWidth) -> f32 {
    use lightningcss::properties::border::BorderSideWidth as BW;
    match v {
        BW::Thin => 1.0,
        BW::Medium => 3.0,
        BW::Thick => 5.0,
        BW::Length(l) => len_px(l),
    }
}

/// border-style none / hidden 的侧不绘制
fn border_style_hidden(s: &lightningcss::properties::border::LineStyle) -> bool {
    use lightningcss::properties::border::LineStyle as LS;
    matches!(s, LS::None | LS::Hidden)
}

/// border-top/right/bottom/left 简写（GenericBorder）落到单侧
fn border_side<const P: u8>(
    r: &mut Resolved,
    i: usize,
    b: &lightningcss::properties::border::GenericBorder<lightningcss::properties::border::LineStyle, P>,
) {
    r.border_hidden[i] = border_style_hidden(&b.style);
    r.border_raw[i] = border_side_width(&b.width);
    let c = css_color(&b.color, r.color).unwrap_or([0, 0, 0, 0]);
    set_side_color(r, i, c);
}

fn set_side_width(r: &mut Resolved, i: usize, w: f32) {
    let target = match i {
        0 => &mut r.border.top.width,
        1 => &mut r.border.right.width,
        2 => &mut r.border.bottom.width,
        _ => &mut r.border.left.width,
    };
    *target = w;
}

fn set_side_color(r: &mut Resolved, i: usize, c: Rgba) {
    let target = match i {
        0 => &mut r.border.top.color,
        1 => &mut r.border.right.color,
        2 => &mut r.border.bottom.color,
        _ => &mut r.border.left.color,
    };
    *target = c;
}

/// 圆角折算（Size2D 取水平分量；百分比按盒宽近似折算）
fn radius_px(sz: &CssLp, basis: f32) -> f32 {
    lp_px(sz, basis).max(0.0)
}

/// LengthPercentage → px（百分比 / calc 按 basis 折算）
fn lp_px(lp: &CssLp, basis: f32) -> f32 {
    match lp {
        CssLp::Dimension(lv) => conv_len(lv),
        CssLp::Percentage(p) => p.0 * basis,
        CssLp::Calc(c) => match calc_dim(c, basis) {
            Some(Dimension::Length(l)) => l,
            _ => 0.0,
        },
    }
}

/// lightningcss Length（无百分比）→ px
fn len_px(l: &Length) -> f32 {
    match l {
        Length::Value(lv) => conv_len(lv),
        Length::Calc(c) => match calc_dim(c, 0.0) {
            Some(Dimension::Length(v)) => v,
            _ => 0.0,
        },
    }
}

/// 2D 仿射矩阵乘法：transform 序列按序组合 M = M1 × M2（p' = M1(M2(p))）
fn mat_mul(m1: &[f32; 6], m2: &[f32; 6]) -> [f32; 6] {
    let [a1, b1, c1, d1, e1, f1] = *m1;
    let [a2, b2, c2, d2, e2, f2] = *m2;
    [
        a1 * a2 + c1 * b2,
        b1 * a2 + d1 * b2,
        a1 * c2 + c1 * d2,
        b1 * c2 + d1 * d2,
        a1 * e2 + c1 * f2 + e1,
        b1 * e2 + d1 * f2 + f1,
    ]
}

/// 单个 transform 函数 → [a, b, c, d, e, f]（列主序仿射：sx, kx, ky, sy, tx, ty）
fn mat_of(t: &lightningcss::properties::transform::Transform) -> [f32; 6] {
    use lightningcss::properties::transform::Transform as T;
    match t {
        T::Translate(x, y) => [1.0, 0.0, 0.0, 1.0, lp_translate(x), lp_translate(y)],
        T::TranslateX(x) => [1.0, 0.0, 0.0, 1.0, lp_translate(x), 0.0],
        T::TranslateY(y) => [1.0, 0.0, 0.0, 1.0, 0.0, lp_translate(y)],
        T::Translate3d(x, y, _) => [1.0, 0.0, 0.0, 1.0, lp_translate(x), lp_translate(y)],
        T::TranslateZ(_) | T::ScaleZ(_) | T::Perspective(_) => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
        T::Scale(x, y) => [nop(x), 0.0, 0.0, nop(y), 0.0, 0.0],
        T::ScaleX(x) => [nop(x), 0.0, 0.0, 1.0, 0.0, 0.0],
        T::ScaleY(y) => [1.0, 0.0, 0.0, nop(y), 0.0, 0.0],
        T::Scale3d(x, y, _) => [nop(x), 0.0, 0.0, nop(y), 0.0, 0.0],
        T::Rotate(a) | T::RotateZ(a) => {
            let (s, c) = angle_rad(a).sin_cos();
            [c, s, -s, c, 0.0, 0.0]
        }
        // 3D 旋转在 2D 光栅下近似为对应轴向的缩放（RotateX 压 y、RotateY 压 x）
        T::RotateX(a) => [1.0, 0.0, 0.0, angle_rad(a).cos(), 0.0, 0.0],
        T::RotateY(a) => [angle_rad(a).cos(), 0.0, 0.0, 1.0, 0.0, 0.0],
        T::Rotate3d(_, _, _, a) => {
            let (s, c) = angle_rad(a).sin_cos();
            [c, s, -s, c, 0.0, 0.0]
        }
        T::Skew(x, y) => [1.0, angle_rad(y).tan(), angle_rad(x).tan(), 1.0, 0.0, 0.0],
        T::SkewX(x) => [1.0, 0.0, angle_rad(x).tan(), 1.0, 0.0, 0.0],
        T::SkewY(y) => [1.0, angle_rad(y).tan(), 0.0, 1.0, 0.0, 0.0],
        T::Matrix(m) => [m.a, m.b, m.c, m.d, m.e, m.f],
        // 3D 矩阵按 2D 投影近似（m11/m12/m21/m22/e/f）
        T::Matrix3d(m) => [m.m11, m.m12, m.m21, m.m22, m.m41, m.m42],
        T::Rotate3d(_, _, _, a) => {
            let rad = a.to_radians();
            [rad.cos(), -rad.sin(), rad.sin(), rad.cos(), 0.0, 0.0]
        }
        T::RotateX(_) | T::RotateY(_) => [1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
    }
}

/// translate 折算：百分比参照自身尺寸（resolve 阶段不可知）按 0 处理，仅 px/绝对单位生效
fn lp_translate(lp: &CssLp) -> f32 {
    match lp {
        CssLp::Dimension(lv) => conv_len(lv),
        CssLp::Percentage(_) => 0.0,
        CssLp::Calc(c) => match calc_dim(c, 0.0) {
            Some(Dimension::Length(l)) => l,
            _ => 0.0,
        },
    }
}

/// NumberOrPercentage → f32（百分比取 0..1 值）
fn nop(v: &NumberOrPercentage) -> f32 {
    match v {
        NumberOrPercentage::Number(n) => *n,
        NumberOrPercentage::Percentage(p) => p.0,
    }
}

/// transform-origin 水平分量 → 0..1 比例（px 值按 100px 基准近似归一，极罕见场景）
fn origin_comp_h(pc: &lightningcss::values::position::HorizontalPosition) -> f32 {
    use lightningcss::values::position::{HorizontalPositionKeyword as H, PositionComponent as PC};
    let from_lp = |lp: &CssLp| match lp {
        CssLp::Percentage(p) => p.0.clamp(0.0, 1.0),
        CssLp::Dimension(lv) => (conv_len(lv) / 100.0).clamp(0.0, 1.0),
        CssLp::Calc(_) => 0.5,
    };
    match pc {
        PC::Center => 0.5,
        PC::Length(lp) => from_lp(lp),
        PC::Side { side: H::Left, offset } => offset.as_ref().map(from_lp).unwrap_or(0.0),
        PC::Side { side: H::Right, offset } => offset.as_ref().map(from_lp).unwrap_or(1.0),
    }
}

/// transform-origin 垂直分量 → 0..1 比例（px 值按 100px 基准近似归一，极罕见场景）
fn origin_comp_v(pc: &lightningcss::values::position::VerticalPosition) -> f32 {
    use lightningcss::values::position::{VerticalPositionKeyword as V, PositionComponent as PC};
    let from_lp = |lp: &CssLp| match lp {
        CssLp::Percentage(p) => p.0.clamp(0.0, 1.0),
        CssLp::Dimension(lv) => (conv_len(lv) / 100.0).clamp(0.0, 1.0),
        CssLp::Calc(_) => 0.5,
    };
    match pc {
        PC::Center => 0.5,
        PC::Length(lp) => from_lp(lp),
        PC::Side { side: V::Top, offset } => offset.as_ref().map(from_lp).unwrap_or(0.0),
        PC::Side { side: V::Bottom, offset } => offset.as_ref().map(from_lp).unwrap_or(1.0),
    }
}

/// calc 值 → 按 basis 折算的 Dimension（无法折算时 None）
fn calc_dim(v: &impl lightningcss::traits::ToCss, basis: f32) -> Option<Dimension> {
    let s = v.to_css_string(PrinterOptions::default()).ok()?;
    let (pct, px) = parse_calc_pct_px(&s)?;
    Some(Dimension::Length(basis * pct + px))
}

/// lightningcss DimensionPercentage<LengthValue> → taffy LengthPercentage
fn tlp(v: &lightningcss::values::percentage::DimensionPercentage<lightningcss::values::length::LengthValue>, basis: f32) -> LengthPercentage {
    use lightningcss::values::length::LengthValue;
    use lightningcss::values::percentage::DimensionPercentage as DP;
    match v {
        DP::Dimension(lv) => LengthPercentage::Length(conv_len(lv)),
        DP::Percentage(p) => LengthPercentage::Percent(p.0),
        DP::Calc(c) => match calc_dim(c, basis) {
            Some(Dimension::Length(l)) => LengthPercentage::Length(l),
            Some(Dimension::Percent(p)) => LengthPercentage::Percent(p),
            _ => LengthPercentage::Length(0.0),
        },
    }
}

fn conv_len(lv: &lightningcss::values::length::LengthValue) -> f32 {
    use lightningcss::values::length::LengthValue::*;
    match lv {
        Px(v) => *v,
        Em(v) => v * 16.0,
        Rem(v) => v * 16.0,
        Pt(v) => v * 96.0 / 72.0,
        Cm(v) => v * 96.0 / 2.54,
        Mm(v) => v * 96.0 / 25.4,
        In(v) => v * 96.0,
        Q(v) => v * 96.0 / 101.6,
        Pc(v) => v * 16.0,
        _ => 0.0,
    }
}

/// lightningcss DimensionPercentage<LengthValue> → taffy Dimension（auto 不会出现在此类型）
fn tdim(v: &lightningcss::values::percentage::DimensionPercentage<lightningcss::values::length::LengthValue>, basis: f32) -> Dimension {
    match tlp(v, basis) {
        LengthPercentage::Length(l) => Dimension::Length(l),
        LengthPercentage::Percent(p) => Dimension::Percent(p),
    }
}

/// lightningcss LengthPercentageOrAuto → taffy LengthPercentageAuto
fn tlpa(v: &lightningcss::values::length::LengthPercentageOrAuto, basis: f32) -> LengthPercentageAuto {
    use lightningcss::values::length::LengthPercentageOrAuto as LPA;
    match v {
        LPA::Auto => LengthPercentageAuto::Auto,
        LPA::LengthPercentage(lp) => match tlp(lp, basis) {
            LengthPercentage::Length(l) => LengthPercentageAuto::Length(l),
            LengthPercentage::Percent(p) => LengthPercentageAuto::Percent(p),
        },
    }
}

/// lightningcss Size（Auto | LengthPercentage | 关键字）→ taffy Dimension
fn size_dim(v: &lightningcss::properties::size::Size, basis: f32) -> Dimension {
    use lightningcss::properties::size::Size as LS;
    match v {
        LS::Auto => Dimension::Auto,
        LS::LengthPercentage(lp) => tdim(lp, basis),
        _ => Dimension::Auto,
    }
}

fn maxsize_dim(v: &lightningcss::properties::size::MaxSize, basis: f32) -> Dimension {
    use lightningcss::properties::size::MaxSize as LM;
    match v {
        LM::None => Dimension::Auto,
        LM::LengthPercentage(lp) => tdim(lp, basis),
        _ => Dimension::Auto,
    }
}
fn gap_val(v: &lightningcss::properties::align::GapValue, basis: f32) -> LengthPercentage {
    use lightningcss::properties::align::GapValue as GV;
    match v {
        GV::Normal => LengthPercentage::Length(0.0),
        GV::LengthPercentage(lp) => tlp(lp, basis),
    }
}
fn ov_str(s: &str) -> Overflow {
    match s.trim() {
        "hidden" | "clip" => Overflow::Hidden,
        "scroll" | "auto" => Overflow::Scroll,
        _ => Overflow::Visible,
    }
}


trait IntoLp {
    fn into_lp(self) -> LengthPercentage;
}
impl IntoLp for LengthPercentageAuto {
    fn into_lp(self) -> LengthPercentage {
        match self {
            LengthPercentageAuto::Length(l) => LengthPercentage::Length(l),
            LengthPercentageAuto::Percent(p) => LengthPercentage::Percent(p),
            LengthPercentageAuto::Auto => LengthPercentage::Length(0.0),
        }
    }
}
trait IntoDim {
    fn into_dim(self) -> Dimension;
}
impl IntoDim for LengthPercentageAuto {
    fn into_dim(self) -> Dimension {
        match self {
            LengthPercentageAuto::Length(l) => Dimension::Length(l),
            LengthPercentageAuto::Percent(p) => Dimension::Percent(p),
            LengthPercentageAuto::Auto => Dimension::Auto,
        }
    }
}
