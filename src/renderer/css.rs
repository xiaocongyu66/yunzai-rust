//! 极简 CSS：样式表解析 + 选择器匹配 + 声明解析
//!
//! 模板生成的 CSS 是规范格式（无 hack），手写解析足够；
//! 值解析（颜色/渐变/尺寸）在 layout/paint 阶段按需处理。

use super::dom::{CssRule, SelectorPart, StyleNode};
use std::collections::BTreeMap;

/// 解析样式表：Lightning CSS 结构化遍历（Style 规则 + @font-face）
pub fn parse_stylesheet(css: &str) -> Vec<CssRule> {
    parse_stylesheet_lc(css).0
}

/// 伪元素规则（::before / ::after）
#[derive(Clone, Debug)]
pub struct PseudoRule {
    pub parent: Vec<SelectorPart>,
    pub which: &'static str,
    pub decls: BTreeMap<String, String>,
}

/// 拆分选择器尾部的伪元素，返回 (主体, Some(伪元素名)) 或 (原样, None)
fn split_pseudo(sel: &str) -> (String, Option<&'static str>) {
    let s = sel.trim();
    for (lit, which) in [
        ("::before", "before"),
        ("::after", "after"),
        (":before", "before"),
        (":after", "after"),
    ] {
        if let Some(rest) = s.strip_suffix(lit) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return (rest.to_string(), Some(which));
            }
        }
    }
    (s.to_string(), None)
}

/// 返回 (样式规则, @font-face 列表, 伪元素规则列表)
pub fn parse_stylesheet_lc(css: &str) -> (Vec<CssRule>, Vec<(String, String)>, Vec<PseudoRule>) {
    use lightningcss::rules::font_face::{FontFaceProperty, Source};
    use lightningcss::rules::CssRule as LcRule;
    use lightningcss::traits::ToCss;
    let mut out = Vec::new();
    let mut faces: Vec<(String, String)> = Vec::new();
    let mut pseudos: Vec<PseudoRule> = Vec::new();
    let Ok(ss) = lightningcss::stylesheet::StyleSheet::parse(css, lightningcss::stylesheet::ParserOptions::default()) else {
        return (out, faces, pseudos);
    };
    for rule in ss.rules.0.iter() {
        match rule {
            LcRule::Style(st) => {
                let mut decls: Vec<(String, String)> = Vec::new();
                for d in st.declarations.declarations.iter() {
                    serialize_decl(d, &mut decls);
                }
                let sel_str = st.selectors.to_css_string(lc_opts()).unwrap_or_default();
                for one in sel_str.split(',') {
                    let one = one.trim();
                    if one.is_empty() {
                        continue;
                    }
                    let (body, which) = split_pseudo(one);
                    // 伪类（:hover/:nth-child 等）暂不支持，跳过
                    if which.is_none() && body.contains(':') {
                        continue;
                    }
                    if let Some(w) = which {
                        let (parts, _) = compile_selector(&body);
                        pseudos.push(PseudoRule {
                            parent: parts,
                            which: match w {
                                "before" => "before",
                                _ => "after",
                            },
                            decls: decls.iter().cloned().collect(),
                        });
                        continue;
                    }
                    let (parts, spec) = compile_selector(&body);
                    out.push(CssRule { selector: parts, decls: decls.iter().cloned().collect(), specificity: spec });
                }
            }
            LcRule::FontFace(ff) => {
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
                            for sv in list {
                                if let Source::Url(u) = sv {
                                    src = u.url.url.to_string();
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if !fam.is_empty() && !src.is_empty() {
                    faces.push((fam, src));
                }
            }
            _ => {}
        }
    }
    (out, faces, pseudos)
}

/// 应用伪元素规则：命中 parent 选择器的节点，注入 ::before/::after 虚拟子节点
pub fn apply_pseudo(root: &mut StyleNode, rules: &[PseudoRule]) {
    if rules.is_empty() {
        return;
    }
    apply_pseudo_rec(root, rules, &[]);
}

fn apply_pseudo_rec(node: &mut StyleNode, rules: &[PseudoRule], ancestors: &[NodeKey]) {
    let key = key_of(node);
    // 当前节点作为"父"匹配：selector_matches 最后一格匹配 cur
    let mut be: Option<PseudoRule> = None;
    let mut af: Option<PseudoRule> = None;
    for r in rules {
        if selector_matches(&r.parent, ancestors, &key) {
            match r.which {
                "before" => be = Some(r.clone()),
                _ => af = Some(r.clone()),
            }
        }
    }
    if let Some(r) = be {
        let content = r.decls.get("content").cloned().unwrap_or_default();
        let content = content.trim_matches(|c| c == '"' || c == '\'').to_string();
        let mut child = StyleNode::new("::before".to_string());
        child.decls = r.decls.clone();
        child.decls.remove("content");
        child.text = content;
        node.children.insert(0, child);
    }
    if let Some(r) = af {
        let content = r.decls.get("content").cloned().unwrap_or_default();
        let content = content.trim_matches(|c| c == '"' || c == '\'').to_string();
        let mut child = StyleNode::new("::after".to_string());
        child.decls = r.decls.clone();
        child.decls.remove("content");
        child.text = content;
        node.children.push(child);
    }
    let mut child_anc: Vec<NodeKey> = ancestors.to_vec();
    child_anc.push(key);
    for c in node.children.iter_mut() {
        apply_pseudo_rec(c, rules, &child_anc);
    }
}

/// `div.card > .name span` → [Tag(div), Class(card), Child, Class(name), Descendant, Tag(span)]
fn compile_selector(sel: &str) -> (Vec<SelectorPart>, u32) {
    let mut parts = Vec::new();
    let mut spec = 0u32;
    for raw in sel.split_whitespace() {
        // 处理 `>`（可能独立或粘连）
        for token in split_combinators(raw) {
            match token {
                Combinator::Child => parts.push(SelectorPart::Child),
                Combinator::Desc => parts.push(SelectorPart::Descendant),
                Combinator::Simple(s) => {
                    if let Some(cls) = s.strip_prefix('.') {
                        parts.push(SelectorPart::Class(cls.to_string()));
                        spec += 10;
                    } else if let Some(id) = s.strip_prefix('#') {
                        parts.push(SelectorPart::Id(id.to_string()));
                        spec += 100;
                    } else if s == "*" {
                        parts.push(SelectorPart::Tag("*".into()));
                    } else {
                        parts.push(SelectorPart::Tag(s.to_string()));
                        spec += 1;
                    }
                }
            }
        }
    }
    (parts, spec)
}

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
    use lightningcss::stylesheet::{ParserOptions, StyleSheet};
    let src = format!("a{{ {} }}", s);
    let mut ss = StyleSheet::parse(&src, ParserOptions::default()).ok()?;
    let rule = ss.rules.0.first_mut()?;
    let style = match rule {
        lightningcss::rules::CssRule::Style(st) => st,
        _ => return None,
    };
    let mut out = Vec::new();
    for decl in style.declarations.declarations.iter() {
        serialize_decl(decl, &mut out);
    }
    Some(out)
}

/// 单条 lightningcss 声明 → (属性名, 值) 列表（简写展开 longhand）
pub fn serialize_decl(d: &lightningcss::properties::Property, out: &mut Vec<(String, String)>) {
    use lightningcss::properties::Property;
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
            for b in list {
                out.push(("background-image".into(), lc_val(&b.image)));
                out.push(("background-color".into(), lc_val(&b.color)));
                out.push(("background-position".into(), lc_val(&b.position)));
                out.push(("background-size".into(), lc_val(&b.size)));
                out.push(("background-repeat".into(), lc_val(&b.repeat)));
            }
        }
        Property::Gap(g) => {
            out.push(("row-gap".into(), lc_val(&g.row)));
            out.push(("column-gap".into(), lc_val(&g.column)));
        }
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
    let mut sorted: Vec<&CssRule> = rules.iter().collect();
    sorted.sort_by_key(|r| r.specificity);
    let refs: Vec<CssRule> = sorted.into_iter().cloned().collect();
    apply_rec(&mut root, &refs, &[]);
    root
}

/// 祖先快照（匹配用）
#[derive(Clone)]
struct NodeKey {
    tag: String,
    classes: Vec<String>,
    id: Option<String>,
}

fn key_of(n: &StyleNode) -> NodeKey {
    NodeKey { tag: n.tag.clone(), classes: n.classes.clone(), id: n.id.clone() }
}

fn apply_rec(node: &mut StyleNode, rules: &[CssRule], ancestors: &[NodeKey]) {
    let key = key_of(node);

    let mut matched_decls: BTreeMap<String, String> = BTreeMap::new();
    for rule in rules {
        if selector_matches(&rule.selector, ancestors, &key) {
            for (k, v) in &rule.decls {
                matched_decls.insert(k.clone(), v.clone());
            }
        }
    }
    // 规则声明先落，inline（已在 decls）覆盖
    let inline = node.decls.clone();
    for (k, v) in matched_decls {
        node.decls.entry(k).or_insert(v);
    }
    for (k, v) in inline {
        node.decls.insert(k, v);
    }

    let mut child_anc: Vec<NodeKey> = ancestors.to_vec();
    child_anc.push(key);
    for c in node.children.iter_mut() {
        apply_rec(c, rules, &child_anc);
    }
}

/// 选择器匹配：从最后一段（当前节点）向前，沿祖先链回溯
fn selector_matches(parts: &[SelectorPart], ancestors: &[NodeKey], cur: &NodeKey) -> bool {
    let Some((last, rest)) = parts.split_last() else {
        return false;
    };
    if !part_matches(last, cur) {
        return false;
    }
    if rest.is_empty() {
        return true;
    }
    // 组合子驱动：逐段消耗 rest（从右向左），ancestors 也从右向左
    let mut ai = ancestors.len(); // 下一个可比较的祖先索引（从最近的父开始）
    let mut ri = rest.len();
    while ri > 0 {
        // 期望一个简单选择器段
        let need = &rest[ri - 1];
        if matches!(need, SelectorPart::Child | SelectorPart::Descendant) {
            ri -= 1;
            continue;
        }
        let combinator = if ri >= 2 { rest[ri - 2].clone() } else { SelectorPart::Descendant };
        match combinator {
            SelectorPart::Child => {
                // 父必须直接匹配 need
                if ai == 0 || !part_matches(need, &ancestors[ai - 1]) {
                    return false;
                }
                ai -= 1;
                ri -= 2;
            }
            _ => {
                // 后代：向上找到第一个匹配
                let mut found = false;
                while ai > 0 {
                    ai -= 1;
                    if part_matches(need, &ancestors[ai]) {
                        found = true;
                        break;
                    }
                }
                if !found {
                    return false;
                }
                ri = ri.saturating_sub(2); // rest 末段无组合子可消耗，防下溢
            }
        }
    }
    true
}

fn part_matches(p: &SelectorPart, n: &NodeKey) -> bool {
    match p {
        SelectorPart::Tag(t) => t == "*" || *t == n.tag,
        SelectorPart::Class(c) => n.classes.iter().any(|x| x == c),
        SelectorPart::Id(i) => n.id.as_deref() == Some(i.as_str()),
        _ => false,
    }
}

fn simple_matches(p: &SelectorPart, s: &str) -> bool {
    match p {
        SelectorPart::Tag(t) => t == "*" || t == s,
        SelectorPart::Class(c) => c == s,
        SelectorPart::Id(i) => i == s,
        _ => false,
    }
}

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
