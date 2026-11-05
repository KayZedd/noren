//! Win32 side of `[[mirror]]`: one topmost popup per mirror that never takes the focus, with a
//! DWM thumbnail of its source window filling it. Where the thumbnail is translucent the
//! popup shows the palette background.

use std::cell::Cell;
use std::ffi::c_void;
use std::io;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_SOURCECLIENTAREAONLY,
    DWM_TNP_VISIBLE, DWMWINDOWATTRIBUTE, DwmRegisterThumbnail, DwmSetWindowAttribute, DwmUnregisterThumbnail,
    DwmUpdateThumbnailProperties,
};
use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, FillRect, HDC, InvalidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, HWND_TOPMOST, IDC_HAND, LoadCursorW,
    MA_NOACTIVATE, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowPos, ShowWindow, WM_ERASEBKGND,
    WM_MOUSEACTIVATE, WNDCLASSW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::w;

use super::rules::ScreenRect;
use super::win::register_class;

const DWMWA_WINDOW_CORNER_PREFERENCE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(33);
const DWMWCP_ROUND_SMALL: u32 = 3;

thread_local! {
    /// The palette background as a COLORREF, shown behind translucent thumbnails.
    static BACKGROUND: Cell<u32> = const { Cell::new(0) };
}

unsafe extern "system" fn mirror_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        // A click brings the source forward (see `win::hold`) without activating the mirror.
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => {
            // SAFETY: wparam is the window's device context for this message; the brush is
            // deleted before returning.
            unsafe {
                let mut r = RECT::default();
                let _ = GetClientRect(hwnd, &mut r);
                let brush = CreateSolidBrush(COLORREF(BACKGROUND.get()));
                FillRect(HDC(wparam.0 as *mut c_void), &r, brush);
                let _ = DeleteObject(brush.into());
            }
            LRESULT(1)
        }
        // SAFETY: default handling for a window we own.
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Where one mirror shows what.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Target {
    pub source: HWND,
    pub rect: ScreenRect,
    pub opacity: u8,
    pub client_only: bool,
}

struct Slot {
    hwnd: HWND,
    /// The registered thumbnail and its source.
    thumb: Option<(isize, HWND)>,
    shown: Option<ScreenRect>,
}

impl Slot {
    fn release(&mut self) {
        if let Some((id, _)) = self.thumb.take() {
            // SAFETY: an id registered by this slot, unregistered once.
            let _ = unsafe { DwmUnregisterThumbnail(id) };
        }
    }

    fn hide(&mut self) {
        self.release();
        if self.shown.take().is_some() {
            // SAFETY: hides our own window.
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
    }

    fn show(&mut self, t: Target) {
        if self.thumb.is_some_and(|(_, src)| src != t.source) {
            self.release();
        }
        if self.thumb.is_none() {
            // SAFETY: registers a thumbnail of `source` onto our window; released in `release`.
            match unsafe { DwmRegisterThumbnail(self.hwnd, t.source) } {
                Ok(id) => self.thumb = Some((id, t.source)),
                Err(_) => return self.hide(),
            }
        }
        let r = t.rect;
        if self.shown != Some(r) {
            // SAFETY: places our own window without activating it.
            let _ = unsafe {
                SetWindowPos(
                    self.hwnd,
                    Some(HWND_TOPMOST),
                    r.left,
                    r.top,
                    r.width(),
                    r.height(),
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                )
            };
            self.shown = Some(r);
        }
        let Some((id, _)) = self.thumb else {
            return;
        };
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION
                | DWM_TNP_VISIBLE
                | DWM_TNP_OPACITY
                | DWM_TNP_SOURCECLIENTAREAONLY,
            rcDestination: RECT {
                left: 0,
                top: 0,
                right: r.width(),
                bottom: r.height(),
            },
            opacity: t.opacity,
            fVisible: true.into(),
            fSourceClientAreaOnly: t.client_only.into(),
            ..Default::default()
        };
        // SAFETY: a live thumbnail id and a fully initialized struct.
        let _ = unsafe { DwmUpdateThumbnailProperties(id, &props) };
    }
}

/// Every mirror's popup.
pub(super) struct Mirrors {
    slots: Vec<Slot>,
}

impl Drop for Mirrors {
    fn drop(&mut self) {
        for slot in &mut self.slots {
            slot.release();
            // SAFETY: our own window, destroyed on the thread that created it.
            let _ = unsafe { DestroyWindow(slot.hwnd) };
        }
    }
}

impl Mirrors {
    pub fn new(count: usize, background: u32) -> io::Result<Self> {
        BACKGROUND.set(background);
        // SAFETY: registers a class and creates hidden windows owned by this thread.
        unsafe {
            let instance = GetModuleHandleW(None).map_err(io::Error::other)?;
            register_class(&WNDCLASSW {
                lpfnWndProc: Some(mirror_proc),
                hInstance: instance.into(),
                hCursor: LoadCursorW(None, IDC_HAND).unwrap_or_default(),
                lpszClassName: w!("noren.mirror"),
                ..Default::default()
            })?;
            let mut slots = Vec::with_capacity(count);
            for _ in 0..count {
                let hwnd = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    w!("noren.mirror"),
                    w!("noren mirror"),
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
                let round = DWMWCP_ROUND_SMALL;
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE,
                    &round as *const u32 as *const c_void,
                    4,
                );
                slots.push(Slot {
                    hwnd,
                    thumb: None,
                    shown: None,
                });
            }
            Ok(Self { slots })
        }
    }

    /// The palette changed (a schedule switch).
    pub fn set_background(&self, color: u32) {
        BACKGROUND.set(color);
        for slot in &self.slots {
            // SAFETY: asks for a repaint of our own window.
            let _ = unsafe { InvalidateRect(Some(slot.hwnd), None, true) };
        }
    }

    /// Shows each mirror at its target, or hides it. `targets` has one entry per mirror.
    pub fn update(&mut self, targets: &[Option<Target>]) {
        for (slot, target) in self.slots.iter_mut().zip(targets) {
            match target {
                Some(t) => slot.show(*t),
                None => slot.hide(),
            }
        }
    }

    /// The window shown by the mirror `hwnd`, when it is one of ours.
    pub fn source_of(&self, hwnd: HWND) -> Option<HWND> {
        self.slots
            .iter()
            .find(|s| s.hwnd == hwnd)
            .and_then(|s| s.thumb.map(|(_, src)| src))
    }
}
