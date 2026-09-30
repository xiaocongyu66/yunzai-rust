//! cosmic-text 封装：字体系统、文本测量、字形光栅化

use cosmic_text::{Attrs, AttrsList, Buffer, Color, Family, FontSystem, Metrics, SwashCache, Weight};
use std::collections::HashMap;

pub struct TextEngine {
    pub font_system: FontSystem,
    /// @font-face 别名表：CSS 引用名 → 字体文件内部真实 family 名
    pub font_aliases: HashMap<String, String>,
    pub swash: SwashCache,
}

impl TextEngine {
    /// 加载字体：项目目录优先，其次系统字体（/system/fonts、/usr/share/fonts 等）
    pub fn load(extra_dirs: &[String]) -> Result<TextEngine, String> {
        let mut fs = FontSystem::new();
        let mut loaded = 0usize;
        for dir in extra_dirs {
            let p = std::path::Path::new(dir);
            if p.is_dir() {
                fs.db_mut().load_fonts_dir(p);
                loaded += 1;
            } else if p.is_file() {
                if let Ok(data) = std::fs::read(p) {
                    fs.db_mut().load_font_data(data);
                    loaded += 1;
                }
            }
        }
        crate::util::make_log1(crate::logger::Level::Info, Some("Renderer"), format!("字体系统就绪（额外字体 {loaded}）"));
        Ok(TextEngine { font_system: fs, swash: SwashCache::new(), font_aliases: HashMap::new() })
    }

    /// 加载 @font-face 指向的字体文件，注册别名（CSS family 名 → 字体内部真实名）
    pub fn load_face_file(&mut self, path: &str, css_family: &str) {
        if self.font_aliases.contains_key(css_family) {
            return;
        }
        let Ok(data) = std::fs::read(path) else { return };
        let before = self.font_system.db().faces().count();
        self.font_system.db_mut().load_font_data(data);
        // 找到新加的 face，取其内部真实 family 名
        if let Some(face) = self.font_system.db().faces().nth(before) {
            let real = face
                .families
                .first()
                .map(|(n, _)| n.clone())
                .unwrap_or_else(|| css_family.to_string());
            self.font_aliases.insert(css_family.to_string(), real);
            crate::util::make_log1(
                crate::logger::Level::Info,
                Some("Renderer"),
                format!("自定义字体 {} → {}", css_family, path),
            );
        }
    }

    /// CSS family 别名解析：命中 @font-face 别名时替换为字体内部真实名
    pub fn resolve_family<'a>(&'a self, name: &'a str) -> &'a str {
        self.font_aliases.get(name).map(String::as_str).unwrap_or(name)
    }

    /// 排版一段文本，返回布局尺寸（宽 = 最长行，高 = 总行高）与行数
    pub fn measure(
        &mut self,
        text: &str,
        font_size: f32,
        weight: u16,
        color: [u8; 4],
        max_width: Option<f32>,
        line_height: f32,
        family: Option<&str>,
    ) -> (f32, f32) {
        let metrics = Metrics::new(font_size, line_height);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        let family = match family {
            Some(f) => Family::Name(f),
            None => Family::SansSerif,
        };
        let attrs = Attrs::new()
            .family(family)
            .weight(Weight(weight))
            .color(Color::rgba(color[0], color[1], color[2], color[3]));
        buffer.set_rich_text(
            &mut self.font_system,
            [(text, attrs)],
            attrs,
            cosmic_text::Shaping::Advanced,
        );
        if let Some(w) = max_width {
            buffer.set_size(&mut self.font_system, Some(w), None);
        }
        buffer.shape_until_scroll(&mut self.font_system, false);

        let mut max_w = 0f32;
        let mut total_h = 0f32;
        for run in buffer.layout_runs() {
            total_h += run.line_height;
            if run.line_w > max_w {
                max_w = run.line_w;
            }
        }
        (max_w, total_h.max(line_height))
    }
}

/// 文本布局结果（paint 用）
pub struct TextLayout {
    pub buffer: Buffer,
}

/// 生成用于绘制的 Buffer（调用方持有）
pub fn layout_buffer(
    engine: &mut TextEngine,
    text: &str,
    font_size: f32,
    weight: u16,
    color: [u8; 4],
    max_width: Option<f32>,
    line_height: f32,
    align: TextAlign,
    family: Option<&str>,
) -> Buffer {
    let metrics = Metrics::new(font_size, line_height);
    let mut buffer = Buffer::new(&mut engine.font_system, metrics);
    let color = Color::rgba(color[0], color[1], color[2], color[3]);
    let family = match family {
        Some(f) => Family::Name(f),
        None => Family::SansSerif,
    };
    let attrs = Attrs::new().family(family).weight(Weight(weight)).color(color);
    buffer.set_rich_text(
        &mut engine.font_system,
        [(text, attrs)],
        attrs,
        cosmic_text::Shaping::Advanced,
    );
    if let Some(w) = max_width {
        buffer.set_size(&mut engine.font_system, Some(w), None);
    }
    buffer.shape_until_scroll(&mut engine.font_system, false);
    // 对齐通过重排 glyph x 偏移在 paint 阶段处理
    let _ = align;
    buffer
}

#[derive(Clone, Copy, Default, PartialEq, Debug)]
pub enum TextAlign {
    #[default]
    Left,
    Center,
    Right,
}

impl TextAlign {
    pub fn parse(s: &str) -> TextAlign {
        match s {
            "center" => TextAlign::Center,
            "right" | "end" => TextAlign::Right,
            _ => TextAlign::Left,
        }
    }
}
