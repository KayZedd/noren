//! Reading the wallpaper: where on it a desktop widget is easiest to read, and which text
//! color reads on it.
//!
//! The wallpaper is decoded small (its longer side at most [`SIDE`] pixels) and mapped onto a
//! monitor the way Windows' "Fill" fit shows it: scaled to cover the screen, centered, the
//! overflow cut off. A region's "busyness" is how much its brightness varies plus how sharp its
//! edges are, so a flat sky scores low and foliage or text scores high.

use std::sync::Arc;
use std::time::Duration;

use crate::surface::Anchor;
use crate::{Color, Rect};

/// Longest side the wallpaper is decoded at; plenty to judge calm against busy.
pub const SIDE: u32 = 256;

/// A decoded wallpaper, straight-alpha RGBA rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Wallpaper {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// What lies under a box on the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    /// The average color.
    pub mean: Color,
    /// Brightness spread plus edge strength, 0 for a flat color.
    pub busy: f32,
}

/// sRGB channel to linear light.
fn linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance, 0 (black) to 1 (white).
pub fn luminance(c: Color) -> f32 {
    0.2126 * linear(c.r) + 0.7152 * linear(c.g) + 0.0722 * linear(c.b)
}

/// WCAG contrast ratio, 1 to 21.
pub fn contrast(a: Color, b: Color) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Of `choices`, the color that contrasts most with `on`; the first wins a tie.
pub fn readable(on: Color, choices: &[Color]) -> Option<Color> {
    choices
        .iter()
        .copied()
        .fold(None, |best: Option<Color>, c| match best {
            Some(b) if contrast(on, b) >= contrast(on, c) => Some(b),
            _ => Some(c),
        })
}

/// Fewest wallpaper pixels a side of a region spans, so a box smaller than a few wallpaper
/// pixels is still judged by its surroundings, not by one flat pixel.
const MIN_SPAN: u32 = 3;

/// `a..b` grown evenly to at least [`MIN_SPAN`] (or `max`) pixels, kept inside `0..max`.
fn span(a: u32, b: u32, max: u32) -> (u32, u32) {
    let (mut a, mut b) = (a.min(max), b.min(max).max(a.min(max)));
    if a == max {
        return (a, b);
    }
    while b - a < MIN_SPAN.min(max) {
        if b < max {
            b += 1;
        }
        if b - a < MIN_SPAN.min(max) && a > 0 {
            a -= 1;
        }
    }
    (a, b)
}

impl Wallpaper {
    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2], self.rgba[i + 3]]
    }

    fn brightness(&self, x: u32, y: u32) -> f32 {
        let [r, g, b, _] = self.pixel(x, y);
        (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0
    }

    /// The wallpaper pixels under logical `rect` on a screen of logical size `screen`, shown
    /// as "Fill". `None` when the box misses the screen or the wallpaper is empty.
    pub fn region(&self, screen: (f32, f32), rect: Rect) -> Option<Region> {
        if self.width == 0 || self.height == 0 || self.rgba.len() < (self.width * self.height * 4) as usize {
            return None;
        }
        let (ww, wh) = (self.width as f32, self.height as f32);
        let k = (screen.0 / ww).max(screen.1 / wh);
        if !(k.is_finite() && k > 0.0) {
            return None;
        }
        let (ox, oy) = ((screen.0 - ww * k) / 2.0, (screen.1 - wh * k) / 2.0);
        let to_px = |v: f32, o: f32, max: u32| (((v - o) / k).floor().max(0.0) as u32).min(max);
        let (x0, x1) = span(
            to_px(rect.x, ox, self.width),
            to_px(rect.x + rect.w, ox, self.width),
            self.width,
        );
        let (y0, y1) = span(
            to_px(rect.y, oy, self.height),
            to_px(rect.y + rect.h, oy, self.height),
            self.height,
        );
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        let (mut sum, mut sum_sq, mut edges) = (0.0f64, 0.0f64, 0.0f64);
        let mut rgb = [0.0f64; 3];
        for y in y0..y1 {
            for x in x0..x1 {
                let [r, g, b, _] = self.pixel(x, y);
                for (acc, v) in rgb.iter_mut().zip([r, g, b]) {
                    *acc += f64::from(v);
                }
                let l = f64::from(self.brightness(x, y));
                sum += l;
                sum_sq += l * l;
                if x + 1 < x1 {
                    edges += (l - f64::from(self.brightness(x + 1, y))).abs();
                }
                if y + 1 < y1 {
                    edges += (l - f64::from(self.brightness(x, y + 1))).abs();
                }
            }
        }
        let n = f64::from((x1 - x0) * (y1 - y0));
        let mean = sum / n;
        let spread = (sum_sq / n - mean * mean).max(0.0).sqrt();
        let channel = |v: f64| ((v / n).round() as u8).clamp(0, 255);
        Some(Region {
            mean: Color::rgb8(channel(rgb[0]), channel(rgb[1]), channel(rgb[2])),
            busy: (spread + 2.0 * edges / n) as f32,
        })
    }

    /// Of `anchors`, the one where a `size` box `offset` in from the edges covers the calmest
    /// wallpaper; the earlier anchor wins a tie (within 0.01).
    pub fn calmest(
        &self,
        screen: (f32, f32),
        size: (f32, f32),
        offset: (f32, f32),
        anchors: &[Anchor],
    ) -> Option<Anchor> {
        let mut best: Option<(Anchor, f32)> = None;
        for &a in anchors {
            let (x, y) = a.place(screen, size, offset);
            let Some(r) = self.region(screen, Rect::new(x, y, size.0, size.1)) else {
                continue;
            };
            if best.is_none_or(|(_, b)| r.busy < b - 0.01) {
                best = Some((a, r.busy));
            }
        }
        best.map(|(a, _)| a)
    }
}

/// Where `anchor = "auto"` may put a widget, in order of preference.
pub const AUTO_ANCHORS: [Anchor; 8] = [
    Anchor::TopRight,
    Anchor::TopLeft,
    Anchor::BottomRight,
    Anchor::BottomLeft,
    Anchor::Right,
    Anchor::Left,
    Anchor::Top,
    Anchor::Bottom,
];

/// Hands over a wallpaper when there is a new one.
pub trait Source {
    /// The wallpaper, once per change; `None` when nothing changed.
    fn poll(&mut self) -> Option<Arc<Wallpaper>>;
}

/// A wallpaper that never changes, handed over once (tests, and no wallpaper at all).
pub struct Fixed(Option<Arc<Wallpaper>>);

impl Fixed {
    pub fn new(wallpaper: Option<Arc<Wallpaper>>) -> Self {
        Self(wallpaper)
    }
}

impl Source for Fixed {
    fn poll(&mut self) -> Option<Arc<Wallpaper>> {
        self.0.take()
    }
}

/// How often the wallpaper file is checked for a change.
pub const CHECK: Duration = Duration::from_secs(30);

/// The desktop's wallpaper, checked every [`CHECK`] on a background thread and decoded there.
/// Off Windows there is none.
pub fn watch() -> Box<dyn Source> {
    #[cfg(windows)]
    {
        Box::new(system::Watcher::spawn())
    }
    #[cfg(not(windows))]
    {
        Box::new(Fixed::new(None))
    }
}

/// The current wallpaper, decoded small, and a key that changes when it does.
#[cfg(windows)]
pub mod system {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::mpsc::{self, Receiver};

    use windows::Storage::StorageFile;
    use windows::Storage::Streams::IRandomAccessStream;
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    use windows::core::{HSTRING, Interface};

    use super::{CHECK, SIDE, Source, Wallpaper};

    /// The wallpaper file Windows shows, `None` for a solid color.
    pub fn path() -> Option<PathBuf> {
        let mut buf = [0u16; 1024];
        // SAFETY: the buffer is as long as the length passed.
        unsafe {
            SystemParametersInfoW(
                SPI_GETDESKWALLPAPER,
                buf.len() as u32,
                Some(buf.as_mut_ptr().cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        }
        .ok()?;
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        (len > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..len])))
    }

    /// Path, size and modification time, so a changed wallpaper is noticed cheaply.
    pub fn key() -> Option<(PathBuf, u64, std::time::SystemTime)> {
        let path = path()?;
        let meta = std::fs::metadata(&path).ok()?;
        Some((path, meta.len(), meta.modified().ok()?))
    }

    pub fn load(path: &std::path::Path) -> Result<Wallpaper, String> {
        let err = |e: windows::core::Error| format!("wallpaper {}: {}", path.display(), e.message());
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str()))
            .and_then(|op| op.join())
            .map_err(err)?;
        let stream = file
            .OpenReadAsync()
            .and_then(|op| op.join())
            .and_then(|s| s.cast::<IRandomAccessStream>())
            .map_err(err)?;
        let (width, height, rgba) = crate::media::decode_stream(&stream, SIDE).map_err(err)?;
        Ok(Wallpaper { width, height, rgba })
    }

    /// Loads the wallpaper on a thread of its own and again whenever its file changes.
    pub struct Watcher {
        rx: Receiver<Wallpaper>,
    }

    impl Watcher {
        pub fn spawn() -> Self {
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let mut last = None;
                loop {
                    let now = key();
                    if now != last {
                        if let Some((path, _, _)) = &now {
                            match load(path) {
                                Ok(w) => {
                                    if tx.send(w).is_err() {
                                        return;
                                    }
                                }
                                Err(e) => eprintln!("noren: {e}"),
                            }
                        }
                        last = now;
                    }
                    std::thread::sleep(CHECK);
                }
            });
            Self { rx }
        }
    }

    impl Source for Watcher {
        fn poll(&mut self) -> Option<Arc<Wallpaper>> {
            self.rx.try_iter().last().map(Arc::new)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 40x20 wallpaper: the left half flat dark blue, the right half a black and white
    /// checkerboard.
    fn split() -> Wallpaper {
        let (w, h) = (40u32, 20u32);
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let px = if x < w / 2 {
                    [20, 30, 90, 255]
                } else if (x + y) % 2 == 0 {
                    [255, 255, 255, 255]
                } else {
                    [0, 0, 0, 255]
                };
                rgba.extend_from_slice(&px);
            }
        }
        Wallpaper {
            width: w,
            height: h,
            rgba,
        }
    }

    #[test]
    fn contrast_follows_wcag() {
        assert!((contrast(Color::BLACK, Color::WHITE) - 21.0).abs() < 0.01);
        assert!((contrast(Color::WHITE, Color::WHITE) - 1.0).abs() < 0.001);
        let grey = Color::rgb8(119, 119, 119);
        assert!((contrast(grey, Color::WHITE) - 4.48).abs() < 0.02);
        assert_eq!(
            readable(Color::rgb8(240, 240, 230), &[Color::WHITE, Color::BLACK]),
            Some(Color::BLACK)
        );
        assert_eq!(
            readable(Color::rgb8(20, 30, 90), &[Color::BLACK, Color::WHITE]),
            Some(Color::WHITE)
        );
        assert_eq!(readable(Color::BLACK, &[]), None);
    }

    #[test]
    fn regions_map_through_the_fill_fit() {
        let wp = split();
        // A 2:1 wallpaper on a 16:9 screen is cropped left and right: the screen's left
        // edge shows wallpaper column 4, not 0.
        let screen = (1600.0, 900.0);
        let left = wp.region(screen, Rect::new(0.0, 0.0, 400.0, 900.0)).unwrap();
        assert_eq!(left.mean, Color::rgb8(20, 30, 90));
        assert_eq!(left.busy, 0.0);
        let right = wp.region(screen, Rect::new(1200.0, 0.0, 400.0, 900.0)).unwrap();
        assert!(right.busy > 1.0, "{right:?}");
        // Off screen and empty wallpapers have no region.
        assert!(wp.region(screen, Rect::new(5000.0, 0.0, 10.0, 10.0)).is_none());
        let empty = Wallpaper {
            width: 0,
            height: 0,
            rgba: vec![],
        };
        assert!(empty.region(screen, Rect::new(0.0, 0.0, 10.0, 10.0)).is_none());
    }

    #[test]
    fn spans_grow_to_three_pixels_inside_the_image() {
        assert_eq!(span(5, 5, 40), (4, 7));
        assert_eq!(span(0, 1, 40), (0, 3));
        assert_eq!(span(39, 40, 40), (37, 40));
        assert_eq!(span(2, 20, 40), (2, 20));
        assert_eq!(span(0, 1, 2), (0, 2));
        // Off the image stays empty.
        assert_eq!(span(40, 40, 40), (40, 40));
    }

    #[test]
    fn a_fixed_source_hands_its_wallpaper_over_once() {
        let mut s = Fixed::new(Some(Arc::new(split())));
        assert!(s.poll().is_some());
        assert!(s.poll().is_none());
        assert!(Fixed::new(None).poll().is_none());
    }

    #[test]
    fn the_calmest_anchor_avoids_the_busy_half() {
        let wp = split();
        let screen = (1600.0, 900.0);
        // Top right is preferred, but it sits on the checkerboard.
        let a = wp.calmest(screen, (300.0, 120.0), (32.0, 32.0), &AUTO_ANCHORS);
        assert_eq!(a, Some(Anchor::TopLeft));
        // Equally calm everywhere: the first choice stays.
        let flat = Wallpaper {
            width: 4,
            height: 4,
            rgba: [9u8, 9, 9, 255].repeat(16),
        };
        assert_eq!(
            flat.calmest(screen, (300.0, 120.0), (32.0, 32.0), &AUTO_ANCHORS),
            Some(Anchor::TopRight)
        );
    }
}
