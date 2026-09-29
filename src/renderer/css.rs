//! 极简 CSS：样式表解析 + 选择器匹配 + 声明解析
//!
//! 模板生成的 CSS 是规范格式（无 hack），手写解析足够；
//! 值解析（颜色/渐变/尺寸）在 layout/paint 阶段按需处理。

use super::dom::{CssRule, SelectorPart, StyleNode};
use std::collections::BTreeMap;

/// 解析一段 CSS 文本（含注释处理）为规则表
pub fn parse_stylesheet(css: &str) -> Vec<CssRule> {
    let css = strip_comments(css);
    let mut rules = Vec::new();
    let mut depth = 0usize;
    let mut buf = String::new();
    let mut sel_part = String::new();

    for ch in css.chars() {
        match ch {
            '{' => {
                if depth == 0 {
                    sel_part = buf.trim().to_string();
                    buf.clear();
                }
                depth += 1;
                if depth > 1 {
                    buf.push(ch);
                }
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let decls = parse_declarations(&buf);
                    for sel in sel_part.split(',') {
                        let sel = sel.trim();
                        if sel.is_empty() {
                            continue;
                        }
                        let (parts, spec) = compile_selector(sel);
                        rules.push(CssRule { selector: parts, decls: decls.iter().cloned().collect(), specificity: spec });
                    }
                    buf.clear();
                    sel_part.clear();
                } else {
                    buf.push(ch);
                }
            }
            _ => {
                if depth >= 1 {
                    buf.push(ch);
                } else {
                    buf.push(ch);
                }
            }
        }
    }
    rules
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '/' && chars.peek() == Some(&'*') {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
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
                ri -= 2;
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
