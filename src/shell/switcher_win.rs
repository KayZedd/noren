//! Win32 side of the window switcher: one topmost popup that never takes the focus, painted
//! from a tiny-skia pixmap in `WM_PAINT`, with a DWM thumbnail of each window over its card.
//!
//! The popup is an ordinary (not layered) window: DWM composes thumbnails onto it reliably,
//! and Windows 11 rounds its corners. Keys never reach it; the runtime's keyboard hook drives
//! it, and clicks arrive through the runtime's message loop (see `win::hold`).

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::mem::size_of;

use tiny_skia::Pixmap;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
    DWMWA_CLOAKED, DWMWINDOWATTRIBUTE, DwmGetWindowAttribute, DwmQueryThumbnailSourceSize,
    DwmRegisterThumbnail, DwmSetWindowAttribute, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BeginPaint, DIB_RGB_COLORS, EndPaint, GetMonitorInfoW,
    InvalidateRect, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow, PAINTSTRUCT, SetDIBitsToDevice,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreateWindowExW, DefWindowProcW, DestroyWindow, GA_ROOTOWNER, GWL_EXSTYLE, GetAncestor,
    GetForegroundWindow, GetLastActivePopup, GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId,
    HWND_TOPMOST, IDC_ARROW, IsIconic, IsWindowVisible, LoadCursorW, MA_NOACTIVATE, SW_HIDE, SW_RESTORE,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SetForegroundWindow, SetWindowPos, ShowWindow, WM_ERASEBKGND,
    WM_MOUSEACTIVATE, WM_PAINT, WNDCLASSW, WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::w;

use super::switcher::{
    self, Candidate, Entry, Grid, Pick, fit, fit_grid, listed, owner_walk, thumb_slots, view,
};
use super::win::{exe_name, register_class, top_level_windows};
use crate::layout::{Frame, layout};
use crate::render::render_frame;
use crate::{Rect, TextSystem, Theme};

const DWMWA_WINDOW_CORNER_PREFERENCE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(33);
const DWMWCP_ROUND: u32 = 2;
/// An unassigned virtual key, sent when the switcher opens so the app that saw Alt go down
/// does not take its release for a menu-bar tap.
const VK_UNASSIGNED: u16 = 0xE8;

thread_local! {
    /// What `WM_PAINT` copies to the window: BGRX rows, width, height.
    static PIXELS: RefCell<(Vec<u8>, i32, i32)> = const { RefCell::new((Vec::new(), 0, 0)) };
}

unsafe extern "system" fn switcher_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // Clicks select a card without taking the focus from the window being left.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            // SAFETY: paints our own window from a buffer of exactly `w * h * 4` bytes.
            unsafe {
                let dc = BeginPaint(hwnd, &mut ps);
                PIXELS.with(|p| {
                    let (bits, w, h) = &*p.borrow();
                    if bits.len() == (*w * *h * 4) as usize && *w > 0 {
                        let info = BITMAPINFO {
                            bmiHeader: BITMAPINFOHEADER {
                                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                                biWidth: *w,
                                biHeight: -*h,
                                biPlanes: 1,
                                biBitCount: 32,
                                biCompression: BI_RGB.0,
                                ..Default::default()
                            },
                            ..Default::default()
                        };
                        SetDIBitsToDevice(
                            dc,
                            0,
                            0,
                            *w as u32,
                            *h as u32,
                            0,
                            0,
                            0,
                            *h as u32,
                            bits.as_ptr() as *const c_void,
                            &info,
                            DIB_RGB_COLORS,
                        );
                    }
                });
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        // SAFETY: default handling for a window we own.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// The switcher popup and what it shows while open.
pub(super) struct Overlay {
    hwnd: HWND,
    thumbnails: bool,
    text: TextSystem,
    theme: Theme,
    windows: Vec<HWND>,
    entries: Vec<Entry>,
    selected: usize,
    grid: Grid,
    scale: f32,
    size: (i32, i32),
    frame: Option<Frame<Pick>>,
    thumbs: Vec<isize>,
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.close();
        // SAFETY: our own window, destroyed on the thread that created it.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

impl Overlay {
    pub fn new(theme: Theme, thumbnails: bool) -> io::Result<Self> {
        // SAFETY: registers a class and creates a hidden window owned by this thread.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None).map_err(io::Error::other)?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(switcher_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: w!("noren.switcher"),
                ..Default::default()
            };
            register_class(&class)?;
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("noren.switcher"),
                w!("noren switcher"),
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
            let round = DWMWCP_ROUND;
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &round as *const u32 as *const c_void,
                4,
            );
            hwnd
        };
        let text = TextSystem::new(&theme.font_families);
        Ok(Self {
            hwnd,
            thumbnails,
            text,
            theme,
            windows: Vec::new(),
            entries: Vec::new(),
            selected: 0,
            grid: fit_grid(1, (1.0, 1.0), thumbnails),
            scale: 1.0,
            size: (0, 0),
            frame: None,
            thumbs: Vec::new(),
        })
    }

    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    pub fn is_open(&self) -> bool {
        self.frame.is_some()
    }

    /// Lists the windows and shows the switcher on the monitor of the foreground window.
    /// Nothing shows when there is nothing to switch to.
    pub fn open(&mut self, theme: Theme, back: bool) {
        self.close();
        mask_alt_release();
        let (windows, entries): (Vec<HWND>, Vec<Entry>) = switchable().into_iter().unzip();
        if entries.is_empty() {
            return;
        }
        self.theme = theme;
        self.selected = switcher::initial(entries.len(), back);
        self.windows = windows;
        self.entries = entries;
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let (mut dx, mut dy) = (96u32, 96u32);
        // SAFETY: read-only queries with correctly sized out structs.
        unsafe {
            let monitor = MonitorFromWindow(GetForegroundWindow(), MONITOR_DEFAULTTOPRIMARY);
            let _ = GetMonitorInfoW(monitor, &mut mi);
            let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dx, &mut dy);
        }
        self.scale = dx as f32 / 96.0;
        let work = mi.rcWork;
        let (ww, wh) = (work.right - work.left, work.bottom - work.top);
        let area = (ww as f32 / self.scale, wh as f32 / self.scale);
        self.grid = fit_grid(self.entries.len(), area, self.thumbnails);
        self.paint();
        let Some(frame) = &self.frame else {
            return;
        };
        let slots = thumb_slots(frame, self.entries.len());
        let (w, h) = self.size;
        // SAFETY: positions and shows our own window without activating it.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                work.left + (ww - w) / 2,
                work.top + (wh - h).max(0) / 2,
                w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        if self.thumbnails {
            self.thumbs = self
                .windows
                .iter()
                .zip(&slots)
                .filter_map(|(&src, &slot)| self.thumbnail(src, slot))
                .collect();
        }
    }

    fn thumbnail(&self, source: HWND, slot: Rect) -> Option<isize> {
        // SAFETY: registers a thumbnail of `source` onto our window; unregistered in `close`.
        unsafe {
            let id = DwmRegisterThumbnail(self.hwnd, source).ok()?;
            let size = DwmQueryThumbnailSourceSize(id).unwrap_or_default();
            let s = self.scale;
            let r = fit((size.cx as f32 / s, size.cy as f32 / s), slot);
            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_SOURCECLIENTAREAONLY,
                rcDestination: RECT {
                    left: (r.x * s).round() as i32,
                    top: (r.y * s).round() as i32,
                    right: ((r.x + r.w) * s).round() as i32,
                    bottom: ((r.y + r.h) * s).round() as i32,
                },
                fVisible: true.into(),
                fSourceClientAreaOnly: false.into(),
                ..Default::default()
            };
            let _ = DwmUpdateThumbnailProperties(id, &props);
            Some(id)
        }
    }

    /// Lays the cards out again with the current selection and repaints.
    fn paint(&mut self) {
        let root = view(&self.entries, self.selected, self.grid, &self.theme);
        let frame = layout(
            root,
            None,
            None,
            &mut self.text,
            self.theme.font_size(),
            &HashMap::new(),
        );
        let s = self.scale;
        let (w, h) = (
            (frame.size.0 * s).ceil().max(1.0) as u32,
            (frame.size.1 * s).ceil().max(1.0) as u32,
        );
        let Some(mut pixmap) = Pixmap::new(w, h) else {
            return;
        };
        pixmap.fill(self.theme.background.to_skia());
        render_frame(
            &frame,
            &mut pixmap,
            s,
            (0.0, 0.0),
            &mut self.text,
            &self.theme,
            None,
            1.0,
        );
        let bgrx: Vec<u8> = pixmap
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|&[r, g, b, _]| [b, g, r, 255])
            .collect();
        PIXELS.with(|p| *p.borrow_mut() = (bgrx, w as i32, h as i32));
        self.size = (w as i32, h as i32);
        self.frame = Some(frame);
        // SAFETY: asks for a repaint of our own window.
        let _ = unsafe { InvalidateRect(Some(self.hwnd), None, false) };
    }

    pub fn move_by(&mut self, by: i32) {
        if self.is_open() {
            self.selected = switcher::step(self.selected, self.entries.len(), by);
            self.paint();
        }
    }

    /// The card under a click at physical client coordinates.
    pub fn pick(&mut self, x: i32, y: i32) -> bool {
        let s = self.scale;
        let hit = self
            .frame
            .as_ref()
            .and_then(|f| f.hit(x as f32 / s, y as f32 / s))
            .map(|p| p.index());
        if let Some(i) = hit {
            self.selected = i;
        }
        hit.is_some()
    }

    /// Hides the switcher; the selected window, when it was open.
    pub fn close(&mut self) -> Option<HWND> {
        for id in self.thumbs.drain(..) {
            // SAFETY: an id registered in `open`, unregistered once.
            let _ = unsafe { DwmUnregisterThumbnail(id) };
        }
        self.frame.take()?;
        // SAFETY: hides our own window.
        let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        self.entries.clear();
        let picked = self.windows.get(self.selected).copied();
        self.windows.clear();
        picked
    }
}

/// The windows the system Alt+Tab would list, most recent first.
fn switchable() -> Vec<(HWND, Entry)> {
    let own = std::process::id();
    top_level_windows()
        .into_iter()
        .filter_map(|hwnd| {
            // SAFETY: read-only queries on window handles; a stale handle just fails.
            unsafe {
                let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
                let mut cloaked = 0u32;
                let _ =
                    DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut c_void, 4);
                let mut pid = 0u32;
                GetWindowThreadProcessId(hwnd, Some(&mut pid));
                let mut buf = [0u16; 512];
                let n = GetWindowTextW(hwnd, &mut buf);
                let title = String::from_utf16_lossy(&buf[..n.clamp(0, 512) as usize]);
                let visible = |w: HWND| IsWindowVisible(w).as_bool();
                let candidate = Candidate {
                    visible: visible(hwnd),
                    cloaked: cloaked != 0,
                    tool_window: ex & WS_EX_TOOLWINDOW.0 != 0,
                    app_window: ex & WS_EX_APPWINDOW.0 != 0,
                    no_activate: ex & WS_EX_NOACTIVATE.0 != 0,
                    shown_for_owner: owner_walk(
                        hwnd,
                        |w| GetAncestor(w, GA_ROOTOWNER),
                        |w| GetLastActivePopup(w),
                        visible,
                    ),
                    own_process: pid == own,
                    title,
                };
                listed(&candidate).then(|| {
                    (
                        hwnd,
                        Entry {
                            title: candidate.title,
                            exe: exe_name(pid),
                        },
                    )
                })
            }
        })
        .collect()
}

/// Brings `hwnd` to the front, restoring it when minimized. Windows only lets the process
/// with the last input take the foreground, so when the plain call is refused, the input
/// queue of the current foreground thread is borrowed for the moment of the switch.
pub(super) fn activate(hwnd: HWND) {
    // SAFETY: plain window calls; the thread input attachment is undone before returning.
    unsafe {
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            return;
        }
        let me = GetCurrentThreadId();
        let theirs = GetWindowThreadProcessId(GetForegroundWindow(), None);
        let attached = theirs != 0 && theirs != me && AttachThreadInput(me, theirs, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(me, theirs, false);
        }
    }
}

fn mask_alt_release() {
    let key = |flags| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(VK_UNASSIGNED),
                dwFlags: flags,
                ..Default::default()
            },
        },
    };
    let inputs = [key(Default::default()), key(KEYEVENTF_KEYUP)];
    // SAFETY: two well-formed keyboard inputs.
    unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
}
