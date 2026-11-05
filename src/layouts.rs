//! Panel layouts per foreground app and virtual desktop: which widgets `noren panel` shows
//! while a given program is in front or a given desktop is current.
//!
//! ```toml
//! [[layout]]
//! exe = ["Code.exe", "WindowsTerminal.exe"]
//! widgets = ["git", "cpu"]
//!
//! [[layout]]
//! desktop = "Music"
//! widgets = ["media"]
//! ```
//!
//! The first layout whose conditions all hold wins; with none, every widget shows. `exe`
//! matches the program file name, `desktop` the virtual desktop's name ("Desktop 2" for one
//! that was never renamed, whatever the Windows language), both ignoring case. The panel itself never counts as the
//! foreground: clicking it keeps the layout it had.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The name the [`crate::media`] widget goes by in `widgets`.
pub const MEDIA: &str = "media";

/// How often the foreground and desktop are looked up.
pub const POLL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutConfig {
    /// Program file names; any one in front matches.
    #[serde(default)]
    pub exe: Vec<String>,
    /// Virtual desktop name.
    pub desktop: Option<String>,
    /// Widget names to show (`[[widget]]` names and `media`); they keep their panel order.
    pub widgets: Vec<String>,
}

impl LayoutConfig {
    fn matches(&self, at: &Where) -> bool {
        let exe = self.exe.is_empty() || self.exe.iter().any(|e| e.eq_ignore_ascii_case(&at.exe));
        let desktop = self.desktop.as_ref().is_none_or(|d| {
            at.desktop
                .as_ref()
                .is_some_and(|c| c.to_lowercase() == d.to_lowercase())
        });
        exe && desktop
    }
}

/// Checks the layouts against the widget names the panel has.
pub fn validate(layouts: &[LayoutConfig], widgets: &[&str]) -> Result<(), String> {
    for (i, l) in layouts.iter().enumerate() {
        let who = format!("layout {}", i + 1);
        if l.exe.is_empty() && l.desktop.is_none() {
            return Err(format!("{who}: set exe, desktop or both"));
        }
        if l.exe.iter().any(|e| e.trim().is_empty())
            || l.desktop.as_ref().is_some_and(|d| d.trim().is_empty())
        {
            return Err(format!("{who}: exe and desktop cannot be empty"));
        }
        for w in &l.widgets {
            if !widgets.contains(&w.as_str()) {
                return Err(format!(
                    "{who}: no widget named {w:?} (the panel has: {})",
                    widgets.join(", ")
                ));
            }
        }
    }
    Ok(())
}

/// The foreground program and the current virtual desktop.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Where {
    /// File name, like `Code.exe`.
    pub exe: String,
    pub desktop: Option<String>,
}

/// The widgets to show at `at`: `None` for all of them.
pub fn pick<'a>(layouts: &'a [LayoutConfig], at: &Where) -> Option<&'a [String]> {
    layouts
        .iter()
        .find(|l| l.matches(at))
        .map(|l| l.widgets.as_slice())
}

/// Follows the foreground and desktop and says when the panel should change its widgets.
pub struct Switcher {
    layouts: Vec<LayoutConfig>,
    probe: Box<dyn FnMut() -> Option<Where>>,
    polled: Option<Instant>,
    shown: Option<Option<Vec<String>>>,
}

impl Switcher {
    /// `probe` reports where the user is, or `None` to keep the current layout (the panel
    /// itself is in front, or the lookup failed).
    pub fn new(layouts: Vec<LayoutConfig>, probe: impl FnMut() -> Option<Where> + 'static) -> Self {
        Self {
            layouts,
            probe: Box::new(probe),
            polled: None,
            shown: None,
        }
    }

    /// Follows this machine's foreground window and virtual desktop.
    pub fn system(layouts: Vec<LayoutConfig>) -> Self {
        Self::new(layouts, system::probe)
    }

    /// The widgets to show from now on, when that changed: `Some(None)` for all of them.
    pub fn poll(&mut self, now: Instant) -> Option<Option<Vec<String>>> {
        if self
            .polled
            .is_some_and(|t| now.saturating_duration_since(t) < POLL)
        {
            return None;
        }
        self.polled = Some(now);
        let at = (self.probe)()?;
        let want = pick(&self.layouts, &at).map(<[String]>::to_vec);
        if self.shown.as_ref() == Some(&want) {
            return None;
        }
        self.shown = Some(want.clone());
        Some(want)
    }
}

/// A desktop's name from the registry: its own `Name` if it was renamed, otherwise "Desktop N"
/// by its place in `VirtualDesktopIDs` (16 bytes per desktop), as Task View shows it.
pub fn desktop_name(current: &[u8], ids: &[u8], name: Option<String>) -> Option<String> {
    if let Some(name) = name.filter(|n| !n.is_empty()) {
        return Some(name);
    }
    let current: &[u8; 16] = current.try_into().ok()?;
    let index = ids.as_chunks::<16>().0.iter().position(|id| id == current)?;
    Some(format!("Desktop {}", index + 1))
}

/// `{XXXXXXXX-XXXX-XXXX-XXXX-XXXXXXXXXXXX}` for a GUID stored as its 16 raw bytes (the first
/// three groups little-endian, as Windows keeps them).
pub fn guid_string(b: &[u8; 16]) -> String {
    format!(
        "{{{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

/// A `REG_SZ` value's bytes as text, without the trailing NUL.
pub fn reg_string(bytes: &[u8]) -> String {
    let wide: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&c| c != 0)
        .collect();
    String::from_utf16_lossy(&wide)
}

#[cfg(windows)]
mod system {
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

    use super::{Where, desktop_name, guid_string, reg_string};
    use crate::rice::store::registry;

    const EXPLORER: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer";

    fn read(key: &str, name: &str) -> Option<Vec<u8>> {
        registry::read(key, name).ok().flatten()
    }

    /// The current desktop's name. Windows 11 keeps the current desktop under
    /// `Explorer\VirtualDesktops`, Windows 10 under `Explorer\SessionInfo\<session>`.
    fn desktop() -> Option<String> {
        let root = format!(r"{EXPLORER}\VirtualDesktops");
        let current = read(&root, "CurrentVirtualDesktop").or_else(|| {
            let mut session = 0u32;
            // SAFETY: writes one u32 for our own process id.
            unsafe { ProcessIdToSessionId(std::process::id(), &mut session) }.ok()?;
            read(
                &format!(r"{EXPLORER}\SessionInfo\{session}\VirtualDesktops"),
                "CurrentVirtualDesktop",
            )
        })?;
        let ids = read(&root, "VirtualDesktopIDs").unwrap_or_default();
        let guid: &[u8; 16] = current.as_slice().try_into().ok()?;
        let name = read(&format!(r"{root}\Desktops\{}", guid_string(guid)), "Name").map(|b| reg_string(&b));
        desktop_name(&current, &ids, name)
    }

    pub fn probe() -> Option<Where> {
        // SAFETY: plain queries on the foreground window handle.
        let pid = unsafe {
            let fg = GetForegroundWindow();
            if fg.is_invalid() {
                return None;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(fg, Some(&mut pid));
            pid
        };
        if pid == 0 || pid == std::process::id() {
            return None;
        }
        let exe = crate::shell::exe_name(pid);
        (!exe.is_empty()).then(|| Where {
            exe,
            desktop: desktop(),
        })
    }
}

#[cfg(not(windows))]
mod system {
    use super::Where;

    pub fn probe() -> Option<Where> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use super::*;

    fn layout(exe: &[&str], desktop: Option<&str>, widgets: &[&str]) -> LayoutConfig {
        LayoutConfig {
            exe: exe.iter().map(|s| s.to_string()).collect(),
            desktop: desktop.map(Into::into),
            widgets: widgets.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn at(exe: &str, desktop: Option<&str>) -> Where {
        Where {
            exe: exe.into(),
            desktop: desktop.map(Into::into),
        }
    }

    #[test]
    fn the_first_matching_layout_wins_and_none_shows_all() {
        let layouts = [
            layout(&["code.exe"], Some("Work"), &["git"]),
            layout(&["Code.exe", "WindowsTerminal.exe"], None, &["git", "cpu"]),
            layout(&[], Some("music"), &["media"]),
        ];
        let names = |w: Option<&[String]>| w.map(|w| w.join(","));
        assert_eq!(
            names(pick(&layouts, &at("CODE.EXE", Some("work")))),
            Some("git".into())
        );
        assert_eq!(
            names(pick(&layouts, &at("Code.exe", Some("Desktop 1")))),
            Some("git,cpu".into())
        );
        assert_eq!(
            names(pick(&layouts, &at("WindowsTerminal.exe", None))),
            Some("git,cpu".into())
        );
        assert_eq!(
            names(pick(&layouts, &at("firefox.exe", Some("Music")))),
            Some("media".into())
        );
        assert_eq!(pick(&layouts, &at("firefox.exe", Some("Desktop 1"))), None);
        // A desktop condition does not hold when the desktop is unknown.
        assert_eq!(pick(&layouts, &at("firefox.exe", None)), None);
    }

    #[test]
    fn layouts_need_a_condition_and_known_widgets() {
        let widgets = ["media", "git"];
        assert!(validate(&[layout(&["a.exe"], None, &["git", "media"])], &widgets).is_ok());
        assert!(validate(&[layout(&["a.exe"], None, &[])], &widgets).is_ok());
        assert!(
            validate(&[layout(&[], None, &["git"])], &widgets)
                .unwrap_err()
                .contains("set exe, desktop or both")
        );
        assert!(validate(&[layout(&[" "], None, &[])], &widgets).is_err());
        let e = validate(&[layout(&["a.exe"], None, &["cpu"])], &widgets).unwrap_err();
        assert!(e.contains("no widget named \"cpu\""), "{e}");
    }

    #[test]
    fn the_switcher_reports_changes_only_and_keeps_the_layout_on_none() {
        let state = Rc::new(RefCell::new(Some(at("Code.exe", None))));
        let probe = {
            let state = Rc::clone(&state);
            move || state.borrow().clone()
        };
        let mut s = Switcher::new(vec![layout(&["Code.exe"], None, &["git"])], probe);
        let t0 = Instant::now();
        assert_eq!(s.poll(t0), Some(Some(vec!["git".into()])));
        // Too soon, then unchanged.
        assert_eq!(s.poll(t0 + POLL / 2), None);
        assert_eq!(s.poll(t0 + POLL), None);
        // The panel in front: keep what is shown.
        *state.borrow_mut() = None;
        assert_eq!(s.poll(t0 + POLL * 2), None);
        *state.borrow_mut() = Some(at("explorer.exe", None));
        assert_eq!(s.poll(t0 + POLL * 3), Some(None));
        assert_eq!(s.poll(t0 + POLL * 4), None);
    }

    #[test]
    fn desktop_names_fall_back_to_their_position() {
        let a = [1u8; 16];
        let b = [2u8; 16];
        let ids = [a, b].concat();
        assert_eq!(desktop_name(&b, &ids, None).as_deref(), Some("Desktop 2"));
        assert_eq!(
            desktop_name(&b, &ids, Some(String::new())).as_deref(),
            Some("Desktop 2")
        );
        assert_eq!(
            desktop_name(&b, &ids, Some("Music".into())).as_deref(),
            Some("Music")
        );
        assert_eq!(desktop_name(&[3u8; 16], &ids, None), None);
        assert_eq!(desktop_name(&[1u8; 15], &ids, None), None);
    }

    #[test]
    fn guids_and_registry_strings_decode() {
        let bytes = [
            0x78, 0x56, 0x34, 0x12, 0xBC, 0x9A, 0xF0, 0xDE, 0x01, 0x23, 0x45, 0x67, 0x89, 0xAB, 0xCD, 0xEF,
        ];
        assert_eq!(guid_string(&bytes), "{12345678-9ABC-DEF0-0123-456789ABCDEF}");
        let music: Vec<u8> = "Muzyka\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(reg_string(&music), "Muzyka");
        assert_eq!(reg_string(&[]), "");
    }
}
