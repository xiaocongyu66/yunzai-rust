//! 选择器匹配：Servo（Firefox 引擎）同款——实现 parcel_selectors 的 Element trait，
//! 匹配交给 selectors crate 的 matches_selector（组合子/伪类/nth 全由成熟实现处理）。
//! lightningcss 解析好的选择器经 convert_selector 结构转译（零字符串）后参与匹配。

use crate::renderer::dom::StyleNode;
use parcel_selectors::context::{MatchingContext, MatchingMode, QuirksMode};
use parcel_selectors::matching::matches_selector;
use parcel_selectors::parser::{
    Combinator, Component, Selector, SelectorImpl, NthSelectorData, NthType,
};
use parcel_selectors::{Element, OpaqueElement};
use parcel_selectors::attr::CaseSensitivity;
use std::fmt;
use std::rc::Rc;

/// parcel_selectors 的 SelectorImpl 实现（Identifier/LocalName 用 String）。
/// lightningcss 的 Selectors 类型私有，这里定义同构 Impl：
/// lightningcss 解析好的 Component 结构转译成本 Impl 的 Component（零字符串），
/// 匹配完全交给 parcel_selectors 的 matches_selector（Servo 同款）。
#[derive(Debug, Clone)]
pub struct SelImpl;

/// 常驻选择器（规则生命周期=进程；解析串 leak 成 'static）
pub type SelSelector = Selector<'static, SelImpl>;

/// 解析并泄漏为 'static（规则数量有限，进程级缓存可接受）
pub fn parse_selector_static(css_sel: &str) -> Option<(SelSelector, Option<&'static str>)> {
    let leaked: &'static str = Box::leak(css_sel.to_string().into_boxed_str());
    parse_selector(leaked)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnyPseudo;

/// String 包装 + cssparser ToCss（0.37 不再为 String 实现）
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Id(pub String);

impl cssparser::ToCss for Id {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str(&self.0)
    }
}

impl<'i> From<cssparser::CowRcStr<'i>> for Id {
    fn from(v: cssparser::CowRcStr<'i>) -> Self {
        Id(v.to_string())
    }
}

impl std::borrow::Borrow<str> for Id {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl cssparser::ToCss for AnyPseudo {
    fn to_css<W: fmt::Write>(&self, dest: &mut W) -> fmt::Result {
        dest.write_str(":any")
    }
}

impl<'i> parcel_selectors::parser::NonTSPseudoClass<'i> for AnyPseudo {
    type Impl = SelImpl;
    fn is_active_or_hover(&self) -> bool {
        false
    }
    fn is_user_action_state(&self) -> bool {
        false
    }
}

impl<'i> parcel_selectors::parser::PseudoElement<'i> for AnyPseudo {
    type Impl = SelImpl;
}

impl<'i> SelectorImpl<'i> for SelImpl {
    type ExtraMatchingData = ();
    type AttrValue = Id;
    type Identifier = Id;
    type LocalName = Id;
    type NamespaceUrl = Id;
    type NamespacePrefix = Id;
    type BorrowedNamespaceUrl = str;
    type BorrowedLocalName = str;
    type NonTSPseudoClass = AnyPseudo;
    type VendorPrefix = Id;
    type PseudoElement = AnyPseudo;
}

/// 选择器解析：lightningcss 序列化字符串 → parcel 标准解析器（Parser trait）→ Selector<SelImpl>。
/// 返回 (Selector, 伪元素 which)。
pub fn parse_selector<'i>(css_sel: &'i str) -> Option<(Selector<'i, SelImpl>, Option<&'static str>)> {
    let mut parser_input = cssparser::ParserInput::new(css_sel);
    let mut input = cssparser::Parser::new(&mut parser_input);
    let sel: Selector<'_, SelImpl> = Selector::parse(&SelParser, &mut input).ok()?;
    let sel = sel.clone();
    // 伪元素检测：主体段的 PseudoElement 组件
    let mut which: Option<&'static str> = None;
    for c in sel.iter() {
        if let Component::PseudoElement(_) = c {
            which = Some(if css_sel.to_lowercase().contains("before") { "before" } else { "after" });
        }
    }
    Some((sel, which))
}

/// parcel Parser trait：非树结构伪类/伪元素一律不支持（返回 err）→ 不匹配
pub struct SelParser;

impl<'i> parcel_selectors::parser::Parser<'i> for SelParser {
    type Impl = SelImpl;
    type Error = SelectorsParseError<'i>;

    fn parse_non_ts_pseudo_class(
        &self,
        location: cssparser::SourceLocation,
        _name: cssparser::CowRcStr<'i>,
    ) -> Result<AnyPseudo, cssparser::ParseError<'i, Self::Error>> {
        Err(cssparser::ParseError {
            kind: cssparser::ParseErrorKind::Custom(SelectorsParseError::Unsupported),
            location,
        })
    }

    fn parse_pseudo_element(
        &self,
        location: cssparser::SourceLocation,
        name: cssparser::CowRcStr<'i>,
    ) -> Result<AnyPseudo, cssparser::ParseError<'i, Self::Error>> {
        match &*name {
            "before" | "after" => Ok(AnyPseudo),
            _ => Err(cssparser::ParseError {
                kind: cssparser::ParseErrorKind::Custom(SelectorsParseError::Unsupported),
                location,
            }),
        }
    }
}

/// SelectorParseErrorKind 的 From 桥（Error: From<SelectorParseErrorKind>）
#[derive(Debug)]
pub enum SelectorsParseError<'i> {
    Unsupported,
    Basic(cssparser::BasicParseError<'i>),
    Sel(parcel_selectors::parser::SelectorParseErrorKind<'i>),
}

impl<'i> From<parcel_selectors::parser::SelectorParseErrorKind<'i>> for SelectorsParseError<'i> {
    fn from(k: parcel_selectors::parser::SelectorParseErrorKind<'i>) -> Self {
        SelectorsParseError::Sel(k)
    }
}

impl<'i> From<cssparser::BasicParseError<'i>> for SelectorsParseError<'i> {
    fn from(e: cssparser::BasicParseError<'i>) -> Self {
        SelectorsParseError::Basic(e)
    }
}


/// 带父链与兄弟序号的元素包装（StyleNode 树本身无父指针）
#[derive(Clone)]
pub struct EWrapDbg<'a>(pub EWrap<'a>);
impl<'a> fmt::Debug for EWrap<'a> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.node.tag)
    }
}
#[derive(Clone)]
pub struct EWrap<'a> {
    pub node: &'a StyleNode,
    pub parent: Option<Rc<EWrap<'a>>>,
    /// 0-based 在父 children 中的位置（根为 0）
    pub index: usize,
}

impl<'a> EWrap<'a> {
    fn child(p: &Rc<EWrap<'a>>, index: usize) -> EWrap<'a> {
        EWrap { node: &p.node.children[index], parent: Some(p.clone()), index }
    }
}

/// 匹配入口：selector 是否命中 node（wrap 需由调用方沿树构建，保证父链完整）
/// 生命周期解耦：'i 为选择器数据生命周期，'a 为样式树借用生命周期（SelImpl 关联类型均无生命周期依赖）
pub fn matches<'i, 'e, 'c, 'a>(
    selector: &'e Selector<'i, SelImpl>,
    wrap: &'e EWrap<'a>,
    ctx: &mut MatchingContext<'c, 'i, SelImpl>,
) -> bool {
    matches_selector(selector, 0, None, wrap, ctx, &mut |_, _| {})
}

impl<'i, 'a> Element<'i> for EWrap<'a> {
    type Impl = SelImpl;

    fn opaque(&self) -> OpaqueElement {
        OpaqueElement::new(self.node)
    }

    fn parent_element(&self) -> Option<EWrap<'a>> {
        self.parent.as_ref().map(|p| EWrap {
            node: p.node,
            parent: p.parent.clone(),
            index: p.index,
        })
    }

    fn parent_node_is_shadow_root(&self) -> bool {
        false
    }

    fn containing_shadow_host(&self) -> Option<EWrap<'a>> {
        None
    }

    fn is_pseudo_element(&self) -> bool {
        false
    }

    fn prev_sibling_element(&self) -> Option<EWrap<'a>> {
        let p = self.parent.as_ref()?;
        let i = self.index.checked_sub(1)?;
        Some(EWrap::child(p, i))
    }

    fn next_sibling_element(&self) -> Option<EWrap<'a>> {
        let p = self.parent.as_ref()?;
        let i = self.index + 1;
        if i >= p.node.children.len() {
            return None;
        }
        Some(EWrap::child(p, i))
    }

    fn is_html_element_in_html_document(&self) -> bool {
        true
    }

    fn has_local_name(
        &self,
        local_name: &<Self::Impl as SelectorImpl<'i>>::BorrowedLocalName,
    ) -> bool {
        // Ident(pub CowArcStr)——小写名比较（HTML 标签不区分大小写）
        self.node.tag.eq_ignore_ascii_case(local_name)
    }

    fn has_namespace(
        &self,
        _ns: &<Self::Impl as SelectorImpl<'i>>::BorrowedNamespaceUrl,
    ) -> bool {
        false
    }

    fn is_same_type(&self, other: &EWrap<'a>) -> bool {
        self.node.tag.eq_ignore_ascii_case(&other.node.tag)
    }

    fn attr_matches(
        &self,
        _ns: &parcel_selectors::attr::NamespaceConstraint<
            &<Self::Impl as SelectorImpl<'i>>::NamespaceUrl,
        >,
        _local_name: &<Self::Impl as SelectorImpl<'i>>::LocalName,
        _operation: &parcel_selectors::attr::AttrSelectorOperation<
            &<Self::Impl as SelectorImpl<'i>>::AttrValue,
        >,
    ) -> bool {
        false
    }

    fn match_non_ts_pseudo_class<F>(
        &self,
        _pc: &<Self::Impl as SelectorImpl<'i>>::NonTSPseudoClass,
        _context: &mut MatchingContext<'_, 'i, Self::Impl>,
        _flags_setter: &mut F,
    ) -> bool
    where
        F: FnMut(&EWrap<'a>, parcel_selectors::matching::ElementSelectorFlags),
    {
        false
    }

    fn match_pseudo_element(
        &self,
        _pe: &<Self::Impl as SelectorImpl<'i>>::PseudoElement,
        _context: &mut MatchingContext<'_, 'i, Self::Impl>,
    ) -> bool {
        false
    }

    fn is_link(&self) -> bool {
        false
    }

    fn is_html_slot_element(&self) -> bool {
        false
    }

    fn has_id(
        &self,
        id: &<Self::Impl as SelectorImpl<'i>>::Identifier,
        _case_sensitivity: CaseSensitivity,
    ) -> bool {
        self.node
            .id
            .as_deref()
            .map(|v| v == id.0)
            .unwrap_or(false)
    }

    fn has_class(
        &self,
        name: &<Self::Impl as SelectorImpl<'i>>::Identifier,
        _case_sensitivity: CaseSensitivity,
    ) -> bool {
        let want: &str = &name.0;
        self.node.classes.iter().any(|c| c == want)
    }

    fn imported_part(
        &self,
        _name: &<Self::Impl as SelectorImpl<'i>>::Identifier,
    ) -> Option<<Self::Impl as SelectorImpl<'i>>::Identifier> {
        None
    }

    fn is_part(
        &self,
        _name: &<Self::Impl as SelectorImpl<'i>>::Identifier,
    ) -> bool {
        false
    }

    fn is_empty(&self) -> bool {
        self.node.children.is_empty() && self.node.text.is_empty()
    }

    fn is_root(&self) -> bool {
        self.parent.is_none()
    }
}
