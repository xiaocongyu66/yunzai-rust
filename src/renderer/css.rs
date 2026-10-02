//! 极简 CSS：样式表解析 + 选择器匹配 + 声明解析
//!
//! 模板生成的 CSS 是规范格式（无 hack），手写解析足够；
//! 值解析（颜色/渐变/尺寸）在 layout/paint 阶段按需处理。

use super::dom::{CssRule, StyleNode};
use super::matcher::{EWrap, SelSelector};
use parcel_selectors::context::{MatchingContext, MatchingMode, QuirksMode};
use parcel_selectors::parser::Selector;
use std::collections::BTreeMap;
use std::rc::Rc;

/// 解析样式表：Lightning CSS 结构化遍历（Style 规则 + @font-face + @media 展开 + @import 递归）
pub fn parse_stylesheet(css: &str) -> Vec<CssRule> {
    parse_stylesheet_lc(css, "").0
}

/// 伪元素规则（::before / ::after）
#[derive(Clone, Debug)]
pub struct PseudoRule {
    pub parent: SelSelector,
    pub which: &'static str,
    pub decls: BTreeMap<String, String>,
}

/// 拆分选择器尾部的伪元素，返回 (主体, Some(伪元素名)) 或 (原样, None)

/// @import 递归深度上限（防循环引用）
const MAX_IMPORT_DEPTH: u32 = 5;

/// 样式表收集器：样式规则 / @font-face / 伪元素
#[derive(Default)]
struct SheetCollector {
    rules: Vec<CssRule>,
    faces: Vec<(String, String)>,
    pseudos: Vec<PseudoRule>,
    /// 当前样式表文件目录：@font-face / @import 的 url 基准
    css_dir: String,
}

/// 返回 (样式规则, @font-face 列表, 伪元素规则列表)；css_dir 为该样式表的文件目录（@font-face url 基准）
pub fn parse_stylesheet_lc(css: &str, css_dir: &str) -> (Vec<CssRule>, Vec<(String, String)>, Vec<PseudoRule>) {
    use lightningcss::stylesheet::StyleSheet;
    let mut ctx = SheetCollector::default();
    ctx.css_dir = css_dir.to_string();
    if let Ok(ss) = StyleSheet::parse(css, lc_parse_opts()) {
        collect_rules(&ss.rules, &mut ctx, 0);
    }
    (ctx.rules, ctx.faces, ctx.pseudos)
}

/// 递归收集规则：Style/FontFace 展开；块容器规则（@media/@supports/@layer/@container/
/// @scope/@starting-style/@-moz-document）展开内层 rules（条件一律忽略，模板为静态宽度）；
/// @import 读本地文件后整体解析并递归（depth 上限防循环）
fn collect_rules(list: &lightningcss::rules::CssRuleList, ctx: &mut SheetCollector, depth: u32) {
    use lightningcss::rules::CssRule as LcRule;
    use lightningcss::stylesheet::StyleSheet;
    for rule in list.0.iter() {
        match rule {
            LcRule::Style(st) => collect_style(st, ctx),
            LcRule::FontFace(ff) => collect_font_face(ff, ctx),
            LcRule::Media(m) => collect_rules(&m.rules, ctx, depth),
            LcRule::Supports(s) => collect_rules(&s.rules, ctx, depth),
            LcRule::LayerBlock(l) => collect_rules(&l.rules, ctx, depth),
            LcRule::Container(c) => collect_rules(&c.rules, ctx, depth),
            LcRule::Scope(s) => collect_rules(&s.rules, ctx, depth),
            LcRule::StartingStyle(s) => collect_rules(&s.rules, ctx, depth),
            LcRule::MozDocument(d) => collect_rules(&d.rules, ctx, depth),
            LcRule::Import(i) => {
                if depth < MAX_IMPORT_DEPTH {
                    if let Some((css, sub_dir)) = read_import_source(&i.url.to_string(), &ctx.css_dir) {
                        let prev = std::mem::replace(&mut ctx.css_dir, sub_dir);
                        if let Ok(ss) = StyleSheet::parse(&css, lc_parse_opts()) {
                            collect_rules(&ss.rules, ctx, depth + 1);
                        }
                        ctx.css_dir = prev;
                    }
                }
            }
            _ => {}
        }
    }
}

/// @import url → 本地 CSS 文本（http(s)/data: 不支持；相对路径基于 BASE_DIR 解析）
fn read_import_source(url: &str, css_dir: &str) -> Option<(String, String)> {
    let path = super::media::resolve(url, css_dir);
    if path.starts_with("http://") || path.starts_with("https://") || path.starts_with("data:") {
        return None;
    }
    let css = std::fs::read_to_string(&path).ok()?;
    let dir = std::path::Path::new(&path)
        .parent()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|| ".".to_string());
    Some((css, dir))
}

struct StylesheetUrls<'a> {
    base: &'a str,
}

impl<'i> lightningcss::visitor::Visitor<'i> for StylesheetUrls<'_> {
    type Error = std::convert::Infallible;

    fn visit_types(&self) -> lightningcss::visitor::VisitTypes {
        lightningcss::visit_types!(URLS)
    }

    fn visit_url(&mut self, url: &mut lightningcss::values::url::Url<'i>) -> Result<(), Self::Error> {
        url.url = super::media::resolve(&url.url, self.base).into();
        Ok(())
    }
}

/// 样式规则展开：声明序列化 + 选择器编译（伪元素单独收集）
fn collect_style(st: &lightningcss::rules::style::StyleRule, ctx: &mut SheetCollector) {
    use lightningcss::traits::ToCss;
    let mut decls: Vec<(String, String)> = Vec::new();
    let mut idecls: Vec<(String, String)> = Vec::new();
    use lightningcss::visitor::Visit;
    let mut urls = StylesheetUrls { base: &ctx.css_dir };
    for d in st.declarations.declarations.iter() {
        let mut d = d.clone();
        match d.visit(&mut urls) {
            Ok(()) => serialize_decl(&d, &mut decls),
            Err(never) => match never {},
        }
    }
    for d in st.declarations.important_declarations.iter() {
        let mut d = d.clone();
        match d.visit(&mut urls) {
            Ok(()) => serialize_decl(&d, &mut idecls),
            Err(never) => match never {},
        }
    }
    // 选择器透传：lightningcss 的结构化 Component 直读（Tag/Class/Id/组合子/nth/伪元素），
    // 不经 to_css_string 再字符串解析
    use lightningcss::selector::{Component, Combinator};
use parcel_selectors::parser::NthType;
    // 选择器解析：lightningcss 序列化字符串 → parcel 标准解析器（Servo 同款匹配链）
    // 选择器列表逐项遍历（此前字符串 split(',') 会把 :is(a, b)/:not(a, b) 内部逗号误切）
    for sel_item in st.selectors.0.iter() {
        let one = sel_item.to_css_string(lc_opts()).unwrap_or_default();
        let one = one.trim();
        if one.is_empty() {
            continue;
        }
        let Some((sel, which)) = super::matcher::parse_selector_static(one) else {
            continue;
        };
        let specificity = sel.specificity();
        css_debug(format!(
            "规则 {one} sp={specificity} 普通={} 重要={} 伪元素={}",
            decls.len(),
            idecls.len(),
            which.is_some()
        ));
        let mut pseudo_decls: BTreeMap<String, String> = decls.iter().cloned().collect();
        for (k, v) in &idecls {
            pseudo_decls.insert(k.clone(), v.clone());
        }
        if let Some(w) = which {
            ctx.pseudos.push(PseudoRule {
                parent: sel,
                which: w,
                decls: pseudo_decls,
            });
            continue;
        }
        ctx.rules.push(CssRule {
            selector: sel,
            decls: decls.iter().cloned().collect(),
            important_decls: idecls.iter().cloned().collect(),
            specificity,
        });
    }
}

/// @font-face 展开：family 别名 + src 首个本地存在的 url
fn collect_font_face(ff: &lightningcss::rules::font_face::FontFaceRule, ctx: &mut SheetCollector) {
    use lightningcss::rules::font_face::{FontFaceProperty, Source};
    use lightningcss::traits::ToCss;
    let mut fam = String::new();
    let mut src = String::new();
    for prop in ff.properties.iter() {
        match prop {
            FontFaceProperty::FontFamily(f) => {
                if let lightningcss::properties::font::FontFamily::FamilyName(n) = f {
                    if let Ok(v) = n.to_css_string(lc_opts()) {
                        fam = v.trim_matches(|c| c == '"' || c == '\'').to_string();
                    }
                }
            }
            FontFaceProperty::Source(list) => {
                let urls: Vec<String> = list
                    .iter()
                    .filter_map(|sv| match sv {
                        Source::Url(u) => Some(u.url.url.to_string()),
                        _ => None,
                    })
                    .collect();
                if let Some(pick) = pick_face_url(&urls, &ctx.css_dir) {
                    // url 相对"所在 css 文件"解析成绝对路径（拼接样式表后基准不能丢）
                    src = crate::renderer::media::resolve(&pick, &ctx.css_dir);
                }
            }
            _ => {}
        }
    }
    if !fam.is_empty() && !src.is_empty() {
        ctx.faces.push((fam, src));
    }
}

/// font-face src 多候选：按所在样式表目录查找；都不存在则取第一个。
fn pick_face_url(urls: &[String], base: &str) -> Option<String> {
    let first = urls.first()?;
    Some(
        urls.iter()
            .find(|u| std::fs::metadata(super::media::resolve(u, &base)).is_ok())
            .unwrap_or(first)
            .clone(),
    )
}

/// 应用伪元素规则：命中 parent 选择器的节点，注入 ::before/::after 虚拟子节点
pub fn apply_pseudo(root: &mut StyleNode, rules: &[PseudoRule]) {
    let root = root;
    if rules.is_empty() {
        return;
    }
    // 两遍法：先不可变匹配收集 (路径, before/after 规则)，再可变插入
    let mut found: Vec<(Vec<usize>, bool, PseudoRule)> = Vec::new();
    {
        let ew = Rc::new(EWrap { node: &*root, parent: None, index: 0 });
        collect_pseudo_at(&root, rules, Some(ew.clone()), 0, &mut Vec::new(), &mut found);
    }
    // 路径深的先插，避免索引位移；同路径 before 先于 after
    found.sort_by_key(|(p, is_before, _)| (std::cmp::Reverse(p.clone()), !*is_before));
    let root: &mut StyleNode = root;
    for (path, is_before, r) in found {
        let Some(n) = node_at_mut(root, &path) else { continue };
        let content = r.decls.get("content").cloned().unwrap_or_default();
        let content = content.trim_matches(|c| c == '"' || c == '\'').to_string();
        let mut child = StyleNode {
            tag: if is_before { "::before".into() } else { "::after".into() },
            id: None,
            classes: Vec::new(),
            decls: r.decls.clone(),
            important_decls: BTreeMap::new(),
            text: content,
            children: Vec::new(),
            src: None,
        };
        child.decls.remove("content");
        // 伪元素继承宿主元素的可继承属性（CSS：::before/::after 从 originating element 继承）
        for k in INHERITED_PROPS {
            if !child.decls.contains_key(*k) {
                if let Some(v) = n.decls.get(*k) {
                    child.decls.insert((*k).to_string(), v.clone());
                }
            }
        }
        if is_before {
            n.children.insert(0, child);
        } else {
            n.children.push(child);
        }
    }
}

fn collect_pseudo_at<'a>(
    node: &'a StyleNode,
    rules: &[PseudoRule],
    parent: Option<Rc<EWrap<'a>>>,
    index: usize,
    path: &mut Vec<usize>,
    out: &mut Vec<(Vec<usize>, bool, PseudoRule)>,
) {
    use super::matcher::matches as sel_matches;
    // SAFETY：同 collect_at——匹配只读、引用不外泄
    let node_static: &'static StyleNode = unsafe { std::mem::transmute::<&StyleNode, &'static StyleNode>(node) };
    let parent_static: Option<Rc<EWrap<'static>>> = unsafe {
        std::mem::transmute::<Option<Rc<EWrap>>, Option<Rc<EWrap<'static>>>>(parent)
    };
    let ew = Rc::new(EWrap { node: node_static, parent: parent_static, index });
    let mut ctx = MatchingContext::new(MatchingMode::Normal, None, None, QuirksMode::NoQuirks);
    for r in rules {
        if sel_matches(&r.parent, &ew, &mut ctx) {
            let is_before = r.which == "before";
            out.push((path.clone(), is_before, r.clone()));
        }
    }
    for (i, c) in node.children.iter().enumerate() {
        path.push(i);
        collect_pseudo_at(c, rules, Some(ew.clone()), i, path, out);
        path.pop();
    }
}

/// `div.card > .name span` → [Tag(div), Class(card), Child, Class(name), Descendant, Tag(span)]
/// 剥离选择器串中的 :nth-child(...)（含大小写/空格形态）→ (NthSpec, 余下选择器)


enum Combinator {
    Child,
    Desc,
    Simple(String),
}

/// "div>x" → [Simple(div), Child, Simple(x)]；"div x" 的空格由外层 split 处理，此处补跨 token 逻辑
fn split_combinators(tok: &str) -> Vec<Combinator> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in tok.chars() {
        if c == '>' {
            if !cur.is_empty() {
                out.push(Combinator::Simple(std::mem::take(&mut cur)));
            }
            out.push(Combinator::Child);
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        out.push(Combinator::Simple(cur));
    }
    out
}

/// inline style 字符串 → 声明对
pub fn parse_declarations(s: &str) -> Vec<(String, String)> {
    // Lightning CSS：按规范解析并展开简写为 longhand（padding/background 等），失败回退手写解析
    match parse_declarations_lc(s) {
        Some(out) if !out.is_empty() && out.iter().all(|(_, v)| !v.is_empty()) => out,
        _ => parse_declarations_legacy(s),
    }
}

/// 解析声明块：返回 (普通声明, !important 声明)。legacy 回退时重要声明并入普通。
pub fn parse_declarations_important(s: &str) -> (Vec<(String, String)>, Vec<(String, String)>) {
    match parse_declarations_lc_important(s) {
        Some((n, i)) if !n.is_empty() || !i.is_empty() => (n, i),
        _ => (parse_declarations_legacy(s), Vec::new()),
    }
}

fn parse_declarations_legacy(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for decl in split_top(s, ';') {
        let decl = decl.trim();
        if decl.is_empty() {
            continue;
        }
        if let Some((k, v)) = decl.split_once(':') {
            out.push((k.trim().to_lowercase(), v.trim().to_string()));
        }
    }
    out
}

/// Lightning CSS 解析：结构化声明 → longhand 字符串表
fn parse_declarations_lc(s: &str) -> Option<Vec<(String, String)>> {
    use lightningcss::printer::PrinterOptions;
    use lightningcss::properties::Property;
    use lightningcss::stylesheet::StyleSheet;
    let src = format!("a{{ {} }}", s);
    let mut ss = StyleSheet::parse(&src, lc_parse_opts()).ok()?;
    let rule = ss.rules.0.first_mut()?;
    let style = match rule {
        lightningcss::rules::CssRule::Style(st) => st,
        _ => return None,
    };
    let mut out = Vec::new();
    for decl in style.declarations.declarations.iter() {
        serialize_decl(decl, &mut out);
    }
    for decl in style.declarations.important_declarations.iter() {
        serialize_decl(decl, &mut out);
    }
    Some(out)
}

/// Lightning CSS 解析：结构化声明 → (普通, 重要) 两张 longhand 字符串表
fn parse_declarations_lc_important(s: &str) -> Option<(Vec<(String, String)>, Vec<(String, String)>)> {
    use lightningcss::stylesheet::StyleSheet;
    let src = format!("a{{ {} }}", s);
    let mut ss = StyleSheet::parse(&src, lc_parse_opts()).ok()?;
    let rule = ss.rules.0.first_mut()?;
    let style = match rule {
        lightningcss::rules::CssRule::Style(st) => st,
        _ => return None,
    };
    let mut normal = Vec::new();
    for decl in style.declarations.declarations.iter() {
        serialize_decl(decl, &mut normal);
    }
    let mut important = Vec::new();
    for decl in style.declarations.important_declarations.iter() {
        serialize_decl(decl, &mut important);
    }
    Some((normal, important))
}

/// CSS 诊断日志（YZ_DEBUG_CSS 开启时输出逐环节明细：收集/丢弃/命中/消费/继承）
static CSS_DEBUG: once_cell::sync::Lazy<bool> =
    once_cell::sync::Lazy::new(|| std::env::var("YZ_DEBUG_CSS").is_ok());

pub fn css_debug(msg: String) {
    if *CSS_DEBUG {
        eprintln!("[css] {msg}");
    }
}

/// 单条 lightningcss 声明 → (属性名, 值) 列表（简写展开 longhand）
pub fn serialize_decl(d: &lightningcss::properties::Property, out: &mut Vec<(String, String)>) {
    use lightningcss::properties::Property;
    // alpha.72 对已知属性名的值解析失败时回落 Property::Unparsed。浏览器语义：
    // 非法值=声明作废（丢弃，级联/继承接管）；CSS 宽关键字值保留交由继承计算处理
    if let Property::Unparsed(u) = d {
        let kw = d.value_to_css_string(lc_opts()).unwrap_or_default().trim().to_lowercase();
        if matches!(kw.as_str(), "inherit" | "initial" | "unset" | "revert") {
            out.push((u.property_id.name().to_string(), kw));
        } else {
            css_debug(format!("丢弃未解析声明 {}：{}", u.property_id.name(), kw));
        }
        return;
    }
    let name = d.property_id().name().to_string();
    match d {
        Property::Padding(r) => {
            out.push(("padding-top".into(), lc_val(&r.top)));
            out.push(("padding-right".into(), lc_val(&r.right)));
            out.push(("padding-bottom".into(), lc_val(&r.bottom)));
            out.push(("padding-left".into(), lc_val(&r.left)));
        }
        Property::Margin(r) => {
            out.push(("margin-top".into(), lc_val(&r.top)));
            out.push(("margin-right".into(), lc_val(&r.right)));
            out.push(("margin-bottom".into(), lc_val(&r.bottom)));
            out.push(("margin-left".into(), lc_val(&r.left)));
        }
        Property::Background(list) => {
            // 多层背景：每层值逗号连接（CSS 规范；background-color 仅取最后一层）
            let images: Vec<String> = list.iter().map(|b| lc_val(&b.image)).collect();
            out.push(("background-image".into(), images.join(", ")));
            if let Some(last) = list.last() {
                out.push(("background-color".into(), lc_val(&last.color)));
            }
            let positions: Vec<String> = list.iter().map(|b| lc_val(&b.position)).collect();
            out.push(("background-position".into(), positions.join(", ")));
            let sizes: Vec<String> = list.iter().map(|b| lc_val(&b.size)).collect();
            out.push(("background-size".into(), sizes.join(", ")));
            let repeats: Vec<String> = list.iter().map(|b| lc_val(&b.repeat)).collect();
            out.push(("background-repeat".into(), repeats.join(", ")));
        }
        Property::Gap(g) => {
            out.push(("row-gap".into(), lc_val(&g.row)));
            out.push(("column-gap".into(), lc_val(&g.column)));
        }
        // box-sizing：style.rs 侧解析为 border_box（layout 侧做内容盒补偿）
        Property::BoxSizing(b, _) => out.push(("box-sizing".into(), lc_val(b))),
        // border 全系列：简写键保留（兼容直接读简写的消费端），同时展开各侧 longhand
        // 保证 border-top-width 等键进入 decls（级联语义正确：简写在先、longhand 可覆盖）
        Property::Border(b) => {
            push_shorthand(d, &name, out);
            for side in ["top", "right", "bottom", "left"] {
                push_border_side(out, side, &b.width, &b.style, &b.color);
            }
        }
        Property::BorderTop(b) => {
            push_shorthand(d, &name, out);
            push_border_side(out, "top", &b.width, &b.style, &b.color);
        }
        Property::BorderRight(b) => {
            push_shorthand(d, &name, out);
            push_border_side(out, "right", &b.width, &b.style, &b.color);
        }
        Property::BorderBottom(b) => {
            push_shorthand(d, &name, out);
            push_border_side(out, "bottom", &b.width, &b.style, &b.color);
        }
        Property::BorderLeft(b) => {
            push_shorthand(d, &name, out);
            push_border_side(out, "left", &b.width, &b.style, &b.color);
        }
        Property::BorderWidth(w) => {
            push_shorthand(d, &name, out);
            for (side, v) in [("top", &w.top), ("right", &w.right), ("bottom", &w.bottom), ("left", &w.left)] {
                out.push((format!("border-{side}-width"), lc_val(v)));
            }
        }
        Property::BorderStyle(s) => {
            push_shorthand(d, &name, out);
            for (side, v) in [("top", &s.top), ("right", &s.right), ("bottom", &s.bottom), ("left", &s.left)] {
                out.push((format!("border-{side}-style"), lc_val(v)));
            }
        }
        Property::BorderColor(c) => {
            push_shorthand(d, &name, out);
            for (side, v) in [("top", &c.top), ("right", &c.right), ("bottom", &c.bottom), ("left", &c.left)] {
                out.push((format!("border-{side}-color"), lc_val(v)));
            }
        }
        // 文本/排版相关透传（文本层按 decls 键读取）
        Property::LetterSpacing(v) => out.push(("letter-spacing".into(), lc_val(v))),
        Property::TextIndent(v) => out.push(("text-indent".into(), lc_val(v))),
        Property::WhiteSpace(v) => out.push(("white-space".into(), lc_val(v))),
        Property::VerticalAlign(v) => out.push(("vertical-align".into(), lc_val(v))),
        other => {
            // 通用：Property 序列化输出为 "name: value"，剥掉前缀只留值
            if let Ok(v) = other.to_css_string(false, lc_opts()) {
                let val = match v.split_once(':') {
                    Some((_, rest)) => rest.trim().to_string(),
                    None => v,
                };
                out.push((name, val));
            }
        }
    }
}

/// 简写键保留：通用序列化 "name: value" 剥出值（兼容直接读简写键的旧消费端）
fn push_shorthand(d: &lightningcss::properties::Property, name: &str, out: &mut Vec<(String, String)>) {
    if let Ok(v) = d.to_css_string(false, lc_opts()) {
        let val = match v.split_once(':') {
            Some((_, rest)) => rest.trim().to_string(),
            None => v,
        };
        out.push((name.to_string(), val));
    }
}

/// 单侧 border 简写展开为 border-{side}-{width,style,color} 三个 longhand
fn push_border_side(
    out: &mut Vec<(String, String)>,
    side: &str,
    width: &lightningcss::properties::border::BorderSideWidth,
    style: &lightningcss::properties::border::LineStyle,
    color: &lightningcss::values::color::CssColor,
) {
    out.push((format!("border-{side}-width"), lc_val(width)));
    out.push((format!("border-{side}-style"), lc_val(style)));
    out.push((format!("border-{side}-color"), lc_val(color)));
}

/// 带浏览器 targets 的序列化选项（空 targets 会让部分属性序列化失败返回空）
fn lc_opts() -> lightningcss::printer::PrinterOptions<'static> {
    use lightningcss::targets::{Browsers, Targets};
    lightningcss::printer::PrinterOptions {
        targets: Targets {
            browsers: Some(Browsers {
                chrome: Some(120 << 16),
                edge: Some(120 << 16),
                firefox: Some(120 << 16),
                safari: Some(17 << 16),
                ..Default::default()
            }),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// lightningcss 值 → 字符串（走 ToCss 序列化）
fn lc_val<T: lightningcss::traits::ToCss>(v: &T) -> String {
    v.to_css_string(lc_opts()).unwrap_or_default()
}

/// 解析选项：错误恢复开启（浏览器语义）——单条非法声明仅该条被丢弃，
/// 不再使整个样式表解析失败（曾导致一张无效色值废掉整页样式）
fn lc_parse_opts<'i>() -> lightningcss::stylesheet::ParserOptions<'i> {
    lightningcss::stylesheet::ParserOptions {
        error_recovery: true,
        ..Default::default()
    }
}


/// 括号感知的顶层分割（linear-gradient(a,b) 内的 ; 逗号不切）
fn split_top(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
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
            c if c == sep && depth == 0 => out.push(std::mem::take(&mut cur)),
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur);
    }
    out
}

/// 逗号分割（括号感知）
pub fn split_commas(s: &str) -> Vec<String> {
    split_top(s, ',')
}

/// 样式匹配：规则表应用到节点树（specificity 升序应用，inline 已在 decls 中最高优先）
pub fn apply_styles(mut root: StyleNode, rules: &[CssRule]) -> StyleNode {
    // 两遍法（Servo 风格解耦匹配与修改）：
    // 1) 不可变遍历：EWrap 链 + matches_selector 收集 (索引路径, 普通/重要级联声明)
    // 2) 可变遍历：按路径回填。级联序：样式表普通 < inline 普通 < 样式表 !important < inline !important
    //    （同级内部按 specificity 与文档序）
    let mut collected: Vec<(Vec<usize>, BTreeMap<String, String>, BTreeMap<String, String>)> = Vec::new();
    {
        let ew = Rc::new(EWrap { node: &root, parent: None, index: 0 });
        collect_at(&root, rules, Some(ew.clone()), 0, &mut Vec::new(), &mut collected);
    }
    for (path, decls, idecls) in collected {
        let Some(n) = node_at_mut(&mut root, &path) else { continue };
        let inline = std::mem::take(&mut n.decls);
        let inline_important = std::mem::take(&mut n.important_decls);
        n.decls.clear();
        for (k, v) in decls {
            n.decls.insert(k, v);
        }
        for (k, v) in inline {
            n.decls.insert(k, v);
        }
        for (k, v) in idecls {
            n.decls.insert(k, v);
        }
        for (k, v) in inline_important {
            n.decls.insert(k, v);
        }
    }
    apply_inheritance(&mut root, None, None);
    root
}

/// 可继承属性（CSS inheritance）：级联后自顶向下传播，子节点未声明时取父级计算值
const INHERITED_PROPS: &[&str] = &[
    "color", "font-family", "font-size", "font-weight", "font-style", "line-height",
    "letter-spacing", "text-align", "text-indent", "text-shadow", "white-space",
    "word-spacing", "visibility",
];

fn apply_inheritance(
    n: &mut StyleNode,
    parent_inherited: Option<&BTreeMap<String, String>>,
    parent_all: Option<&BTreeMap<String, String>>,
) {
    let mut computed: BTreeMap<String, String> = BTreeMap::new();
    if let Some(p) = parent_inherited {
        for k in INHERITED_PROPS {
            if let Some(v) = p.get(*k) {
                computed.insert((*k).to_string(), v.clone());
            }
        }
    }
    // CSS 宽关键字（全属性）：inherit=取父级计算值；unset/revert 在可继承属性上等同
    // inherit、其余属性上等同 initial；initial=回到初始值（删声明走默认）
    let mut copies: Vec<(String, String)> = Vec::new();
    let mut to_remove: Vec<String> = Vec::new();
    for (k, v) in n.decls.iter() {
        let kw = v.trim();
        let takes_parent = kw == "inherit"
            || ((kw == "unset" || kw == "revert") && INHERITED_PROPS.contains(&k.as_str()));
        if takes_parent {
            match parent_all.and_then(|p| p.get(k)) {
                Some(pv) => copies.push((k.clone(), pv.clone())),
                None => to_remove.push(k.clone()),
            }
        } else if kw == "unset" || kw == "revert" || kw == "initial" {
            to_remove.push(k.clone());
        }
    }
    for k in to_remove {
        n.decls.remove(&k);
    }
    for (k, v) in copies {
        n.decls.insert(k, v);
    }
    for k in INHERITED_PROPS {
        if n.decls.get(*k).is_none() {
            if let Some(v) = computed.get(*k) {
                n.decls.insert((*k).to_string(), v.clone());
                css_debug(format!("继承 <{} class={:?}> ← {k}: {v}", n.tag, n.classes));
            }
        }
    }
    for k in INHERITED_PROPS {
        if let Some(v) = n.decls.get(*k) {
            computed.insert((*k).to_string(), v.clone());
        }
    }
    for c in n.children.iter_mut() {
        apply_inheritance(c, Some(&computed), Some(&n.decls));
    }
}

fn node_at_mut<'n>(n: &'n mut StyleNode, path: &[usize]) -> Option<&'n mut StyleNode> {
    let [i, rest @ ..] = path else { return Some(n) };
    let c = n.children.get_mut(*i)?;
    node_at_mut(c, rest)
}

fn collect_at<'a>(
    node: &'a StyleNode,
    rules: &[CssRule],
    parent: Option<Rc<EWrap<'a>>>,
    index: usize,
    path: &mut Vec<usize>,
    out: &mut Vec<(Vec<usize>, BTreeMap<String, String>, BTreeMap<String, String>)>,
) {
    use super::matcher::matches as sel_matches;
    // SAFETY：匹配阶段为只读，且 'static 引用不逃逸出本函数（out 仅存路径与声明的拷贝）；
    // StyleNode 树由调用方持有，生命周期覆盖整个匹配过程。
    let node_static: &'static StyleNode = unsafe { std::mem::transmute::<&StyleNode, &'static StyleNode>(node) };
    let parent_static: Option<Rc<EWrap<'static>>> = unsafe {
        std::mem::transmute::<Option<Rc<EWrap>>, Option<Rc<EWrap<'static>>>>(parent)
    };
    let ew = Rc::new(EWrap { node: node_static, parent: parent_static, index });
    let mut ctx = MatchingContext::new(MatchingMode::Normal, None, None, QuirksMode::NoQuirks);

    let mut matched: Vec<(u32, usize, &BTreeMap<String, String>, &BTreeMap<String, String>)> = Vec::new();
    for (ri, rule) in rules.iter().enumerate() {
        if sel_matches(&rule.selector, &ew, &mut ctx) {
            matched.push((rule.specificity, ri, &rule.decls, &rule.important_decls));
        }
    }
    if !matched.is_empty() {
        matched.sort_by_key(|(sp, ri, _, _)| (*sp, *ri));
        css_debug(format!(
            "命中 <{} class={:?}> {} 条规则 {}",
            node.tag,
            node.classes,
            matched.len(),
            matched.iter().map(|(_, ri, _, _)| ri.to_string()).collect::<Vec<_>>().join(",")
        ));
        let mut cascaded = BTreeMap::new();
        let mut cascaded_important = BTreeMap::new();
        for (_, _, decls, idecls) in &matched {
            for (k, v) in decls.iter() {
                cascaded.insert(k.clone(), v.clone());
            }
            for (k, v) in idecls.iter() {
                cascaded_important.insert(k.clone(), v.clone());
            }
        }
        out.push((path.clone(), cascaded, cascaded_important));
    }
    for (i, c) in node.children.iter().enumerate() {
        path.push(i);
        collect_at(c, rules, Some(ew.clone()), i, path, out);
        path.pop();
    }
}

/// 选择器匹配：从最后一段（当前节点）向前，沿祖先链回溯



/// @font-face 别名替换：遍历样式树，把 font-family 里引用的别名换成字体内部真实名
pub fn apply_font_aliases(
    mut n: StyleNode,
    fonts: &super::text::TextEngine,
) -> StyleNode {
    if let Some(ff) = n.decls.get("font-family").cloned() {
        let parts: Vec<String> = ff
            .split(',')
            .map(|seg| {
                let seg = seg.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
                fonts.resolve_family(&seg).to_string()
            })
            .collect();
        n.decls.insert("font-family".into(), parts.join(", "));
    }
    for c in n.children.iter_mut() {
        *c = apply_font_aliases(std::mem::take(c), fonts);
    }
    n
}

#[cfg(test)]
mod cascade_tests {
    use super::super::dom::{parse, StyleNode};
    use super::*;

    fn styled(html: &str) -> StyleNode {
        let (root, rules, _, _) = parse(html).unwrap();
        apply_styles(root, &rules)
    }

    fn find_class<'a>(n: &'a StyleNode, cls: &str) -> Option<&'a StyleNode> {
        if n.classes.iter().any(|c| c == cls) {
            return Some(n);
        }
        n.children.iter().find_map(|c| find_class(c, cls))
    }

    fn find_tag<'a>(n: &'a StyleNode, tag: &str) -> Option<&'a StyleNode> {
        if n.tag == tag {
            return Some(n);
        }
        n.children.iter().find_map(|c| find_tag(c, tag))
    }

    fn rgb(s: &str) -> [u8; 4] {
        crate::renderer::paint::parse_color(s).unwrap()
    }

    #[test]
    fn specificity_beats_source_order() {
        let n = styled(r#"<style>div{color:red}.title{color:green}</style><div class="title">x</div>"#);
        assert_eq!(rgb(find_class(&n, "title").unwrap().decl("color").unwrap()), [0, 128, 0, 255]);
    }

    #[test]
    fn stylesheet_important_beats_inline_and_specificity() {
        let n = styled(
            r#"<style>.a{color:red}div{color:green !important}</style><div class="a" style="color:blue">x</div>"#,
        );
        assert_eq!(rgb(find_class(&n, "a").unwrap().decl("color").unwrap()), [0, 128, 0, 255]);
    }

    #[test]
    fn inline_important_beats_stylesheet_important() {
        let n = styled(
            r#"<style>div{color:red !important}</style><div style="color:blue !important">x</div>"#,
        );
        assert_eq!(rgb(find_tag(&n, "div").unwrap().decl("color").unwrap()), [0, 0, 255, 255]);
    }

    #[test]
    fn color_and_font_inherit_to_descendants() {
        let html = r#"<style>body{color:#ffffff;font-family:Miao;font-size:14px}</style><div><span class="t">x</span></div>"#;
        let n = styled(html);
        let span = find_class(&n, "t").unwrap();
        assert_eq!(rgb(span.decl("color").unwrap()), [255, 255, 255, 255]);
        assert_eq!(span.decl("font-family").unwrap().trim_matches('"'), "Miao");
        assert_eq!(span.decl("font-size").unwrap(), "14px");
    }

    #[test]
    fn invalid_hex_color_does_not_become_black() {
        let n = styled(r#"<style>body{color:#ffffff}div{color:#0000000}</style><div>x</div>"#);
        assert_eq!(rgb(find_tag(&n, "div").unwrap().decl("color").unwrap()), [255, 255, 255, 255]);
    }

    #[test]
    fn multi_layer_background_preserved() {
        let decls = parse_declarations("background: url(a.png) no-repeat, url(b.png) repeat-x");
        let mut n = StyleNode::default();
        for (k, v) in decls {
            n.decls.insert(k, v);
        }
        let r = crate::renderer::style::resolve(&n, 100.0);
        assert_eq!(r.bg_layers.len(), 2);
        assert!(matches!(r.bg_layers[0].paint, crate::renderer::style::BgPaint::Url(_)));
        assert!(matches!(r.bg_layers[1].paint, crate::renderer::style::BgPaint::Url(_)));
    }

    #[test]
    fn explicit_inherit_on_non_inherited_property() {
        let n = styled(r#"<style>body{height:40px}div{height:inherit}</style><div>x</div>"#);
        assert_eq!(find_tag(&n, "div").unwrap().decl("height").unwrap(), "40px");
    }

    #[test]
    fn unset_on_inherited_property_takes_parent() {
        let n = styled(r#"<style>body{color:#ffffff}div{color:unset}</style><div>x</div>"#);
        assert_eq!(rgb(find_tag(&n, "div").unwrap().decl("color").unwrap()), [255, 255, 255, 255]);
    }

    #[test]
    fn important_split_on_inline_parse() {
        let (normal, important) = parse_declarations_important("color: red; width: 10px !important");
        assert!(normal.iter().any(|(k, _)| k == "color"));
        assert!(important.iter().any(|(k, v)| k == "width" && v == "10px"));
        assert!(!important.iter().any(|(k, _)| k == "color"));
    }
}
