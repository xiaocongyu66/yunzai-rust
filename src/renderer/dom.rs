//! html5ever → StyleNode 树 + `<style>` 块提取

use html5ever::{parse_document, LocalName, QualName};
use tendril::TendrilSink;
use markup5ever_rcdom::{Handle, NodeData, RcDom};
use std::collections::BTreeMap;

#[derive(Debug, Default, Clone)]
pub struct StyleNode {
    pub tag: String,
    pub id: Option<String>,
    pub classes: Vec<String>,
    /// 合并后的声明（inline 优先）
    pub decls: BTreeMap<String, String>,
    /// 文本叶子节点的内容
    pub text: String,
    pub children: Vec<StyleNode>,
    /// 图片源（img src，渲染期解析）
    pub src: Option<String>,
}

impl StyleNode {
    pub fn decl(&self, name: &str) -> Option<&str> {
        self.decls.get(name).map(String::as_str)
    }
}

/// 标签默认 display：CSS 行内（phrasing）元素 → true(inline)，其余默认块级。
/// 供 layout::style_of 做 display 近似（CSS 显式声明 display 时优先于标签默认）。
pub fn tag_inline(tag: &str) -> bool {
    matches!(
        tag,
        "span" | "a" | "strong" | "em" | "b" | "i" | "code" | "label" | "small" | "u" | "s"
            | "sub" | "sup" | "abbr" | "bdi" | "bdo" | "big" | "br" | "button" | "cite" | "data"
            | "dfn" | "font" | "ins" | "kbd" | "mark" | "nobr" | "output" | "q" | "rp" | "rt"
            | "ruby" | "samp" | "time" | "tt" | "var" | "wbr" | "::before" | "::after"
    )
}

#[derive(Clone)]
pub struct CssRule {
    /// parcel_selectors 结构化选择器（Servo 同款匹配，nth/组合子全由其处理）
    pub selector: crate::renderer::matcher::SelSelector,
    pub decls: BTreeMap<String, String>,
    pub specificity: u32,
}

/// 选择器单段（如 `.a .b > span` → 三段）
#[derive(Debug, Clone, PartialEq)]
pub enum SelectorPart {
    Tag(String),
    Class(String),
    Id(String),
    /// 后代关系分隔（空格）
    Descendant,
    /// 子代关系分隔（>）
    Child,
}

pub fn parse(html: &str) -> Result<(StyleNode, Vec<CssRule>, Vec<(String, String)>, Vec<crate::renderer::css::PseudoRule>), String> {
    parse_with_base(html, ".")
}

pub fn parse_with_base(
    html: &str,
    base_dir: &str,
) -> Result<(StyleNode, Vec<CssRule>, Vec<(String, String)>, Vec<crate::renderer::css::PseudoRule>), String> {
    let dom: RcDom = parse_document(RcDom::default(), Default::default())
        .from_utf8()
        .one(html.as_bytes());

    let mut rules = vec![];
    // (样式文本, 基准目录)——@font-face/@import 的 url 相对各自来源解析
    let mut chunks: Vec<(String, String)> = Vec::new();
    let mut link_hrefs: Vec<String> = vec![];
    walk(&dom.document, &mut |h| {
        if let NodeData::Element { name, attrs, .. } = &h.data {
            let local = name.local.to_string();
            match local.as_str() {
                "style" => {
                    let mut text = String::new();
                    collect_text(h, &mut text);
                    chunks.push((text, base_dir.to_string()));
                }
                "link" => {
                    let mut rel_ok = false;
                    let mut href = String::new();
                    for a in attrs.borrow().iter() {
                        match a.name.local.to_string().as_str() {
                            "rel" => {
                                if a.value.to_lowercase().contains("stylesheet") {
                                    rel_ok = true;
                                }
                            }
                            "href" => href = a.value.to_string(),
                            _ => {}
                        }
                    }
                    if rel_ok && !href.is_empty() {
                        link_hrefs.push(href);
                    }
                }
                _ => {}
            }
        }
    });

    // 外部 CSS：<link href>（art-template 已把 _res_path 替换为绝对路径）——基准目录 = css 文件所在目录
    for href in link_hrefs {
        let path = crate::renderer::media::resolve(&href, base_dir);
        if let Ok(css) = std::fs::read_to_string(&path) {
            let dir = std::path::Path::new(&path)
                .parent()
                .map(|d| d.display().to_string())
                .unwrap_or_else(|| base_dir.to_string());
            chunks.push((css, dir));
        }
    }

    let (mut parsed_rules, mut font_faces, mut pseudo_rules) = (vec![], vec![], vec![]);
    for (css_text, dir) in &chunks {
        let (r, f, p) = crate::renderer::css::parse_stylesheet_lc(css_text, dir);
        parsed_rules.extend(r);
        font_faces.extend(f);
        pseudo_rules.extend(p);
    }
    rules = parsed_rules;

    let mut root = build(&dom.document);
    // 取 body（没有则用整个树）
    if root.children.len() == 1 {
        root = root.children.remove(0);
    } else if let Some(idx) = root.children.iter().position(|c| c.tag == "body") {
        root = root.children.remove(idx);
    }
    Ok((root, rules, font_faces, pseudo_rules))
}

fn walk(h: &Handle, f: &mut impl FnMut(&Handle)) {
    f(h);
    for c in h.children.borrow().iter() {
        walk(c, f);
    }
}

fn collect_text(h: &Handle, out: &mut String) {
    for c in h.children.borrow().iter() {
        if let NodeData::Text { contents } = &c.data {
            out.push_str(&contents.borrow());
        }
    }
}

fn tag_name(h: &Handle) -> String {
    match &h.data {
        NodeData::Element { name, .. } => name.local.to_string(),
        NodeData::Document => "#document".into(),
        NodeData::Text { .. } => "#text".into(),
        NodeData::Comment { .. } => "#comment".into(),
        _ => "#other".into(),
    }
}

fn build(h: &Handle) -> StyleNode {
    let mut node = StyleNode { tag: tag_name(h), ..Default::default() };

    if let NodeData::Element { name, attrs, .. } = &h.data {
        node.tag = name.local.to_string();
        // presentation hints（width/height 属性）先入 decls，style 属性后入（覆盖属性，与 CSS 优先级一致）
        let mut style_attr = None;
        for a in attrs.borrow().iter() {
            let k = a.name.local.to_string();
            let v = a.value.to_string();
            match k.as_str() {
                "id" => node.id = Some(v),
                "class" => node.classes = v.split_whitespace().map(String::from).collect(),
                "style" => style_attr = Some(v),
                "src" => node.src = Some(v),
                "width" | "height" if node.tag == "img" => {
                    if let Ok(n) = v.trim().parse::<f32>() {
                        node.decls.insert(k, format!("{n}px"));
                    }
                }
                _ => {}
            }
        }
        if let Some(sv) = style_attr {
            for (dk, dv) in crate::renderer::css::parse_declarations(&sv) {
                node.decls.insert(dk, dv);
            }
        }
    }

    // 文本聚合：元素内直接文本
    let mut direct = String::new();
    for c in h.children.borrow().iter() {
        match &c.data {
            NodeData::Text { contents } => direct.push_str(&contents.borrow()),
            _ => {
                let t = tag_name(c);
                // 不可见标签不产生节点：script 直接跳过子树；style 的 CSS 内容
                // 已由 walk 收集进样式表；head/title/meta/link/base/noscript/template 不渲染
                if matches!(
                    t.as_str(),
                    "#comment" | "head" | "script" | "style" | "title" | "meta" | "link"
                        | "base" | "noscript" | "template"
                ) {
                    continue;
                }
                node.children.push(build(c));
            }
        }
    }
    node.text = direct.split_whitespace().collect::<Vec<_>>().join(" ");
    node
}
