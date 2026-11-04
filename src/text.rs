//! Text shaping, measuring and rasterizing (cosmic-text + swash).

use std::collections::HashMap;
use std::path::Path;

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, Weight, Wrap};
use tiny_skia::Pixmap;

use crate::element::{TextAlign, TextSpec};
use crate::{Color, Rect};

/// Line height relative to font size; ascent + descent of Noto Sans, matching Qt's
/// `fontMetrics.height`.
pub const LINE_HEIGHT: f32 = 1.362;

const ELLIPSIS: &str = "…";

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Key {
    text: String,
    size: u32,
    weight: u16,
    width: Option<u32>,
}

struct Cached {
    buffer: Buffer,
    size: (f32, f32),
    used: u64,
}

pub struct TextSystem {
    fonts: FontSystem,
    swash: SwashCache,
    preferred: Vec<String>,
    family: Option<String>,
    cache: HashMap<Key, Cached>,
    frame: u64,
}

impl TextSystem {
    /// Loads installed system fonts. `families` are tried in order.
    pub fn new(families: &[String]) -> Self {
        Self::with_font_system(FontSystem::new(), families)
    }

    /// Only the given font files, no system fonts: deterministic output for tests.
    pub fn from_font_data(fonts: Vec<Vec<u8>>, families: &[String]) -> Self {
        let mut db = cosmic_text::fontdb::Database::new();
        for data in fonts {
            db.load_font_data(data);
        }
        Self::with_font_system(FontSystem::new_with_locale_and_db("en-US".into(), db), families)
    }

    fn with_font_system(fonts: FontSystem, families: &[String]) -> Self {
        let mut system = Self {
            fonts,
            swash: SwashCache::new(),
            preferred: families.to_vec(),
            family: None,
            cache: HashMap::new(),
            frame: 0,
        };
        system.resolve_family();
        system
    }

    /// Adds fonts from a file or every font file in a directory (e.g. bundled Noto Sans).
    pub fn load_fonts(&mut self, path: &Path) {
        if path.is_dir() {
            self.fonts.db_mut().load_fonts_dir(path);
        } else {
            let _ = self.fonts.db_mut().load_font_file(path);
        }
        self.cache.clear();
        self.resolve_family();
    }

    /// The installed family in use, `None` when falling back to the generic sans-serif.
    pub fn family(&self) -> Option<&str> {
        self.family.as_deref()
    }

    fn resolve_family(&mut self) {
        let db = self.fonts.db();
        self.family = self
            .preferred
            .iter()
            .find(|wanted| {
                db.faces().any(|face| {
                    face.families
                        .iter()
                        .any(|(name, _)| name.eq_ignore_ascii_case(wanted))
                })
            })
            .cloned();
    }

    fn entry(&mut self, text: &str, size: f32, weight: u16, width: Option<f32>) -> &mut Cached {
        let key = Key {
            text: text.to_owned(),
            size: size.to_bits(),
            weight,
            width: width.map(|w| w.max(0.0).to_bits()),
        };
        let frame = self.frame;
        let fonts = &mut self.fonts;
        let family = self.family.as_deref();
        let cached = self.cache.entry(key).or_insert_with(|| {
            let mut buffer = Buffer::new(fonts, Metrics::new(size, size * LINE_HEIGHT));
            buffer.set_wrap(if width.is_some() { Wrap::Word } else { Wrap::None });
            buffer.set_size(width.map(|w| w.max(0.0)), None);
            let attrs = Attrs::new()
                .family(family.map_or(Family::SansSerif, Family::Name))
                .weight(Weight(weight));
            buffer.set_text(text, &attrs, Shaping::Advanced, None);
            buffer.shape_until_scroll(fonts, false);
            let mut w: f32 = 0.0;
            let mut lines = 0;
            for run in buffer.layout_runs() {
                w = w.max(run.line_w);
                lines += 1;
            }
            let h = size * LINE_HEIGHT * lines.max(1) as f32;
            Cached {
                buffer,
                size: (w, h),
                used: frame,
            }
        });
        cached.used = frame;
        cached
    }

    /// Size in logical pixels. `wrap_width` applies only to wrapping text.
    pub fn measure(&mut self, spec: &TextSpec, default_size: f32, wrap_width: Option<f32>) -> (f32, f32) {
        let size = spec.size.unwrap_or(default_size);
        let width = if spec.wrap { wrap_width } else { None };
        self.entry(&spec.text, size, spec.weight, width).size
    }

    /// Longest prefix that fits `max_width` with an ellipsis appended.
    pub fn elide(&mut self, text: &str, size: f32, weight: u16, max_width: f32) -> String {
        if self.entry(text, size, weight, None).size.0 <= max_width + 0.01 {
            return text.to_owned();
        }
        let bounds: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
        let (mut lo, mut hi) = (0, bounds.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let prefix = &text[..bounds.get(mid).copied().unwrap_or(text.len())];
            let candidate = format!("{}{ELLIPSIS}", prefix.trim_end());
            if self.entry(&candidate, size, weight, None).size.0 <= max_width + 0.01 {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let prefix = &text[..bounds.get(lo).copied().unwrap_or(text.len())];
        format!("{}{ELLIPSIS}", prefix.trim_end())
    }

    /// Draws text into `rect` (logical), vertically centered, clipped to `clip` (logical).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn draw(
        &mut self,
        pixmap: &mut Pixmap,
        spec: &TextSpec,
        default_size: f32,
        rect: Rect,
        scale: f32,
        color: Color,
        clip: Rect,
    ) {
        let size = spec.size.unwrap_or(default_size);
        let elided;
        let text = if spec.elide && !spec.wrap {
            // Layout rounds edges to whole pixels, which can take up to a pixel off a text's
            // measured width; that is not a reason to cut it.
            elided = self.elide(&spec.text, size, spec.weight, rect.w + 1.0);
            elided.as_str()
        } else {
            spec.text.as_str()
        };
        let width = spec.wrap.then_some(rect.w);
        let (_, text_h) = self.entry(text, size, spec.weight, width).size;
        let key = Key {
            text: text.to_owned(),
            size: size.to_bits(),
            weight: spec.weight,
            width: width.map(|w| w.max(0.0).to_bits()),
        };
        let Self {
            fonts, swash, cache, ..
        } = self;
        let Some(cached) = cache.get(&key) else {
            return;
        };
        let y0 = rect.y + ((rect.h - text_h) / 2.0).max(0.0);
        let clip_px = (
            (clip.x * scale).floor() as i32,
            (clip.y * scale).floor() as i32,
            (clip.right() * scale).ceil() as i32,
            (clip.bottom() * scale).ceil() as i32,
        );
        let [cr, cg, cb, _] = color.to_rgba8();
        let base = cosmic_text::Color::rgba(cr, cg, cb, 255);
        let (pw, ph) = (pixmap.width() as i32, pixmap.height() as i32);
        let data = pixmap.data_mut();
        for run in cached.buffer.layout_runs() {
            let dx = match spec.align {
                TextAlign::Left => 0.0,
                TextAlign::Center => (rect.w - run.line_w) / 2.0,
                TextAlign::Right => rect.w - run.line_w,
            };
            let offset = ((rect.x + dx) * scale, (y0 + run.line_y) * scale);
            for glyph in run.glyphs {
                let physical = glyph.physical(offset, scale);
                swash.with_pixels(fonts, physical.cache_key, base, |x, y, c| {
                    let (x, y) = (physical.x + x, physical.y + y);
                    if x < clip_px.0.max(0)
                        || y < clip_px.1.max(0)
                        || x >= clip_px.2.min(pw)
                        || y >= clip_px.3.min(ph)
                    {
                        return;
                    }
                    let alpha = f32::from(c.a()) / 255.0 * color.a;
                    blend(data, (y * pw + x) as usize * 4, [c.r(), c.g(), c.b()], alpha);
                });
            }
        }
    }

    /// Drops cached layouts not used during the previous frame.
    pub(crate) fn end_frame(&mut self) {
        let current = self.frame;
        self.cache.retain(|_, c| c.used >= current);
        self.frame += 1;
    }

    #[cfg(test)]
    pub(crate) fn cached_layouts(&self) -> usize {
        self.cache.len()
    }
}

/// Source-over blend of a straight color into premultiplied RGBA8.
fn blend(data: &mut [u8], index: usize, rgb: [u8; 3], alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let inv = 1.0 - alpha.min(1.0);
    for channel in 0..3 {
        let src = f32::from(rgb[channel]) * alpha;
        data[index + channel] = (src + f32::from(data[index + channel]) * inv).round().min(255.0) as u8;
    }
    data[index + 3] = (alpha * 255.0 + f32::from(data[index + 3]) * inv)
        .round()
        .min(255.0) as u8;
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A real font file from the machine running the tests, loaded alone so measurements do
    /// not depend on the rest of the installed fonts. `None` skips text assertions.
    pub(crate) fn test_text() -> Option<TextSystem> {
        const CANDIDATES: &[&str] = &[
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "C:\\Windows\\Fonts\\segoeui.ttf",
            "/System/Library/Fonts/Supplemental/Arial.ttf",
        ];
        let data = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok())?;
        Some(TextSystem::from_font_data(vec![data], &[]))
    }

    fn spec(text: &str) -> TextSpec {
        TextSpec {
            text: text.into(),
            size: None,
            weight: 400,
            color: None,
            wrap: false,
            elide: false,
            align: TextAlign::Left,
        }
    }

    #[test]
    fn measure_grows_with_text_and_wraps_to_width() {
        let Some(mut text) = test_text() else { return };
        let (short, h) = text.measure(&spec("CPU"), 13.0, None);
        let (long, _) = text.measure(&spec("CPU usage"), 13.0, None);
        assert!(short > 0.0 && long > short);
        assert!((h - 13.0 * LINE_HEIGHT).abs() < 0.01);
        let wrapping = TextSpec {
            wrap: true,
            ..spec("one two three four")
        };
        let (w, wrapped_h) = text.measure(&wrapping, 13.0, Some(40.0));
        assert!(w <= 40.0 + 0.5, "{w}");
        assert!(wrapped_h >= 2.0 * h);
    }

    #[test]
    fn elide_keeps_fitting_text_and_shortens_long_text() {
        let Some(mut text) = test_text() else { return };
        assert_eq!(text.elide("abc", 13.0, 400, 1000.0), "abc");
        let elided = text.elide("very long repository name", 13.0, 400, 60.0);
        assert!(elided.ends_with('…'), "{elided}");
        assert!(text.measure(&spec(&elided), 13.0, None).0 <= 60.01);
        assert_eq!(text.elide("abc", 13.0, 400, 0.0), "…");
    }

    #[test]
    fn text_in_a_rect_rounded_down_by_layout_is_not_elided() {
        let Some(mut text) = test_text() else { return };
        let elide = TextSpec {
            elide: true,
            ..spec("Low Tide · Harbour Lights")
        };
        let (w, h) = text.measure(&elide, 11.2, None);
        let mut draw = |width: f32| {
            let mut p = Pixmap::new(300, 30).unwrap();
            let rect = Rect::new(0.0, 0.0, width, h);
            text.draw(
                &mut p,
                &elide,
                11.2,
                rect,
                1.0,
                Color::WHITE,
                Rect::new(0.0, 0.0, 300.0, 30.0),
            );
            p
        };
        assert!(
            draw(w.floor()) == draw(w + 20.0),
            "measured {w}, cut at {}",
            w.floor()
        );
        assert!(draw(w - 10.0) != draw(w + 20.0), "a narrower rect still elides");
    }

    #[test]
    fn cache_evicts_layouts_unused_for_a_frame() {
        let Some(mut text) = test_text() else { return };
        text.measure(&spec("a"), 13.0, None);
        text.end_frame();
        text.measure(&spec("b"), 13.0, None);
        text.end_frame();
        assert_eq!(text.cached_layouts(), 1);
    }

    #[test]
    fn missing_preferred_family_falls_back_to_generic() {
        let Some(data) = test_text() else { return };
        drop(data);
        let text = TextSystem::from_font_data(Vec::new(), &["Nope".into()]);
        assert_eq!(text.family(), None);
    }

    #[test]
    fn blend_is_source_over_in_premultiplied_space() {
        let mut data = [0u8, 0, 0, 0];
        blend(&mut data, 0, [255, 0, 0], 0.5);
        assert_eq!(data, [128, 0, 0, 128]);
        blend(&mut data, 0, [0, 0, 255], 1.0);
        assert_eq!(data, [0, 0, 255, 255]);
        blend(&mut data, 0, [255, 255, 255], 0.0);
        assert_eq!(data, [0, 0, 255, 255]);
    }
}
