//! Surfaces: anything that owns a transparent, per-pixel-alpha window on one monitor.
//!
//! The panel is one surface. [`Floating`] is another: an element tree pinned to a monitor
//! corner on the desktop layer (desktop widgets), as a topmost overlay (HUDs, OSDs), or as a
//! normal window. Hosts run any number of surfaces; each gets its own window.

use std::collections::HashMap;
use std::time::Instant;

use tiny_skia::Pixmap;

use crate::layout::{Frame, layout};
use crate::render::render_frame;
use crate::{Command, Element, Rect, TextSystem, Theme};

/// Monitor a surface lives on. Logical size = physical size / scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MonitorInfo {
    pub width: f32,
    pub height: f32,
    pub scale: f32,
}

/// Z-order band of a surface window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Layer {
    /// Below all application windows, above the wallpaper (desktop widgets).
    Desktop,
    /// Ordinary window stacking.
    Normal,
    /// Above application windows (panels, overlays).
    #[default]
    Top,
}

/// Where the window goes this frame, in logical monitor coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub rect: Rect,
    pub layer: Layer,
    /// `false` hides the window entirely.
    pub visible: bool,
    /// Ignore all mouse input (pure overlay). Otherwise transparent pixels still pass clicks
    /// through and opaque pixels receive them.
    pub click_through: bool,
}

/// Result of an event for the host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Response {
    /// Render a new frame.
    pub redraw: bool,
    /// Exit the host event loop.
    pub quit: bool,
}

impl Response {
    pub const REDRAW: Self = Self {
        redraw: true,
        quit: false,
    };
    pub const QUIT: Self = Self {
        redraw: false,
        quit: true,
    };

    pub fn merge(self, other: Self) -> Self {
        Self {
            redraw: self.redraw || other.redraw,
            quit: self.quit || other.quit,
        }
    }
}

/// Host-facing contract. All coordinates are logical pixels relative to the monitor's
/// top-left corner; the host converts to and from window/physical space.
pub trait Surface {
    /// Called whenever the monitor changes (start, DPI change, hotplug).
    fn set_monitor(&mut self, monitor: MonitorInfo);

    /// Advance time and pull data. `pointer` is `None` when the cursor is on another monitor.
    fn tick(&mut self, now: Instant, pointer: Option<(f32, f32)>, fullscreen: bool) -> Response;

    fn placement(&self, now: Instant) -> Placement;

    /// Paint into a pixmap covering `placement().rect` at the monitor scale.
    fn render(&mut self, pixmap: &mut Pixmap, now: Instant);

    /// Needs frames at animation rate.
    fn animating(&self, now: Instant) -> bool;

    /// Left click on an opaque pixel of this surface.
    fn click(&mut self, x: f32, y: f32, now: Instant) -> Response;

    /// Mouse button pressed anywhere outside this surface (closes popups).
    fn click_outside(&mut self, _now: Instant) -> Response {
        Response::default()
    }

    fn escape(&mut self, _now: Instant) -> Response {
        Response::default()
    }

    /// Wheel; positive `dy` moves content up.
    fn scroll(&mut self, _x: f32, _y: f32, _dy: f32) -> Response {
        Response::default()
    }
}

/// Context for a [`View`].
pub struct ViewContext<'a> {
    pub theme: &'a Theme,
    pub now: Instant,
    pub monitor: MonitorInfo,
}

/// A standalone element tree with state (desktop widget, overlay, dock contents).
pub trait View {
    type Message: Clone + 'static;

    fn poll(&mut self, _now: Instant) -> bool {
        false
    }

    fn animating(&self, _now: Instant) -> bool {
        false
    }

    fn view(&self, ctx: &ViewContext<'_>) -> Element<Self::Message>;

    /// Popup commands are ignored for floating surfaces; `Quit` exits the host.
    fn update(&mut self, _message: Self::Message, _now: Instant) -> Command {
        Command::None
    }
}

/// Corner or edge the surface is pinned to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Anchor {
    #[default]
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    /// Top-left position of a `size` box anchored inside `area`, `offset` pulled inward.
    pub fn place(self, area: (f32, f32), size: (f32, f32), offset: (f32, f32)) -> (f32, f32) {
        let (fx, fy) = match self {
            Self::TopLeft => (0.0, 0.0),
            Self::Top => (0.5, 0.0),
            Self::TopRight => (1.0, 0.0),
            Self::Left => (0.0, 0.5),
            Self::Center => (0.5, 0.5),
            Self::Right => (1.0, 0.5),
            Self::BottomLeft => (0.0, 1.0),
            Self::Bottom => (0.5, 1.0),
            Self::BottomRight => (1.0, 1.0),
        };
        let sign = |f: f32| {
            if f == 0.0 {
                1.0
            } else if f == 1.0 {
                -1.0
            } else {
                0.0
            }
        };
        (
            ((area.0 - size.0) * fx + offset.0 * sign(fx)).round(),
            ((area.1 - size.1) * fy + offset.1 * sign(fy)).round(),
        )
    }
}

#[derive(Clone, Debug)]
pub struct FloatingConfig {
    pub anchor: Anchor,
    /// Distance from the anchored edges.
    pub offset: (f32, f32),
    pub layer: Layer,
    pub click_through: bool,
    /// Fixed size; `None` sizes to content.
    pub size: Option<(f32, f32)>,
    /// Hide while a fullscreen app is in front.
    pub hide_on_fullscreen: bool,
}

impl Default for FloatingConfig {
    fn default() -> Self {
        Self {
            anchor: Anchor::TopRight,
            offset: (24.0, 24.0),
            layer: Layer::Desktop,
            click_through: false,
            size: None,
            hide_on_fullscreen: false,
        }
    }
}

/// A [`View`] in its own window.
pub struct Floating<V: View> {
    view: V,
    config: FloatingConfig,
    theme: Theme,
    text: TextSystem,
    monitor: MonitorInfo,
    frame: Option<Frame<V::Message>>,
    pointer: Option<(f32, f32)>,
    scrolls: HashMap<String, f32>,
    hidden: bool,
    dirty: bool,
}

impl<V: View> Floating<V> {
    pub fn new(view: V, config: FloatingConfig, theme: Theme) -> Self {
        let text = TextSystem::new(&theme.font_families);
        Self::with_text(view, config, theme, text)
    }

    pub fn with_text(view: V, config: FloatingConfig, theme: Theme, text: TextSystem) -> Self {
        Self {
            view,
            config,
            theme,
            text,
            monitor: MonitorInfo {
                width: 1920.0,
                height: 1080.0,
                scale: 1.0,
            },
            frame: None,
            pointer: None,
            scrolls: HashMap::new(),
            hidden: false,
            dirty: true,
        }
    }

    pub fn view(&self) -> &V {
        &self.view
    }

    pub fn view_mut(&mut self) -> &mut V {
        self.dirty = true;
        &mut self.view
    }

    fn build(&mut self, now: Instant) {
        self.text.end_frame();
        let ctx = ViewContext {
            theme: &self.theme,
            now,
            monitor: self.monitor,
        };
        let root = self.view.view(&ctx);
        let (w, h) = self.config.size.map_or((None, None), |(w, h)| (Some(w), Some(h)));
        self.frame = Some(layout(
            root,
            w,
            h,
            &mut self.text,
            self.theme.font_size(),
            &self.scrolls,
        ));
    }

    fn rect(&self) -> Rect {
        let size = self.frame.as_ref().map_or((0.0, 0.0), |f| f.size);
        let (x, y) = self.config.anchor.place(
            (self.monitor.width, self.monitor.height),
            size,
            self.config.offset,
        );
        Rect::new(x, y, size.0, size.1)
    }
}

impl<V: View> Surface for Floating<V> {
    fn set_monitor(&mut self, monitor: MonitorInfo) {
        self.monitor = monitor;
        self.dirty = true;
    }

    fn tick(&mut self, now: Instant, pointer: Option<(f32, f32)>, fullscreen: bool) -> Response {
        let mut redraw = std::mem::take(&mut self.dirty) | self.view.poll(now);
        let hidden = self.config.hide_on_fullscreen && fullscreen;
        redraw |= hidden != self.hidden;
        self.hidden = hidden;
        if pointer != self.pointer {
            let rect = self.rect();
            let inside = |p: Option<(f32, f32)>| p.is_some_and(|(x, y)| rect.contains(x, y));
            redraw |= inside(pointer) || inside(self.pointer);
            self.pointer = pointer;
        }
        let animating = self.view.animating(now);
        if redraw || animating || self.frame.is_none() {
            self.build(now);
        }
        Response {
            redraw: redraw || animating,
            quit: false,
        }
    }

    fn placement(&self, _now: Instant) -> Placement {
        let rect = self.rect();
        Placement {
            rect,
            layer: self.config.layer,
            visible: !self.hidden && !rect.is_empty(),
            click_through: self.config.click_through,
        }
    }

    fn render(&mut self, pixmap: &mut Pixmap, _now: Instant) {
        pixmap.fill(tiny_skia::Color::TRANSPARENT);
        let rect = self.rect();
        let Some(frame) = &self.frame else {
            return;
        };
        let pointer = self.pointer.map(|(x, y)| (x - rect.x, y - rect.y));
        render_frame(
            frame,
            pixmap,
            self.monitor.scale,
            (0.0, 0.0),
            &mut self.text,
            &self.theme,
            pointer,
            1.0,
        );
    }

    fn animating(&self, now: Instant) -> bool {
        !self.hidden && self.view.animating(now)
    }

    fn click(&mut self, x: f32, y: f32, now: Instant) -> Response {
        let rect = self.rect();
        let Some(message) = self
            .frame
            .as_ref()
            .and_then(|f| f.hit(x - rect.x, y - rect.y))
            .cloned()
        else {
            return Response::default();
        };
        self.dirty = true;
        match self.view.update(message, now) {
            Command::Quit => Response::QUIT,
            _ => Response::REDRAW,
        }
    }

    fn scroll(&mut self, x: f32, y: f32, dy: f32) -> Response {
        let rect = self.rect();
        let Some((key, max)) = self
            .frame
            .as_ref()
            .and_then(|f| f.scroll_area(x - rect.x, y - rect.y))
        else {
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
        Response::REDRAW
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{column, container};

    #[test]
    fn anchors_place_boxes_inside_the_area_with_inward_offsets() {
        let area = (1000.0, 500.0);
        let size = (100.0, 50.0);
        assert_eq!(Anchor::TopLeft.place(area, size, (10.0, 20.0)), (10.0, 20.0));
        assert_eq!(Anchor::TopRight.place(area, size, (10.0, 20.0)), (890.0, 20.0));
        assert_eq!(
            Anchor::BottomRight.place(area, size, (10.0, 20.0)),
            (890.0, 430.0)
        );
        assert_eq!(Anchor::Center.place(area, size, (10.0, 20.0)), (450.0, 225.0));
        assert_eq!(Anchor::Bottom.place(area, size, (10.0, 20.0)), (450.0, 430.0));
    }

    struct Clicker(u32);

    impl View for Clicker {
        type Message = u32;
        fn view(&self, _: &ViewContext<'_>) -> Element<u32> {
            column()
                .child(container().size(40.0, 10.0).on_click(1))
                .child(container().size(40.0, 10.0).on_click(100))
        }
        fn update(&mut self, m: u32, _: Instant) -> Command {
            self.0 += m;
            if self.0 > 150 {
                Command::Quit
            } else {
                Command::None
            }
        }
    }

    #[test]
    fn floating_surface_places_by_anchor_and_routes_clicks() {
        let now = Instant::now();
        let text = TextSystem::from_font_data(Vec::new(), &[]);
        let config = FloatingConfig {
            anchor: Anchor::BottomRight,
            offset: (5.0, 5.0),
            ..FloatingConfig::default()
        };
        let mut s = Floating::with_text(Clicker(0), config, Theme::kanagawa_wave(), text);
        s.set_monitor(MonitorInfo {
            width: 200.0,
            height: 100.0,
            scale: 1.0,
        });
        assert!(s.tick(now, None, false).redraw);
        let p = s.placement(now);
        assert_eq!(p.rect, Rect::new(155.0, 75.0, 40.0, 20.0));
        assert_eq!(p.layer, Layer::Desktop);
        assert_eq!(s.click(160.0, 80.0, now), Response::REDRAW);
        assert_eq!(s.view().0, 1);
        assert_eq!(s.click(160.0, 90.0, now), Response::REDRAW);
        assert_eq!(s.click(0.0, 0.0, now), Response::default());
        assert_eq!(s.click(160.0, 90.0, now), Response::QUIT);
    }
}
