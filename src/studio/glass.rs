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
    /// Which side of a mode switch the egui base visuals come from.
    pub dark: bool,
    /// 0 for dark, 1 for light, eased during a switch so mode-dependent strengths glide.
    pub light: f32,
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
            light: if dark { 0.0 } else { 1.0 },
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
            light: self.light + (to.light - self.light) * t,
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

    fn shadow(&self) -> Color32 {
        Color32::from_black_alpha((70.0 - 42.0 * self.light).round() as u8)
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
        v.widgets.open.bg_stroke = Stroke::new(1.0, self.accent);
        // Fields that differ between egui's dark and light presets, set from the eased theme
        // so the preset swap halfway through a mode switch does not show.
        v.code_bg_color = self.glass_hi;
        v.window_shadow.color = self.shadow();
        v.popup_shadow.color = self.shadow();
        v.text_cursor.stroke = Stroke::new(2.0, self.accent);
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
/// The drifting aurora over the base; `base_alpha` None leaves the base to the caller (a
/// reveal wave paints it).
pub fn paint_aurora(painter: &Painter, rect: Rect, theme: &Theme, t: f32, base_alpha: Option<f32>) {
    if let Some(a) = base_alpha {
        painter.rect_filled(rect, 0.0, with_alpha(theme.base, a));
    }
    let mut mesh = Mesh::default();
    let s = rect.size();
    let strength = 0.55 - 0.05 * theme.light;
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
        .corner_radius(CornerRadius::same(CARD_RADIUS as u8))
        .inner_margin(Margin::same(18))
        .shadow(Shadow {
            offset: [0, 8],
            blur: 28,
            spread: 0,
            color: theme.shadow(),
        })
}

/// The aero highlight on an accent pill: a light sheen fading down from its top.
pub fn shine(painter: &Painter, rect: Rect) {
    painter.add(sheen(rect, rect.height() / 2.0, 0.5, PILL_SHEEN));
}

/// A card with the aero sheen painted under its content.
pub fn glass<R>(ui: &mut Ui, theme: &Theme, add: impl FnOnce(&mut Ui) -> R) -> R {
    let shape = ui.painter().add(Shape::Noop);
    let r = card(theme).show(ui, add);
    let alpha = (14.0 + 41.0 * theme.light).round() as u8;
    ui.painter()
        .set(shape, sheen(r.response.rect, CARD_RADIUS, 0.45, alpha));
    r.inner
}

const CARD_RADIUS: f32 = 18.0;
const PILL_SHEEN: u8 = 55;
/// Never deeper than this, so tall cards keep the sheen at their top.
const SHEEN_MAX: f32 = 70.0;

/// A white sheen over the top `depth` (fraction of the height) of a rounded rect, fading to
/// nothing. Its outline follows the rounded corners, so the gradient fills them instead of
/// stopping at a square.
fn sheen(rect: Rect, radius: f32, depth: f32, alpha: u8) -> Shape {
    let h = (rect.height() * depth).min(SHEEN_MAX);
    let radius = radius.min(rect.height() / 2.0).min(rect.width() / 2.0);
    let mut mesh = Mesh::default();
    for (dy, inset) in sheen_rows(radius, h) {
        let a = (f32::from(alpha) * (1.0 - dy / h)).round() as u8;
        let c = Color32::from_white_alpha(a);
        mesh.colored_vertex(Pos2::new(rect.left() + inset, rect.top() + dy), c);
        mesh.colored_vertex(Pos2::new(rect.right() - inset, rect.top() + dy), c);
    }
    let rows = mesh.vertices.len() as u32 / 2;
    for i in 0..rows.saturating_sub(1) {
        let (l, r) = (2 * i, 2 * i + 1);
        mesh.add_triangle(l, r, r + 2);
        mesh.add_triangle(l, r + 2, l + 2);
    }
    Shape::mesh(mesh)
}

/// Rows of the sheen as `(distance from the top, horizontal inset)`: one pixel below the top
/// hairline down to `h`, the inset tracing a corner of `radius` and dense where it curves.
fn sheen_rows(radius: f32, h: f32) -> Vec<(f32, f32)> {
    let top = 1.0_f32.min(h);
    let mut ys: Vec<f32> = (0..=12)
        .map(|i| {
            let f = i as f32 / 12.0;
            top + (radius - top).max(0.0) * f * f
        })
        .filter(|&y| y < h)
        .collect();
    ys.extend((0..=8).map(|i| radius.max(top) + (h - radius.max(top)).max(0.0) * i as f32 / 8.0));
    ys.retain(|&y| y < h - 0.05);
    ys.push(h);
    ys.dedup_by(|a, b| (*a - *b).abs() < 0.05);
    ys.into_iter()
        .map(|y| {
            let d = (radius - y).max(0.0);
            (y, radius - (radius * radius - d * d).max(0.0).sqrt())
        })
        .collect()
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
    shine(p, rect);
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
    segmented_with(ui, motion, theme, id, options, selected, None)
}

/// Paints option `i`'s icon centered on a point, `on` easing from 0 to 1 as it gets selected.
type Icon<'a> = &'a dyn Fn(&Painter, Pos2, usize, f32, Color32);

const ICON_W: f32 = 20.0;

fn segmented_with(
    ui: &mut Ui,
    motion: &mut Motion,
    theme: &Theme,
    id: Id,
    options: &[&str],
    selected: &mut usize,
    icon: Option<Icon>,
) -> bool {
    let font = egui::FontId::proportional(13.5);
    let lead = if icon.is_some() { ICON_W } else { 0.0 };
    let widths: Vec<f32> = options
        .iter()
        .map(|o| {
            ui.painter()
                .layout_no_wrap(o.to_string(), font.clone(), theme.text)
                .size()
                .x
                + 26.0
                + lead
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
    shine(p, pill);
    for (i, (cell, hovered)) in cells.iter().enumerate() {
        let on = motion.get(id.with(("t", i)), if i == *selected { 1.0 } else { 0.0 }, 200);
        let base = if *hovered { theme.text } else { theme.dim };
        let color = lerp(base, theme.on_accent, on);
        if let Some(icon) = icon {
            icon(
                p,
                Pos2::new(cell.left() + 6.0 + ICON_W / 2.0, cell.center().y),
                i,
                on,
                color,
            );
        }
        p.text(
            cell.center() + Vec2::new(lead / 2.0, 0.0),
            egui::Align2::CENTER_CENTER,
            options[i],
            font.clone(),
            color,
        );
    }
    changed
}

/// The Dark / Light switch: a moon and a sun that come alive when picked. Returns where the
/// pointer was when the mode changed, the origin of the reveal wave.
pub fn mode_switch(
    ui: &mut Ui,
    motion: &mut Motion,
    theme: &Theme,
    id: Id,
    labels: [&str; 2],
    light: &mut bool,
) -> Option<Pos2> {
    let mut i = usize::from(*light);
    let changed = segmented_with(ui, motion, theme, id, &labels, &mut i, Some(&mode_icon));
    *light = i == 1;
    changed.then(|| {
        ui.ctx()
            .pointer_interact_pos()
            .unwrap_or_else(|| ui.min_rect().center())
    })
}

fn mode_icon(p: &Painter, c: Pos2, i: usize, on: f32, color: Color32) {
    if i == 0 {
        moon(p, c, on, color);
    } else {
        sun(p, c, on, color);
    }
}

/// Eight rays that grow and turn an eighth of a circle as the sun gets picked.
fn sun(p: &Painter, c: Pos2, on: f32, color: Color32) {
    p.circle_filled(c, 3.0 + 0.6 * on, color);
    let (inner, outer) = (5.2 + 0.4 * on, 6.6 + 1.6 * on);
    for k in 0..8 {
        let a = k as f32 * TAU / 8.0 + on * TAU / 8.0;
        let d = Vec2::angled(a);
        p.line_segment([c + d * inner, c + d * outer], Stroke::new(1.5, color));
    }
}

/// A crescent that tilts in as the moon gets picked, with two stars popping in beside it.
fn moon(p: &Painter, c: Pos2, on: f32, color: Color32) {
    let tilt = -0.5 - 0.5 * on;
    let outline = crescent(c, 6.0, 4.6, 3.6, tilt);
    let mut mesh = Mesh::default();
    let n = outline.len() / 2;
    for &q in &outline {
        mesh.colored_vertex(q, color);
    }
    // Outer arc runs 0..n, inner arc n..2n backwards: stitch them into a strip.
    for k in 0..n as u32 - 1 {
        let (o, o2) = (k, k + 1);
        let (i, i2) = (2 * n as u32 - 1 - k, 2 * n as u32 - 2 - k);
        mesh.add_triangle(o, o2, i);
        mesh.add_triangle(o2, i2, i);
    }
    p.add(Shape::mesh(mesh));
    // A hairline of the same color along the outline smooths the mesh's hard edge.
    p.add(Shape::closed_line(outline, Stroke::new(0.8, color)));
    for (offset, size, delay) in [(Vec2::new(6.5, -5.0), 1.3, 0.0), (Vec2::new(8.5, 1.5), 0.9, 0.3)] {
        let s = ((on - delay) / (1.0 - delay)).clamp(0.0, 1.0);
        if s > 0.0 {
            star(p, c + offset, size * s * 1.6, color);
        }
    }
}

fn star(p: &Painter, c: Pos2, r: f32, color: Color32) {
    let st = Stroke::new(1.0, color);
    p.line_segment([c - Vec2::X * r, c + Vec2::X * r], st);
    p.line_segment([c - Vec2::Y * r, c + Vec2::Y * r], st);
}

/// A crescent's outline: the arc of the circle of radius `big` at `c` left visible by a circle
/// of radius `small` whose center sits `shift` away in direction `angle`, then the small
/// circle's arc back. Both arcs have the same number of points and share their ends.
fn crescent(c: Pos2, big: f32, small: f32, shift: f32, angle: f32) -> Vec<Pos2> {
    const N: usize = 18;
    // Where the circles cross, as angles from each center (measured from the shift direction).
    let x = (shift * shift + big * big - small * small) / (2.0 * shift);
    let a = (x / big).clamp(-1.0, 1.0).acos();
    let b = ((x - shift) / small).clamp(-1.0, 1.0).acos();
    let dir = Vec2::angled(angle);
    let rot = |v: Vec2| Vec2::new(v.x * dir.x - v.y * dir.y, v.x * dir.y + v.y * dir.x);
    let small_c = c + rot(Vec2::new(shift, 0.0));
    let mut pts = Vec::with_capacity(2 * N);
    for k in 0..N {
        let t = k as f32 / (N - 1) as f32;
        let ang = a + t * (TAU - 2.0 * a);
        pts.push(c + rot(Vec2::angled(ang) * big));
    }
    for k in 0..N {
        let t = k as f32 / (N - 1) as f32;
        let ang = TAU - b - t * (TAU - 2.0 * b);
        pts.push(small_c + rot(Vec2::angled(ang) * small));
    }
    pts
}

/// The background of a mode switch: the old base, and the new one spreading as a soft-edged
/// disc from `origin` with a glowing wavefront. `p` is linear progress, `0..=1`.
pub fn paint_reveal(
    painter: &Painter,
    rect: Rect,
    origin: Pos2,
    p: f32,
    from: &Theme,
    to: &Theme,
    base_alpha: f32,
) {
    const FEATHER: f32 = 90.0;
    painter.rect_filled(rect, 0.0, with_alpha(from.base, base_alpha));
    let reach = reveal_reach(rect, origin) + FEATHER;
    let radius = crate::anim::ease_in_out(p) * reach;
    let solid = with_alpha(to.base, base_alpha);
    let clear = with_alpha(to.base, 0.0);
    let glow = with_alpha(to.accent, 0.35 * (1.0 - p));
    let mut mesh = Mesh::default();
    mesh.colored_vertex(origin, solid);
    let rings = [
        ((radius - FEATHER).max(0.0), solid),
        (radius, clear),
        ((radius - 14.0).max(0.0), Color32::TRANSPARENT),
        (radius, glow),
        (radius + 22.0, Color32::TRANSPARENT),
    ];
    const N: u32 = 120;
    for (r, color) in rings {
        for k in 0..N {
            mesh.colored_vertex(origin + Vec2::angled(k as f32 * TAU / N as f32) * r, color);
        }
    }
    let ring = |i: u32| 1 + i * N;
    for k in 0..N {
        let k2 = (k + 1) % N;
        mesh.add_triangle(0, ring(0) + k, ring(0) + k2);
        // Feathered edge of the disc, then the glow band in two halves.
        for (a, b) in [(0, 1), (2, 3), (3, 4)] {
            mesh.add_triangle(ring(a) + k, ring(b) + k, ring(b) + k2);
            mesh.add_triangle(ring(a) + k, ring(b) + k2, ring(a) + k2);
        }
    }
    painter.add(Shape::mesh(mesh));
}

/// Distance from `origin` to the farthest corner of `rect`.
fn reveal_reach(rect: Rect, origin: Pos2) -> f32 {
    [
        rect.left_top(),
        rect.right_top(),
        rect.left_bottom(),
        rect.right_bottom(),
    ]
    .into_iter()
    .map(|q| q.distance(origin))
    .fold(0.0, f32::max)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheen_rows_stay_inside_the_rounded_corner() {
        for (radius, h) in [(18.0, 70.0), (13.0, 13.0), (18.0, 6.0), (0.0, 20.0)] {
            let rows = sheen_rows(radius, h);
            assert!(rows.len() >= 2, "{radius} {h}");
            assert!(rows.windows(2).all(|w| w[0].0 < w[1].0), "rows go down");
            assert!((rows.last().unwrap().0 - h).abs() < 1e-3, "reaches the bottom");
            for (y, inset) in rows {
                // A point on the row's left end lies on or inside the corner circle.
                let (cx, cy) = (radius - inset, radius - y);
                if y < radius {
                    assert!(
                        cx * cx + cy * cy <= radius * radius + 1e-2,
                        "{radius} {y} {inset}"
                    );
                } else {
                    assert_eq!(inset, 0.0);
                }
            }
        }
    }

    #[test]
    fn crescent_arcs_meet_and_the_inner_one_stays_inside() {
        let c = Pos2::new(10.0, 10.0);
        for angle in [-1.0, -0.5, 0.0, 2.0] {
            let pts = crescent(c, 6.0, 4.6, 3.6, angle);
            assert_eq!(pts.len(), 36);
            let (outer, inner) = pts.split_at(18);
            assert!(outer[0].distance(inner[17]) < 1e-3, "start meets");
            assert!(outer[17].distance(inner[0]) < 1e-3, "end meets");
            assert!(outer.iter().all(|q| (q.distance(c) - 6.0).abs() < 1e-3));
            assert!(inner.iter().all(|q| q.distance(c) <= 6.0 + 1e-3));
        }
    }

    #[test]
    fn the_reveal_reaches_the_farthest_corner() {
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(300.0, 400.0));
        assert_eq!(reveal_reach(rect, Pos2::ZERO), 500.0);
        assert_eq!(reveal_reach(rect, Pos2::new(150.0, 200.0)), 250.0);
    }

    #[test]
    fn pill_sheen_reaches_into_the_round_ends() {
        // The old sheen started a full radius in; the new one starts near the edge midway down.
        let rows = sheen_rows(13.0, 13.0);
        let (_, inset) = rows.iter().find(|(y, _)| *y >= 6.0).unwrap();
        assert!(*inset < 3.0, "{inset}");
    }

    #[test]
    fn blending_dark_to_light_eases_the_strengths() {
        let t = |light: f32| Theme {
            dark: light < 0.5,
            light,
            base: Color32::BLACK,
            glass: Color32::BLACK,
            glass_hi: Color32::BLACK,
            edge: Color32::BLACK,
            text: Color32::BLACK,
            dim: Color32::BLACK,
            accent: Color32::BLACK,
            accent_soft: Color32::BLACK,
            on_accent: Color32::BLACK,
            positive: Color32::BLACK,
            warning: Color32::BLACK,
            negative: Color32::BLACK,
            aurora: [Color32::BLACK; 4],
        };
        let mid = t(0.0).blend(&t(1.0), 0.25);
        assert!((mid.light - 0.25).abs() < 1e-6);
        assert!(mid.dark);
        assert_eq!(mid.shadow(), Color32::from_black_alpha(60));
    }
}
