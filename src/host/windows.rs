//! Native Win32 host: one layered (per-pixel alpha) window per surface, single thread.
//!
//! Transparent pixels pass clicks to whatever is below, so a panel window can cover its popup
//! area without blocking the desktop. Frames go straight from tiny-skia into a DIB section and
//! `UpdateLayeredWindow`; there is no GPU context and no redirection surface.

use std::cell::RefCell;
use std::ffi::c_void;
use std::mem::size_of;
use std::time::{Duration, Instant};

use tiny_skia::Pixmap;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, ClientToScreen,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, EnumDisplayMonitors,
    GetMonitorInfoW, HBITMAP, HDC, HGDIOBJ, HMONITOR, MONITORINFOEXW, SelectObject,
};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForMonitor, MDT_EFFECTIVE_DPI,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_ESCAPE, VK_LBUTTON, VK_RBUTTON};
use windows::Win32::UI::Shell::{
    ABE_TOP, ABM_GETAUTOHIDEBAREX, ABM_NEW, ABM_REMOVE, ABM_SETAUTOHIDEBAREX, APPBARDATA, SHAppBarMessage,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CS_HREDRAW, CS_VREDRAW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
    DestroyWindow, DispatchMessageW, GW_HWNDPREV, GWL_EXSTYLE, GWL_STYLE, GWLP_USERDATA, GetClassNameW,
    GetCursorPos, GetForegroundWindow, GetWindow, GetWindowLongPtrW, GetWindowRect, HWND_BOTTOM,
    HWND_NOTOPMOST, HWND_TOP, HWND_TOPMOST, IDC_ARROW, LoadCursorW, MA_NOACTIVATE, MB_ICONERROR, MB_OK,
    MF_STRING, MONITORINFOF_PRIMARY, MSG, MessageBoxW, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW,
    QS_ALLINPUT, RegisterClassW, SW_HIDE, SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
    SWP_NOZORDER, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, ULW_ALPHA, UpdateLayeredWindow, WINDOW_EX_STYLE,
    WINDOWPOS, WM_APP, WM_CLOSE, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_MOUSEACTIVATE, WM_MOUSEWHEEL, WM_RBUTTONUP, WM_SETTINGCHANGE, WM_WINDOWPOSCHANGING, WNDCLASSW,
    WS_CAPTION, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
    WS_POPUP, WindowFromPoint,
};
use windows::core::{BOOL, PCWSTR, w};

use super::{Display, Entry, SurfaceOptions, display_at, rgba_to_bgra, select_display, window_to_logical};
use crate::Rect;
use crate::glue::Glue;
use crate::shell::{find_window, shown_frame};
use crate::surface::{Layer, Placement, Response, Surface};

const CLASS: PCWSTR = w!("noren.surface");
const APPBAR_MESSAGE: u32 = WM_APP + 41;
const MENU_QUIT: usize = 1;
/// One wheel notch scrolls this many logical pixels.
const WHEEL_STEP: f32 = 48.0;
const FRAME: Duration = Duration::from_millis(16);
const IDLE: Duration = Duration::from_millis(50);
const MONITOR_CHECK: Duration = Duration::from_secs(1);
/// How often a glued widget looks for its window while it has none on screen.
const GLUE_SEARCH: Duration = Duration::from_millis(500);
/// `GWLP_USERDATA` of a glued widget's window: never activated, z-order kept by the host.
const GLUED: isize = 4;

/// Input collected by the window procedure, handled by the main loop outside the callback.
enum Event {
    /// Left button down at a physical screen point.
    Click(HWND, i32, i32),
    /// Wheel notches (positive = away from the user) at a physical screen point.
    Wheel(HWND, i32, i32, f32),
    Escape(HWND),
    Menu(HWND),
    Close,
    Displays,
}

thread_local! {
    static EVENTS: RefCell<Vec<Event>> = const { RefCell::new(Vec::new()) };
}

fn push(event: Event) {
    EVENTS.with(|e| e.borrow_mut().push(event));
}

pub fn run(entries: Vec<Entry>) -> Result<(), String> {
    // Process settings that must precede window creation; repeated calls are harmless.
    unsafe {
        let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    register_class()?;
    let displays = monitors()?;
    let mut hosts = entries
        .into_iter()
        .map(|entry| Host::new(entry, &displays))
        .collect::<Result<Vec<_>, _>>()?;
    let result = event_loop(&mut hosts, displays);
    drop(hosts);
    if let Err(e) = &result {
        report_error(e);
    }
    result
}

fn event_loop(hosts: &mut [Host], mut displays: Vec<Display>) -> Result<(), String> {
    let mut last_tick = Instant::now() - IDLE;
    let mut last_monitor_check = Instant::now();
    let mut last_cursor = None;
    let mut buttons_down = false;
    loop {
        let now = Instant::now();
        let animating = hosts.iter().any(|h| h.surface.animating(now));
        let cadence = if animating || last_cursor.is_some_and(|c| c != cursor()) {
            FRAME
        } else {
            IDLE
        };
        let wait = cadence.saturating_sub(now - last_tick);
        // Blocks until input arrives or the next frame is due.
        unsafe { MsgWaitForMultipleObjects(None, false, wait.as_millis() as u32, QS_ALLINPUT) };
        let mut msg = MSG::default();
        // Standard non-blocking pump for all windows of this thread.
        while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        let now = Instant::now();
        let events = EVENTS.with(|e| std::mem::take(&mut *e.borrow_mut()));
        let mut quit = false;
        let mut displays_changed = false;
        for event in events {
            match event {
                Event::Click(hwnd, x, y) => {
                    for host in hosts.iter_mut().filter(|h| h.hwnd == hwnd) {
                        if let Some((lx, ly)) = host.to_logical(x, y) {
                            quit |= host.handle(|s| s.click(lx, ly, now));
                        }
                    }
                }
                Event::Wheel(hwnd, x, y, notches) => {
                    for host in hosts.iter_mut().filter(|h| h.hwnd == hwnd) {
                        if let Some((lx, ly)) = host.to_logical(x, y) {
                            quit |= host.handle(|s| s.scroll(lx, ly, -notches * WHEEL_STEP));
                        }
                    }
                }
                Event::Escape(hwnd) => {
                    for host in hosts.iter_mut().filter(|h| h.hwnd == hwnd) {
                        quit |= host.handle(|s| s.escape(now));
                    }
                }
                Event::Menu(hwnd) => quit |= context_menu(hwnd),
                Event::Close => quit = true,
                Event::Displays => displays_changed = true,
            }
        }

        if displays_changed || now - last_monitor_check >= MONITOR_CHECK {
            last_monitor_check = now;
            displays = monitors()?;
            for host in hosts.iter_mut() {
                host.check_display(&displays)?;
            }
        }
        for host in hosts.iter_mut() {
            host.follow(now, &displays);
        }

        // A button press that does not land on a surface's opaque pixels, or the surface losing
        // focus (Alt+Tab), counts as an outside click. WindowFromPoint skips transparent
        // pixels of layered windows, like real hit testing does.
        let down = unsafe {
            GetAsyncKeyState(i32::from(VK_LBUTTON.0)) < 0 || GetAsyncKeyState(i32::from(VK_RBUTTON.0)) < 0
        };
        let pressed = down && !buttons_down;
        buttons_down = down;
        let foreground = unsafe { GetForegroundWindow() };
        // Read-only hit test of the desktop.
        let under = cursor().map(|(x, y)| unsafe { WindowFromPoint(POINT { x, y }) });
        for host in hosts.iter_mut() {
            let is_foreground = foreground == host.hwnd;
            let lost_focus = host.was_foreground && !is_foreground;
            host.was_foreground = is_foreground;
            if lost_focus || (pressed && under != Some(host.hwnd)) {
                quit |= host.handle(|s| s.click_outside(now));
            }
        }

        let cursor = cursor();
        last_cursor = Some(cursor);
        let fullscreen = fullscreen_rect(hosts);
        if now - last_tick >= FRAME || quit {
            last_tick = now;
            for host in hosts.iter_mut() {
                let pointer = cursor.and_then(|(x, y)| host.to_logical(x, y));
                let covered = fullscreen.is_some_and(|r| host.display_covered_by(r));
                quit |= host.handle(|s| s.tick(now, pointer, covered));
                if host.surface.animating(now) {
                    host.dirty = true;
                }
            }
        }
        if quit {
            return Ok(());
        }
        for host in hosts.iter_mut().filter(|h| h.dirty) {
            host.present(now)?;
        }
    }
}

fn cursor() -> Option<(i32, i32)> {
    let mut point = POINT::default();
    // Reads global cursor state only; failure (secure desktop) means unknown.
    unsafe { GetCursorPos(&mut point) }
        .ok()
        .map(|()| (point.x, point.y))
}

/// Screen rect of a foreground fullscreen application (games, video), if any.
fn fullscreen_rect(hosts: &[Host]) -> Option<RECT> {
    // Foreground handles are only queried, never modified.
    unsafe {
        let fg = GetForegroundWindow();
        if fg.is_invalid() || hosts.iter().any(|h| h.hwnd == fg) {
            return None;
        }
        let mut name = [0u16; 128];
        let len = GetClassNameW(fg, &mut name) as usize;
        let name = String::from_utf16_lossy(&name[..len]);
        if matches!(
            name.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) {
            return None;
        }
        if GetWindowLongPtrW(fg, GWL_STYLE) as u32 & WS_CAPTION.0 != 0 {
            return None;
        }
        let mut rect = RECT::default();
        GetWindowRect(fg, &mut rect).ok()?;
        Some(rect)
    }
}

fn context_menu(hwnd: HWND) -> bool {
    let Some((x, y)) = cursor() else {
        return false;
    };
    // The menu lives only for this synchronous call; TrackPopupMenu needs the owner in front
    // so that clicking elsewhere dismisses it.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return false;
        };
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit"));
        let _ = SetForegroundWindow(hwnd);
        let chosen = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, x, y, None, hwnd, None);
        let _ = DestroyMenu(menu);
        chosen.0 as usize == MENU_QUIT
    }
}

fn report_error(error: &str) {
    eprintln!("noren: {error}");
    let message: Vec<u16> = error.encode_utf16().chain(Some(0)).collect();
    // Both strings outlive the synchronous dialog.
    unsafe {
        MessageBoxW(None, PCWSTR(message.as_ptr()), w!("noren"), MB_OK | MB_ICONERROR);
    }
}

fn register_class() -> Result<(), String> {
    // Registration copies the class data; a second registration in the same process fails
    // with "class already exists", which is fine.
    unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
        let class = WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(|e| e.to_string())?,
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassW(&class);
    }
    Ok(())
}

/// Layer is stored in GWLP_USERDATA so the window procedure can pin desktop windows.
fn layer_code(layer: Layer) -> isize {
    match layer {
        Layer::Desktop => 1,
        Layer::Normal => 2,
        Layer::Top => 3,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let code = || unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    let desktop = || code() == layer_code(Layer::Desktop);
    match msg {
        WM_LBUTTONDOWN => {
            let mut point = POINT {
                x: (lparam.0 & 0xffff) as i16 as i32,
                y: ((lparam.0 >> 16) & 0xffff) as i16 as i32,
            };
            // Client → screen for this live window.
            let _ = unsafe { ClientToScreen(hwnd, &mut point) };
            push(Event::Click(hwnd, point.x, point.y));
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let delta = ((wparam.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
            let x = (lparam.0 & 0xffff) as i16 as i32;
            let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
            push(Event::Wheel(hwnd, x, y, delta));
            LRESULT(0)
        }
        WM_KEYDOWN if wparam.0 == usize::from(VK_ESCAPE.0) => {
            push(Event::Escape(hwnd));
            LRESULT(0)
        }
        WM_RBUTTONUP => {
            push(Event::Menu(hwnd));
            LRESULT(0)
        }
        WM_CLOSE => {
            push(Event::Close);
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_DPICHANGED | WM_SETTINGCHANGE => {
            push(Event::Displays);
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        WM_MOUSEACTIVATE if desktop() || code() == GLUED => LRESULT(MA_NOACTIVATE as isize),
        WM_WINDOWPOSCHANGING if desktop() => {
            // Keep desktop widgets at the bottom of the z-order whenever anything restacks them.
            let pos = lparam.0 as *mut WINDOWPOS;
            if let Some(pos) = unsafe { pos.as_mut() } {
                pos.hwndInsertAfter = HWND_BOTTOM;
                pos.flags &= !SWP_NOZORDER;
            }
            LRESULT(0)
        }
        APPBAR_MESSAGE => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

pub(super) fn monitors() -> Result<Vec<Display>, String> {
    let mut displays: Vec<Display> = Vec::new();
    // The callback runs synchronously and only borrows this vector.
    unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut displays as *mut _ as isize),
        )
        .ok()
        .map_err(|e| e.to_string())?;
    }
    if displays.is_empty() {
        return Err("Windows reported no active displays.".into());
    }
    Ok(displays)
}

unsafe extern "system" fn collect_monitor(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // OS-provided handle and a correctly sized struct.
    if !unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
        return true.into();
    }
    let rect = info.monitorInfo.rcMonitor;
    let (mut dpi_x, mut dpi_y) = (96, 96);
    // Missing DPI falls back to 100 %.
    let _ = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    let len = info
        .szDevice
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(info.szDevice.len());
    let display = Display {
        name: String::from_utf16_lossy(&info.szDevice[..len]),
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        left: rect.left,
        top: rect.top,
        width: rect.right.saturating_sub(rect.left).max(1) as u32,
        height: rect.bottom.saturating_sub(rect.top).max(1) as u32,
        scale: dpi_x as f32 / 96.0,
    };
    // data is the vector borrowed by monitors() for the duration of the enumeration.
    unsafe { &mut *(data.0 as *mut Vec<Display>) }.push(display);
    true.into()
}

fn display_rect(d: &Display) -> RECT {
    RECT {
        left: d.left,
        top: d.top,
        right: d.left.saturating_add(d.width as i32),
        bottom: d.top.saturating_add(d.height as i32),
    }
}

/// One surface and its window.
struct Host {
    surface: Box<dyn Surface>,
    options: SurfaceOptions,
    hwnd: HWND,
    display: Display,
    dib: Option<Dib>,
    pixmap: Option<Pixmap>,
    applied: Option<(Layer, bool)>,
    shown: bool,
    dirty: bool,
    was_foreground: bool,
    appbar: Option<APPBARDATA>,
    follow: Option<Follow>,
}

/// A glued widget's window and where it last was.
struct Follow {
    glue: Glue,
    target: Option<HWND>,
    searched: Option<Instant>,
    /// The target's frame while it is on screen.
    frame: Option<crate::shell::rules::ScreenRect>,
    /// Where the widget was drawn: physical origin and size, and its logical rect.
    drawn: Option<Drawn>,
}

/// Physical origin and size of a glued widget's window, and the logical rect drawn into it.
type Drawn = ((i32, i32), (u32, u32), Rect);

impl Host {
    fn new(entry: Entry, displays: &[Display]) -> Result<Self, String> {
        let display = select_display(displays.to_vec(), &entry.options.monitor)
            .ok_or("Windows reported no active displays.")?;
        let follow = match &entry.options.glue {
            Some(g) => Some(Follow {
                glue: Glue::new("glued widget", g)?,
                target: None,
                searched: None,
                frame: None,
                drawn: None,
            }),
            None => None,
        };
        // Class registered above; the window is owned by Host and destroyed in Drop.
        let hwnd = unsafe {
            let instance = GetModuleHandleW(None).map_err(|e| e.to_string())?;
            CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOOLWINDOW,
                CLASS,
                w!("noren"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .map_err(|e| e.to_string())?
        };
        let mut host = Self {
            surface: entry.surface,
            options: entry.options,
            hwnd,
            display,
            dib: None,
            pixmap: None,
            applied: None,
            shown: false,
            dirty: true,
            was_foreground: false,
            appbar: None,
            follow,
        };
        host.surface.set_monitor(host.display.info());
        if host.options.top_appbar {
            host.register_appbar()?;
        }
        Ok(host)
    }

    /// Runs one surface call; returns true when the surface asked to quit.
    fn handle(&mut self, call: impl FnOnce(&mut dyn Surface) -> Response) -> bool {
        let response = call(self.surface.as_mut());
        self.dirty |= response.redraw;
        response.quit
    }

    fn display_covered_by(&self, r: RECT) -> bool {
        let m = display_rect(&self.display);
        r.left <= m.left && r.top <= m.top && r.right >= m.right && r.bottom >= m.bottom
    }

    /// Screen pixel → logical surface coordinate, `None` outside the surface's window area.
    fn to_logical(&self, x: i32, y: i32) -> Option<(f32, f32)> {
        match &self.follow {
            Some(f) => {
                let (origin, size, rect) = f.drawn?;
                window_to_logical(origin, size, rect, self.display.scale, (x, y))
            }
            None => self.display.to_logical(x, y),
        }
    }

    /// A glued widget keeps up with its window: finds it, moves to its monitor, and redraws
    /// when it moved, showed or hid.
    fn follow(&mut self, now: Instant, displays: &[Display]) {
        let Some(f) = self.follow.as_mut() else {
            return;
        };
        let mut frame = f.target.and_then(shown_frame);
        if frame.is_none() && f.searched.is_none_or(|t| now - t >= GLUE_SEARCH) {
            f.searched = Some(now);
            if let Some(found) = find_window(|w| f.glue.matches(w)) {
                f.target = Some(found);
                frame = shown_frame(found);
            }
        }
        let moved = frame != f.frame;
        f.frame = frame;
        let target = f.target;
        self.dirty |= moved;
        let Some(r) = frame else {
            return;
        };
        if let Some(d) = display_at(displays, (r.left, r.top, r.right, r.bottom))
            && *d != self.display
        {
            self.display = d.clone();
            self.surface.set_monitor(self.display.info());
            self.dirty = true;
        }
        // Just above the target, so what covers the window covers the widget too.
        if let Some(target) = target {
            // SAFETY: read-only z-order query; a stale handle just fails.
            let above = unsafe { GetWindow(target, GW_HWNDPREV) }.ok();
            if above != Some(self.hwnd) && self.shown {
                // SAFETY: restacks our own window without moving or activating it.
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        Some(above.unwrap_or(HWND_TOP)),
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            }
        }
    }

    fn check_display(&mut self, displays: &[Display]) -> Result<(), String> {
        if self.follow.is_some() {
            // A glued widget's monitor is its window's (see `follow`).
            return Ok(());
        }
        let next = select_display(displays.to_vec(), &self.options.monitor)
            .ok_or("Windows reported no active displays.")?;
        let owner_lost = self.appbar.as_mut().is_some_and(|bar| {
            // Shell only reads the struct; recovers after Explorer restarts.
            unsafe { SHAppBarMessage(ABM_GETAUTOHIDEBAREX, bar) != self.hwnd.0 as usize }
        });
        if next != self.display || owner_lost {
            self.unregister_appbar();
            self.display = next;
            self.surface.set_monitor(self.display.info());
            self.dirty = true;
            if self.options.top_appbar {
                self.register_appbar()?;
            }
        }
        Ok(())
    }

    fn register_appbar(&mut self) -> Result<(), String> {
        let mut bar = APPBARDATA {
            cbSize: size_of::<APPBARDATA>() as u32,
            hWnd: self.hwnd,
            uCallbackMessage: APPBAR_MESSAGE,
            uEdge: ABE_TOP,
            rc: display_rect(&self.display),
            lParam: LPARAM(1),
        };
        // Shell copies the struct; the HWND stays owned by this host.
        unsafe {
            if SHAppBarMessage(ABM_NEW, &mut bar) == 0 {
                return Err("Windows refused to register the panel AppBar.".into());
            }
            if SHAppBarMessage(ABM_SETAUTOHIDEBAREX, &mut bar) == 0 {
                SHAppBarMessage(ABM_REMOVE, &mut bar);
                return Err(format!(
                    "The top auto-hide edge on {} is already taken by another AppBar.",
                    self.display.name
                ));
            }
        }
        self.appbar = Some(bar);
        Ok(())
    }

    fn unregister_appbar(&mut self) {
        if let Some(mut bar) = self.appbar.take() {
            bar.lParam = LPARAM(0);
            // Removes only this window's registration.
            unsafe {
                SHAppBarMessage(ABM_SETAUTOHIDEBAREX, &mut bar);
                SHAppBarMessage(ABM_REMOVE, &mut bar);
            }
        }
    }

    fn present(&mut self, now: Instant) -> Result<(), String> {
        self.dirty = false;
        let placement = self.surface.placement(now);
        let target = self.follow.as_ref().map(|f| f.frame);
        if !placement.visible || target == Some(None) {
            if self.shown {
                // Hiding our own window.
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
                self.shown = false;
            }
            return Ok(());
        }
        self.apply_style(&placement);
        let (mut x, mut y, w, h) = self.display.to_physical(placement.rect);
        if let (Some(f), Some(Some(frame))) = (self.follow.as_mut(), target) {
            (x, y) = f.glue.place(frame, (w, h), self.display.scale);
            f.drawn = Some(((x, y), (w, h), placement.rect));
        }
        if self
            .pixmap
            .as_ref()
            .is_none_or(|p| (p.width(), p.height()) != (w, h))
        {
            self.pixmap = Some(Pixmap::new(w, h).ok_or("frame size out of range")?);
            self.dib = Some(Dib::new(w, h)?);
        }
        let (Some(pixmap), Some(dib)) = (self.pixmap.as_mut(), self.dib.as_mut()) else {
            return Ok(());
        };
        self.surface.render(pixmap, now);
        rgba_to_bgra(pixmap.data(), dib.pixels());
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let dst = POINT { x, y };
        let size = SIZE {
            cx: w as i32,
            cy: h as i32,
        };
        let src = POINT::default();
        // The DIB is selected into dib.dc and matches `size`; all pointers outlive the call.
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
        Ok(())
    }

    fn apply_style(&mut self, placement: &Placement) {
        let state = (placement.layer, placement.click_through);
        if self.applied == Some(state) {
            return;
        }
        self.applied = Some(state);
        let glued = self.follow.is_some();
        let mut ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW;
        if glued {
            ex |= WS_EX_NOACTIVATE;
        }
        match placement.layer {
            Layer::Desktop => ex |= WS_EX_NOACTIVATE,
            Layer::Normal => {}
            Layer::Top => ex |= WS_EX_TOPMOST,
        }
        if placement.click_through {
            ex |= WS_EX_TRANSPARENT;
        }
        let after = match placement.layer {
            Layer::Desktop => HWND_BOTTOM,
            Layer::Normal => HWND_NOTOPMOST,
            Layer::Top => HWND_TOPMOST,
        };
        // Style and z-order changes on our own window; position comes from UpdateLayeredWindow.
        unsafe {
            let code = if glued { GLUED } else { layer_code(placement.layer) };
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, code);
            SetWindowLongPtrW(self.hwnd, GWL_EXSTYLE, WINDOW_EX_STYLE(ex.0).0 as isize);
            let _ = SetWindowPos(
                self.hwnd,
                Some(after),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
        }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.unregister_appbar();
        self.dib = None;
        // The window was created by this host and is destroyed exactly once.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

/// Top-down 32-bit DIB section selected into a memory DC.
pub(crate) struct Dib {
    pub(crate) dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u8,
    len: usize,
}

impl Dib {
    pub(crate) fn new(w: u32, h: u32) -> Result<Self, String> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w as i32,
                biHeight: -(h as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        // The DC and bitmap are owned by Dib and released in Drop, in reverse order.
        unsafe {
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() {
                return Err("CreateCompatibleDC failed".into());
            }
            let bitmap = match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
                Ok(b) if !bits.is_null() => b,
                _ => {
                    let _ = DeleteDC(dc);
                    return Err(format!("CreateDIBSection {w}x{h} failed"));
                }
            };
            let old = SelectObject(dc, bitmap.into());
            Ok(Self {
                dc,
                bitmap,
                old,
                bits: bits.cast(),
                len: w as usize * h as usize * 4,
            })
        }
    }

    pub(crate) fn pixels(&mut self) -> &mut [u8] {
        // The section holds exactly w*h*4 bytes, lives as long as self, and nothing else
        // writes to it while this borrow exists (GDI only reads it in UpdateLayeredWindow).
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.len) }
    }
}

impl Drop for Dib {
    fn drop(&mut self) {
        // Deselect before deleting; both handles are owned by this struct.
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}
