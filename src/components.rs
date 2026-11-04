//! Plasma/Kirigami look-alike building blocks used by the KDE widgets.

use std::f32::consts::{FRAC_PI_2, TAU};

use tiny_skia::PathBuilder;

use crate::element::{Element, Paint, TextAlign, column, container, row, spacer, text};
use crate::{Canvas, Color, History, Rect, Theme};

/// Circular gauge (`Ring.qml`): track, arc from 12 o'clock, bold centered label.
#[derive(Clone, Debug)]
pub struct RingPaint {
    /// 0..=1; values outside are clamped, non-finite draws only the track.
    pub fraction: f32,
    pub color: Color,
    pub track: Color,
}

impl Paint for RingPaint {
    fn paint(&self, canvas: &mut Canvas<'_>, rect: Rect) {
        let size = rect.w.min(rect.h);
        let thickness = (size * 0.13).max(2.0);
        let radius = (size - thickness) / 2.0;
        let (cx, cy) = (rect.center_x(), rect.y + rect.h / 2.0);
        canvas.stroke_arc(cx, cy, radius, 0.0, TAU, thickness, self.track);
        if self.fraction.is_finite() && self.fraction > 0.0 {
            let sweep = TAU * self.fraction.min(1.0);
            canvas.stroke_arc(cx, cy, radius, -FRAC_PI_2, sweep, thickness, self.color);
        }
    }
}

/// Ring with a centered label, `size` × `size`.
pub fn ring<M>(theme: &Theme, size: f32, fraction: f32, color: Color, label: &str) -> Element<M> {
    container()
        .size(size, size)
        .center()
        .painter(RingPaint {
            fraction,
            color,
            track: theme.text_alpha(0.15),
        })
        .child(
            text(label)
                .bold()
                .font_size((size * 0.36).max(7.0))
                .color(theme.text),
        )
}

/// Sparkline (`Sparkline.qml`): right-aligned samples, filled gradient, 1.5 px line.
#[derive(Clone, Debug)]
pub struct SparklinePaint {
    pub values: Vec<f32>,
    pub capacity: usize,
    pub max: f32,
    pub color: Color,
    pub fill: bool,
}

impl SparklinePaint {
    pub fn from_history(history: &History, max: f32, color: Color) -> Self {
        Self {
            values: history.values(),
            capacity: history.capacity(),
            max,
            color,
            fill: true,
        }
    }

    fn points(&self, rect: Rect) -> Vec<(f32, f32)> {
        let n = self.values.len();
        if n < 2 || rect.w <= 0.0 || rect.h <= 0.0 {
            return Vec::new();
        }
        let step = rect.w / (self.capacity.max(2) - 1) as f32;
        let max = if self.max > 0.0 { self.max } else { 1.0 };
        let x0 = rect.right() - step * (n - 1) as f32;
        self.values
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let t = (v / max).clamp(0.0, 1.0);
                (x0 + step * i as f32, rect.bottom() - t * (rect.h - 2.0) - 1.0)
            })
            .collect()
    }
}

impl Paint for SparklinePaint {
    fn paint(&self, canvas: &mut Canvas<'_>, rect: Rect) {
        let points = self.points(rect);
        let Some(&(first_x, first_y)) = points.first() else {
            return;
        };
        let last_x = points.last().map_or(first_x, |p| p.0);
        if self.fill {
            let mut pb = PathBuilder::new();
            pb.move_to(first_x, rect.bottom());
            for &(x, y) in &points {
                pb.line_to(x, y);
            }
            pb.line_to(last_x, rect.bottom());
            pb.close();
            if let Some(path) = pb.finish() {
                canvas.fill_path_vertical_gradient(
                    &path,
                    rect.y,
                    self.color.with_alpha(0.45),
                    rect.bottom(),
                    self.color.with_alpha(0.02),
                );
            }
        }
        let mut pb = PathBuilder::new();
        pb.move_to(first_x, first_y);
        for &(x, y) in &points[1..] {
            pb.line_to(x, y);
        }
        if let Some(path) = pb.finish() {
            canvas.stroke_path(&path, 1.5, self.color, false);
        }
    }
}

pub fn sparkline<M>(paint: SparklinePaint) -> Element<M> {
    container().fill_width().painter(paint)
}

/// `Kirigami.Heading { level }`: level 1 = 1.35×, 2 = 1.2×, 3 = 1.15×, else 1×; bold-ish.
pub fn heading<M>(theme: &Theme, label: &str, level: u8) -> Element<M> {
    let factor = match level {
        1 => 1.35,
        2 => 1.2,
        3 => 1.15,
        _ => 1.0,
    };
    text(label)
        .font_size(theme.font_size() * factor)
        .weight(600)
        .elide()
}

/// `PlasmaComponents.ToolButton` with text: flat, hover highlight.
pub fn tool_button<M: Clone>(theme: &Theme, label: &str, message: M) -> Element<M> {
    row()
        .align_center()
        .padding_xy(theme.units.small_spacing * 1.5, theme.units.small_spacing)
        .radius(theme.units.corner_radius)
        .hover_background(theme.text_alpha(0.1))
        .on_click(message)
        .child(text(label))
}

/// Horizontal rule (`Kirigami.Separator`).
pub fn separator<M>(theme: &Theme) -> Element<M> {
    container()
        .fill_width()
        .height(1.0)
        .background(theme.text_alpha(0.2))
}

/// Header row of a popup: heading, flexible space, trailing items.
pub fn plasmoid_heading<M>(theme: &Theme, title: &str, trailing: Vec<Element<M>>) -> Element<M> {
    row()
        .align_center()
        .gap(theme.units.small_spacing)
        .child(heading(theme, title, 3).grow())
        .child(spacer())
        .children(trailing)
}

/// Rounded card used for stat tiles: faint fill, accent border, `large_spacing` padding.
pub fn card<M>(theme: &Theme) -> Element<M> {
    column()
        .padding(theme.units.large_spacing)
        .gap(theme.units.small_spacing)
        .radius(theme.units.corner_radius)
        .background(theme.text_alpha(0.05))
        .border(theme.highlight.with_alpha(0.25), 1.0)
}

/// Clickable list row with hover highlight.
pub fn list_item<M: Clone>(theme: &Theme, message: Option<M>) -> Element<M> {
    let item = row()
        .align_center()
        .gap(theme.units.small_spacing)
        .padding_xy(theme.units.small_spacing * 2.0, theme.units.small_spacing)
        .radius(theme.units.corner_radius)
        .hover_background(theme.text_alpha(0.08));
    match message {
        Some(m) => item.on_click(m),
        None => item,
    }
}

/// Secondary label: small font, reduced opacity.
pub fn caption<M>(theme: &Theme, label: &str) -> Element<M> {
    text(label).font_size(theme.small_font_size()).opacity(0.6)
}

/// Centered label filling its parent.
pub fn centered_label<M>(label: &str) -> Element<M> {
    text(label).text_align(TextAlign::Center).fill_width()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Pixmap;

    #[test]
    fn sparkline_points_are_right_aligned_and_scaled() {
        let p = SparklinePaint {
            values: vec![0.0, 50.0, 100.0],
            capacity: 5,
            max: 100.0,
            color: Color::WHITE,
            fill: true,
        };
        let pts = p.points(Rect::new(0.0, 0.0, 40.0, 22.0));
        assert_eq!(pts, vec![(20.0, 21.0), (30.0, 11.0), (40.0, 1.0)]);
        let one = SparklinePaint {
            values: vec![1.0],
            ..p
        };
        assert!(one.points(Rect::new(0.0, 0.0, 40.0, 22.0)).is_empty());
    }

    #[test]
    fn ring_draws_arc_only_for_the_filled_fraction() {
        let mut px = Pixmap::new(40, 40).unwrap();
        RingPaint {
            fraction: 0.25,
            color: Color::rgb8(255, 0, 0),
            track: Color::TRANSPARENT,
        }
        .paint(&mut Canvas::new(&mut px, 1.0), Rect::new(0.0, 0.0, 40.0, 40.0));
        let red = |x: u32, y: u32| px.pixel(x, y).unwrap().red();
        // Top-right quadrant is red, bottom-left is not.
        assert!(red(32, 8) > 200);
        assert_eq!(red(6, 34), 0);
    }
}
