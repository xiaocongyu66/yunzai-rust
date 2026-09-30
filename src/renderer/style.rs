//! 全量 CSS → taffy Style 映射
//!
//! 值解析统一走 Lightning CSS 的 Property::parse_string（结构化），
//! 不再逐属性手写 parser。这里只做 Property → taffy 字段的搬运。

use super::dom::StyleNode;
use lightningcss::properties::Property;
use lightningcss::stylesheet::ParserOptions;
use taffy::prelude::*;

/// 解析单个 CSS 属性声明为 lightningcss Property（结构化值）
pub fn parse_prop(name: &str, value: &str) -> Option<Property<'static>> {
    let pid = property_id_of(name)?;
    Property::parse_string(pid, value, ParserOptions::default()).ok()
}

/// 属性名 → lightningcss PropertyId（静态）
fn property_id_of(name: &str) -> Option<lightningcss::properties::PropertyId<'static>> {
    use lightningcss::properties::{PropertyId, VendorPrefix};
    unsafe {
        let ext = std::mem::transmute::<&str, &'static str>(name);
        PropertyId::from_name_and_prefix(ext, VendorPrefix::None).ok()
    }
}

/// lightningcss LengthPercentage → taffy LengthPercentage
fn tlp(v: &lightningcss::values::length::LengthPercentage) -> LengthPercentage {
    use lightningcss::values::length::{Length, LengthValue};
    match v {
        LengthPercentage::Length(l) => match l {
            Length::Value(lv) => LengthPercentage::Length(tlen(lv)),
            Length::Calc(_) => LengthPercentage::Length(0.0),
        },
        LengthPercentage::Percentage(p) => LengthPercentage::Percent(p.0),
    }
}

fn tlen(lv: &lightningcss::values::length::LengthValue) -> f32 {
    use lightningcss::values::length::LengthValue::*;
    match lv {
        Px(v) => *v,
        Em(v) => v * 16.0,
        Rem(v) => v * 16.0,
        Pt(v) => v * 96.0 / 72.0,
        Percent(_) => 0.0,
        _ => 0.0,
    }
}

/// lightningcss LengthPercentageOrAuto → taffy LengthPercentageAuto
fn tlpa(v: &lightningcss::values::length::LengthPercentageOrAuto) -> LengthPercentageAuto {
    use lightningcss::values::length::LengthPercentageOrAuto;
    match v {
        LengthPercentageOrAuto::LengthPercentage(lp) => match tlp(lp) {
            LengthPercentage::Length(l) => LengthPercentageAuto::Length(l),
            LengthPercentage::Percent(p) => LengthPercentageAuto::Percent(p),
        },
        LengthPercentageOrAuto::Auto => LengthPercentageAuto::Auto,
    }
}

/// 解析后的属性集 → taffy 尺寸类字段
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

pub fn resolve(n: &StyleNode) -> Resolved {
    let mut r = Resolved {
        width: Dimension::Auto,
        height: Dimension::Auto,
        min_width: Dimension::Auto,
        min_height: Dimension::Auto,
        max_width: Dimension::Auto,
        max_height: Dimension::Auto,
        margin: Rect::auto(),
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
    };
    // 声明以 (name, value) 字符串表进来；先转结构化 Property
    let props: Vec<(String, Property<'static>)> = n
        .decls
        .iter()
        .filter_map(|(k, v)| parse_prop(k, v).map(|p| (k.clone(), p)))
        .collect();
    for (name, p) in &props {
        apply(name, p, &mut r);
    }
    r
}

fn apply(name: &str, p: &Property, r: &mut Resolved) {
    use lightningcss::properties::Property as P;
    let lpa_dim = |v: &lightningcss::values::length::LengthPercentageOrAuto| match tlpa(v) {
        LengthPercentageAuto::Length(l) => Dimension::Length(l),
        LengthPercentageAuto::Percent(p) => Dimension::Percent(p),
        LengthPercentageAuto::Auto => Dimension::Auto,
    };
    let lp_dim = |v: &lightningcss::values::length::LengthPercentage| match tlp(v) {
        LengthPercentage::Length(l) => Dimension::Length(l),
        LengthPercentage::Percent(p) => Dimension::Percent(p),
    };
    let lpp = tlp;
    match p {
        P::Width(v) => r.width = lp_dim(&v.0),
        P::Height(v) => r.height = lp_dim(&v.0),
        P::MinWidth(v) => r.min_width = lp_dim(&v.0),
        P::MinHeight(v) => r.min_height = lp_dim(&v.0),
        P::MaxWidth(v) => r.max_width = lp_dim(&v.0),
        P::MaxHeight(v) => r.max_height = lp_dim(&v.0),
        P::Margin(m) => {
            r.margin.top = tlpa(&m.top);
            r.margin.right = tlpa(&m.right);
            r.margin.bottom = tlpa(&m.bottom);
            r.margin.left = tlpa(&m.left);
        }
        P::Padding(pd) => {
            r.padding.top = tlp(&pd.top);
            r.padding.right = tlp(&pd.right);
            r.padding.bottom = tlp(&pd.bottom);
            r.padding.left = tlp(&pd.left);
        }
        P::Top(v) => r.inset.top = tlpa(v),
        P::Right(v) => r.inset.right = tlpa(v),
        P::Bottom(v) => r.inset.bottom = tlpa(v),
        P::Left(v) => r.inset.left = tlpa(v),
        P::Display(d) => {
            use lightningcss::properties::display::{Display as LD, DisplayInside};
            r.display = match d {
                LD::None => Display::None,
                LD::Inside(DisplayInside::Flex) => Display::Flex,
                LD::Inside(DisplayInside::Grid) => Display::Grid,
                _ => Display::Flex,
            };
        }
        P::Position(pos) => {
            use lightningcss::properties::positioning::Position as LP2;
            r.position = match pos {
                LP2::Absolute => Position::Absolute,
                LP2::Fixed => Position::Absolute,
                _ => Position::Relative,
            };
        }
        P::Overflow(o) => {
            r.overflow.x = ovk(&o.x);
            r.overflow.y = ovk(&o.y);
        }
        P::Gap(g) => {
            r.gap.width = tlp(&g.row);
            r.gap.height = tlp(&g.column);
        }
        P::RowGap(v) => r.gap.width = tlp(v),
        P::ColumnGap(v) => r.gap.height = tlp(v),
        P::ZIndex(z) => {
            if let lightningcss::values::integer::IntegerOrAuto::Integer(i) = z {
                r.z_index = *i;
            }
        }
        P::FlexGrow(v) => r.flex_grow = *v,
        P::FlexShrink(v) => r.flex_shrink = *v,
        P::FlexBasis(v, _) => r.flex_basis = lpa_dim(v),
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

fn ovk(v: &lightningcss::properties::overflow::OverflowKeyword) -> Overflow {
    use lightningcss::properties::overflow::OverflowKeyword::*;
    match v {
        Hidden | Clip => Overflow::Hidden,
        Scroll | Auto => Overflow::Scroll,
        Visible => Overflow::Visible,
    }
}
