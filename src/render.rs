//! Software painting: a [`Canvas`] for custom painters plus the frame renderer.

use std::collections::HashMap;
use std::f32::consts::{FRAC_PI_2, TAU};

use tiny_skia::{
    FillRule, GradientStop, LineCap, LinearGradient, Mask, Paint as SkPaint, Path, PathBuilder, Pixmap,
    PixmapPaint, Point, SpreadMode, Stroke, Transform,
};

use crate::element::Content;
use crate::layout::Frame;
use crate::text::TextSystem;
use crate::{Color, Rect, Theme};

/// Drawing surface handed to [`crate::Paint`] implementations. Coordinates are logical
/// pixels; scaling to the monitor DPI and clipping are applied for you.
pub struct Canvas<'a> {
    pixmap: &'a mut Pixmap,
    scale: f32,
    opacity: f32,
    mask: Option<&'a Mask>,
}

impl<'a> Canvas<'a> {
    pub fn new(pixmap: &'a mut Pixmap, scale: f32) -> Self {
        Self {
            pixmap,
            scale,
            opacity: 1.0,
            mask: None,
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Logical → physical transform, for drawing with tiny-skia directly.
    pub fn transform(&self) -> Transform {
        Transform::from_scale(self.scale, self.scale)
    }

    /// Clip mask in physical pixels, for drawing with tiny-skia directly.
    pub fn mask(&self) -> Option<&Mask> {
        self.mask
    }

    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    pub fn pixmap(&mut self) -> &mut Pixmap {
        self.pixmap
    }

    fn paint(&self, color: Color) -> SkPaint<'static> {
        let mut paint = SkPaint::default();
        paint.set_color(color.fade(self.opacity).to_skia());
        paint.anti_alias = true;
        paint
    }

    pub fn fill_path(&mut self, path: &Path, color: Color) {
        let paint = self.paint(color);
        let transform = self.transform();
        self.pixmap
            .fill_path(path, &paint, FillRule::Winding, transform, self.mask);
    }

    /// Fill with a vertical gradient from `top` (at `y0`) to `bottom` (at `y1`), logical y.
    pub fn fill_path_vertical_gradient(&mut self, path: &Path, y0: f32, top: Color, y1: f32, bottom: Color) {
        let stops = vec![
            GradientStop::new(0.0, top.fade(self.opacity).to_skia()),
            GradientStop::new(1.0, bottom.fade(self.opacity).to_skia()),
        ];
        let Some(shader) = LinearGradient::new(
            Point::from_xy(0.0, y0),
            Point::from_xy(0.0, y1.max(y0 + 0.001)),
            stops,
            SpreadMode::Pad,
            Transform::identity(),
        ) else {
            return;
        };
        let paint = SkPaint {
            shader,
            anti_alias: true,
            ..SkPaint::default()
        };
        let transform = self.transform();
        self.pixmap
            .fill_path(path, &paint, FillRule::Winding, transform, self.mask);
    }

    pub fn stroke_path(&mut self, path: &Path, width: f32, color: Color, round_cap: bool) {
        let paint = self.paint(color);
        let stroke = Stroke {
            width,
            line_cap: if round_cap { LineCap::Round } else { LineCap::Butt },
            ..Stroke::default()
        };
        let transform = self.transform();
        self.pixmap
            .stroke_path(path, &paint, &stroke, transform, self.mask);
    }

    pub fn fill_rounded_rect(&mut self, rect: Rect, radius: f32, color: Color) {
        if let Some(path) = rounded_rect_path(rect, radius) {
            self.fill_path(&path, color);
        }
    }

    /// Stroke inside `rect`, like a QML `Rectangle.border`.
    pub fn stroke_rounded_rect(&mut self, rect: Rect, radius: f32, width: f32, color: Color) {
        let inner = rect.inset(width / 2.0);
        if let Some(path) = rounded_rect_path(inner, (radius - width / 2.0).max(0.0)) {
            self.stroke_path(&path, width, color, false);
        }
    }

    /// Arc around (`cx`, `cy`). Angles in radians, 0 = 3 o'clock, positive = clockwise.
    #[allow(clippy::too_many_arguments)]
    pub fn stroke_arc(
        &mut self,
        cx: f32,
        cy: f32,
        radius: f32,
        start: f32,
        sweep: f32,
        width: f32,
        color: Color,
    ) {
        if let Some(path) = arc_path(cx, cy, radius, start, sweep) {
            self.stroke_path(&path, width, color, true);
        }
    }
}

/// Rounded rectangle; the radius is clamped to half the shorter side.
pub fn rounded_rect_path(rect: Rect, radius: f32) -> Option<Path> {
    if rect.is_empty() {
        return None;
    }
    let r = radius.min(rect.w / 2.0).min(rect.h / 2.0).max(0.0);
    if r == 0.0 {
        return Some(PathBuilder::from_rect(tiny_skia::Rect::from_xywh(
            rect.x, rect.y, rect.w, rect.h,
        )?));
    }
    // Cubic approximation of a quarter circle.
    let k = r * 0.552_284_8;
    let (l, t, rt, b) = (rect.x, rect.y, rect.right(), rect.bottom());
    let mut pb = PathBuilder::new();
    pb.move_to(l + r, t);
    pb.line_to(rt - r, t);
    pb.cubic_to(rt - r + k, t, rt, t + r - k, rt, t + r);
    pb.line_to(rt, b - r);
    pb.cubic_to(rt, b - r + k, rt - r + k, b, rt - r, b);
    pb.line_to(l + r, b);
    pb.cubic_to(l + r - k, b, l, b - r + k, l, b - r);
    pb.line_to(l, t + r);
    pb.cubic_to(l, t + r - k, l + r - k, t, l + r, t);
    pb.close();
    pb.finish()
}

/// Circular arc as cubic segments of at most 90°. A full sweep yields a closed circle.
pub fn arc_path(cx: f32, cy: f32, radius: f32, start: f32, sweep: f32) -> Option<Path> {
    if radius <= 0.0 || sweep == 0.0 || !sweep.is_finite() {
        return None;
    }
    let sweep = sweep.clamp(-TAU, TAU);
    let segments = (sweep.abs() / FRAC_PI_2).ceil().max(1.0) as usize;
    let step = sweep / segments as f32;
    let k = 4.0 / 3.0 * (step / 4.0).tan();
    let point = |a: f32| (cx + radius * a.cos(), cy + radius * a.sin());
    let mut pb = PathBuilder::new();
    let (x0, y0) = point(start);
    pb.move_to(x0, y0);
    for i in 0..segments {
        let a0 = start + step * i as f32;
        let a1 = a0 + step;
        let (x0, y0) = point(a0);
        let (x1, y1) = point(a1);
        pb.cubic_to(
            x0 - k * radius * a0.sin(),
            y0 + k * radius * a0.cos(),
            x1 + k * radius * a1.sin(),
            y1 - k * radius * a1.cos(),
            x1,
            y1,
        );
    }
    pb.finish()
}

fn clip_mask(width: u32, height: u32, clip: Rect, scale: f32) -> Option<Mask> {
    let mut mask = Mask::new(width, height)?;
    let rect = tiny_skia::Rect::from_xywh(clip.x * scale, clip.y * scale, clip.w * scale, clip.h * scale)?;
    mask.fill_path(
        &PathBuilder::from_rect(rect),
        FillRule::Winding,
        false,
        Transform::identity(),
    );
    Some(mask)
}

/// Draws laid-out nodes. `offset` shifts the whole frame (slide animations), `pointer` is the
/// hover position in the same logical coordinates as the frame.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_frame<A>(
    frame: &Frame<A>,
    pixmap: &mut Pixmap,
    scale: f32,
    offset: (f32, f32),
    text: &mut TextSystem,
    theme: &Theme,
    pointer: Option<(f32, f32)>,
    opacity: f32,
) {
    let mut masks: HashMap<[u32; 4], Option<Mask>> = HashMap::new();
    let (pw, ph) = (pixmap.width(), pixmap.height());
    for node in &frame.nodes {
        let rect = node.rect.translate(offset.0, offset.1);
        let clip = node.clip.translate(offset.0, offset.1);
        let visible = rect.intersect(clip);
        if visible.is_empty() || node.opacity * opacity <= 0.0 {
            continue;
        }
        let hovered = pointer.is_some_and(|(x, y)| node.rect.contains(x, y) && node.clip.contains(x, y));
        let mask = if node.clipped {
            masks
                .entry([
                    clip.x.to_bits(),
                    clip.y.to_bits(),
                    clip.w.to_bits(),
                    clip.h.to_bits(),
                ])
                .or_insert_with(|| clip_mask(pw, ph, clip, scale))
                .as_ref()
        } else {
            None
        };
        let mut canvas = Canvas {
            pixmap: &mut *pixmap,
            scale,
            opacity: node.opacity * opacity,
            mask,
        };
        let background = if hovered {
            node.hover_background.or(node.background)
        } else {
            node.background
        };
        if let Some(color) = background {
            canvas.fill_rounded_rect(rect, node.radius, color);
        }
        if let Some((color, width)) = node.border {
            canvas.stroke_rounded_rect(rect, node.radius, width, color);
        }
        if let Some(painter) = &node.painter {
            painter.paint(&mut canvas, rect);
        }
        match &node.content {
            Content::None => {}
            Content::Text(spec) => {
                let color = spec.color.unwrap_or(theme.text).fade(node.opacity * opacity);
                text.draw(pixmap, spec, theme.font_size(), rect, scale, color, clip);
            }
            Content::Svg(tree, tint) => {
                draw_svg(pixmap, tree, *tint, rect, scale, node.opacity * opacity, mask);
            }
        }
    }
}

fn draw_svg(
    pixmap: &mut Pixmap,
    tree: &resvg::usvg::Tree,
    tint: Option<Color>,
    rect: Rect,
    scale: f32,
    opacity: f32,
    mask: Option<&Mask>,
) {
    let (w, h) = ((rect.w * scale).round() as u32, (rect.h * scale).round() as u32);
    let Some(mut icon) = Pixmap::new(w.max(1), h.max(1)) else {
        return;
    };
    let size = tree.size();
    let fit = (w as f32 / size.width()).min(h as f32 / size.height());
    let dx = (w as f32 - size.width() * fit) / 2.0;
    let dy = (h as f32 - size.height() * fit) / 2.0;
    resvg::render(
        tree,
        Transform::from_row(fit, 0.0, 0.0, fit, dx, dy),
        &mut icon.as_mut(),
    );
    if let Some(tint) = tint {
        let [r, g, b, a] = tint.to_rgba8();
        for px in icon.data_mut().as_chunks_mut::<4>().0 {
            let alpha = u16::from(px[3]) * u16::from(a) / 255;
            let pre = |c: u8| (u16::from(c) * alpha / 255) as u8;
            *px = [pre(r), pre(g), pre(b), alpha as u8];
        }
    }
    let paint = PixmapPaint {
        opacity,
        ..PixmapPaint::default()
    };
    pixmap.draw_pixmap(
        (rect.x * scale).round() as i32,
        (rect.y * scale).round() as i32,
        icon.as_ref(),
        &paint,
        Transform::identity(),
        mask,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pixel(p: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let c = p.pixel(x, y).unwrap();
        [c.red(), c.green(), c.blue(), c.alpha()]
    }

    #[test]
    fn rounded_rect_leaves_corners_empty_and_fills_center() {
        let mut p = Pixmap::new(20, 20).unwrap();
        Canvas::new(&mut p, 1.0).fill_rounded_rect(Rect::new(0.0, 0.0, 20.0, 20.0), 8.0, Color::WHITE);
        assert_eq!(pixel(&p, 0, 0)[3], 0);
        assert_eq!(pixel(&p, 10, 10), [255, 255, 255, 255]);
        assert_eq!(pixel(&p, 10, 0)[3], 255);
    }

    #[test]
    fn scale_maps_logical_to_physical_pixels() {
        let mut p = Pixmap::new(20, 20).unwrap();
        Canvas::new(&mut p, 2.0).fill_rounded_rect(Rect::new(0.0, 0.0, 5.0, 5.0), 0.0, Color::WHITE);
        assert_eq!(pixel(&p, 9, 9)[3], 255);
        assert_eq!(pixel(&p, 10, 10)[3], 0);
    }

    #[test]
    fn quarter_arc_from_twelve_oclock_covers_the_right_top_quadrant_only() {
        let mut p = Pixmap::new(40, 40).unwrap();
        let mut canvas = Canvas::new(&mut p, 1.0);
        canvas.stroke_arc(20.0, 20.0, 15.0, -FRAC_PI_2, FRAC_PI_2, 4.0, Color::WHITE);
        // 45° along the arc, inside the stroke.
        let d = 15.0 * std::f32::consts::FRAC_1_SQRT_2;
        assert_eq!(pixel(&p, (20.0 + d) as u32, (20.0 - d) as u32)[3], 255);
        // Mirror positions in the other quadrants stay empty.
        assert_eq!(pixel(&p, (20.0 - d) as u32, (20.0 - d) as u32)[3], 0);
        assert_eq!(pixel(&p, (20.0 + d) as u32, (20.0 + d) as u32)[3], 0);
    }

    #[test]
    fn arc_rejects_degenerate_input() {
        assert!(arc_path(0.0, 0.0, 0.0, 0.0, 1.0).is_none());
        assert!(arc_path(0.0, 0.0, 5.0, 0.0, 0.0).is_none());
        assert!(arc_path(0.0, 0.0, 5.0, 0.0, f32::NAN).is_none());
        assert!(arc_path(0.0, 0.0, 5.0, 0.0, 100.0).is_some());
    }

    #[test]
    fn opacity_scales_alpha() {
        let mut p = Pixmap::new(4, 4).unwrap();
        let mut canvas = Canvas::new(&mut p, 1.0);
        canvas.opacity = 0.5;
        canvas.fill_rounded_rect(Rect::new(0.0, 0.0, 4.0, 4.0), 0.0, Color::WHITE);
        assert_eq!(pixel(&p, 1, 1)[3], 128);
    }
}
