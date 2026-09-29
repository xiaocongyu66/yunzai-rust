//! cosmic-text 封装：字体系统、文本测量、字形光栅化

use cosmic_text::{Attrs, AttrsList, Buffer, Color, Family, FontSystem, Metrics, SwashCache, Weight};

pub struct TextEngine {
    pub font_system: FontSystem,
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
        Ok(TextEngine { font_system: fs, swash: SwashCache::new() })
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
    ) -> (f32, f32) {
        let metrics = Metrics::new(font_size, line_height);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        let attrs = Attrs::new()
            .family(Family::SansSerif)
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
) -> Buffer {
    let metrics = Metrics::new(font_size, line_height);
    let mut buffer = Buffer::new(&mut engine.font_system, metrics);
    let color = Color::rgba(color[0], color[1], color[2], color[3]);
    let attrs = Attrs::new().family(Family::SansSerif).weight(Weight(weight)).color(color);
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
