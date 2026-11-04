//! Win32 side of edge glows: one layered, click-through, topmost popup per lit band, drawn
//! with `UpdateLayeredWindow`. The band's pixels are rebuilt only when its color or size
//! changes; breathing and fading only change the blend's constant alpha.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::io;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{AC_SRC_ALPHA, AC_SRC_OVER, BLENDFUNCTION, HMONITOR};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, ShowWindow,
    ULW_ALPHA, UpdateLayeredWindow, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::w;

use super::glow::{Bar, Edge, Glow, band};
use super::rules::ScreenRect;
use crate::host::windows::Dib;

/// A monitor as glows see it: the whole screen, taskbar included.
pub(super) struct Screen {
    pub handle: HMONITOR,
    pub rect: ScreenRect,
}

struct Band {
    hwnd: HWND,
    dib: Option<Dib>,
    /// What the DIB holds.
    drawn: Option<(ScreenRect, [u8; 3])>,
    alpha: u8,
    shown: bool,
}

impl Drop for Band {
    fn drop(&mut self) {
        self.dib = None;
        // SAFETY: our own window, destroyed once.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Every band window, keyed by glow, monitor index and edge.
pub(super) struct Layers {
    bands: HashMap<(usize, usize, Edge), Band>,
}

unsafe extern "system" fn band_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: default handling for a window we own.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

impl Layers {
    pub fn new() -> io::Result<Self> {
        // SAFETY: registers a window class for this process.
        unsafe {
            let instance = GetModuleHandleW(None).map_err(io::Error::other)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(band_proc),
                hInstance: instance.into(),
                lpszClassName: w!("noren.glow"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(Self {
            bands: HashMap::new(),
        })
    }

    /// Shows `bars` on `screens` (in `monitor = N` order), skipping the monitor in `hidden`
    /// (a fullscreen app has it). Bands no longer lit are destroyed.
    pub fn show(&mut self, glows: &[Glow], bars: &[Bar], screens: &[Screen], hidden: Option<HMONITOR>) {
        let mut keep = Vec::new();
        for bar in bars {
            let g = &glows[bar.glow];
            let on: Vec<usize> = match g.monitor {
                Some(n) => vec![n as usize - 1],
                None => (0..screens.len()).collect(),
            };
            for m in on {
                let Some(screen) = screens.get(m).filter(|s| Some(s.handle) != hidden) else {
                    continue;
                };
                let width = (g.width as f32 * scale(screen.handle)).round().max(1.0) as i32;
                for &edge in &g.edges {
                    let key = (bar.glow, m, edge);
                    let rect = edge.rect(screen.rect, width);
                    let alpha = (bar.alpha * 255.0).round().clamp(1.0, 255.0) as u8;
                    if let Some(b) = self.band(key) {
                        if let Err(e) = b.draw(edge, rect, bar.rgb, alpha) {
                            eprintln!("noren: glow: {e}");
                        }
                        keep.push(key);
                    }
                }
            }
        }
        self.bands.retain(|k, _| keep.contains(k));
    }

    fn band(&mut self, key: (usize, usize, Edge)) -> Option<&mut Band> {
        match self.bands.entry(key) {
            Entry::Occupied(e) => Some(e.into_mut()),
            Entry::Vacant(e) => match create() {
                Ok(hwnd) => Some(e.insert(Band {
                    hwnd,
                    dib: None,
                    drawn: None,
                    alpha: 0,
                    shown: false,
                })),
                Err(err) => {
                    eprintln!("noren: glow window: {err}");
                    None
                }
            },
        }
    }

    pub fn clear(&mut self) {
        self.bands.clear();
    }
}

impl Band {
    fn draw(&mut self, edge: Edge, rect: ScreenRect, rgb: [u8; 3], alpha: u8) -> Result<(), String> {
        let fresh = self.drawn != Some((rect, rgb));
        if !fresh && alpha == self.alpha && self.shown {
            return Ok(());
        }
        let (w, h) = (rect.width().max(1) as u32, rect.height().max(1) as u32);
        if fresh {
            let len = if matches!(edge, Edge::Top | Edge::Bottom) {
                w
            } else {
                h
            };
            let depth = if len == w { h } else { w };
            let (bw, bh, px) = band(edge, len, depth, rgb);
            debug_assert_eq!((bw, bh), (w, h));
            let mut dib = Dib::new(w, h)?;
            dib.pixels().copy_from_slice(&px);
            self.dib = Some(dib);
            self.drawn = Some((rect, rgb));
        }
        let Some(dib) = self.dib.as_ref() else {
            return Ok(());
        };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let dst = POINT {
            x: rect.left,
            y: rect.top,
        };
        let size = SIZE {
            cx: w as i32,
            cy: h as i32,
        };
        let src = POINT::default();
        // SAFETY: the DIB is selected into its DC and matches `size`; all pointers outlive the
        // call, and the window is ours.
        unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&dst),
                Some(&size),
                Some(dib.dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            )
            .map_err(|e| format!("UpdateLayeredWindow: {e}"))?;
            if !self.shown {
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                self.shown = true;
            }
        }
        self.alpha = alpha;
        Ok(())
    }
}

/// A hidden band window: topmost, never activates, every click goes through.
fn create() -> io::Result<HWND> {
    // SAFETY: creates a window of the class registered in `Layers::new`, owned by this thread.
    unsafe {
        let instance = GetModuleHandleW(None).map_err(io::Error::other)?;
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            w!("noren.glow"),
            w!("noren glow"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(io::Error::other)?;
        let _ = ShowWindow(hwnd, SW_HIDE);
        Ok(hwnd)
    }
}

/// The monitor's scale when this process is DPI aware; 1 when it is not (Windows then scales
/// coordinates for us).
fn scale(monitor: HMONITOR) -> f32 {
    let (mut x, mut y) = (96u32, 96u32);
    // SAFETY: OS-provided handle and plain out pointers.
    let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) };
    x as f32 / 96.0
}
