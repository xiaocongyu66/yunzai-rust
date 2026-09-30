//! 全量 CSS → taffy Style 映射
//!
//! 值解析统一走 Lightning CSS 的 Property::parse_string（结构化），
//! 不再逐属性手写 parser。属性名到 PropertyId 用白名单（alpha.72 的
//! from_name_and_prefix 为私有 API）。

use super::dom::StyleNode;
use lightningcss::properties::Property;
use lightningcss::stylesheet::ParserOptions;
use taffy::geometry::Point;
use taffy::prelude::*;

/// 属性名 → lightningcss PropertyId（白名单）
fn property_id_of(name: &str) -> Option<lightningcss::properties::PropertyId<'static>> {
    use lightningcss::properties::{PropertyId, VendorPrefix};
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
        "overflow-x" => PropertyId::OverflowX,
        "overflow-y" => PropertyId::OverflowY,
        "gap" => PropertyId::Gap,
        "row-gap" => PropertyId::RowGap,
        "column-gap" => PropertyId::ColumnGap,
        "z-index" => PropertyId::ZIndex,
        "flex" => PropertyId::Flex,
        "flex-grow" => PropertyId::FlexGrow,
        "flex-shrink" => PropertyId::FlexShrink,
        "flex-basis" => PropertyId::FlexBasis(VendorPrefix::None),
        "flex-direction" => PropertyId::FlexDirection,
        "flex-wrap" => PropertyId::FlexWrap,
        "justify-content" => PropertyId::JustifyContent,
        "align-items" => PropertyId::AlignItems,
        "align-content" => PropertyId::AlignContent,
        _ => return None,
    })
}

/// 解析单个 CSS 属性声明为 lightningcss Property（结构化值）
pub fn parse_prop(name: &str, value: &str) -> Option<Property<'static>> {
    let pid = property_id_of(name)?;
    Property::parse_string(pid, value, ParserOptions::default()).ok()
}

/// lightningcss LengthPercentage → taffy LengthPercentage
fn tlp(v: &lightningcss::values::percentage::DimensionPercentage<lightningcss::values::length::Length>) -> LengthPercentage {
    use lightningcss::values::percentage::DimensionPercentage as DP;
    match v {
        DP::Dimension(l) => match l.value {
            lightningcss::values::length::LengthValue::Px(v) => LengthPercentage::Length(v),
            lightningcss::values::length::LengthValue::Em(v) => LengthPercentage::Length(v * 16.0),
            lightningcss::values::length::LengthValue::Rem(v) => LengthPercentage::Length(v * 16.0),
            lightningcss::values::length::LengthValue::Pt(v) => LengthPercentage::Length(v * 96.0 / 72.0),
            _ => LengthPercentage::Length(0.0),
        },
        DP::Percentage(p) => LengthPercentage::Percent(p.0),
        DP::Calc(_) => LengthPercentage::Length(0.0),
    }
}

/// lightningcss LengthPercentageOrAuto → taffy LengthPercentageAuto
fn tlpa(v: &lightningcss::values::percentage::DimensionPercentage<lightningcss::values::length::LengthPercentage>) -> LengthPercentageAuto {
    use lightningcss::values::percentage::DimensionPercentage as DP;
    match v {
        DP::Dimension(lp) => match lp.value {
            lightningcss::values::length::LengthValue::Px(v) => LengthPercentageAuto::Length(v),
            lightningcss::values::length::LengthValue::Em(v) => LengthPercentageAuto::Length(v * 16.0),
            lightningcss::values::length::LengthValue::Rem(v) => LengthPercentageAuto::Length(v * 16.0),
            _ => LengthPercentageAuto::Length(0.0),
        },
        DP::Percentage(p) => LengthPercentageAuto::Percent(p.0),
        DP::Calc(_) => LengthPercentageAuto::Length(0.0),
    }
}

/// 解析后的属性集
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
        }
    }
}

pub fn resolve(n: &StyleNode) -> Resolved {
    let mut r = Resolved::default();
    for (k, v) in n.decls.iter() {
        if let Some(p) = parse_prop(k, v) {
            apply(k, &p, &mut r);
        }
    }
    r
}

fn apply(name: &str, p: &Property, r: &mut Resolved) {
    use lightningcss::properties::Property as P;
    let dim_of = |v: &lightningcss::values::percentage::DimensionPercentage<lightningcss::values::length::Length>| match tlp(v) {
        LengthPercentage::Length(l) => Dimension::Length(l),
        LengthPercentage::Percent(p) => Dimension::Percent(p),
    };
    let lp_dim = |v: &lightningcss::values::percentage::DimensionPercentage<lightningcss::values::length::LengthPercentage>| match tlpa(v) {
        LengthPercentageAuto::Length(l) => Dimension::Length(l),
        LengthPercentageAuto::Percent(p) => Dimension::Percent(p),
        LengthPercentageAuto::Auto => Dimension::Auto,
    };
    match p {
        P::Width(v) => r.width = lp_dim(v),
        P::Height(v) => r.height = lp_dim(v),
        P::MinWidth(v) => r.min_width = dim_of(&dp_to_len(v)),
        P::MinHeight(v) => r.min_height = dim_of(&dp_to_len(v)),
        P::MaxWidth(v) => r.max_width = dim_of(&dp_to_len(v)),
        P::MaxHeight(v) => r.max_height = dim_of(&dp_to_len(v)),
        P::Margin(m) => {
            r.margin.top = tlpa(&m.top);
            r.margin.right = tlpa(&m.right);
            r.margin.bottom = tlpa(&m.bottom);
            r.margin.left = tlpa(&m.left);
        }
        P::Padding(pd) => {
            r.padding.top = tlp(&dp_to_len(&pd.top));
            r.padding.right = tlp(&dp_to_len(&pd.right));
            r.padding.bottom = tlp(&dp_to_len(&pd.bottom));
            r.padding.left = tlp(&dp_to_len(&pd.left));
        }
        P::Top(v) => r.inset.top = tlpa(v),
        P::Right(v) => r.inset.right = tlpa(v),
        P::Bottom(v) => r.inset.bottom = tlpa(v),
        P::Left(v) => r.inset.left = tlpa(v),
        P::Display(d) => {
            use lightningcss::properties::display::{Display as LD, DisplayInside};
            r.display = match d {
                LD::None => Display::None,
                LD::Inside(DisplayInside::Flex(_)) => Display::Flex,
                LD::Inside(DisplayInside::Grid(_)) => Display::Grid,
                _ => Display::Flex,
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
            r.overflow.x = ovk(&o.x);
            r.overflow.y = ovk(&o.y);
        }
        P::Gap(g) => {
            r.gap.width = tlp(&dp_to_len(&g.row));
            r.gap.height = tlp(&dp_to_len(&g.column));
        }
        P::RowGap(v) => r.gap.width = tlp(&dp_to_len(v)),
        P::ColumnGap(v) => r.gap.height = tlp(&dp_to_len(v)),
        P::ZIndex(z) => {
            if let lightningcss::properties::position::ZIndex::Integer(i) = z {
                r.z_index = *i;
            }
        }
        P::FlexGrow(v) => r.flex_grow = *v,
        P::FlexShrink(v) => r.flex_shrink = *v,
        P::FlexBasis(v, _) => r.flex_basis = lp_dim(v),
        P::FlexDirection(d) => {
            use lightningcss::properties::flex::FlexDirection::*;
            r.flex_direction = match d {
                Row => FlexDirection::Row,
                RowReverse => FlexDirection::RowReverse,
                Column => FlexDirection::Column,
                ColumnReverse => FlexDirection::ColumnReverse,
            };
        }
        P::FlexWrap(w) => {
            use lightningcss::properties::flex::FlexWrap::*;
            r.flex_wrap = match w {
                Wrap => FlexWrap::Wrap,
                WrapReverse => FlexWrap::WrapReverse,
                NoWrap => FlexWrap::NoWrap,
            };
        }
        P::JustifyContent(j) => {
            use lightningcss::properties::align::AlignContent as LJ;
            r.justify_content = match j {
                LJ::FlexStart => Some(JustifyContent::FlexStart),
                LJ::FlexEnd => Some(JustifyContent::FlexEnd),
                LJ::Center => Some(JustifyContent::Center),
                LJ::SpaceBetween => Some(JustifyContent::SpaceBetween),
                LJ::SpaceAround => Some(JustifyContent::SpaceAround),
                LJ::SpaceEvenly => Some(JustifyContent::SpaceEvenly),
                LJ::Stretch => Some(JustifyContent::Stretch),
                _ => None,
            };
        }
        P::AlignItems(a) => {
            use lightningcss::properties::align::AlignItems as LA;
            r.align_items = match a {
                LA::FlexStart => Some(AlignItems::FlexStart),
                LA::FlexEnd => Some(AlignItems::FlexEnd),
                LA::Center => Some(AlignItems::Center),
                LA::Baseline => Some(AlignItems::Baseline),
                LA::Stretch => Some(AlignItems::Stretch),
                _ => None,
            };
        }
        P::AlignContent(a) => {
            use lightningcss::properties::align::AlignContent as LC;
            r.align_content = match a {
                LC::FlexStart => Some(AlignContent::FlexStart),
                LC::FlexEnd => Some(AlignContent::FlexEnd),
                LC::Center => Some(AlignContent::Center),
                LC::SpaceBetween => Some(AlignContent::SpaceBetween),
                LC::SpaceAround => Some(AlignContent::SpaceAround),
                LC::SpaceEvenly => Some(AlignContent::SpaceEvenly),
                LC::Stretch => Some(AlignContent::Stretch),
                _ => None,
            };
        }
        _ => {}
    }
    let _ = name;
}

/// LengthPercentage 形态的值转 Length 形态（百分比保留）
fn dp_to_len<'a>(
    v: &'a lightningcss::values::percentage::DimensionPercentage<lightningcss::values::length::LengthPercentage>,
) -> lightningcss::values::percentage::DimensionPercentage<lightningcss::values::length::Length> {
    use lightningcss::values::percentage::DimensionPercentage as DP;
    match v {
        DP::Dimension(lp) => match &lp.value {
            lightningcss::values::length::LengthValue::Px(v) => {
                DP::Dimension(lightningcss::values::length::Length::px(*v))
            }
            _ => DP::Calc(Default::default()),
        },
        DP::Percentage(p) => DP::Percentage(*p),
        DP::Calc(_) => DP::Calc(Default::default()),
    }
}

fn ovk(v: &lightningcss::properties::overflow::OverflowKeyword) -> Overflow {
    use lightningcss::properties::overflow::OverflowKeyword::*;
    match v {
        Hidden | Clip => Overflow::Hidden,
        Scroll | Auto => Overflow::Scroll,
        Visible => Overflow::Visible,
    }
}
