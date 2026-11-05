//! Headless desktop: runs surfaces on a virtual monitor and composites them into one pixmap.
//! Used by the preview window, for screenshots, and in tests. Input routing mirrors the native
//! host: clicks go to the topmost surface with an opaque pixel under the cursor, every other
//! surface gets `click_outside`.

use std::time::Instant;

use tiny_skia::{Pixmap, PixmapPaint, Transform};

use crate::surface::{Layer, MonitorInfo, Placement, Response, Surface};
use crate::{Color, Rect};

pub struct Simulator {
    surfaces: Vec<Box<dyn Surface>>,
    frames: Vec<Option<(Placement, Pixmap)>>,
    monitor: MonitorInfo,
    background: Color,
    pointer: Option<(f32, f32)>,
    fullscreen: bool,
}

impl Simulator {
    pub fn new(monitor: MonitorInfo, background: Color) -> Self {
        Self {
            surfaces: Vec::new(),
            frames: Vec::new(),
            monitor,
            background,
            pointer: None,
            fullscreen: false,
        }
    }

    pub fn add(&mut self, mut surface: Box<dyn Surface>) {
        surface.set_monitor(self.monitor);
        self.surfaces.push(surface);
        self.frames.push(None);
    }

    pub fn monitor(&self) -> MonitorInfo {
        self.monitor
    }

    pub fn set_monitor(&mut self, monitor: MonitorInfo) {
        if monitor != self.monitor {
            self.monitor = monitor;
            for s in &mut self.surfaces {
                s.set_monitor(monitor);
            }
        }
    }

    /// Pretend a fullscreen application covers the monitor.
    pub fn set_fullscreen(&mut self, fullscreen: bool) {
        self.fullscreen = fullscreen;
    }

    /// Logical cursor position, `None` when outside the monitor.
    pub fn set_pointer(&mut self, pointer: Option<(f32, f32)>) {
        self.pointer = pointer;
    }

    pub fn pointer(&self) -> Option<(f32, f32)> {
        self.pointer
    }

    pub fn tick(&mut self, now: Instant) -> Response {
        let (pointer, fullscreen) = (self.pointer, self.fullscreen);
        self.surfaces.iter_mut().fold(Response::default(), |r, s| {
            r.merge(s.tick(now, pointer, fullscreen))
        })
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.surfaces.iter().any(|s| s.animating(now))
    }

    /// Topmost surface with an opaque pixel at the logical point.
    fn hit(&self, x: f32, y: f32) -> Option<usize> {
        let scale = self.monitor.scale;
        self.z_order().into_iter().rev().find(|&i| {
            let Some((p, pixmap)) = &self.frames[i] else {
                return false;
            };
            if p.click_through || !p.rect.contains(x, y) {
                return false;
            }
            let px = ((x - p.rect.x) * scale) as u32;
            let py = ((y - p.rect.y) * scale) as u32;
            pixmap.pixel(px, py).is_some_and(|c| c.alpha() > 0)
        })
    }

    /// Indices from bottom to top.
    fn z_order(&self) -> Vec<usize> {
        let rank = |l: Layer| match l {
            Layer::Desktop => 0,
            Layer::Normal => 1,
            Layer::Top => 2,
        };
        let mut order: Vec<usize> = (0..self.surfaces.len()).collect();
        order.sort_by_key(|&i| self.frames[i].as_ref().map_or(0, |(p, _)| rank(p.layer)));
        order
    }

    pub fn click(&mut self, x: f32, y: f32, now: Instant) -> Response {
        let target = self.hit(x, y);
        let mut response = Response::default();
        for (i, s) in self.surfaces.iter_mut().enumerate() {
            response = response.merge(if Some(i) == target {
                s.click(x, y, now)
            } else {
                s.click_outside(now)
            });
        }
        response
    }

    pub fn scroll(&mut self, x: f32, y: f32, dy: f32) -> Response {
        match self.hit(x, y) {
            Some(i) => self.surfaces[i].scroll(x, y, dy),
            None => Response::default(),
        }
    }

    pub fn escape(&mut self, now: Instant) -> Response {
        self.surfaces
            .iter_mut()
            .fold(Response::default(), |r, s| r.merge(s.escape(now)))
    }

    /// Renders every visible surface and composites them over the background, at physical size.
    pub fn render(&mut self, now: Instant) -> Pixmap {
        let scale = self.monitor.scale;
        let w = (self.monitor.width * scale).round().max(1.0) as u32;
        let h = (self.monitor.height * scale).round().max(1.0) as u32;
        for (surface, frame) in self.surfaces.iter_mut().zip(&mut self.frames) {
            let placement = surface.placement(now);
            *frame = None;
            if !placement.visible || placement.rect.is_empty() {
                continue;
            }
            let pw = (placement.rect.w * scale).ceil().max(1.0) as u32;
            let ph = (placement.rect.h * scale).ceil().max(1.0) as u32;
            if let Some(mut pixmap) = Pixmap::new(pw, ph) {
                surface.render(&mut pixmap, now);
                *frame = Some((placement, pixmap));
            }
        }
        let mut out = Pixmap::new(w, h).expect("monitor size is non-zero");
        let [r, g, b, a] = self.background.to_rgba8();
        out.fill(tiny_skia::Color::from_rgba8(r, g, b, a));
        for i in self.z_order() {
            if let Some((p, pixmap)) = &self.frames[i] {
                let (x, y) = (
                    (p.rect.x * scale).round() as i32,
                    (p.rect.y * scale).round() as i32,
                );
                out.draw_pixmap(
                    x,
                    y,
                    pixmap.as_ref(),
                    &PixmapPaint::default(),
                    Transform::identity(),
                    None,
                );
            }
        }
        out
    }

    /// Logical rect of a surface's last rendered frame.
    pub fn rect(&self, index: usize) -> Option<Rect> {
        self.frames.get(index)?.as_ref().map(|(p, _)| p.rect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::{Anchor, FloatingConfig, View, ViewContext};
    use crate::{Command, Element, Floating, TextSystem, Theme, container};

    struct Square(u32, Color);

    impl View for Square {
        type Message = ();
        fn view(&self, _: &ViewContext<'_>) -> Element<()> {
            container().size(40.0, 40.0).background(self.1).on_click(())
        }
        fn update(&mut self, _: (), _: Instant) -> Command {
            self.0 += 1;
            Command::None
        }
    }

    fn floating(anchor: Anchor, layer: Layer, color: Color) -> Box<dyn Surface> {
        let config = FloatingConfig {
            anchor,
            offset: (10.0, 10.0),
            layer,
            ..FloatingConfig::default()
        };
        Box::new(Floating::with_text(
            Square(0, color),
            config,
            Theme::aizome(),
            TextSystem::from_font_data(Vec::new(), &[]),
        ))
    }

    #[test]
    fn composites_by_layer_at_scale_and_routes_clicks_to_the_top_surface() {
        let monitor = MonitorInfo {
            width: 200.0,
            height: 100.0,
            scale: 2.0,
            origin: (0, 0),
        };
        let mut sim = Simulator::new(monitor, Color::rgb8(0, 0, 255));
        // Added top first: z-order must still put the desktop surface below.
        sim.add(floating(Anchor::TopLeft, Layer::Top, Color::rgb8(255, 0, 0)));
        sim.add(floating(Anchor::TopLeft, Layer::Desktop, Color::rgb8(0, 255, 0)));
        let now = Instant::now();
        sim.tick(now);
        let px = sim.render(now);
        assert_eq!((px.width(), px.height()), (400, 200));
        let at = |x, y| px.pixel(x, y).unwrap();
        assert_eq!(at(60, 60).red(), 255, "top layer wins");
        assert_eq!(at(300, 150).blue(), 255, "background elsewhere");
        assert_eq!(sim.rect(0), Some(Rect::new(10.0, 10.0, 40.0, 40.0)));
        assert!(sim.click(20.0, 20.0, now).redraw);
        assert!(!sim.click(150.0, 80.0, now).redraw, "nothing under the cursor");
    }
}
