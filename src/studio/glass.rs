//! The studio's look: a slowly drifting aurora behind translucent glass cards, every color
//! taken from the noren palette being edited, and eased motion on everything that changes.

use std::collections::HashMap;
use std::f32::consts::TAU;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Color32, CornerRadius, Frame, Id, Margin, Mesh, Painter, Pos2, Rect, Response, Sense, Shadow,
    Stroke, StrokeKind, Ui, Vec2, epaint::Shape,
};

use super::model::aurora;
use crate::Color;
use crate::anim::Animated;
use crate::palette::{Mode, Palette};

pub fn c32(c: Color) -> Color32 {
    let [r, g, b, a] = c.to_rgba8();
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

pub fn with_alpha(c: Color32, a: f32) -> Color32 {
    let [r, g, b, _] = c.to_srgba_unmultiplied();
    Color32::from_rgba_unmultiplied(r, g, b, (a.clamp(0.0, 1.0) * 255.0) as u8)
}

pub fn lerp(a: Color32, b: Color32, t: f32) -> Color32 {
    let (a, b) = (a.to_srgba_unmultiplied(), b.to_srgba_unmultiplied());
    let m = |i: usize| (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t).round() as u8;
    Color32::from_rgba_unmultiplied(m(0), m(1), m(2), m(3))
}

/// Palette roles as egui colors.
#[derive(Clone, Debug)]
pub struct Theme {
    pub dark: bool,
    pub base: Color32,
    pub glass: Color32,
    pub glass_hi: Color32,
    pub edge: Color32,
    pub text: Color32,
    pub dim: Color32,
    pub accent: Color32,
    pub accent_soft: Color32,
    pub on_accent: Color32,
    pub positive: Color32,
    pub warning: Color32,
    pub negative: Color32,
    pub aurora: [Color32; 4],
}

impl Theme {
    pub fn new(p: &Palette) -> Self {
        let dark = p.spec.mode == Mode::Dark;
        Self {
            dark,
            base: c32(p.neutral.step(1)),
            glass: with_alpha(c32(p.neutral.step(2)), if dark { 0.58 } else { 0.62 }),
            glass_hi: with_alpha(c32(p.neutral.step(4)), if dark { 0.55 } else { 0.7 }),
            edge: if dark {
                Color32::from_white_alpha(28)
            } else {
                Color32::from_white_alpha(150)
            },
            text: c32(p.neutral.step(12)),
            dim: c32(p.neutral.step(11)),
            accent: c32(p.accent.step(9)),
            accent_soft: with_alpha(c32(p.accent.step(9)), 0.18),
            on_accent: c32(p.on_accent),
            positive: c32(p.positive.step(9)),
            warning: c32(p.warning.step(9)),
            negative: c32(p.negative.step(9)),
            aurora: aurora(p).map(c32),
        }
    }

    /// Eases every color from `self` toward `to`, so a palette change glides instead of
    /// snapping.
    pub fn blend(&self, to: &Self, t: f32) -> Self {
        let l = |a: Color32, b: Color32| lerp(a, b, t);
        Self {
            dark: if t < 0.5 { self.dark } else { to.dark },
            base: l(self.base, to.base),
            glass: l(self.glass, to.glass),
            glass_hi: l(self.glass_hi, to.glass_hi),
            edge: l(self.edge, to.edge),
            text: l(self.text, to.text),
            dim: l(self.dim, to.dim),
            accent: l(self.accent, to.accent),
            accent_soft: l(self.accent_soft, to.accent_soft),
            on_accent: l(self.on_accent, to.on_accent),
            positive: l(self.positive, to.positive),
            warning: l(self.warning, to.warning),
            negative: l(self.negative, to.negative),
            aurora: [0, 1, 2, 3].map(|i| l(self.aurora[i], to.aurora[i])),
        }
    }

    pub fn apply(&self, ctx: &egui::Context) {
        let mut v = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        v.override_text_color = Some(self.text);
        v.panel_fill = Color32::TRANSPARENT;
        v.window_fill = with_alpha(self.base, 0.94);
        v.window_stroke = Stroke::new(1.0, self.edge);
        v.window_corner_radius = CornerRadius::same(16);
        v.menu_corner_radius = CornerRadius::same(12);
        v.extreme_bg_color = with_alpha(self.base, 0.55);
        v.faint_bg_color = self.glass;
        v.hyperlink_color = self.accent;
        v.selection.bg_fill = with_alpha(self.accent, 0.35);
        v.selection.stroke = Stroke::new(1.0, self.accent);
        for (w, fill) in [
            (&mut v.widgets.noninteractive, Color32::TRANSPARENT),
            (&mut v.widgets.inactive, self.glass_hi),
            (&mut v.widgets.hovered, with_alpha(self.accent, 0.28)),
            (&mut v.widgets.active, with_alpha(self.accent, 0.45)),
            (&mut v.widgets.open, with_alpha(self.accent, 0.3)),
        ] {
            w.bg_fill = fill;
            w.weak_bg_fill = fill;
            w.corner_radius = CornerRadius::same(10);
            w.fg_stroke = Stroke::new(1.0, self.text);
        }
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, self.edge);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, self.edge);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, with_alpha(self.accent, 0.8));
        v.widgets.active.bg_stroke = Stroke::new(1.0, self.accent);
        ctx.all_styles_mut(|s| {
            s.visuals = v.clone();
            s.spacing.item_spacing = Vec2::new(10.0, 10.0);
            s.spacing.button_padding = Vec2::new(14.0, 7.0);
            s.spacing.interact_size.y = 30.0;
            s.animation_time = 0.18;
        });
    }
}

/// Eased values keyed by id. Ask for a target every frame; [`Motion::busy`] says whether to
/// keep repainting.
#[derive(Default)]
pub struct Motion {
    values: HashMap<Id, Animated>,
    busy: bool,
    pub fast: bool,
}

impl Motion {
    pub fn begin_frame(&mut self) {
        self.busy = false;
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn get(&mut self, id: Id, target: f32, ms: u64) -> f32 {
        let now = Instant::now();
        let ms = if self.fast { ms / 3 } else { ms };
        let a = self
            .values
            .entry(id)
            .or_insert_with(|| Animated::new(target, Duration::from_millis(ms)));
        a.set(target, now);
        self.busy |= a.is_animating(now);
        a.value(now)
    }
}

/// Soft radial blob: a fan from a colored center to a transparent rim.
fn blob(mesh: &mut Mesh, center: Pos2, radius: Vec2, color: Color32) {
    const N: u32 = 48;
    let base = mesh.vertices.len() as u32;
    mesh.colored_vertex(center, color);
    for i in 0..N {
        let a = i as f32 / N as f32 * TAU;
        mesh.colored_vertex(
            center + Vec2::new(a.cos() * radius.x, a.sin() * radius.y),
            Color32::TRANSPARENT,
        );
    }
    for i in 0..N {
        mesh.add_triangle(base, base + 1 + i, base + 1 + (i + 1) % N);
    }
}

/// Fills `rect` with the base color and four drifting aurora blobs. `t` is seconds; a still
/// frame when motion is reduced.
/// `base_alpha` below 1 lets a system backdrop (Windows acrylic) show through.
pub fn paint_aurora(painter: &Painter, rect: Rect, theme: &Theme, t: f32, base_alpha: f32) {
    painter.rect_filled(rect, 0.0, with_alpha(theme.base, base_alpha));
    let mut mesh = Mesh::default();
    let s = rect.size();
    let strength = if theme.dark { 0.55 } else { 0.5 };
    let orbits = [
        (0.18, 0.22, 0.55, 0.050, 0.0),
        (0.82, 0.18, 0.50, 0.037, 1.7),
        (0.70, 0.85, 0.60, 0.043, 3.1),
        (0.20, 0.82, 0.45, 0.031, 4.4),
    ];
    for (i, (x, y, r, speed, phase)) in orbits.into_iter().enumerate() {
        let a = t * speed * TAU + phase;
        let c = rect.min + Vec2::new((x + 0.08 * a.cos()) * s.x, (y + 0.07 * (a * 1.3).sin()) * s.y);
        let breathe = 1.0 + 0.08 * (a * 0.7).sin();
        let radius = Vec2::new(r * s.x.max(s.y) * breathe, r * s.x.max(s.y) * 0.8 * breathe);
        blob(&mut mesh, c, radius, with_alpha(theme.aurora[i], strength));
    }
    painter.add(Shape::mesh(mesh));
}

/// A glass card: translucent fill, hairline edge, soft shadow.
pub fn card(theme: &Theme) -> Frame {
    Frame::new()
        .fill(theme.glass)
        .stroke(Stroke::new(1.0, theme.edge))
        .corner_radius(CornerRadius::same(18))
        .inner_margin(Margin::same(18))
        .shadow(Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: Color32::from_black_alpha(if theme.dark { 70 } else { 28 }),
        })
}

/// The aero highlight: a light sheen fading down from the top of `rect`.
pub fn shine(painter: &Painter, rect: Rect, theme: &Theme) {
    // Pills: inset by the radius so the corners stay round.
    let inset = (rect.height() / 2.0).min(18.0);
    let h = (rect.height() * 0.5).min(70.0);
    let top = Rect::from_min_size(
        rect.min + Vec2::new(inset, 1.0),
        Vec2::new(rect.width() - 2.0 * inset, h),
    );
    painter.add(sheen_mesh(top, theme));
}

/// A card with the aero sheen painted under its content.
pub fn glass<R>(ui: &mut Ui, theme: &Theme, add: impl FnOnce(&mut Ui) -> R) -> R {
    let sheen = ui.painter().add(Shape::Noop);
    let r = card(theme).show(ui, add);
    let rect = r.response.rect;
    // Inset by the corner radius so the sheen never pokes past the rounded corners.
    let h = (rect.height() * 0.45).min(70.0);
    let top = Rect::from_min_size(rect.min + Vec2::new(18.0, 1.0), Vec2::new(rect.width() - 36.0, h));
    ui.painter().set(sheen, sheen_mesh(top, theme));
    r.inner
}

fn sheen_mesh(top: Rect, theme: &Theme) -> Shape {
    let mut mesh = Mesh::default();
    let hi = Color32::from_white_alpha(if theme.dark { 14 } else { 55 });
    mesh.colored_vertex(top.left_top(), hi);
    mesh.colored_vertex(top.right_top(), hi);
    mesh.colored_vertex(top.right_bottom(), Color32::TRANSPARENT);
    mesh.colored_vertex(top.left_bottom(), Color32::TRANSPARENT);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    Shape::mesh(mesh)
}

/// An iOS-style switch whose knob glides and whose track fills with the accent.
pub fn toggle(ui: &mut Ui, motion: &mut Motion, theme: &Theme, on: &mut bool) -> Response {
    let size = Vec2::new(44.0, 24.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = motion.get(resp.id, if *on { 1.0 } else { 0.0 }, 220);
    let hover = motion.get(resp.id.with("h"), if resp.hovered() { 1.0 } else { 0.0 }, 150);
    let p = ui.painter();
    let track = lerp(theme.glass_hi, theme.accent, t);
    p.rect_filled(rect, CornerRadius::same(12), track);
    p.rect_stroke(
        rect,
        CornerRadius::same(12),
        Stroke::new(1.0, theme.edge),
        StrokeKind::Inside,
    );
    let x = rect.left() + 12.0 + t * (rect.width() - 24.0);
    let knob = Pos2::new(x, rect.center().y);
    p.circle_filled(knob + Vec2::new(0.0, 1.5), 9.5, Color32::from_black_alpha(50));
    p.circle_filled(knob, 9.0 + hover, Color32::WHITE);
    resp
}

/// The primary action: accent pill with a glow that swells on hover and a press dip.
pub fn accent_button(
    ui: &mut Ui,
    motion: &mut Motion,
    theme: &Theme,
    label: &str,
    enabled: bool,
) -> Response {
    let galley = ui.painter().layout_no_wrap(
        label.to_string(),
        egui::FontId::proportional(14.5),
        theme.on_accent,
    );
    let size = galley.size() + Vec2::new(36.0, 16.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let hover = motion.get(resp.id, if enabled && resp.hovered() { 1.0 } else { 0.0 }, 180);
    let press = motion.get(
        resp.id.with("p"),
        if resp.is_pointer_button_down_on() {
            1.0
        } else {
            0.0
        },
        90,
    );
    let rect = rect.shrink(press * 1.5);
    let p = ui.painter();
    let alpha = if enabled { 1.0 } else { 0.4 };
    for i in 1..=4 {
        let grow = i as f32 * (2.0 + 3.0 * hover);
        p.rect_filled(
            rect.expand(grow),
            CornerRadius::same(255),
            with_alpha(theme.accent, 0.06 * hover * alpha),
        );
    }
    p.rect_filled(rect, CornerRadius::same(255), with_alpha(theme.accent, alpha));
    shine(
        p,
        rect,
        &Theme {
            dark: false,
            ..theme.clone()
        },
    );
    p.galley(
        rect.center() - galley.size() / 2.0,
        galley,
        with_alpha(theme.on_accent, alpha),
    );
    if enabled {
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
    } else {
        resp
    }
}

/// A row of options with a highlight that slides to the selected one. Returns true when the
/// selection changed.
pub fn segmented(
    ui: &mut Ui,
    motion: &mut Motion,
    theme: &Theme,
    id: Id,
    options: &[&str],
    selected: &mut usize,
) -> bool {
    let font = egui::FontId::proportional(13.5);
    let widths: Vec<f32> = options
        .iter()
        .map(|o| {
            ui.painter()
                .layout_no_wrap(o.to_string(), font.clone(), theme.text)
                .size()
                .x
                + 26.0
        })
        .collect();
    let size = Vec2::new(widths.iter().sum::<f32>() + 6.0, 32.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let mut changed = false;
    let mut x = rect.left() + 3.0;
    let mut cells = Vec::new();
    for (i, w) in widths.iter().enumerate() {
        let cell = Rect::from_min_size(Pos2::new(x, rect.top() + 3.0), Vec2::new(*w, size.y - 6.0));
        let resp = ui.interact(cell, id.with(i), Sense::click());
        if resp.clicked() && *selected != i {
            *selected = i;
            changed = true;
        }
        cells.push((cell, resp.hovered()));
        x += w;
    }
    let sel = cells[*selected].0;
    let left = motion.get(id.with("l"), sel.left(), 240);
    let right = motion.get(id.with("r"), sel.right(), 240);
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(255), with_alpha(theme.base, 0.45));
    p.rect_stroke(
        rect,
        CornerRadius::same(255),
        Stroke::new(1.0, theme.edge),
        StrokeKind::Inside,
    );
    let pill = Rect::from_x_y_ranges(left..=right, sel.y_range());
    p.rect_filled(pill, CornerRadius::same(255), theme.accent);
    shine(
        p,
        pill,
        &Theme {
            dark: false,
            ..theme.clone()
        },
    );
    for (i, (cell, hovered)) in cells.iter().enumerate() {
        let on = motion.get(id.with(("t", i)), if i == *selected { 1.0 } else { 0.0 }, 200);
        let base = if *hovered { theme.text } else { theme.dim };
        p.text(
            cell.center(),
            egui::Align2::CENTER_CENTER,
            options[i],
            font.clone(),
            lerp(base, theme.on_accent, on),
        );
    }
    changed
}

/// A status dot that breathes while `alive`.
pub fn pulse_dot(ui: &mut Ui, color: Color32, alive: bool, t: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
    let c = rect.center();
    if alive {
        let phase = (t * 1.6).fract();
        ui.painter()
            .circle_filled(c, 4.0 + 6.0 * phase, with_alpha(color, 0.45 * (1.0 - phase)));
    }
    ui.painter().circle_filled(c, 4.5, color);
}

/// A small rounded label in `color`.
pub fn chip(ui: &mut Ui, text: &str, color: Color32) -> Response {
    Frame::new()
        .fill(with_alpha(color, 0.18))
        .stroke(Stroke::new(1.0, with_alpha(color, 0.5)))
        .corner_radius(CornerRadius::same(255))
        .inner_margin(Margin::symmetric(9, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(12.0).color(color))
        })
        .response
}
