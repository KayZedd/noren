//! Win32 side of the shell runtime: finds top-level windows, applies [`Style`]s with
//! `DwmSetWindowAttribute`, and undoes everything on exit.
//!
//! New windows, title changes and focus changes arrive through out-of-context
//! `SetWinEventHook`s, the x-ray key and command chords through a `WH_KEYBOARD_LL` hook; state
//! files are polled twice a second, the pointer every 50 ms when hot corners are set. Hook callbacks only queue an event and wake the loop, so no Win32 call made
//! while applying a style can re-enter the runtime.
//!
//! Focus dimming is one layered, click-through popup over the virtual screen, kept directly
//! behind the foreground window with `SetWindowPos(insert_after = foreground)`. Edge glows are
//! topmost popups of their own (see `glow_win`), redrawn every 33 ms only while they move.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use windows::Win32::Foundation::{COLORREF, CloseHandle, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWINDOWATTRIBUTE, DwmGetWindowAttribute, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DeleteObject, EnumDisplayMonitors, GetMonitorInfoW, HBRUSH, HDC, HMONITOR,
    MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint, MonitorFromWindow,
};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, OpenProcess, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EVENT_OBJECT_DESTROY,
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND,
    EVENT_SYSTEM_MINIMIZEEND, EVENT_SYSTEM_MINIMIZESTART, EnumWindows, GA_ROOT, GWL_EXSTYLE, GetAncestor,
    GetClassNameW, GetCursorPos, GetForegroundWindow, GetMessageW, GetSystemMetrics, GetWindowLongPtrW,
    GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, HC_ACTION, IsIconic, IsWindow, IsWindowVisible,
    IsZoomed, KBDLLHOOKSTRUCT, KillTimer, LWA_ALPHA, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW,
    RegisterClassW, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SW_HIDE,
    SW_MAXIMIZE, SW_RESTORE, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW, SetLayeredWindowAttributes,
    SetTimer, SetWindowLongPtrW, SetWindowPos, SetWindowsHookExW, ShowWindow, UnhookWindowsHookEx,
    WH_KEYBOARD_LL, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_APP, WM_KEYDOWN, WM_QUIT,
    WM_SYSKEYDOWN, WM_TIMER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    WS_POPUP,
};
use windows::core::{BOOL, PWSTR, w};

use super::Live;
use super::command::{self, Action, Chords, Commands, HotCorners};
use super::focus::{Focus, Xray, fullscreen};
use super::glow::Glows;
use super::glow_win::{Layers, Screen};
use super::keys::Hold;
use super::rules::{Op, Rules, ScreenRect, Style, WindowInfo, monitor_order, ops, relocate};
use super::schedule::{Local, Tracker};
use super::state::{self, State};
use crate::palette::{Mode, Palette};
use crate::rice::{self, Journal, Profile, SystemStore};

const DWMWA_USE_IMMERSIVE_DARK_MODE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(20);
const DWMWA_WINDOW_CORNER_PREFERENCE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(33);
const DWMWA_BORDER_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(34);
const DWMWA_CAPTION_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(35);
const DWMWA_TEXT_COLOR: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(36);
const DWMWA_SYSTEMBACKDROP_TYPE: DWMWINDOWATTRIBUTE = DWMWINDOWATTRIBUTE(38);

/// Applies `profile` to the system and journals it in the default journal; returns how many
/// values changed.
fn reapply(profile: &Profile) -> Result<usize, String> {
    let dir = Journal::default_dir().ok_or("no journal directory")?;
    let journal = Journal::open(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut store = SystemStore::default();
    let changes = profile.changes(&store).map_err(|e| e.to_string())?;
    let done = rice::apply(&mut store, &journal, &profile.name, &changes).map_err(|e| e.to_string())?;
    Ok(done.map_or(0, |t| t.entries.len()))
}

/// Shell surfaces noren must never restyle or move.
const SKIP_CLASSES: [&str; 4] = ["Progman", "WorkerW", "Shell_TrayWnd", "Shell_SecondaryTrayWnd"];

const WAKE: u32 = WM_APP + 1;
const STATE_POLL_MS: u32 = 500;
const POINTER_POLL_MS: u32 = 50;
/// Glow animation frames, about 30 a second, only while a glow moves.
const FRAME_MS: u32 = 33;

/// Queue entry from the keyboard hook: x-ray held (`1`) or released (`0`). Not a WinEvent id.
const XRAY: u32 = 0xFFFF_0001;
/// Queue entry from the keyboard hook: the command at this index finished its chord.
const CHORD: u32 = 0xFFFF_0002;

static LOOP_THREAD: AtomicU32 = AtomicU32::new(0);

thread_local! {
    static QUEUE: RefCell<Vec<(u32, isize)>> = const { RefCell::new(Vec::new()) };
    /// The x-ray key state, owned by the keyboard hook (which runs on the loop thread).
    static HOLD: RefCell<Option<Hold>> = const { RefCell::new(None) };
    /// The chord machine, also owned by the keyboard hook.
    static CHORDS: RefCell<Option<Chords>> = const { RefCell::new(None) };
}

fn wake() {
    // SAFETY: wakes GetMessageW so the queue is drained now, not at the next timer tick.
    let _ = unsafe { PostThreadMessageW(LOOP_THREAD.load(Ordering::SeqCst), WAKE, WPARAM(0), LPARAM(0)) };
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
    xray: Option<Xray>,
    xray_on: bool,
    focus: Option<(Focus, HWND)>,
    commands: Commands,
    corners: Option<HotCorners>,
    /// The profile as TOML, for the command palette.
    source: String,
    glows: Glows,
    layers: Option<Layers>,
    /// The glow frame timer, `0` while no glow moves.
    frame: usize,
    /// The dark/light schedule and the profile it re-applies.
    schedule: Option<(Tracker, Profile)>,
}

fn win_event_hook(event: u32) -> HWINEVENTHOOK {
    // SAFETY: out-of-context hook; `on_event` runs on this thread inside GetMessageW.
    unsafe {
        SetWinEventHook(
            event,
            event,
            None,
            Some(on_event),
            0,
            0,
            WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
        )
    }
}

/// Holds `live` until Ctrl+C or the console closes, then restores every window it touched.
pub fn run(live: Live, palette: Palette, state_dir: Option<PathBuf>) -> io::Result<()> {
    // SAFETY: the handler only posts to the loop thread.
    unsafe { SetConsoleCtrlHandler(Some(on_ctrl), true) }.map_err(io::Error::other)?;
    hold(live, palette, state_dir, |_| {})
}

/// Holds `live` on the calling thread until [`stop`] (or `WM_QUIT`) reaches it, then restores
/// every window it touched. `ready` gets the thread id once the thread has a message queue,
/// so a `stop` sent after it is never lost. One runtime per process.
pub fn hold(
    live: Live,
    palette: Palette,
    state_dir: Option<PathBuf>,
    ready: impl FnOnce(u32),
) -> io::Result<()> {
    let Live {
        rules,
        focus,
        xray,
        commands,
        glows,
        schedule,
        profile,
        source,
    } = live;
    // SAFETY: plain Win32 calls; the hook and handler only touch thread-safe statics or the
    // thread-local queue of the thread that installed them.
    unsafe {
        let tid = GetCurrentThreadId();
        LOOP_THREAD.store(tid, Ordering::SeqCst);
        // Any message call creates the thread's queue.
        let mut probe = MSG::default();
        let _ = PeekMessageW(&mut probe, None, 0, 0, PM_NOREMOVE);
        ready(tid);
        let mut hooks: Vec<HWINEVENTHOOK> =
            [EVENT_OBJECT_SHOW, EVENT_OBJECT_NAMECHANGE, EVENT_OBJECT_DESTROY]
                .into_iter()
                .map(win_event_hook)
                .collect();
        let overlay = match &focus {
            Some(f) => Some(create_overlay(f.colorref(&palette), f.alpha)?),
            None => None,
        };
        if overlay.is_some() {
            hooks.extend(
                [
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_MINIMIZESTART,
                    EVENT_SYSTEM_MINIMIZEEND,
                    EVENT_OBJECT_LOCATIONCHANGE,
                ]
                .into_iter()
                .map(win_event_hook),
            );
        } else if !glows.is_empty() {
            // Glows step aside for fullscreen apps, which arrive through the foreground.
            hooks.push(win_event_hook(EVENT_SYSTEM_FOREGROUND));
        }
        let layers = if glows.is_empty() {
            None
        } else {
            Some(Layers::new()?)
        };
        HOLD.with(|h| *h.borrow_mut() = xray.as_ref().map(|x| Hold::new(x.combo.clone())));
        let chords = commands.chords();
        let chording = chords.is_some();
        CHORDS.with(|c| *c.borrow_mut() = chords);
        let keyboard = if xray.is_some() || chording {
            let module = GetModuleHandleW(None).map_err(io::Error::other)?;
            Some(
                SetWindowsHookExW(WH_KEYBOARD_LL, Some(on_key), Some(module.into()), 0)
                    .map_err(io::Error::other)?,
            )
        } else {
            None
        };
        let watch_states = (rules.uses_states() || !glows.is_empty()) && state_dir.is_some();
        let mut rt = Runtime {
            rules,
            palette,
            state_dir,
            states: BTreeMap::new(),
            applied: HashMap::new(),
            exe: HashMap::new(),
            moved: HashSet::new(),
            own_pid: GetCurrentProcessId(),
            xray,
            xray_on: false,
            focus: focus.zip(overlay),
            corners: commands.hot_corners(),
            commands,
            source,
            glows: Glows::new(glows, Instant::now()),
            layers,
            frame: 0,
            schedule: schedule.map(|t| (t, profile)),
        };
        if watch_states {
            rt.states = rt.read_states();
        }
        rt.glows.update(&rt.states, Instant::now());
        rt.follow_schedule();
        let slow = if watch_states || rt.xray.is_some() || chording || rt.schedule.is_some() {
            SetTimer(None, 0, STATE_POLL_MS, None)
        } else {
            0
        };
        let pointer = if rt.corners.is_some() {
            SetTimer(None, 0, POINTER_POLL_MS, None)
        } else {
            0
        };
        rt.refresh_all();
        rt.update_focus();
        rt.draw_glows();
        eprintln!(
            "noren: {} windows styled{}{}, Ctrl+C restores them and exits",
            rt.applied.len(),
            if rt.focus.is_some() {
                ", focus dimming on"
            } else {
                ""
            },
            match &rt.xray {
                Some(x) => format!(", hold {} for x-ray", x.combo),
                None => String::new(),
            },
        );
        if let Some(leader) = rt.commands.leader.as_ref().filter(|_| chording) {
            eprintln!("noren: {leader} starts a chord");
        }
        if !rt.glows.is_empty() {
            eprintln!("noren: {} edge glow(s) follow their states", rt.glows.list.len());
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            match msg.message {
                WM_TIMER if pointer != 0 && msg.wParam.0 == pointer => rt.poll_pointer(),
                WM_TIMER if rt.frame != 0 && msg.wParam.0 == rt.frame => rt.draw_glows(),
                WM_TIMER if slow != 0 && msg.wParam.0 == slow => {
                    if watch_states {
                        let now = rt.read_states();
                        if now != rt.states {
                            rt.states = now;
                            rt.glows.update(&rt.states, Instant::now());
                            rt.refresh_all();
                        }
                    }
                    rt.follow_schedule();
                    // Also catches monitors that came, went or changed resolution.
                    rt.draw_glows();
                    // A key-up the hook never saw must not leave every window see-through.
                    let lost = HOLD.with(|h| {
                        h.borrow_mut()
                            .as_mut()
                            .and_then(|h| h.verify(|vk| GetAsyncKeyState(vk as i32) < 0))
                    });
                    if lost.is_some() {
                        rt.set_xray(false);
                        rt.update_focus();
                    }
                    let now = Instant::now();
                    CHORDS.with(|c| {
                        if let Some(c) = c.borrow_mut().as_mut() {
                            c.verify(|vk| GetAsyncKeyState(vk as i32) < 0);
                            c.expire(now);
                        }
                    });
                }
                WM_TIMER => {}
                WAKE => {}
                _ => {
                    DispatchMessageW(&msg);
                }
            }
            let queue = QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut()));
            let focus_changed = !queue.is_empty();
            for (event, hwnd) in queue {
                match event {
                    EVENT_OBJECT_DESTROY => rt.forget(hwnd),
                    EVENT_OBJECT_SHOW | EVENT_OBJECT_NAMECHANGE => rt.refresh(HWND(hwnd as *mut c_void)),
                    XRAY => rt.set_xray(hwnd != 0),
                    CHORD => rt.fire(hwnd as usize),
                    _ => {}
                }
            }
            if focus_changed {
                rt.update_focus();
                rt.draw_glows();
            }
        }

        for hook in hooks {
            let _ = UnhookWinEvent(hook);
        }
        if let Some(k) = keyboard {
            let _ = UnhookWindowsHookEx(k);
        }
        rt.xray_on = false;
        rt.restore_all();
        if let Some((_, overlay)) = rt.focus.take() {
            let _ = DestroyWindow(overlay);
        }
        if let Some(mut layers) = rt.layers.take() {
            layers.clear();
        }
        if rt.frame != 0 {
            let _ = KillTimer(None, rt.frame);
        }
    }
    Ok(())
}

unsafe extern "system" fn overlay_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // SAFETY: default handling for a window we own.
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

/// The dim layer: a hidden popup filled with `color`, `alpha` opaque, that never activates and
/// lets every click through.
fn create_overlay(color: u32, alpha: u8) -> io::Result<HWND> {
    // SAFETY: registers a class and creates a window owned by this thread; the brush is owned
    // by the class for the life of the process.
    unsafe {
        let instance = GetModuleHandleW(None).map_err(io::Error::other)?;
        let brush: HBRUSH = CreateSolidBrush(COLORREF(color));
        let class = WNDCLASSW {
            lpfnWndProc: Some(overlay_proc),
            hInstance: instance.into(),
            hbrBackground: brush,
            lpszClassName: w!("noren.focus"),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            let _ = DeleteObject(brush.into());
            return Err(io::Error::last_os_error());
        }
        let hwnd = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("noren.focus"),
            w!("noren focus"),
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
        SetLayeredWindowAttributes(hwnd, COLORREF(0), alpha, LWA_ALPHA).map_err(io::Error::other)?;
        Ok(hwnd)
    }
}

unsafe extern "system" fn on_key(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        // SAFETY: for HC_ACTION, lparam points at a KBDLLHOOKSTRUCT for this call.
        let kb = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let down = matches!(wparam.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let mut swallow = false;
        let step = HOLD.with(|h| h.borrow_mut().as_mut().map(|h| h.event(kb.vkCode, down)));
        if let Some(step) = step {
            if let Some(on) = step.changed {
                QUEUE.with(|q| q.borrow_mut().push((XRAY, isize::from(on))));
                wake();
            }
            swallow |= step.swallow;
        }
        let now = Instant::now();
        let step = CHORDS.with(|c| c.borrow_mut().as_mut().map(|c| c.event(kb.vkCode, down, now)));
        if let Some(step) = step {
            if let Some(i) = step.fired {
                QUEUE.with(|q| q.borrow_mut().push((CHORD, i as isize)));
                wake();
            }
            swallow |= step.swallow;
        }
        if swallow {
            return LRESULT(1);
        }
    }
    // SAFETY: passes the event on unchanged.
    unsafe { CallNextHookEx(None, code, wparam, lparam) }
}

/// Ends the [`hold`] loop running on thread `tid`.
pub fn stop(tid: u32) -> io::Result<()> {
    // SAFETY: posting a message to a thread is valid for any id; failure is reported.
    unsafe { PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)) }.map_err(io::Error::other)
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
    wake();
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
    let out = unsafe { &mut *(data.0 as *mut Vec<Monitor>) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: OS-provided handle and a correctly sized struct.
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        out.push(Monitor {
            handle: monitor,
            work: screen_rect(info.rcWork),
            full: screen_rect(info.rcMonitor),
            primary: info.dwFlags & 1 != 0,
        });
    }
    true.into()
}

#[derive(Clone, Copy)]
struct Monitor {
    handle: HMONITOR,
    work: ScreenRect,
    full: ScreenRect,
    primary: bool,
}

/// Monitors in `monitor = N` order.
fn monitors() -> Vec<Monitor> {
    let mut all: Vec<Monitor> = Vec::new();
    // SAFETY: synchronous enumeration; the callback only borrows `all`.
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(collect_monitor),
            LPARAM(&mut all as *mut _ as isize),
        );
    }
    let keyed: Vec<(ScreenRect, bool)> = all.iter().map(|m| (m.work, m.primary)).collect();
    monitor_order(&keyed).into_iter().map(|i| all[i]).collect()
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

    /// Switches to the scheduled mode when it changes: live windows take the new palette, and
    /// the profile's files and registry values are applied again (journaled, so
    /// `noren rollback` steps back through the switches).
    fn follow_schedule(&mut self) {
        let Some((tracker, profile)) = self.schedule.as_mut() else {
            return;
        };
        let Some(mode) = tracker.due(Local::now()) else {
            return;
        };
        let mut profile = profile.clone();
        profile.palette.mode = mode;
        self.palette = profile.palette();
        self.refresh_all();
        self.draw_glows();
        let name = if mode == Mode::Light { "light" } else { "dark" };
        match reapply(&profile) {
            Ok(0) => eprintln!("noren: schedule: {name}"),
            Ok(n) => eprintln!("noren: schedule: {name}, {n} changes applied"),
            Err(e) => eprintln!("noren: schedule: {name}, applying the profile failed: {e}"),
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
        let mut style = self.rules.resolve(&info, &self.palette, &self.states);
        if let Some(x) = self.xray.as_ref().filter(|_| self.xray_on) {
            style = x.apply(style);
        }
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

    fn set_xray(&mut self, on: bool) {
        if self.xray_on != on {
            self.xray_on = on;
            self.refresh_all();
        }
    }

    /// Puts the dim layer right behind the foreground window, or hides it: during x-ray, and
    /// when the foreground is the desktop, the taskbar, minimized, fullscreen or excluded.
    fn update_focus(&mut self) {
        let Some((_, overlay)) = self.focus else {
            return;
        };
        // SAFETY: queries on the foreground window; positioning and hiding our own overlay.
        unsafe {
            let fg = GetForegroundWindow();
            let place = !self.xray_on && !fg.is_invalid() && !IsIconic(fg).as_bool() && self.dims(fg);
            if !place {
                let _ = ShowWindow(overlay, SW_HIDE);
                return;
            }
            // The virtual screen can change (monitor plugged in), so it is read every time.
            let _ = SetWindowPos(
                overlay,
                Some(fg),
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
    }

    fn dims(&mut self, fg: HWND) -> bool {
        let Some(info) = self.info(fg) else {
            return false;
        };
        let mut r = RECT::default();
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: read-only queries with correctly sized out structs.
        let known = unsafe {
            GetWindowRect(fg, &mut r).is_ok()
                && GetMonitorInfoW(MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool()
        };
        let Some((focus, _)) = &self.focus else {
            return false;
        };
        known && focus.dims(&info.exe, screen_rect(r), screen_rect(mi.rcMonitor))
    }

    /// Runs the command at `i` of the profile's list. Failures are reported, never fatal.
    fn fire(&mut self, i: usize) {
        let Some(cmd) = self.commands.list.get(i) else {
            return;
        };
        let done = match &cmd.action {
            Action::Palette => command::open_palette(&self.source, self.state_dir.as_deref()),
            action => command::exec(action, self.state_dir.as_deref()),
        };
        if let Err(e) = done {
            eprintln!("noren: command {:?}: {e}", cmd.name);
        }
    }

    /// One hot-corner sample: where the pointer is, on which monitor, and whether a button is
    /// held or a fullscreen window has the focus.
    fn poll_pointer(&mut self) {
        let mut p = POINT::default();
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: read-only queries with correctly sized out structs.
        let known = unsafe {
            GetCursorPos(&mut p).is_ok()
                && GetMonitorInfoW(MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST), &mut mi).as_bool()
        };
        if !known {
            return;
        }
        // SAFETY: GetAsyncKeyState accepts any virtual key; 1, 2 and 4 are the mouse buttons.
        let button = [0x01, 0x02, 0x04]
            .into_iter()
            .any(|vk| unsafe { GetAsyncKeyState(vk) } < 0);
        let blocked = button || self.fullscreen_foreground();
        let fired = self
            .corners
            .as_mut()
            .and_then(|c| c.tick(p.x, p.y, screen_rect(mi.rcMonitor), blocked, Instant::now()));
        if let Some(i) = fired {
            self.fire(i);
        }
    }

    /// A game or video has the screen: hot corners stay quiet.
    fn fullscreen_foreground(&mut self) -> bool {
        self.fullscreen_monitor().is_some()
    }

    /// The monitor a fullscreen foreground window covers, if any.
    fn fullscreen_monitor(&mut self) -> Option<HMONITOR> {
        // SAFETY: read-only queries on the foreground window.
        let fg = unsafe { GetForegroundWindow() };
        if fg.is_invalid() || self.info(fg).is_none() {
            return None;
        }
        let mut r = RECT::default();
        let mut mi = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        // SAFETY: read-only queries with correctly sized out structs.
        let monitor = unsafe { MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST) };
        // SAFETY: as above.
        let known =
            unsafe { GetWindowRect(fg, &mut r).is_ok() && GetMonitorInfoW(monitor, &mut mi).as_bool() };
        (known && fullscreen(screen_rect(r), screen_rect(mi.rcMonitor))).then_some(monitor)
    }

    /// Draws the glows as they are now and keeps the frame timer running exactly while one
    /// moves.
    fn draw_glows(&mut self) {
        if self.layers.is_none() {
            return;
        }
        let now = Instant::now();
        let hidden = self.fullscreen_monitor();
        let screens: Vec<Screen> = monitors()
            .into_iter()
            .map(|m| Screen {
                handle: m.handle,
                rect: m.full,
            })
            .collect();
        let bars = self.glows.bars(&self.palette, now);
        if let Some(layers) = self.layers.as_mut() {
            layers.show(&self.glows.list, &bars, &screens, hidden);
        }
        let moving = self.glows.animating(now);
        // SAFETY: thread timers owned by this loop.
        unsafe {
            if moving && self.frame == 0 {
                self.frame = SetTimer(None, 0, FRAME_MS, None);
            } else if !moving && self.frame != 0 {
                let _ = KillTimer(None, self.frame);
                self.frame = 0;
            }
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
    let Some(&Monitor {
        handle: target,
        work: to,
        ..
    }) = all.get(n as usize - 1)
    else {
        return;
    };
    // SAFETY: position queries and a move of a foreign top-level window.
    unsafe {
        let current = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        if current == target {
            return;
        }
        let Some(from) = all.iter().find(|m| m.handle == current).map(|m| m.work) else {
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
        xray: None,
        xray_on: false,
        focus: None,
        commands: Commands::new(&[], None, None).expect("no commands are valid"),
        corners: None,
        source: String::new(),
        glows: Glows::new(Vec::new(), Instant::now()),
        layers: None,
        frame: 0,
        schedule: None,
    };
    top_level_windows()
        .into_iter()
        .filter_map(|h| rt.info(h))
        .collect()
}
