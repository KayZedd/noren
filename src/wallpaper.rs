//! Reading the wallpaper: where on it a desktop widget is easiest to read, and which text
//! color reads on it.
//!
//! Each monitor's wallpaper is decoded small (its longer side at most [`SIDE`] pixels) and
//! mapped onto that monitor the way Windows shows it: filled, fitted, stretched, centered,
//! tiled or spanned across every monitor, with the background color where the picture does
//! not reach. A region's "busyness" is how much its brightness varies plus how sharp its
//! edges are, so a flat sky scores low and foliage or text scores high.
//!
//! One background thread watches the wallpaper for the whole process ([`watch`]); every
//! desktop widget subscribes to it.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::surface::{Anchor, MonitorInfo};
use crate::{Color, Rect};

/// Longest side the wallpaper is decoded at; plenty to judge calm against busy.
pub const SIDE: u32 = 256;

/// Most samples a region is judged by along one side.
const MAX_SAMPLES: u32 = 256;

/// Fewest samples along one side, so a box smaller than a wallpaper pixel is still judged by
/// its surroundings, not by one flat pixel.
const MIN_SAMPLES: u32 = 3;

/// A decoded wallpaper, straight-alpha RGBA rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Wallpaper {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// The image file's own size in pixels, before decoding small. Centered and tiled
    /// pictures are shown at this size.
    pub source: (u32, u32),
}

impl Wallpaper {
    fn pixel(&self, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * self.width + x) * 4) as usize;
        [self.rgba[i], self.rgba[i + 1], self.rgba[i + 2]]
    }

    fn usable(&self) -> bool {
        self.width > 0 && self.height > 0 && self.rgba.len() >= (self.width * self.height * 4) as usize
    }
}

/// How Windows lays the picture on a monitor ("Choose a fit" in Settings).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Fit {
    /// The picture's own size, in the middle.
    Center,
    /// The picture's own size, repeated from the top left corner.
    Tile,
    /// Squashed to the monitor's shape.
    Stretch,
    /// Scaled to fit inside, bars of background color on two sides.
    Fit,
    /// Scaled to cover, the overflow cut off.
    #[default]
    Fill,
    /// One picture filling the box around every monitor.
    Span,
}

/// One monitor's wallpaper.
#[derive(Clone, Debug, PartialEq)]
pub struct Screen {
    /// The monitor in physical desktop pixels (left, top, width, height); `None` stands for
    /// every monitor.
    pub rect: Option<(i32, i32, u32, u32)>,
    /// `None` is a solid background color.
    pub image: Option<Arc<Wallpaper>>,
}

/// Every monitor's wallpaper, the fit and the background color.
#[derive(Clone, Debug, PartialEq)]
pub struct Desktop {
    pub screens: Vec<Screen>,
    pub fit: Fit,
    pub background: Color,
}

impl Desktop {
    /// The same picture on every monitor.
    pub fn uniform(image: Option<Arc<Wallpaper>>, fit: Fit, background: Color) -> Self {
        Self {
            screens: vec![Screen { rect: None, image }],
            fit,
            background,
        }
    }

    /// The wallpaper as `monitor` shows it; `None` when the monitor is not known.
    pub fn shown(&self, monitor: &MonitorInfo) -> Option<Shown> {
        let (mx, my) = monitor.origin;
        let contains = |&(l, t, w, h): &(i32, i32, u32, u32)| {
            let (dx, dy) = (i64::from(mx) - i64::from(l), i64::from(my) - i64::from(t));
            dx >= 0 && dy >= 0 && dx < i64::from(w) && dy < i64::from(h)
        };
        let screen = self
            .screens
            .iter()
            .find(|s| s.rect.as_ref().is_some_and(contains))
            .or_else(|| self.screens.iter().find(|s| s.rect.is_none()))?;
        let scale = if monitor.scale > 0.0 { monitor.scale } else { 1.0 };
        let (pw, ph) = (monitor.width * scale, monitor.height * scale);
        let mut shown = Shown {
            image: None,
            tile: false,
            scale,
            screen: (monitor.width, monitor.height),
            k: (1.0, 1.0),
            origin: (0.0, 0.0),
            background: self.background,
        };
        let Some(image) = screen.image.clone().filter(|i| i.usable()) else {
            return Some(shown);
        };
        // The frame the picture is laid into, relative to this monitor's top left corner.
        let (fx, fy, fw, fh) = match (self.fit, self.bounds()) {
            (Fit::Span, Some((l, t, w, h))) => (
                (i64::from(l) - i64::from(mx)) as f32,
                (i64::from(t) - i64::from(my)) as f32,
                w as f32,
                h as f32,
            ),
            _ => (0.0, 0.0, pw, ph),
        };
        let (ow, oh) = (image.source.0.max(1) as f32, image.source.1.max(1) as f32);
        let (sw, sh) = match self.fit {
            Fit::Fill | Fit::Span => {
                let k = (fw / ow).max(fh / oh);
                (ow * k, oh * k)
            }
            Fit::Fit => {
                let k = (fw / ow).min(fh / oh);
                (ow * k, oh * k)
            }
            Fit::Stretch => (fw, fh),
            Fit::Center | Fit::Tile => (ow, oh),
        };
        shown.tile = self.fit == Fit::Tile;
        shown.origin = if shown.tile {
            (fx, fy)
        } else {
            (fx + (fw - sw) / 2.0, fy + (fh - sh) / 2.0)
        };
        shown.k = (sw / image.width as f32, sh / image.height as f32);
        if !(shown.k.0.is_finite() && shown.k.0 > 0.0 && shown.k.1.is_finite() && shown.k.1 > 0.0) {
            return None;
        }
        shown.image = Some(image);
        Some(shown)
    }

    /// The box around every known monitor, physical pixels.
    fn bounds(&self) -> Option<(i32, i32, u32, u32)> {
        let rects = self.screens.iter().filter_map(|s| s.rect);
        let (mut l, mut t, mut r, mut b) = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        for (x, y, w, h) in rects {
            l = l.min(i64::from(x));
            t = t.min(i64::from(y));
            r = r.max(i64::from(x) + i64::from(w));
            b = b.max(i64::from(y) + i64::from(h));
        }
        (l < r && t < b).then(|| (l as i32, t as i32, (r - l) as u32, (b - t) as u32))
    }
}

/// What lies under a box on the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    /// The average color.
    pub mean: Color,
    /// Brightness spread plus edge strength, 0 for a flat color.
    pub busy: f32,
}

/// One monitor's wallpaper, mapped from its logical coordinates to picture pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    image: Option<Arc<Wallpaper>>,
    tile: bool,
    /// Physical pixels per logical one.
    scale: f32,
    /// The monitor's logical size.
    screen: (f32, f32),
    /// Physical pixels per decoded picture pixel.
    k: (f32, f32),
    /// The picture's top left corner, physical pixels from the monitor's.
    origin: (f32, f32),
    background: Color,
}

impl Shown {
    /// The color at physical point `(x, y)` on the monitor.
    fn sample(&self, x: f32, y: f32) -> [u8; 3] {
        if let Some(img) = &self.image {
            let (mut u, mut v) = ((x - self.origin.0) / self.k.0, (y - self.origin.1) / self.k.1);
            let (w, h) = (img.width as f32, img.height as f32);
            if self.tile {
                u = u.rem_euclid(w);
                v = v.rem_euclid(h);
            }
            if (0.0..w).contains(&u) && (0.0..h).contains(&v) {
                return img.pixel((u as u32).min(img.width - 1), (v as u32).min(img.height - 1));
            }
        }
        let c = self.background.to_rgba8();
        [c[0], c[1], c[2]]
    }

    /// The wallpaper under logical `rect`; `None` when the box misses the monitor.
    pub fn region(&self, rect: Rect) -> Option<Region> {
        let x0 = rect.x.max(0.0);
        let y0 = rect.y.max(0.0);
        let x1 = (rect.x + rect.w).min(self.screen.0);
        let y1 = (rect.y + rect.h).min(self.screen.1);
        if !(x1 > x0 && y1 > y0) {
            return None;
        }
        // About one sample per picture pixel the box covers.
        let count = |len: f32, k: f32| {
            if self.image.is_none() {
                return MIN_SAMPLES;
            }
            let n = (len * self.scale / k).ceil();
            (n.min(MAX_SAMPLES as f32) as u32).max(MIN_SAMPLES)
        };
        let (nx, ny) = (count(x1 - x0, self.k.0), count(y1 - y0, self.k.1));
        let (sx, sy) = ((x1 - x0) / nx as f32, (y1 - y0) / ny as f32);
        let mut light = Vec::with_capacity((nx * ny) as usize);
        let mut rgb = [0.0f64; 3];
        for j in 0..ny {
            for i in 0..nx {
                let x = (x0 + (i as f32 + 0.5) * sx) * self.scale;
                let y = (y0 + (j as f32 + 0.5) * sy) * self.scale;
                let px = self.sample(x, y);
                for (acc, v) in rgb.iter_mut().zip(px) {
                    *acc += f64::from(v);
                }
                let [r, g, b] = px.map(f64::from);
                light.push((0.299 * r + 0.587 * g + 0.114 * b) / 255.0);
            }
        }
        let n = light.len() as f64;
        // Measured from the first sample, so a flat region comes out exactly 0.
        let first = light[0];
        let mean = light.iter().map(|l| l - first).sum::<f64>() / n;
        let square = light.iter().map(|l| (l - first).powi(2)).sum::<f64>() / n;
        let spread = (square - mean * mean).max(0.0).sqrt();
        let (nx, ny) = (nx as usize, ny as usize);
        let mut edges = 0.0;
        for j in 0..ny {
            for i in 0..nx {
                let l = light[j * nx + i];
                if i + 1 < nx {
                    edges += (l - light[j * nx + i + 1]).abs();
                }
                if j + 1 < ny {
                    edges += (l - light[(j + 1) * nx + i]).abs();
                }
            }
        }
        let channel = |v: f64| ((v / n).round() as u8).clamp(0, 255);
        Some(Region {
            mean: Color::rgb8(channel(rgb[0]), channel(rgb[1]), channel(rgb[2])),
            busy: (spread + 2.0 * edges / n) as f32,
        })
    }

    /// Of `anchors`, the one where a `size` box `offset` in from the edges covers the calmest
    /// wallpaper; the earlier anchor wins a tie (within 0.01).
    pub fn calmest(&self, size: (f32, f32), offset: (f32, f32), anchors: &[Anchor]) -> Option<Anchor> {
        let mut best: Option<(Anchor, f32)> = None;
        for &a in anchors {
            let (x, y) = a.place(self.screen, size, offset);
            let Some(r) = self.region(Rect::new(x, y, size.0, size.1)) else {
                continue;
            };
            if best.is_none_or(|(_, b)| r.busy < b - 0.01) {
                best = Some((a, r.busy));
            }
        }
        best.map(|(a, _)| a)
    }
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

/// Hands over the desktop's wallpaper when there is a new one.
pub trait Source {
    /// The wallpaper, once per change; `None` when nothing changed.
    fn poll(&mut self) -> Option<Arc<Desktop>>;
}

/// A wallpaper that never changes, handed over once (tests, and no wallpaper at all).
pub struct Fixed(Option<Arc<Desktop>>);

impl Fixed {
    pub fn new(desktop: Option<Arc<Desktop>>) -> Self {
        Self(desktop)
    }
}

impl Source for Fixed {
    fn poll(&mut self) -> Option<Arc<Desktop>> {
        self.0.take()
    }
}

/// The newest wallpaper, shared by every subscriber.
#[derive(Default)]
pub struct Hub {
    /// How many times a wallpaper was published, and the newest one.
    state: Mutex<(u64, Option<Arc<Desktop>>)>,
}

impl Hub {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn publish(&self, desktop: Desktop) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        s.0 += 1;
        s.1 = Some(Arc::new(desktop));
    }

    /// A source that hands over the current wallpaper, if any, and then every new one.
    pub fn subscribe(self: &Arc<Self>) -> Subscriber {
        Subscriber {
            hub: self.clone(),
            seen: 0,
        }
    }
}

/// One reader of a [`Hub`].
pub struct Subscriber {
    hub: Arc<Hub>,
    seen: u64,
}

impl Source for Subscriber {
    fn poll(&mut self) -> Option<Arc<Desktop>> {
        let s = self.hub.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.0 == self.seen {
            return None;
        }
        self.seen = s.0;
        s.1.clone()
    }
}

/// How often the wallpaper is checked for a change.
pub const CHECK: Duration = Duration::from_secs(30);

/// The desktop's wallpaper. The first call starts one thread that checks it every [`CHECK`]
/// and decodes it; every call subscribes to that thread. Off Windows nothing ever arrives.
pub fn watch() -> Box<dyn Source> {
    static HUB: OnceLock<Arc<Hub>> = OnceLock::new();
    let hub = HUB.get_or_init(|| {
        let hub = Hub::new();
        #[cfg(windows)]
        system::spawn(hub.clone());
        hub
    });
    Box::new(hub.subscribe())
}

/// The wallpaper of every monitor, read through the shell's `IDesktopWallpaper`.
#[cfg(windows)]
pub mod system {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::SystemTime;

    use windows::Storage::StorageFile;
    use windows::Storage::Streams::IRandomAccessStream;
    use windows::Win32::System::Com::{
        CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    };
    use windows::Win32::UI::Shell::{
        DESKTOP_WALLPAPER_POSITION, DWPOS_CENTER, DWPOS_FIT, DWPOS_SPAN, DWPOS_STRETCH, DWPOS_TILE,
        DesktopWallpaper, IDesktopWallpaper,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        SPI_GETDESKWALLPAPER, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SystemParametersInfoW,
    };
    use windows::core::{HSTRING, Interface, PCWSTR, PWSTR};

    use super::{CHECK, Desktop, Fit, Hub, SIDE, Screen, Wallpaper};
    use crate::Color;

    type Rect = (i32, i32, u32, u32);
    /// Path, size and modification time, so a changed file is noticed cheaply.
    type FileKey = (PathBuf, u64, SystemTime);

    /// What the shell says is shown, before any picture is decoded.
    #[derive(Clone, Debug, PartialEq)]
    struct Layout {
        fit: Fit,
        background: Color,
        screens: Vec<(Option<Rect>, Option<FileKey>)>,
    }

    fn fit(p: DESKTOP_WALLPAPER_POSITION) -> Fit {
        match p {
            DWPOS_CENTER => Fit::Center,
            DWPOS_TILE => Fit::Tile,
            DWPOS_STRETCH => Fit::Stretch,
            DWPOS_FIT => Fit::Fit,
            DWPOS_SPAN => Fit::Span,
            _ => Fit::Fill,
        }
    }

    fn file_key(path: &str) -> Option<FileKey> {
        if path.is_empty() {
            return None;
        }
        let path = PathBuf::from(path);
        let meta = std::fs::metadata(&path).ok()?;
        Some((path, meta.len(), meta.modified().ok()?))
    }

    /// A string the shell allocated, freed after reading.
    fn take(p: PWSTR) -> String {
        // SAFETY: the shell hands over a NUL-terminated string allocated with
        // CoTaskMemAlloc, which is ours to free.
        unsafe {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            s
        }
    }

    fn shell() -> windows::core::Result<Layout> {
        // SAFETY: plain COM calls on an object this thread created; every string returned is
        // freed by `take`.
        unsafe {
            let wp: IDesktopWallpaper = CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL)?;
            let fit = fit(wp.GetPosition()?);
            let c = wp.GetBackgroundColor()?.0;
            let background = Color::rgb8(c as u8, (c >> 8) as u8, (c >> 16) as u8);
            let mut screens = Vec::new();
            for i in 0..wp.GetMonitorDevicePathCount()? {
                let id = HSTRING::from(take(wp.GetMonitorDevicePathAt(i)?));
                // Monitors that are attached but off have no rect; they show nothing.
                let Ok(r) = wp.GetMonitorRECT(PCWSTR(id.as_ptr())) else {
                    continue;
                };
                let rect = (
                    r.left,
                    r.top,
                    (r.right - r.left).max(0) as u32,
                    (r.bottom - r.top).max(0) as u32,
                );
                let path = wp.GetWallpaper(PCWSTR(id.as_ptr())).map(take).unwrap_or_default();
                screens.push((Some(rect), file_key(&path)));
            }
            Ok(Layout {
                fit,
                background,
                screens,
            })
        }
    }

    /// The single wallpaper file, for when the shell object is not there.
    fn legacy() -> Layout {
        let mut buf = [0u16; 1024];
        // SAFETY: the buffer is as long as the length passed.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETDESKWALLPAPER,
                buf.len() as u32,
                Some(buf.as_mut_ptr().cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        }
        .is_ok();
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        let path = if ok {
            String::from_utf16_lossy(&buf[..len])
        } else {
            String::new()
        };
        Layout {
            fit: Fit::Fill,
            background: Color::BLACK,
            screens: vec![(None, file_key(&path))],
        }
    }

    pub fn load(path: &Path) -> Result<Wallpaper, String> {
        let err = |e: windows::core::Error| format!("wallpaper {}: {}", path.display(), e.message());
        let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(path.as_os_str()))
            .and_then(|op| op.join())
            .map_err(err)?;
        let stream = file
            .OpenReadAsync()
            .and_then(|op| op.join())
            .and_then(|s| s.cast::<IRandomAccessStream>())
            .map_err(err)?;
        let (source, (width, height), rgba) = crate::media::decode_sized(&stream, SIDE).map_err(err)?;
        Ok(Wallpaper {
            width,
            height,
            rgba,
            source,
        })
    }

    /// Decodes each file once; a file that fails to decode drops its monitor, so widgets on it
    /// keep their last pick instead of reading a wallpaper that is not there.
    fn build(layout: &Layout, cache: &mut HashMap<FileKey, Arc<Wallpaper>>) -> Desktop {
        let mut fresh: HashMap<FileKey, Arc<Wallpaper>> = HashMap::new();
        let mut screens = Vec::new();
        for (rect, key) in &layout.screens {
            let image = match key {
                None => None,
                Some(key) => {
                    let found = fresh.get(key).or_else(|| cache.get(key)).cloned();
                    let image = match found {
                        Some(i) => i,
                        None => match load(&key.0) {
                            Ok(w) => Arc::new(w),
                            Err(e) => {
                                eprintln!("noren: {e}");
                                continue;
                            }
                        },
                    };
                    fresh.insert(key.clone(), image.clone());
                    Some(image)
                }
            };
            screens.push(Screen { rect: *rect, image });
        }
        *cache = fresh;
        Desktop {
            screens,
            fit: layout.fit,
            background: layout.background,
        }
    }

    /// Checks the wallpaper every [`CHECK`] and publishes it when anything about it changed.
    pub fn spawn(hub: Arc<Hub>) {
        std::thread::spawn(move || {
            // SAFETY: once, on this thread's start, before any COM call.
            let com = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
            let mut last: Option<Layout> = None;
            let mut cache = HashMap::new();
            loop {
                let layout = com.then(|| shell().ok()).flatten().unwrap_or_else(legacy);
                if last.as_ref() != Some(&layout) {
                    hub.publish(build(&layout, &mut cache));
                    last = Some(layout);
                }
                std::thread::sleep(CHECK);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: u32, h: u32, px: impl Fn(u32, u32) -> [u8; 3]) -> Arc<Wallpaper> {
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = px(x, y);
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
        }
        Arc::new(Wallpaper {
            width: w,
            height: h,
            rgba,
            source: (w, h),
        })
    }

    /// A 40x20 wallpaper: the left half flat dark blue, the right half a black and white
    /// checkerboard.
    fn split() -> Arc<Wallpaper> {
        image(40, 20, |x, y| {
            if x < 20 {
                [20, 30, 90]
            } else if (x + y) % 2 == 0 {
                [255, 255, 255]
            } else {
                [0, 0, 0]
            }
        })
    }

    fn monitor(origin: (i32, i32), width: f32, height: f32, scale: f32) -> MonitorInfo {
        MonitorInfo {
            width,
            height,
            scale,
            origin,
        }
    }

    const FHD: MonitorInfo = MonitorInfo {
        width: 1600.0,
        height: 900.0,
        scale: 1.0,
        origin: (0, 0),
    };

    const RED: Color = Color::rgb8(200, 0, 0);
    const BLUE: Color = Color::rgb8(20, 30, 90);

    fn shown(image: Arc<Wallpaper>, fit: Fit, m: MonitorInfo) -> Shown {
        Desktop::uniform(Some(image), fit, RED).shown(&m).unwrap()
    }

    fn at(s: &Shown, x: f32, y: f32, w: f32, h: f32) -> Region {
        s.region(Rect::new(x, y, w, h)).unwrap()
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
        assert_eq!(readable(BLUE, &[Color::BLACK, Color::WHITE]), Some(Color::WHITE));
        assert_eq!(readable(Color::BLACK, &[]), None);
    }

    #[test]
    fn fill_crops_the_overflow() {
        // A 2:1 wallpaper on a 16:9 screen is cropped left and right.
        let s = shown(split(), Fit::Fill, FHD);
        let left = at(&s, 0.0, 0.0, 400.0, 900.0);
        assert_eq!((left.mean, left.busy), (BLUE, 0.0));
        let right = at(&s, 1200.0, 0.0, 400.0, 900.0);
        assert!(right.busy > 1.0, "{right:?}");
        // Off screen there is no region.
        assert!(s.region(Rect::new(5000.0, 0.0, 10.0, 10.0)).is_none());
        // A box smaller than a picture pixel is still judged by a few samples around it.
        let tiny = at(&s, 1500.0, 400.0, 60.0, 60.0);
        assert!(tiny.busy > 0.0, "{tiny:?}");
    }

    #[test]
    fn fit_and_center_leave_background_bars() {
        // Fitted, the 1600x800 picture leaves 50 pixel bars top and bottom.
        let s = shown(split(), Fit::Fit, FHD);
        assert_eq!(at(&s, 0.0, 0.0, 400.0, 40.0).mean, RED);
        assert_eq!(at(&s, 0.0, 60.0, 400.0, 100.0).mean, BLUE);
        // Centered at its own 40x20 pixels, the corners are background.
        let s = shown(split(), Fit::Center, FHD);
        assert_eq!(at(&s, 0.0, 0.0, 300.0, 120.0).mean, RED);
        assert_eq!(at(&s, 785.0, 445.0, 10.0, 10.0).mean, BLUE);
    }

    #[test]
    fn stretch_and_tile_cover_the_screen() {
        // Stretched, the left half of the screen is the flat half of the picture.
        let s = shown(split(), Fit::Stretch, FHD);
        assert_eq!(at(&s, 0.0, 0.0, 790.0, 900.0).busy, 0.0);
        // Tiled, the 40 pixel picture repeats: x = 400..420 is a flat stretch again.
        let s = shown(split(), Fit::Tile, FHD);
        let flat = at(&s, 400.0, 0.0, 20.0, 900.0);
        assert_eq!((flat.mean, flat.busy), (BLUE, 0.0));
        assert!(at(&s, 420.0, 0.0, 20.0, 20.0).busy > 1.0);
    }

    #[test]
    fn a_small_decode_keeps_the_source_size_for_centered_pictures() {
        // Decoded at 40x20 from a 400x200 file: centered it covers x = 600..1000.
        let mut w = (*split()).clone();
        w.source = (400, 200);
        let s = shown(Arc::new(w), Fit::Center, FHD);
        assert_eq!(at(&s, 610.0, 360.0, 180.0, 180.0).mean, BLUE);
        assert_eq!(at(&s, 500.0, 360.0, 90.0, 180.0).mean, RED);
        // At 200% scale a 40x20 picture covers 20x10 logical pixels in the middle.
        let s2 = shown(split(), Fit::Center, monitor((0, 0), 800.0, 450.0, 2.0));
        assert_eq!(at(&s2, 391.0, 220.0, 4.0, 4.0).mean, BLUE);
        assert_eq!(at(&s2, 370.0, 220.0, 8.0, 4.0).mean, RED);
    }

    #[test]
    fn each_monitor_reads_its_own_wallpaper_and_span_crosses_them() {
        let white = image(16, 9, |_, _| [250, 250, 250]);
        let black = image(16, 9, |_, _| [5, 5, 5]);
        let rects = [(0, 0, 1600, 900), (1600, 0, 1600, 900)];
        let desk = Desktop {
            screens: vec![
                Screen {
                    rect: Some(rects[0]),
                    image: Some(white),
                },
                Screen {
                    rect: Some(rects[1]),
                    image: Some(black),
                },
            ],
            fit: Fit::Fill,
            background: Color::BLACK,
        };
        let right = monitor((1600, 0), 1600.0, 900.0, 1.0);
        let r = at(&desk.shown(&right).unwrap(), 0.0, 0.0, 100.0, 100.0);
        assert_eq!(r.mean, Color::rgb8(5, 5, 5));
        let l = at(&desk.shown(&FHD).unwrap(), 0.0, 0.0, 100.0, 100.0);
        assert_eq!(l.mean, Color::rgb8(250, 250, 250));
        // An unknown monitor has no wallpaper to read.
        assert!(desk.shown(&monitor((-5000, 0), 800.0, 600.0, 1.0)).is_none());

        // Spanned, a 2:1 picture fills the 3200x900 box around both monitors at 3200x1600,
        // cropped top and bottom: the left monitor shows the flat half, the right one the
        // checkerboard.
        let span = Desktop {
            screens: rects
                .iter()
                .map(|&r| Screen {
                    rect: Some(r),
                    image: Some(split()),
                })
                .collect(),
            fit: Fit::Span,
            background: Color::BLACK,
        };
        assert!(at(&span.shown(&right).unwrap(), 200.0, 0.0, 800.0, 900.0).busy > 1.0);
        assert_eq!(at(&span.shown(&FHD).unwrap(), 200.0, 0.0, 800.0, 900.0).busy, 0.0);
    }

    #[test]
    fn a_solid_color_desktop_is_calm_everywhere() {
        let gray = Color::rgb8(240, 240, 240);
        let s = Desktop::uniform(None, Fit::Fill, gray).shown(&FHD).unwrap();
        let r = at(&s, 10.0, 10.0, 200.0, 50.0);
        assert_eq!((r.mean, r.busy), (gray, 0.0));
        assert_eq!(
            s.calmest((300.0, 120.0), (32.0, 32.0), &AUTO_ANCHORS),
            Some(Anchor::TopRight)
        );
    }

    #[test]
    fn the_calmest_anchor_avoids_the_busy_half() {
        let s = shown(split(), Fit::Fill, FHD);
        // Top right is preferred, but it sits on the checkerboard.
        assert_eq!(
            s.calmest((300.0, 120.0), (32.0, 32.0), &AUTO_ANCHORS),
            Some(Anchor::TopLeft)
        );
    }

    #[test]
    fn a_fixed_source_hands_its_wallpaper_over_once() {
        let mut s = Fixed::new(Some(Arc::new(Desktop::uniform(None, Fit::Fill, Color::BLACK))));
        assert!(s.poll().is_some());
        assert!(s.poll().is_none());
        assert!(Fixed::new(None).poll().is_none());
    }

    #[test]
    fn every_subscriber_of_one_hub_gets_each_wallpaper_once() {
        let hub = Hub::new();
        let mut early = hub.subscribe();
        assert!(early.poll().is_none());
        let a = Desktop::uniform(None, Fit::Fill, Color::BLACK);
        hub.publish(a.clone());
        // A subscriber that joins late still gets the current wallpaper.
        let mut late = hub.subscribe();
        assert_eq!(early.poll().as_deref(), Some(&a));
        assert_eq!(late.poll().as_deref(), Some(&a));
        assert!(early.poll().is_none());
        assert!(late.poll().is_none());
        let b = Desktop::uniform(None, Fit::Tile, Color::WHITE);
        hub.publish(b.clone());
        assert_eq!(early.poll().as_deref(), Some(&b));
        assert_eq!(late.poll().as_deref(), Some(&b));
        // Calls to `watch` share one hub; off Windows nothing is ever published.
        #[cfg(not(windows))]
        {
            assert!(watch().poll().is_none());
            assert!(watch().poll().is_none());
        }
    }
}
