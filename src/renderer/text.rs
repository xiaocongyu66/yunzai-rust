//! cosmic-text 封装：字体系统、文本测量、字形光栅化

use cosmic_text::{Attrs, AttrsList, Buffer, Color, Family, FontSystem, Metrics, SwashCache, Weight};
use std::collections::{BTreeMap, HashMap};

/// white-space 是否为不换行形态（nowrap / pre）
pub fn white_space_nowrap(decls: &BTreeMap<String, String>) -> bool {
    matches!(
        decls.get("white-space").map(String::as_str).unwrap_or("").trim(),
        "nowrap" | "pre"
    )
}

/// text-overflow 是否为 ellipsis（配合 white-space:nowrap 单行截断）
pub fn text_overflow_ellipsis(decls: &BTreeMap<String, String>) -> bool {
    decls.get("text-overflow").map(String::as_str).unwrap_or("").trim() == "ellipsis"
}

/// letter-spacing（px；normal / 非法值按 0）
pub fn letter_spacing_px(decls: &BTreeMap<String, String>) -> f32 {
    decls
        .get("letter-spacing")
        .and_then(|v| v.trim().trim_end_matches("px").trim().parse::<f32>().ok())
        .unwrap_or(0.0)
}

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
        // fontdb（ttf-parser）只认 sfnt：WOFF1 解包成 TTF 再灌入
        let data = if data.starts_with(b"wOFF") {
            match woff_to_ttf(&data) {
                Some(ttf) => ttf,
                None => {
                    crate::util::make_log1(crate::logger::Level::Warn, Some("Renderer"), format!("WOFF 解包失败: {path}"));
                    return;
                }
            }
        } else {
            data
        };
        let before = self.font_system.db().faces().count();
        self.font_system.db_mut().load_font_data(data);
        // fontdb 对非法数据静默忽略：faces 数没涨说明没注册成功
        if self.font_system.db().faces().count() == before {
            crate::util::make_log1(crate::logger::Level::Warn, Some("Renderer"), format!("字体未注册（格式不支持）: {path}"));
            return;
        }
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
    /// letter_spacing（px）：每行按 glyph 数补加累计字距（与 draw 回调的逐 glyph 偏移一致）
    pub fn measure(
        &mut self,
        text: &str,
        font_size: f32,
        weight: u16,
        color: [u8; 4],
        max_width: Option<f32>,
        line_height: f32,
        family: Option<&str>,
        letter_spacing: f32,
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
            // letter-spacing：第 i 个 glyph 右移 i*spacing，行宽补加 (n-1)*spacing
            let mut w = run.line_w;
            if letter_spacing != 0.0 && run.glyphs.len() > 1 {
                w += letter_spacing * (run.glyphs.len() - 1) as f32;
            }
            if w > max_w {
                max_w = w;
            }
        }
        (max_w, total_h.max(line_height))
    }

    /// 单行省略号截断（text-overflow:ellipsis）：自然宽度超 max_width 时从尾部剔字
    /// 并追加 "…"（U+2026，同字体同色随正文绘制）。无需截断时返回 None。
    /// 宽度按"最大可行前缀"二分近似，与 measure 同一排版管线。
    pub fn ellipsize(
        &mut self,
        text: &str,
        font_size: f32,
        weight: u16,
        line_height: f32,
        family: Option<&str>,
        max_width: f32,
        letter_spacing: f32,
    ) -> Option<String> {
        if max_width <= 0.0 || text.is_empty() {
            return None;
        }
        let probe = [0u8, 0, 0, 255];
        let (full_w, _) = self.measure(text, font_size, weight, probe, None, line_height, family, letter_spacing);
        if full_w <= max_width {
            return None;
        }
        let ell = "…";
        let (ell_w, _) = self.measure(ell, font_size, weight, probe, None, line_height, family, letter_spacing);
        if ell_w > max_width {
            // 连省略号都放不下：截为空
            return Some(String::new());
        }
        let budget = max_width - ell_w;
        let chars: Vec<char> = text.chars().collect();
        // 宽度随前缀长度单调不减 → 二分最大可行前缀
        let mut lo = 0usize;
        let mut hi = chars.len();
        while lo < hi {
            let mid = (lo + hi + 1) / 2;
            let cand: String = chars[..mid].iter().collect();
            let (w, _) = self.measure(&cand, font_size, weight, probe, None, line_height, family, letter_spacing);
            if w <= budget {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let mut out: String = chars[..lo].iter().collect();
        while out.ends_with(char::is_whitespace) {
            out.pop();
        }
        out.push_str(ell);
        Some(out)
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

/// 带 letter-spacing 的 buffer 绘制：回调签名与 Buffer::draw 完全一致
/// (gx, gy, gw, gh, color)，第 i 个 glyph 的 x 坐标累计偏移 i * letter_spacing
/// （cosmic-text 0.12 的 shaping 阶段无原生字距 API，故在物理定位阶段逐 glyph 推进）。
/// letter_spacing == 0 时与 buffer.draw 等价。
/// 绘制层（paint）可将 `buffer.draw(..)` 直接替换为本调用（字距取 PaintNode.letter_spacing）。
#[allow(dead_code)]
pub fn draw_with_letter_spacing<F>(
    buffer: &Buffer,
    engine: &mut TextEngine,
    color: Color,
    letter_spacing: f32,
    mut f: F,
) where
    F: FnMut(i32, i32, u32, u32, Color),
{
    if letter_spacing == 0.0 {
        buffer.draw(&mut engine.font_system, &mut engine.swash, color, f);
        return;
    }
    for run in buffer.layout_runs() {
        for (i, glyph) in run.glyphs.iter().enumerate() {
            let physical = glyph.physical((i as f32 * letter_spacing, 0.0), 1.0);
            let glyph_color = glyph.color_opt.unwrap_or(color);
            engine.swash.with_pixels(
                &mut engine.font_system,
                physical.cache_key,
                glyph_color,
                |x, y, c| {
                    f(
                        physical.x + x,
                        run.line_y as i32 + physical.y + y,
                        1,
                        1,
                        c,
                    );
                },
            );
        }
    }
}

/// WOFF1 → sfnt(TTF) 解包：44B 头 + 20B/项表目录，表数据 zlib；重组为标准 TTF 布局
fn woff_to_ttf(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    if data.len() < 44 || &data[0..4] != b"wOFF" {
        return None;
    }
    let u32at = |o: usize| -> Option<u32> {
        data.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    };
    let flavor = u32at(4)?;
    let num_tables = u16::from_be_bytes([data[12], data[13]]) as usize;
    if num_tables == 0 || data.len() < 44 + num_tables * 20 {
        return None;
    }

    struct Tbl {
        tag: [u8; 4],
        data: Vec<u8>,
        checksum: u32,
    }
    let mut tables: Vec<Tbl> = Vec::with_capacity(num_tables);
    for i in 0..num_tables {
        let o = 44 + i * 20;
        let tag = [data[o], data[o + 1], data[o + 2], data[o + 3]];
        let off = u32at(o + 4)? as usize;
        let comp = u32at(o + 8)? as usize;
        let orig = u32at(o + 12)? as usize;
        let checksum = u32at(o + 16)?;
        let raw = data.get(off..off + comp)?;
        let tbl = if comp == orig {
            raw.to_vec()
        } else {
            let mut d = Vec::with_capacity(orig);
            flate2::read::ZlibDecoder::new(raw).read_to_end(&mut d).ok()?;
            if d.len() != orig {
                return None;
            }
            d
        };
        tables.push(Tbl { tag, data: tbl, checksum });
    }
    // sfnt 目录要求 tag 升序
    tables.sort_by(|a, b| a.tag.cmp(&b.tag));

    let hdr_len = 12 + tables.len() * 16;
    let mut offsets = Vec::with_capacity(tables.len());
    let mut cur = hdr_len;
    for t in &tables {
        offsets.push(cur as u32);
        cur += (t.data.len() + 3) & !3;
    }

    let mut out = Vec::with_capacity(cur);
    out.extend_from_slice(&flavor.to_be_bytes());
    let n = tables.len() as u16;
    out.extend_from_slice(&n.to_be_bytes());
    let mut entry_selector = 0u16;
    while (1u16 << (entry_selector + 1)) <= n {
        entry_selector += 1;
    }
    let search_range = (1u16 << entry_selector) * 16;
    out.extend_from_slice(&search_range.to_be_bytes());
    out.extend_from_slice(&entry_selector.to_be_bytes());
    out.extend_from_slice(&(n * 16 - search_range).to_be_bytes());
    for (i, t) in tables.iter().enumerate() {
        out.extend_from_slice(&t.tag);
        out.extend_from_slice(&t.checksum.to_be_bytes());
        out.extend_from_slice(&offsets[i].to_be_bytes());
        out.extend_from_slice(&(t.data.len() as u32).to_be_bytes());
    }
    for t in &tables {
        out.extend_from_slice(&t.data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    Some(out)
}
