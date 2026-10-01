//! 全量 CSS → taffy Style 映射
//!
//! 值解析统一走 Lightning CSS 的 Property::parse_string（结构化），
//! 属性名→PropertyId 用白名单（alpha.72 的 from_name_and_prefix 为私有 API）。
//! calc() 经序列化后用 parse_calc_pct_px 按 basis 折算。

use super::dom::StyleNode;
use super::layout::parse_calc_pct_px;
use lightningcss::properties::Property;
use lightningcss::stylesheet::ParserOptions;
use lightningcss::printer::PrinterOptions;
use lightningcss::traits::ToCss;
use lightningcss::vendor_prefix::VendorPrefix;
use taffy::geometry::Point;
use taffy::prelude::*;
use taffy::style::Overflow;

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
        _ => return None,
    })
}

/// 解析单个 CSS 属性声明为 lightningcss Property（结构化值）
pub fn parse_prop<'a>(name: &str, value: &'a str) -> Option<Property<'a>> {
    let pid = property_id_of(name)?;
    Property::parse_string(pid, value, ParserOptions::default()).ok()
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

/// 解析后的属性集（basis 为百分比参照宽，一般为父容器宽）
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
            use lightningcss::properties::display::{Display as LD, DisplayInside};
            r.display = match d {
                LD::Keyword(_) => Display::None,
                LD::Pair(p) => match p.inside {
                    DisplayInside::Flex(_) => Display::Flex,
                    DisplayInside::Grid => Display::Grid,
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
        _ => {}
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
