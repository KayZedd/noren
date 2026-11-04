//! Win32 side of the shell runtime: finds top-level windows, applies [`Style`]s with
//! `DwmSetWindowAttribute`, and undoes everything on exit.
//!
//! New windows and title changes arrive through an out-of-context `SetWinEventHook`; state
//! files are polled twice a second. The hook callback only queues the window and wakes the
//! loop, so no Win32 call made while applying a style can re-enter the runtime.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{COLORREF, CloseHandle, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmGetWindowAttribute, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromWindow,
};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EVENT_OBJECT_DESTROY, EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_SHOW, EnumWindows, GA_ROOT,
    GWL_EXSTYLE, GetAncestor, GetClassNameW, GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowTextW,
    GetWindowThreadProcessId, IsWindow, IsWindowVisible, IsZoomed, LWA_ALPHA, MSG, PostThreadMessageW,
    SW_MAXIMIZE, SW_RESTORE, SWP_NOACTIVATE, SWP_NOZORDER, SetLayeredWindowAttributes, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP,
    WM_QUIT, WM_TIMER, WS_EX_LAYERED, WS_EX_TOOLWINDOW,
};
use windows::core::{BOOL, PWSTR};

use super::rules::{Op, Rules, ScreenRect, Style, WindowInfo, monitor_order, ops, relocate};
use super::state::{self, State};
use crate::palette::Palette;

const DWMWA_USE_IMMERSIVE_DARK_MODE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(20);
const DWMWA_WINDOW_CORNER_PREFERENCE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(33);
const DWMWA_BORDER_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(34);
const DWMWA_CAPTION_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(35);
const DWMWA_TEXT_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(36);
const DWMWA_SYSTEMBACKDROP_TYPE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(38);

/// Shell surfaces noren must never restyle or move.
const SKIP_CLASSES: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

const WAKE: u32 = WM_APP + 1;
const STATE_POLL_MS: u32 = 500;

static LOOP_THREAD: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static QUEUE: RefCell<Vec<(u32, isize)>> = const { RefCell::new(Vec::new()) };
}

/// What noren changed on one window, and what to put back.
#[derive(Default)]
struct Applied {
    style: Style,
    original_dark: Option<bool>,
    /// We added `WS_EX_LAYERED` for opacity and must remove it again.
    added_layered: bool,
}

struct Runtime {
    rules: Rules,
    palette: Palette,
    state_dir: Option<PathBuf>,
    states: BTreeMap<String, State>,
    applied: HashMap<isize, Applied>,
    exe: HashMap<isize, String>,
    moved: HashSet<isize>,
    own_pid: u32,
}

/// Applies `rules` to every window until Ctrl+C or the console closes, then restores every
/// window it touched.
pub fn run(rules: Rules, palette: Palette, state_dir: Option<PathBuf>) -> io::Result<()> {
    // SAFETY: plain Win32 calls; the hook and handler only touch thread-safe statics or the
    // thread-local queue of the thread that installed them.
    unsafe {
        LOOP_THREAD.store(GetCurrentThreadId(), Ordering::SeqCst);
        SetConsoleCtrlHandler(Some(on_ctrl), true).map_err(io::Error::other)?;
        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;
        let show = SetWinEventHook(
            EVENT_OBJECT_SHOW,
            EVENT_OBJECT_SHOW,
            None,
            Some(on_event),
            0,
            0,
            flags,
        );
        let name = SetWinEventHook(
            EVENT_OBJECT_NAMECHANGE,
            EVENT_OBJECT_NAMECHANGE,
            None,
            Some(on_event),
            0,
            0,
            flags,
        );
        let destroy = SetWinEventHook(
            EVENT_OBJECT_DESTROY,
            EVENT_OBJECT_DESTROY,
            None,
            Some(on_event),
            0,
            0,
            flags,
        );
        let watch_states = rules.uses_states() && state_dir.is_some();
        let mut rt = Runtime {
            rules,
            palette,
            state_dir,
            states: BTreeMap::new(),
            applied: HashMap::new(),
            exe: HashMap::new(),
            moved: HashSet::new(),
            own_pid: GetCurrentProcessId(),
        };
        if watch_states {
            rt.states = rt.read_states();
            SetTimer(None, 0, STATE_POLL_MS, None);
        }
        rt.refresh_all();
        eprintln!(
            "noren: {} windows styled, Ctrl+C restores them and exits",
            rt.applied.len()
        );

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match msg.message {
                WM_TIMER if watch_states => {
                    let now = rt.read_states();
                    if now != rt.states {
                        rt.states = now;
                        rt.refresh_all();
                    }
                }
                WAKE => {}
                _ => {
                    DispatchMessageW(&msg);
                }
            }
            for (event, hwnd) in QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut())) {
                if event == EVENT_OBJECT_DESTROY {
                    rt.forget(hwnd);
                } else {
                    rt.refresh(HWND(hwnd as *mut c_void));
                }
            }
        }

        for hook in [show, name, destroy] {
            let _ = UnhookWinEvent(hook);
        }
        rt.restore_all();
    }
    Ok(())
}

unsafe extern "system" fn on_ctrl(_: u32) -> BOOL {
    // SAFETY: posting to our own loop thread; WM_QUIT ends GetMessageW, restore runs after.
    let _ = unsafe { PostThreadMessageW(LOOP_THREAD.load(Ordering::SeqCst), WM_QUIT, WPARAM(0), LPARAM(0)) };
    // Give the loop time to restore before Windows ends the process on console close.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    true.into()
}

unsafe extern "system" fn on_event(
    _: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _: u32,
    _: u32,
) {
    // OBJID_WINDOW and CHILDID_SELF: the window itself, not a control inside it.
    if hwnd.is_invalid() || id_object != 0 || id_child != 0 {
        return;
    }
    QUEUE.with(|q| q.borrow_mut().push((event, hwnd.0 as isize)));
    // SAFETY: wakes GetMessageW so the queue is drained now, not at the next timer tick.
    let _ = unsafe { PostThreadMessageW(LOOP_THREAD.load(Ordering::SeqCst), WAKE, WPARAM(0), LPARAM(0)) };
}

unsafe extern "system" fn collect_window(hwnd: HWND, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the Vec passed by `top_level_windows`, alive for the whole enumeration.
    let out = unsafe { &mut *(data.0 as *mut Vec<HWND>) };
    out.push(hwnd);
    true.into()
}

fn top_level_windows() -> Vec<HWND> {
    let mut out: Vec<HWND> = Vec::new();
    // SAFETY: synchronous enumeration; the callback only borrows `out`.
    unsafe {
        let _ = EnumWindows(Some(collect_window), LPARAM(&mut out as *mut _ as isize));
    }
    out
}

fn wide_to_string(buf: &[u16], len: i32) -> String {
    String::from_utf16_lossy(&buf[..len.clamp(0, buf.len() as i32) as usize])
}

fn exe_name(pid: u32) -> String {
    // SAFETY: handle opened with the least access that allows the query, closed below.
    unsafe {
        let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return String::new();
        };
        let mut buf = [0u16; 1024];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len);
        let _ = CloseHandle(process);
        if ok.is_err() {
            return String::new();
        }
        let path = String::from_utf16_lossy(&buf[..len as usize]);
        path.rsplit(['\\', '/']).next().unwrap_or_default().to_string()
    }
}

fn set_u32(hwnd: HWND, attr: DWMWINDOWATTRIBUTE, value: u32) {
    // SAFETY: a 4-byte value for a 4-byte attribute. Failures (old Windows, a window that
    // refuses) are ignored: the rule simply has no effect there.
    let _ = unsafe { DwmSetWindowAttribute(hwnd, attr, (&raw const value).cast(), size_of::<u32>() as u32) };
}

fn get_dark(hwnd: HWND) -> Option<bool> {
    let mut v = BOOL(0);
    // SAFETY: reads a BOOL-sized attribute into a BOOL.
    unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw mut v).cast(),
            size_of::<BOOL>() as u32,
        )
    }
    .ok()
    .map(|()| v.as_bool())
}

fn screen_rect(r: RECT) -> ScreenRect {
    ScreenRect {
        left: r.left,
        top: r.top,
        right: r.right,
        bottom: r.bottom,
    }
}

unsafe extern "system" fn collect_monitor(monitor: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the Vec passed by `monitors`.
    let out = unsafe { &mut *(data.0 as *mut Vec<(HMONITOR, ScreenRect, bool)>) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: OS-provided handle and a correctly sized struct.
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        out.push((monitor, screen_rect(info.rcWork), info.dwFlags & 1 != 0));
    }
    true.into()
}

/// Work areas in `monitor = N` order.
fn monitors() -> Vec<(HMONITOR, ScreenRect)> {
    let mut all: Vec<(HMONITOR, ScreenRect, bool)> = Vec::new();
    // SAFETY: synchronous enumeration; the callback only borrows `all`.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut all as *mut _ as isize),
        );
    }
    let keyed: Vec<(ScreenRect, bool)> = all.iter().map(|&(_, r, p)| (r, p)).collect();
    monitor_order(&keyed)
        .into_iter()
        .map(|i| (all[i].0, all[i].1))
        .collect()
}

impl Runtime {
    fn read_states(&self) -> BTreeMap<String, State> {
        self.state_dir
            .as_deref()
            .and_then(|d| state::read_all(d).ok())
            .unwrap_or_default()
    }

    /// `None` for windows rules never apply to: invisible, tool windows, noren's own, the
    /// desktop and taskbar.
    fn info(&mut self, hwnd: HWND) -> Option<WindowInfo> {
        // SAFETY: read-only queries on a window handle; a stale handle just fails.
        unsafe {
            if !IsWindowVisible(hwnd).as_bool() || GetAncestor(hwnd, GA_ROOT) != hwnd {
                return None;
            }
            if GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32 & WS_EX_TOOLWINDOW.0 != 0 {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == self.own_pid {
                return None;
            }
            let mut buf = [0u16; 256];
            let n = GetClassNameW(hwnd, &mut buf);
            let class = wide_to_string(&buf, n);
            if SKIP_CLASSES.contains(&class.as_str()) {
                return None;
            }
            let mut tbuf = [0u16; 512];
            let n = GetWindowTextW(hwnd, &mut tbuf);
            let title = wide_to_string(&tbuf, n);
            let exe = self
                .exe
                .entry(hwnd.0 as isize)
                .or_insert_with(|| exe_name(pid))
                .clone();
            Some(WindowInfo { exe, class, title })
        }
    }

    fn refresh_all(&mut self) {
        for hwnd in top_level_windows() {
            self.refresh(hwnd);
        }
        let dead: Vec<isize> = self
            .applied
            .keys()
            .copied()
            // SAFETY: IsWindow accepts any value.
            .filter(|&h| !unsafe { IsWindow(Some(HWND(h as *mut c_void))) }.as_bool())
            .collect();
        for h in dead {
            self.forget(h);
        }
    }

    fn forget(&mut self, hwnd: isize) {
        self.applied.remove(&hwnd);
        self.exe.remove(&hwnd);
        self.moved.remove(&hwnd);
    }

    fn refresh(&mut self, hwnd: HWND) {
        let Some(info) = self.info(hwnd) else {
            return;
        };
        let style = self.rules.resolve(&info, &self.palette, &self.states);
        let key = hwnd.0 as isize;
        if style == Style::default() && !self.applied.contains_key(&key) {
            return;
        }
        if let Some(n) = style.monitor
            && self.moved.insert(key)
        {
            move_to_monitor(hwnd, n);
        }
        let mut applied = self.applied.remove(&key).unwrap_or_default();
        apply(hwnd, &mut applied, &style);
        if style != Style::default() {
            self.applied.insert(key, applied);
        }
    }

    fn restore_all(&mut self) {
        for (hwnd, mut applied) in std::mem::take(&mut self.applied) {
            apply(HWND(hwnd as *mut c_void), &mut applied, &Style::default());
        }
    }
}

fn apply(hwnd: HWND, applied: &mut Applied, next: &Style) {
    for op in ops(&applied.style, next) {
        match op {
            Op::Corners(v) => set_u32(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, v),
            Op::Backdrop(v) => set_u32(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, v),
            Op::Border(v) => set_u32(hwnd, DWMWA_BORDER_COLOR, v),
            Op::Caption(v) => set_u32(hwnd, DWMWA_CAPTION_COLOR, v),
            Op::TitleText(v) => set_u32(hwnd, DWMWA_TEXT_COLOR, v),
            Op::Dark(v) => {
                if applied.style.dark.is_none() {
                    applied.original_dark = get_dark(hwnd);
                }
                let dark = v.or(applied.original_dark).unwrap_or(false);
                set_u32(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, u32::from(dark));
            }
            Op::Opacity(v) => set_opacity(hwnd, applied, v),
        }
    }
    applied.style = *next;
}

/// Windows that are already layered draw their own alpha (`UpdateLayeredWindow`); those are
/// left alone. Others get `WS_EX_LAYERED` for as long as a rule sets opacity.
fn set_opacity(hwnd: HWND, applied: &mut Applied, alpha: Option<u8>) {
    // SAFETY: style bits and layered attributes on a foreign top-level window, both
    // documented as settable from another process.
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        match alpha {
            Some(a) => {
                if ex as u32 & WS_EX_LAYERED.0 == 0 {
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_LAYERED.0 as isize);
                    applied.added_layered = true;
                }
                if applied.added_layered {
                    let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), a, LWA_ALPHA);
                }
            }
            None => {
                if std::mem::take(&mut applied.added_layered) {
                    SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex & !(WS_EX_LAYERED.0 as isize));
                }
            }
        }
    }
}

fn move_to_monitor(hwnd: HWND, n: u32) {
    let all = monitors();
    let Some(&(target, to)) = all.get(n as usize - 1) else {
        return;
    };
    // SAFETY: position queries and a move of a foreign top-level window.
    unsafe {
        let current = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if current == target {
            return;
        }
        let Some(&(_, from)) = all.iter().find(|(m, _)| *m == current) else {
            return;
        };
        let zoomed = IsZoomed(hwnd).as_bool();
        if zoomed {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
        let mut r = RECT::default();
        if GetWindowRect(hwnd, &mut r).is_err() {
            return;
        }
        let dest = relocate(screen_rect(r), from, to);
        let _ = SetWindowPos(
            hwnd,
            None,
            dest.left,
            dest.top,
            dest.width(),
            dest.height(),
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        if zoomed {
            let _ = ShowWindow(hwnd, SW_MAXIMIZE);
        }
    }
}

/// Every window rules could apply to, for `noren windows`.
pub fn list() -> Vec<WindowInfo> {
    let mut rt = Runtime {
        rules: Rules::new(&[]).expect("no rules are valid"),
        palette: Palette::generate(&crate::palette::PaletteSpec::new(crate::Color::BLACK)),
        state_dir: None,
        states: BTreeMap::new(),
        applied: HashMap::new(),
        exe: HashMap::new(),
        moved: HashSet::new(),
        // SAFETY: no preconditions.
        own_pid: unsafe { GetCurrentProcessId() },
    };
    top_level_windows()
        .into_iter()
        .filter_map(|h| rt.info(h))
        .collect()
}
