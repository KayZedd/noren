//! Hosts put surfaces on screen. [`run`] uses the native Windows host; with the `preview`
//! feature, [`preview::run`] shows surfaces in a regular window on any OS for development.

#[cfg(feature = "preview")]
pub mod preview;
pub mod sim;
#[cfg(windows)]
mod windows;

use crate::Rect;
use crate::surface::Surface;

/// Which monitor a surface goes on. Out-of-range indices fall back to the primary monitor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MonitorSelect {
    #[default]
    Primary,
    /// Index into monitors sorted primary-first, then left to right.
    Index(usize),
    /// Device name, e.g. `\\.\DISPLAY2`.
    Name(String),
}

#[derive(Clone, Debug, Default)]
pub struct SurfaceOptions {
    pub monitor: MonitorSelect,
    /// Register as the auto-hide AppBar of the top edge, so the shell knows about the panel
    /// (maximized windows leave the hot edge free, the taskbar does not claim it).
    pub top_appbar: bool,
}

impl SurfaceOptions {
    /// Options for a [`crate::Panel`]: primary monitor, top auto-hide AppBar.
    pub fn panel() -> Self {
        Self {
            monitor: MonitorSelect::Primary,
            top_appbar: true,
        }
    }

    pub fn on(mut self, monitor: MonitorSelect) -> Self {
        self.monitor = monitor;
        self
    }
}

/// A surface with its window options.
pub struct Entry {
    pub surface: Box<dyn Surface>,
    pub options: SurfaceOptions,
}

impl Entry {
    pub fn new(surface: impl Surface + 'static, options: SurfaceOptions) -> Self {
        Self {
            surface: Box::new(surface),
            options,
        }
    }
}

/// Physical monitor as reported by the OS.
#[derive(Clone, Debug, PartialEq)]
pub struct Display {
    pub name: String,
    pub primary: bool,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f32,
}

impl Display {
    pub fn info(&self) -> crate::surface::MonitorInfo {
        crate::surface::MonitorInfo {
            width: self.width as f32 / self.scale,
            height: self.height as f32 / self.scale,
            scale: self.scale,
        }
    }

    /// Screen pixel → logical monitor coordinate, `None` if outside this monitor.
    pub fn to_logical(&self, x: i32, y: i32) -> Option<(f32, f32)> {
        let (dx, dy) = (
            i64::from(x) - i64::from(self.left),
            i64::from(y) - i64::from(self.top),
        );
        (dx >= 0 && dy >= 0 && dx < i64::from(self.width) && dy < i64::from(self.height))
            .then(|| (dx as f32 / self.scale, dy as f32 / self.scale))
    }

    /// Logical rect → screen pixels (x, y, w, h), never empty, clamped to the monitor bottom.
    pub fn to_physical(&self, rect: Rect) -> (i32, i32, u32, u32) {
        let x = (rect.x * self.scale).round() as i32;
        let y = (rect.y * self.scale).round() as i32;
        let w = ((rect.w * self.scale).ceil() as u32).max(1);
        let max_h = (self.height as i32 - y).max(1) as u32;
        let h = ((rect.h * self.scale).ceil() as u32).clamp(1, max_h);
        (self.left + x, self.top + y, w, h)
    }
}

/// Sorts primary first, then by position; picks the selected one.
pub fn select_display(mut displays: Vec<Display>, select: &MonitorSelect) -> Option<Display> {
    displays.sort_by(|a, b| (!a.primary, a.left, a.top, &a.name).cmp(&(!b.primary, b.left, b.top, &b.name)));
    let index = match select {
        MonitorSelect::Primary => 0,
        MonitorSelect::Index(i) if *i < displays.len() => *i,
        MonitorSelect::Index(_) => 0,
        MonitorSelect::Name(name) => displays.iter().position(|d| &d.name == name).unwrap_or(0),
    };
    (index < displays.len()).then(|| displays.swap_remove(index))
}

/// Premultiplied RGBA (tiny-skia) → premultiplied BGRA (GDI DIB, UpdateLayeredWindow).
pub fn rgba_to_bgra(src: &[u8], dst: &mut [u8]) {
    for (s, d) in src.as_chunks::<4>().0.iter().zip(dst.as_chunks_mut::<4>().0) {
        *d = [s[2], s[1], s[0], s[3]];
    }
}

/// Runs the surfaces until one asks to quit or the user picks Quit from the context menu.
#[cfg(windows)]
pub fn run(entries: Vec<Entry>) -> Result<(), String> {
    windows::run(entries)
}

#[cfg(not(windows))]
pub fn run(_entries: Vec<Entry>) -> Result<(), String> {
    Err("the native host runs on Windows; build with --features preview for a dev window".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(name: &str, primary: bool, left: i32) -> Display {
        Display {
            name: name.into(),
            primary,
            left,
            top: -100,
            width: 2560,
            height: 1440,
            scale: 1.5,
        }
    }

    #[test]
    fn selection_is_primary_first_then_left_to_right_with_fallback() {
        let all = vec![
            display("C", false, 2560),
            display("B", true, 0),
            display("A", false, -2560),
        ];
        assert_eq!(
            select_display(all.clone(), &MonitorSelect::Primary).unwrap().name,
            "B"
        );
        assert_eq!(
            select_display(all.clone(), &MonitorSelect::Index(1))
                .unwrap()
                .name,
            "A"
        );
        assert_eq!(
            select_display(all.clone(), &MonitorSelect::Index(9))
                .unwrap()
                .name,
            "B"
        );
        assert_eq!(
            select_display(all.clone(), &MonitorSelect::Name("C".into()))
                .unwrap()
                .name,
            "C"
        );
        assert_eq!(
            select_display(all, &MonitorSelect::Name("?".into()))
                .unwrap()
                .name,
            "B"
        );
        assert!(select_display(Vec::new(), &MonitorSelect::Primary).is_none());
    }

    #[test]
    fn coordinates_round_trip_through_scale_and_negative_origins() {
        let d = display("A", false, -2560);
        assert_eq!(d.info().width, 2560.0 / 1.5);
        assert_eq!(d.to_logical(-2560, -100), Some((0.0, 0.0)));
        assert_eq!(d.to_logical(-2560 + 15, -100 + 3), Some((10.0, 2.0)));
        assert_eq!(d.to_logical(0, -100), None, "right edge is exclusive");
        assert_eq!(d.to_logical(-2560, -101), None);
        assert_eq!(
            d.to_physical(Rect::new(10.0, 2.0, 100.0, 36.0)),
            (-2545, -97, 150, 54)
        );
        // Clamped to the monitor bottom, never zero sized.
        assert_eq!(d.to_physical(Rect::new(0.0, 0.0, 0.0, 5000.0)).2, 1);
        assert_eq!(d.to_physical(Rect::new(0.0, 0.0, 0.0, 5000.0)).3, 1440);
    }

    #[test]
    fn pixels_swap_red_and_blue_keeping_premultiplied_alpha() {
        let src = [10, 20, 30, 40, 1, 2, 3, 4];
        let mut dst = [0; 8];
        rgba_to_bgra(&src, &mut dst);
        assert_eq!(dst, [30, 20, 10, 40, 3, 2, 1, 4]);
    }
}
