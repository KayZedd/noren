//! Floating, auto-hiding top-edge panel holding widgets, with one popup at a time.
//!
//! Everything here is platform independent: hosts feed it the pointer, clicks and time, and
//! hand it a pixmap covering the top strip of one monitor. Coordinates are logical pixels with
//! the origin at the monitor's top-left corner.

pub mod edge;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tiny_skia::Pixmap;

use crate::layout::{Frame, layout};
use crate::render::{Canvas, render_frame};
use crate::surface::{Layer, MonitorInfo, Placement, Response, Surface};
use crate::widget::{AnyWidget, Erased};
use crate::{Animated, Command, Context, Element, Rect, TextSystem, Theme, Widget, container, row};
use edge::{Controller, Visibility};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    Start,
    #[default]
    Center,
    End,
}

#[derive(Clone, Debug)]
pub struct PanelConfig {
    /// Panel thickness (KDE default 36).
    pub height: f32,
    /// Floating gap between screen edges and the bar.
    pub margin: f32,
    pub radius: f32,
    /// Space between the bar and an open popup.
    pub popup_gap: f32,
    /// Space between widget cells.
    pub spacing: f32,
    pub align: Align,
    pub hide_delay: Duration,
    /// Height of the reveal strip at the top screen edge, in physical pixels.
    pub hot_edge_px: f32,
    /// Disable auto-hide.
    pub always_visible: bool,
    /// Drop shadow extent around bar and popup.
    pub shadow: f32,
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            height: 36.0,
            margin: 8.0,
            radius: 8.0,
            popup_gap: 6.0,
            spacing: 4.0,
            align: Align::Center,
            hide_delay: Duration::from_millis(350),
            hot_edge_px: 2.0,
            always_visible: false,
            shadow: 10.0,
        }
    }
}

/// What a click on a laid-out node means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Toggle(usize),
    Message(usize, usize),
}

struct Built {
    frame: Frame<Target>,
    /// Final position (fully revealed / fully open).
    rect: Rect,
}

pub struct Panel {
    config: PanelConfig,
    theme: Theme,
    text: TextSystem,
    widgets: Vec<Box<dyn AnyWidget>>,
    open: Option<usize>,
    /// Widget whose popup is drawn (stays set during the close animation).
    shown: Option<usize>,
    popup_anim: Animated,
    reveal: Animated,
    controller: Controller,
    epoch: Instant,
    viewport: (f32, f32),
    scale: f32,
    pointer: Option<(f32, f32)>,
    scrolls: HashMap<String, f32>,
    bar: Option<Built>,
    cells: Vec<Rect>,
    popup: Option<Built>,
    dirty: bool,
}

impl Panel {
    pub fn new(config: PanelConfig, theme: Theme) -> Self {
        let text = TextSystem::new(&theme.font_families);
        Self::with_text(config, theme, text)
    }

    /// Use a prepared text system (bundled fonts, tests).
    pub fn with_text(config: PanelConfig, theme: Theme, text: TextSystem) -> Self {
        let duration = theme.units.long_duration;
        let mut controller = Controller::new(config.hide_delay);
        let mut reveal = Animated::new(0.0, duration);
        if config.always_visible {
            controller.set(Visibility::Revealed);
            reveal.reset(1.0);
        }
        Self {
            config,
            theme,
            text,
            widgets: Vec::new(),
            open: None,
            shown: None,
            popup_anim: Animated::new(0.0, duration),
            reveal,
            controller,
            epoch: Instant::now(),
            viewport: (1920.0, 1080.0),
            scale: 1.0,
            pointer: None,
            scrolls: HashMap::new(),
            bar: None,
            cells: Vec::new(),
            popup: None,
            dirty: true,
        }
    }

    pub fn add(&mut self, widget: impl Widget + 'static) -> &mut Self {
        self.widgets.push(Box::new(Erased::new(widget)));
        self.dirty = true;
        self
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.dirty = true;
    }

    pub fn config(&self) -> &PanelConfig {
        &self.config
    }

    pub fn text_system(&mut self) -> &mut TextSystem {
        &mut self.text
    }

    /// Monitor size in logical pixels and its DPI scale.
    pub fn set_viewport(&mut self, width: f32, height: f32, scale: f32) {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        if (width, height, scale) != (self.viewport.0, self.viewport.1, self.scale) {
            self.viewport = (width.max(1.0), height.max(1.0));
            self.scale = scale;
            self.dirty = true;
        }
    }

    pub fn popup_open(&self) -> Option<&str> {
        self.open.map(|i| self.widgets[i].id())
    }

    pub fn visibility(&self) -> Visibility {
        self.controller.visibility()
    }

    /// Nothing to draw: the host may hide its window.
    pub fn is_hidden(&self, now: Instant) -> bool {
        self.reveal.target() == 0.0 && self.reveal.value(now) <= 0.0
    }

    /// Frames are needed continuously (animations); otherwise draw only on [`Response::redraw`].
    pub fn animating(&self, now: Instant) -> bool {
        self.reveal.is_animating(now)
            || self.popup_anim.is_animating(now)
            || (!self.is_hidden(now) && self.widgets.iter().any(|w| w.animating(now)))
    }

    /// Advance time: poll widgets, run auto-hide, rebuild views. `pointer` is the cursor in
    /// panel coordinates (`None` when it is on another monitor).
    pub fn tick(&mut self, now: Instant, pointer: Option<(f32, f32)>, fullscreen: bool) -> Response {
        let mut redraw = std::mem::take(&mut self.dirty);
        for widget in &mut self.widgets {
            redraw |= widget.poll(now);
        }
        if fullscreen && self.open.is_some() {
            self.set_open(None, now);
        }
        let mut active = Vec::with_capacity(2);
        active.push(self.bar_rect());
        if let Some(popup) = &self.popup {
            active.push(popup.rect);
        }
        let hot = Rect::new(0.0, 0.0, self.viewport.0, self.config.hot_edge_px / self.scale);
        let before = self.controller.visibility();
        let visibility = if self.config.always_visible && !fullscreen {
            self.controller.set(Visibility::Revealed);
            Visibility::Revealed
        } else {
            self.controller.update(
                now.saturating_duration_since(self.epoch),
                pointer,
                hot,
                &active,
                self.open.is_some(),
                fullscreen,
            )
        };
        if visibility != before {
            redraw = true;
        }
        self.reveal.set(
            if visibility == Visibility::Revealed {
                1.0
            } else {
                0.0
            },
            now,
        );
        if pointer != self.pointer {
            self.pointer = pointer;
            redraw |= !self.is_hidden(now);
        }
        if self.shown.is_some() && self.open.is_none() && !self.popup_anim.is_animating(now) {
            self.shown = None;
            redraw = true;
        }
        let animating = self.animating(now);
        if !self.is_hidden(now) && (redraw || animating || self.bar.is_none()) {
            self.build(now);
        }
        Response {
            redraw: redraw || animating,
            quit: false,
        }
    }

    fn bar_rect(&self) -> Rect {
        let m = self.config.margin;
        Rect::new(m, m, (self.viewport.0 - 2.0 * m).max(0.0), self.config.height)
    }

    /// Logical size of the window the host must provide (full width, down to the popup).
    pub fn window_size(&self, now: Instant) -> (f32, f32) {
        let bar = self.bar_rect().bottom() + self.config.shadow;
        let popup = match (&self.popup, self.shown) {
            (Some(p), Some(_)) if !self.is_hidden(now) => p.rect.bottom() + self.config.shadow,
            _ => 0.0,
        };
        (self.viewport.0, bar.max(popup).min(self.viewport.1).ceil())
    }

    fn build(&mut self, now: Instant) {
        self.text.end_frame();
        let theme = self.theme.clone();
        let units = theme.units.clone();
        let bar_rect = self.bar_rect();
        for w in &mut self.widgets {
            w.begin_frame();
        }

        // Bar.
        let mut cells = Vec::with_capacity(self.widgets.len());
        for (i, widget) in self.widgets.iter_mut().enumerate() {
            let ctx = Context {
                theme: &theme,
                now,
                popup_open: self.open == Some(i),
                panel_height: self.config.height,
            };
            let compact = widget.compact(&ctx).map(&mut |m| Target::Message(i, m));
            let mut cell = row()
                .align_center()
                .padding_xy(units.small_spacing, 0.0)
                .radius(units.corner_radius)
                .hover_background(theme.text_alpha(0.08))
                .on_click(Target::Toggle(i))
                .child(compact);
            if self.open == Some(i) {
                cell = cell.background(theme.highlight.with_alpha(0.25));
            }
            cells.push(cell);
        }
        let justify = match self.config.align {
            Align::Start => taffy::JustifyContent::START,
            Align::Center => taffy::JustifyContent::CENTER,
            Align::End => taffy::JustifyContent::END,
        };
        let bar: Element<Target> = row()
            .padding_xy(units.small_spacing, units.small_spacing / 2.0)
            .gap(self.config.spacing)
            .style(|s| s.justify_content = justify)
            .background(theme.background.with_alpha(theme.panel_opacity))
            .border(theme.text_alpha(0.1), 1.0)
            .radius(self.config.radius)
            .children(cells);
        let frame = layout(
            bar,
            Some(bar_rect.w),
            Some(bar_rect.h),
            &mut self.text,
            theme.font_size(),
            &self.scrolls,
        );
        // Cell rectangles are the root's direct children, in order.
        self.cells = cell_rects(&frame, self.widgets.len());
        self.bar = Some(Built {
            frame,
            rect: bar_rect,
        });

        // Popup.
        self.popup = None;
        let Some(i) = self.shown else {
            return;
        };
        let ctx = Context {
            theme: &theme,
            now,
            popup_open: self.open == Some(i),
            panel_height: self.config.height,
        };
        let Some(content) = self.widgets[i].popup(&ctx) else {
            return;
        };
        let content = content.map(&mut |m| Target::Message(i, m));
        let max_w = (self.viewport.0 - 2.0 * self.config.margin).max(1.0);
        let max_h =
            (self.viewport.1 - bar_rect.bottom() - self.config.popup_gap - self.config.margin).max(1.0);
        let wrap = |content: Element<Target>| {
            container()
                .padding(units.large_spacing)
                .background(theme.background)
                .border(theme.text_alpha(0.12), 1.0)
                .radius(self.config.radius)
                .child(content)
        };
        let mut frame = layout(
            wrap(content.clone()),
            None,
            None,
            &mut self.text,
            theme.font_size(),
            &self.scrolls,
        );
        if frame.size.0 > max_w || frame.size.1 > max_h {
            frame = layout(
                wrap(content),
                Some(frame.size.0.min(max_w)),
                Some(frame.size.1.min(max_h)),
                &mut self.text,
                theme.font_size(),
                &self.scrolls,
            );
        }
        let cell = self.cells.get(i).copied().unwrap_or_default();
        let anchor = bar_rect.x + cell.center_x();
        let (w, h) = frame.size;
        let x = (anchor - w / 2.0).clamp(
            self.config.margin,
            (self.viewport.0 - self.config.margin - w).max(self.config.margin),
        );
        let y = bar_rect.bottom() + self.config.popup_gap;
        self.popup = Some(Built {
            frame,
            rect: Rect::new(x.round(), y, w, h),
        });
    }

    fn reveal_offset(&self, now: Instant) -> f32 {
        let t = self.reveal.value(now);
        -(self.bar_rect().bottom() + self.config.shadow) * (1.0 - t)
    }

    fn popup_offset(&self, now: Instant) -> (f32, f32) {
        let t = self.popup_anim.value(now);
        (-self.config.popup_gap * 2.0 * (1.0 - t), t)
    }

    /// Paints the panel into `pixmap` (cleared first). The pixmap covers [`Self::window_size`]
    /// at `self.scale`.
    pub fn render(&mut self, pixmap: &mut Pixmap, now: Instant) {
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        if self.is_hidden(now) {
            return;
        }
        let scale = self.scale;
        let dy = self.reveal_offset(now);
        let shadow = self.config.shadow;
        let radius = self.config.radius;
        if let Some(bar) = &self.bar {
            let origin = (bar.rect.x, bar.rect.y + dy);
            draw_shadow(pixmap, scale, bar.rect.translate(0.0, dy), radius, shadow, 1.0);
            let pointer = self.pointer.map(|(x, y)| (x - origin.0, y - origin.1));
            render_frame(
                &bar.frame,
                pixmap,
                scale,
                origin,
                &mut self.text,
                &self.theme,
                pointer,
                1.0,
            );
        }
        if let Some(popup) = &self.popup {
            let (pdy, opacity) = self.popup_offset(now);
            let origin = (popup.rect.x, popup.rect.y + dy + pdy);
            draw_shadow(
                pixmap,
                scale,
                Rect::new(origin.0, origin.1, popup.rect.w, popup.rect.h),
                radius,
                shadow,
                opacity,
            );
            let pointer = self.pointer.map(|(x, y)| (x - origin.0, y - origin.1));
            render_frame(
                &popup.frame,
                pixmap,
                scale,
                origin,
                &mut self.text,
                &self.theme,
                pointer,
                opacity,
            );
        }
    }

    /// Whether the point is over the bar or the open popup (as last built).
    pub fn contains(&self, x: f32, y: f32) -> bool {
        self.bar.as_ref().is_some_and(|b| b.rect.contains(x, y))
            || self
                .popup
                .as_ref()
                .is_some_and(|p| self.open.is_some() && p.rect.contains(x, y))
    }

    fn hit(&self, x: f32, y: f32) -> Option<Target> {
        if let Some(p) = self.popup.as_ref().filter(|_| self.open.is_some())
            && p.rect.contains(x, y)
        {
            return p.frame.hit(x - p.rect.x, y - p.rect.y).copied();
        }
        let b = self.bar.as_ref()?;
        b.rect
            .contains(x, y)
            .then(|| b.frame.hit(x - b.rect.x, y - b.rect.y).copied())
            .flatten()
    }

    fn set_open(&mut self, open: Option<usize>, now: Instant) {
        if open == self.open {
            return;
        }
        if open.is_some() && open != self.shown {
            self.scrolls.clear();
        }
        self.open = open;
        if open.is_some() {
            if self.shown != open {
                // Switching widgets: show the new popup without a fade from the old one.
                self.popup_anim.reset(0.0);
            }
            self.shown = open;
        }
        self.popup_anim.set(if open.is_some() { 1.0 } else { 0.0 }, now);
        self.dirty = true;
    }

    fn apply(&mut self, widget: usize, command: Command, now: Instant) -> Response {
        match command {
            Command::None => {}
            Command::OpenPopup => self.set_open(Some(widget), now),
            Command::ClosePopup => self.set_open(None, now),
            Command::TogglePopup => {
                let next = (self.open != Some(widget)).then_some(widget);
                self.set_open(next, now);
            }
            Command::Quit => {
                return Response {
                    redraw: false,
                    quit: true,
                };
            }
        }
        self.dirty = true;
        Response {
            redraw: true,
            quit: false,
        }
    }

    /// Left click at a panel coordinate. A click outside the bar and popup closes the popup.
    pub fn click(&mut self, x: f32, y: f32, now: Instant) -> Response {
        if !self.contains(x, y) {
            if self.open.is_some() {
                self.set_open(None, now);
                return Response {
                    redraw: true,
                    quit: false,
                };
            }
            return Response::default();
        }
        match self.hit(x, y) {
            Some(Target::Toggle(i)) => {
                let has_popup = {
                    let theme = self.theme.clone();
                    let ctx = Context {
                        theme: &theme,
                        now,
                        popup_open: false,
                        panel_height: self.config.height,
                    };
                    self.widgets[i].popup(&ctx).is_some()
                };
                if has_popup {
                    self.apply(i, Command::TogglePopup, now)
                } else {
                    Response::default()
                }
            }
            Some(Target::Message(i, m)) => {
                let command = self.widgets[i].update(m, now);
                self.apply(i, command, now)
            }
            None => Response::default(),
        }
    }

    /// Escape key: close the popup.
    pub fn escape(&mut self, now: Instant) -> Response {
        if self.open.is_some() {
            self.set_open(None, now);
            return Response {
                redraw: true,
                quit: false,
            };
        }
        Response::default()
    }

    /// Wheel over a scroll area of the popup; positive `dy` scrolls content up (moves down
    /// the list), in logical pixels.
    pub fn scroll(&mut self, x: f32, y: f32, dy: f32) -> Response {
        let Some(p) = self.popup.as_ref().filter(|_| self.open.is_some()) else {
            return Response::default();
        };
        if !p.rect.contains(x, y) {
            return Response::default();
        }
        let Some((key, max)) = p.frame.scroll_area(x - p.rect.x, y - p.rect.y) else {
            return Response::default();
        };
        let key = key.to_owned();
        let offset = self.scrolls.entry(key).or_insert(0.0);
        let next = (*offset + dy).clamp(0.0, max);
        if next == *offset {
            return Response::default();
        }
        *offset = next;
        self.dirty = true;
        Response {
            redraw: true,
            quit: false,
        }
    }

    pub fn close_popup(&mut self, now: Instant) {
        self.set_open(None, now);
    }

    /// Open a widget's popup by id (keyboard shortcut, tray, screenshot).
    pub fn open_popup(&mut self, id: &str, now: Instant) -> bool {
        let Some(i) = self.widgets.iter().position(|w| w.id() == id) else {
            return false;
        };
        self.set_open(Some(i), now);
        true
    }

    /// Jump to fully revealed with no animations (screenshots, tests).
    pub fn reveal_now(&mut self) {
        self.controller.set(Visibility::Revealed);
        self.reveal.reset(1.0);
        if self.open.is_some() {
            self.popup_anim.reset(1.0);
        }
        self.dirty = true;
    }

    /// Text of every node currently laid out (bar then popup), for tests.
    pub fn texts(&self) -> Vec<String> {
        let bar = self.bar.iter().flat_map(|b| b.frame.texts());
        let popup = self.popup.iter().flat_map(|p| p.frame.texts());
        bar.chain(popup).map(str::to_owned).collect()
    }

    /// Screen-space rectangle of a text node (bar or popup), fully revealed, for tests.
    pub fn find_text(&self, needle: &str) -> Option<Rect> {
        if let Some(b) = &self.bar
            && let Some(r) = b.frame.find_text(needle)
        {
            return Some(r.translate(b.rect.x, b.rect.y));
        }
        let p = self.popup.as_ref()?;
        p.frame.find_text(needle).map(|r| r.translate(p.rect.x, p.rect.y))
    }

    /// Bar cell of a widget in panel coordinates.
    pub fn cell_rect(&self, id: &str) -> Option<Rect> {
        let i = self.widgets.iter().position(|w| w.id() == id)?;
        let bar = self.bar.as_ref()?;
        self.cells.get(i).map(|r| r.translate(bar.rect.x, bar.rect.y))
    }

    pub fn popup_rect(&self) -> Option<Rect> {
        self.popup
            .as_ref()
            .filter(|_| self.open.is_some())
            .map(|p| p.rect)
    }
}

/// Cell rectangles: the nodes carrying `Toggle(i)`, in widget order. Hidden cells get an
/// empty rect.
fn cell_rects(frame: &Frame<Target>, count: usize) -> Vec<Rect> {
    let mut cells = Vec::with_capacity(count);
    for node in &frame.nodes {
        if let Some(Target::Toggle(i)) = node.action
            && i == cells.len()
        {
            cells.push(node.rect);
        }
    }
    cells.resize(count, Rect::default());
    cells
}

/// Soft shadow: stacked rounded rectangles with quadratic falloff.
fn draw_shadow(pixmap: &mut Pixmap, scale: f32, rect: Rect, radius: f32, extent: f32, opacity: f32) {
    if extent <= 0.0 || opacity <= 0.0 {
        return;
    }
    let mut canvas = Canvas::new(pixmap, scale);
    let steps = extent.ceil() as usize;
    for step in (1..=steps).rev() {
        let d = step as f32;
        let falloff = 1.0 - d / (extent + 1.0);
        let alpha = 0.035 * falloff * falloff * opacity;
        let r = Rect::new(rect.x - d, rect.y - d + 2.0, rect.w + 2.0 * d, rect.h + 2.0 * d);
        canvas.fill_rounded_rect(r, radius + d, crate::Color::BLACK.with_alpha(alpha));
    }
}

impl Surface for Panel {
    fn set_monitor(&mut self, monitor: MonitorInfo) {
        self.set_viewport(monitor.width, monitor.height, monitor.scale);
    }

    fn tick(&mut self, now: Instant, pointer: Option<(f32, f32)>, fullscreen: bool) -> Response {
        Panel::tick(self, now, pointer, fullscreen)
    }

    fn placement(&self, now: Instant) -> Placement {
        let (w, h) = self.window_size(now);
        Placement {
            rect: Rect::new(0.0, 0.0, w, h),
            layer: Layer::Top,
            visible: !self.is_hidden(now),
            click_through: false,
        }
    }

    fn render(&mut self, pixmap: &mut Pixmap, now: Instant) {
        Panel::render(self, pixmap, now);
    }

    fn animating(&self, now: Instant) -> bool {
        Panel::animating(self, now)
    }

    fn click(&mut self, x: f32, y: f32, now: Instant) -> Response {
        Panel::click(self, x, y, now)
    }

    fn click_outside(&mut self, now: Instant) -> Response {
        Panel::escape(self, now)
    }

    fn escape(&mut self, now: Instant) -> Response {
        Panel::escape(self, now)
    }

    fn scroll(&mut self, x: f32, y: f32, dy: f32) -> Response {
        Panel::scroll(self, x, y, dy)
    }
}

#[cfg(test)]
mod tests;
