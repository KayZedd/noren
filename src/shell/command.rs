//! Commands: named actions bound to leader-key chords, hot corners and the command palette.
//!
//! ```toml
//! [chords]
//! leader = "Alt+Space"       # press, release, then type a command's keys
//! timeout = 1.5              # seconds to type the next key, default 1.5
//!
//! [corners]
//! dwell = 0.25               # seconds the pointer rests in a corner, default 0.25
//!
//! [[command]]
//! name = "Terminal"
//! keys = "t"                 # chord after the leader; "g s" is two keys
//! run = ["wt.exe"]           # program and arguments
//!
//! [[command]]
//! name = "Mark build failed"
//! state = "build=fail"       # ok | fail | warn | busy | clear
//!
//! [[command]]
//! name = "noren on GitHub"
//! open = "https://github.com/KayZedd/noren"   # a URL, file or folder
//!
//! [[command]]
//! name = "Command palette"
//! corner = "top-left"        # top-left | top-right | bottom-left | bottom-right
//! builtin = "palette"
//! ```
//!
//! Every command shows up in the palette whether or not it has keys or a corner. This module
//! is pure: the Win32 runtime feeds [`Chords`] key events and [`HotCorners`] the pointer.

use std::collections::BTreeSet;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::keys::{Combo, Hold, Mod, key_name, key_vk};
use super::rules::ScreenRect;
use super::state::{self, State};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandConfig {
    pub name: String,
    pub keys: Option<String>,
    pub corner: Option<String>,
    pub run: Option<Vec<String>>,
    pub open: Option<String>,
    pub state: Option<String>,
    pub builtin: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChordsConfig {
    pub leader: String,
    #[serde(default = "ChordsConfig::default_timeout")]
    pub timeout: f32,
}

impl ChordsConfig {
    fn default_timeout() -> f32 {
        1.5
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CornersConfig {
    #[serde(default = "CornersConfig::default_dwell")]
    pub dwell: f32,
}

impl CornersConfig {
    fn default_dwell() -> f32 {
        0.25
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    const ALL: [(Corner, &'static str); 4] = [
        (Corner::TopLeft, "top-left"),
        (Corner::TopRight, "top-right"),
        (Corner::BottomLeft, "bottom-left"),
        (Corner::BottomRight, "bottom-right"),
    ];

    fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().find(|(_, n)| *n == s.trim()).map(|(c, _)| *c)
    }

    /// The corner of `m` that `(x, y)` sits in, within 2 px. The pointer is clamped to the
    /// screen, so a fling into a corner lands exactly on its last pixel.
    pub fn at(x: i32, y: i32, m: ScreenRect) -> Option<Self> {
        const ZONE: i32 = 2;
        let left = x < m.left + ZONE;
        let right = x >= m.right - ZONE;
        let top = y < m.top + ZONE;
        let bottom = y >= m.bottom - ZONE;
        match (left, right, top, bottom) {
            (true, false, true, false) => Some(Self::TopLeft),
            (false, true, true, false) => Some(Self::TopRight),
            (true, false, false, true) => Some(Self::BottomLeft),
            (false, true, false, true) => Some(Self::BottomRight),
            _ => None,
        }
    }
}

impl fmt::Display for Corner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = Self::ALL.iter().find(|(c, _)| c == self).map_or("", |(_, n)| n);
        f.write_str(name)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Program and arguments, started detached.
    Run(Vec<String>),
    /// A URL, file or folder, opened with its default handler.
    Open(String),
    /// Sets (`Some`) or clears (`None`) a named state.
    State(String, Option<State>),
    /// Opens the command palette.
    Palette,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Command {
    pub name: String,
    /// Virtual keys typed after the leader; empty when the command has no chord.
    pub keys: Vec<u32>,
    pub corner: Option<Corner>,
    pub action: Action,
}

impl Command {
    /// The chord as typed, `"g s"`; empty without one.
    pub fn keys_label(&self) -> String {
        self.keys
            .iter()
            .map(|&vk| key_name(vk).to_lowercase())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Every `[[command]]` of a profile, validated, with the chord and corner settings.
#[derive(Clone, Debug)]
pub struct Commands {
    pub list: Vec<Command>,
    pub leader: Option<Combo>,
    pub timeout: Duration,
    pub dwell: Duration,
}

const ESC: u32 = 0x1B;

fn seconds(
    table: &str,
    field: &str,
    v: f32,
    range: std::ops::RangeInclusive<f32>,
) -> Result<Duration, String> {
    if range.contains(&v) {
        Ok(Duration::from_secs_f32(v))
    } else {
        Err(format!(
            "[{table}] {field} {v} is outside {}..={}",
            range.start(),
            range.end()
        ))
    }
}

fn action(c: &CommandConfig) -> Result<Action, String> {
    let bad = |why: String| format!("[[command]] {:?}: {why}", c.name);
    let set = [
        c.run.is_some(),
        c.open.is_some(),
        c.state.is_some(),
        c.builtin.is_some(),
    ];
    if set.iter().filter(|s| **s).count() != 1 {
        return Err(bad("needs exactly one of run, open, state, builtin".into()));
    }
    if let Some(argv) = &c.run {
        if argv.first().is_none_or(|p| p.trim().is_empty()) {
            return Err(bad("run needs a program".into()));
        }
        return Ok(Action::Run(argv.clone()));
    }
    if let Some(target) = &c.open {
        if target.trim().is_empty() {
            return Err(bad("open needs a URL or path".into()));
        }
        return Ok(Action::Open(target.clone()));
    }
    if let Some(s) = &c.state {
        let (name, value) = s
            .split_once('=')
            .ok_or_else(|| bad(format!("state {s:?}: want \"name=ok|fail|warn|busy|clear\"")))?;
        let name = name.trim();
        if !state::valid_name(name) {
            return Err(bad(format!("state name {name:?}: use letters, digits, - and _")));
        }
        let value = match value.trim() {
            "clear" => None,
            v => Some(v.parse::<State>().map_err(|e| bad(format!("state {s:?}: {e}")))?),
        };
        return Ok(Action::State(name.to_string(), value));
    }
    match c.builtin.as_deref().map(str::trim) {
        Some("palette") => Ok(Action::Palette),
        other => Err(bad(format!(
            "unknown builtin {:?}, want \"palette\"",
            other.unwrap_or("")
        ))),
    }
}

fn keys(c: &CommandConfig) -> Result<Vec<u32>, String> {
    let Some(text) = &c.keys else {
        return Ok(Vec::new());
    };
    let bad = |why: String| format!("[[command]] {:?} keys {text:?}: {why}", c.name);
    let seq: Vec<u32> = text
        .split_whitespace()
        .map(|k| {
            let vk = key_vk(k).ok_or_else(|| bad(format!("unknown key {k:?}")))?;
            if vk == ESC {
                return Err(bad("Esc cancels a chord and cannot be part of one".into()));
            }
            Ok(vk)
        })
        .collect::<Result<_, _>>()?;
    if seq.is_empty() {
        return Err(bad("empty".into()));
    }
    Ok(seq)
}

impl Commands {
    pub fn new(
        list: &[CommandConfig],
        chords: Option<&ChordsConfig>,
        corners: Option<&CornersConfig>,
    ) -> Result<Self, String> {
        let leader = chords
            .map(|c| {
                let combo = Combo::parse(&c.leader).map_err(|e| format!("[chords] leader: {e}"))?;
                if combo.key.is_none() {
                    return Err(format!(
                        "[chords] leader {:?} needs a key besides modifiers",
                        c.leader
                    ));
                }
                Ok(combo)
            })
            .transpose()?;
        let timeout = match chords {
            Some(c) => seconds("chords", "timeout", c.timeout, 0.3..=10.0)?,
            None => Duration::from_secs_f32(ChordsConfig::default_timeout()),
        };
        let dwell = match corners {
            Some(c) => seconds("corners", "dwell", c.dwell, 0.0..=2.0)?,
            None => Duration::from_secs_f32(CornersConfig::default_dwell()),
        };
        let mut out: Vec<Command> = Vec::new();
        for c in list {
            if c.name.trim().is_empty() {
                return Err("[[command]] needs a name".into());
            }
            if out.iter().any(|o| o.name.eq_ignore_ascii_case(c.name.trim())) {
                return Err(format!("[[command]] {:?} is defined twice", c.name));
            }
            let cmd = Command {
                name: c.name.trim().to_string(),
                keys: keys(c)?,
                corner: c
                    .corner
                    .as_deref()
                    .map(|s| {
                        Corner::parse(s).ok_or_else(|| {
                            format!(
                                "[[command]] {:?} corner {s:?}: want top-left, top-right, bottom-left or bottom-right",
                                c.name
                            )
                        })
                    })
                    .transpose()?,
                action: action(c)?,
            };
            if !cmd.keys.is_empty() && leader.is_none() {
                return Err(format!(
                    "[[command]] {:?} has keys but the profile has no [chords] leader",
                    cmd.name
                ));
            }
            for o in &out {
                if !cmd.keys.is_empty() && !o.keys.is_empty() {
                    let n = cmd.keys.len().min(o.keys.len());
                    if cmd.keys[..n] == o.keys[..n] {
                        return Err(format!(
                            "[[command]] {:?} keys {:?} clash with {:?} keys {:?}: one starts the other",
                            cmd.name,
                            cmd.keys_label(),
                            o.name,
                            o.keys_label()
                        ));
                    }
                }
                if cmd.corner.is_some() && cmd.corner == o.corner {
                    return Err(format!(
                        "[[command]] {:?} and {:?} share the {} corner",
                        cmd.name,
                        o.name,
                        cmd.corner.map(|c| c.to_string()).unwrap_or_default()
                    ));
                }
            }
            out.push(cmd);
        }
        Ok(Self {
            list: out,
            leader,
            timeout,
            dwell,
        })
    }

    /// The chord machine, when a leader and at least one chord exist.
    pub fn chords(&self) -> Option<Chords> {
        let seqs: Vec<(Vec<u32>, usize)> = self
            .list
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.keys.is_empty())
            .map(|(i, c)| (c.keys.clone(), i))
            .collect();
        let leader = self.leader.clone()?;
        (!seqs.is_empty()).then(|| Chords::new(leader, seqs, self.timeout))
    }

    /// The corner watcher, when at least one command has a corner.
    pub fn hot_corners(&self) -> Option<HotCorners> {
        let corners: Vec<(Corner, usize)> = self
            .list
            .iter()
            .enumerate()
            .filter_map(|(i, c)| c.corner.map(|k| (k, i)))
            .collect();
        (!corners.is_empty()).then(|| HotCorners::new(corners, self.dwell))
    }

    /// Whether `noren run` has anything to hold for these commands.
    pub fn bound(&self) -> bool {
        self.chords().is_some() || self.hot_corners().is_some()
    }
}

/// What the keyboard hook should do with one key event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ChordStep {
    pub swallow: bool,
    /// Index into [`Commands::list`] of the command whose chord was just completed.
    pub fired: Option<usize>,
    /// `Some(true)` when the leader was just pressed, `Some(false)` when a pending chord just
    /// ended (fired, canceled with Esc or a wrong key).
    pub pending: Option<bool>,
}

/// Leader, then a sequence of plain keys. Keys typed while a chord is pending never reach the
/// focused app; modifiers pass through and are ignored, so `Alt+Space` then `Alt+T` works as
/// well as `Alt+Space`, release, `T`.
#[derive(Clone, Debug)]
pub struct Chords {
    leader: Hold,
    seqs: Vec<(Vec<u32>, usize)>,
    timeout: Duration,
    pending: Option<(Vec<u32>, Instant)>,
    down: BTreeSet<u32>,
    /// Keys whose key-down was swallowed: their key-up is swallowed too, so the app never
    /// sees half a keystroke.
    swallowed: BTreeSet<u32>,
}

impl Chords {
    pub fn new(leader: Combo, seqs: Vec<(Vec<u32>, usize)>, timeout: Duration) -> Self {
        Self {
            leader: Hold::new(leader),
            seqs,
            timeout,
            pending: None,
            down: BTreeSet::new(),
            swallowed: BTreeSet::new(),
        }
    }

    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn event(&mut self, vk: u32, down: bool, now: Instant) -> ChordStep {
        let repeat = down && !self.down.insert(vk);
        if !down {
            self.down.remove(&vk);
        }
        let mut step = ChordStep::default();
        let held = self.leader.event(vk, down);
        if !down {
            step.swallow = self.swallowed.remove(&vk) || held.swallow;
            return step;
        }
        if repeat {
            step.swallow = self.swallowed.contains(&vk) || held.swallow;
            return step;
        }
        if let Some((seq, since)) = &mut self.pending
            && Mod::of(vk).is_none()
        {
            step.swallow = true;
            self.swallowed.insert(vk);
            if vk == ESC {
                self.pending = None;
                step.pending = Some(false);
                return step;
            }
            seq.push(vk);
            *since = now;
            let seq = seq.clone();
            if let Some(&(_, i)) = self.seqs.iter().find(|(s, _)| *s == seq) {
                step.fired = Some(i);
            } else if self.seqs.iter().any(|(s, _)| s.starts_with(&seq)) {
                return step;
            }
            self.pending = None;
            step.pending = Some(false);
            return step;
        }
        // A key the app sees going down must also be seen going up.
        self.swallowed.remove(&vk);
        if held.swallow {
            step.swallow = true;
            self.swallowed.insert(vk);
        }
        if held.changed == Some(true) && self.pending.is_none() {
            self.pending = Some((Vec::new(), now));
            step.pending = Some(true);
        }
        step
    }

    /// Recovers from key-ups the hook never saw (see [`Hold::verify`]), so the leader works
    /// again after a lock screen or UAC prompt ate its release.
    pub fn verify(&mut self, is_down: impl Fn(u32) -> bool) {
        if self.leader.verify(&is_down).is_some() {
            self.down.clear();
        }
        self.down.retain(|&vk| is_down(vk));
    }

    /// Cancels a chord left unfinished for longer than the timeout. True when it did.
    pub fn expire(&mut self, now: Instant) -> bool {
        match &self.pending {
            Some((_, since)) if now.duration_since(*since) >= self.timeout => {
                self.pending = None;
                true
            }
            _ => false,
        }
    }
}

/// Fires a corner's command once the pointer has rested in it for the dwell time. It fires
/// once per visit, and never while a mouse button is held (dragging a window or a selection
/// into a corner) or a fullscreen app has the focus.
#[derive(Clone, Debug)]
pub struct HotCorners {
    corners: Vec<(Corner, usize)>,
    dwell: Duration,
    inside: Option<(Corner, Instant)>,
    spent: bool,
}

impl HotCorners {
    pub fn new(corners: Vec<(Corner, usize)>, dwell: Duration) -> Self {
        Self {
            corners,
            dwell,
            inside: None,
            spent: false,
        }
    }

    /// One pointer sample: `(x, y)` on the monitor `m`. Returns the command to run.
    pub fn tick(&mut self, x: i32, y: i32, m: ScreenRect, blocked: bool, now: Instant) -> Option<usize> {
        let Some(corner) = Corner::at(x, y, m) else {
            self.inside = None;
            self.spent = false;
            return None;
        };
        if self.inside.is_none_or(|(c, _)| c != corner) {
            self.inside = Some((corner, now));
            self.spent = false;
        }
        if blocked {
            self.spent = true;
        }
        let (_, since) = self.inside?;
        if self.spent || now.duration_since(since) < self.dwell {
            return None;
        }
        self.spent = true;
        self.corners.iter().find(|(c, _)| *c == corner).map(|&(_, i)| i)
    }
}

/// Subsequence match of `query` in `text`, case-insensitive. Higher is better; `None` when
/// some query character is missing. Matches at word starts and runs of adjacent characters
/// score up, so "gs" ranks "Git status" over "Ogres".
///
/// Written here rather than pulled in: palettes hold tens of commands, the scorer is a few
/// lines, and the popular matcher crates are MPL-2.0 or unmaintained.
pub fn fuzzy(query: &str, text: &str) -> Option<i32> {
    let text: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let mut score = 0;
    let mut at = 0;
    let mut last: Option<usize> = None;
    for q in query
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|c| !c.is_whitespace())
    {
        let i = at + text[at..].iter().position(|&c| c == q)?;
        score += 1;
        if i == 0 || matches!(text[i - 1], ' ' | '-' | '_' | '.' | '/' | '\\') {
            score += 8;
        }
        if last.is_some_and(|l| l + 1 == i) {
            score += 5;
        }
        score -= (i - last.map_or(0, |l| l + 1)).min(3) as i32;
        last = Some(i);
        at = i + 1;
    }
    Some(score)
}

/// Indices of `names` that match `query`, best first; ties keep their order.
pub fn rank<'a>(query: &str, names: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
    let mut hits: Vec<(i32, usize)> = names
        .into_iter()
        .enumerate()
        .filter_map(|(i, n)| fuzzy(query, n).map(|s| (s, i)))
        .collect();
    hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|(_, i)| i).collect()
}

/// Runs `action`. [`Action::Palette`] needs a UI and is left to [`open_palette`].
pub fn exec(action: &Action, state_dir: Option<&Path>) -> io::Result<()> {
    match action {
        Action::Run(argv) => std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .spawn()
            .map(drop)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", argv[0]))),
        Action::Open(target) => open(target),
        Action::State(name, value) => {
            let dir = state_dir
                .map(Path::to_path_buf)
                .or_else(state::default_dir)
                .ok_or_else(|| io::Error::other("no state directory"))?;
            state::set(&dir, name, *value)
        }
        Action::Palette => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the palette opens through open_palette",
        )),
    }
}

#[cfg(windows)]
fn open(target: &str) -> io::Result<()> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, w};
    let target = HSTRING::from(target);
    // SAFETY: both strings outlive the call; no window handle is needed.
    let r = unsafe { ShellExecuteW(None, w!("open"), &target, None, None, SW_SHOWNORMAL) };
    // ShellExecuteW reports success as a value above 32.
    if r.0 as isize > 32 {
        Ok(())
    } else {
        Err(io::Error::other(format!("cannot open {target}")))
    }
}

#[cfg(not(windows))]
fn open(target: &str) -> io::Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(opener).arg(target).spawn().map(drop)
}

/// The studio binary next to the running one.
pub fn studio_exe() -> io::Result<PathBuf> {
    let me = std::env::current_exe()?;
    let exe = me.with_file_name(format!("noren-studio{}", std::env::consts::EXE_SUFFIX));
    if exe.is_file() {
        Ok(exe)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} not found: the palette needs noren studio (cargo build --release --features studio)",
                exe.display()
            ),
        ))
    }
}

/// Opens the command palette for the profile whose TOML is `source`, as a small studio window.
/// `state_dir` reaches the palette, so its state commands write where the runtime reads.
pub fn open_palette(source: &str, state_dir: Option<&Path>) -> io::Result<()> {
    let exe = studio_exe()?;
    let file = std::env::temp_dir().join(format!("noren-palette-{}.toml", std::process::id()));
    std::fs::write(&file, source)?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--palette").arg(&file);
    if let Some(dir) = state_dir {
        cmd.arg("--state-dir").arg(dir);
    }
    cmd.spawn().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LALT: u32 = 0xA4;
    const SPACE: u32 = 0x20;
    const G: u32 = b'G' as u32;
    const S: u32 = b'S' as u32;
    const T: u32 = b'T' as u32;
    const X: u32 = b'X' as u32;

    fn cmd(name: &str, extra: &str) -> CommandConfig {
        toml::from_str(&format!("name = {name:?}\n{extra}")).unwrap()
    }

    fn chords_cfg() -> ChordsConfig {
        ChordsConfig {
            leader: "Alt+Space".into(),
            timeout: 1.5,
        }
    }

    fn commands() -> Commands {
        Commands::new(
            &[
                cmd("Terminal", "keys = \"t\"\nrun = [\"wt.exe\"]"),
                cmd("Git status", "keys = \"g s\"\nstate = \"git=warn\""),
                cmd("Palette", "corner = \"top-left\"\nbuiltin = \"palette\""),
                cmd("Docs", "open = \"https://example.com\""),
            ],
            Some(&chords_cfg()),
            None,
        )
        .unwrap()
    }

    #[test]
    fn commands_parse_every_action() {
        let c = commands();
        assert_eq!(c.list[0].action, Action::Run(vec!["wt.exe".into()]));
        assert_eq!(c.list[1].action, Action::State("git".into(), Some(State::Warn)));
        assert_eq!(c.list[1].keys, vec![G, S]);
        assert_eq!(c.list[1].keys_label(), "g s");
        assert_eq!(c.list[2].corner, Some(Corner::TopLeft));
        assert_eq!(c.list[2].action, Action::Palette);
        assert_eq!(c.list[3].action, Action::Open("https://example.com".into()));
        assert!(c.bound());
        let clear = Commands::new(&[cmd("c", "state = \"build = clear\"")], None, None).unwrap();
        assert_eq!(clear.list[0].action, Action::State("build".into(), None));
        assert!(!clear.bound(), "palette-only commands hold nothing");
    }

    #[test]
    fn bad_commands_say_why() {
        let leader = Some(chords_cfg());
        for (list, chords, why) in [
            (vec![cmd("a", "")], leader.clone(), "exactly one"),
            (
                vec![cmd("a", "run = []\nopen = \"x\"")],
                leader.clone(),
                "exactly one",
            ),
            (vec![cmd("a", "run = []")], leader.clone(), "needs a program"),
            (vec![cmd("a", "state = \"x\"")], leader.clone(), "name=ok"),
            (vec![cmd("a", "state = \"b/c=ok\"")], leader.clone(), "state name"),
            (
                vec![cmd("a", "state = \"b=meh\"")],
                leader.clone(),
                "state \"b=meh\"",
            ),
            (
                vec![cmd("a", "builtin = \"rofl\"")],
                leader.clone(),
                "unknown builtin",
            ),
            (
                vec![cmd("a", "keys = \"t\"\nbuiltin = \"palette\"")],
                None,
                "no [chords] leader",
            ),
            (
                vec![cmd("a", "keys = \"esc\"\nbuiltin = \"palette\"")],
                leader.clone(),
                "Esc",
            ),
            (
                vec![cmd("a", "keys = \"nope\"\nbuiltin = \"palette\"")],
                leader.clone(),
                "unknown key",
            ),
            (
                vec![cmd("a", "corner = \"middle\"\nbuiltin = \"palette\"")],
                leader.clone(),
                "corner",
            ),
            (
                vec![cmd(" ", "builtin = \"palette\"")],
                leader.clone(),
                "needs a name",
            ),
            (
                vec![
                    cmd("a", "builtin = \"palette\""),
                    cmd("A", "builtin = \"palette\""),
                ],
                leader.clone(),
                "twice",
            ),
            (
                vec![
                    cmd("a", "keys = \"g\"\nbuiltin = \"palette\""),
                    cmd("b", "keys = \"g s\"\nbuiltin = \"palette\""),
                ],
                leader.clone(),
                "one starts the other",
            ),
            (
                vec![
                    cmd("a", "corner = \"top-left\"\nbuiltin = \"palette\""),
                    cmd("b", "corner = \"top-left\"\nbuiltin = \"palette\""),
                ],
                leader.clone(),
                "share the top-left corner",
            ),
        ] {
            let err = Commands::new(&list, chords.as_ref(), None).unwrap_err();
            assert!(err.contains(why), "want {why:?} in {err}");
        }
        let modifiers_only = ChordsConfig {
            leader: "Ctrl+Win".into(),
            timeout: 1.5,
        };
        assert!(
            Commands::new(&[], Some(&modifiers_only), None)
                .unwrap_err()
                .contains("needs a key")
        );
        let slow = ChordsConfig {
            timeout: 30.0,
            ..chords_cfg()
        };
        assert!(
            Commands::new(&[], Some(&slow), None)
                .unwrap_err()
                .contains("timeout")
        );
        let dwell = CornersConfig { dwell: 5.0 };
        assert!(
            Commands::new(&[], None, Some(&dwell))
                .unwrap_err()
                .contains("dwell")
        );
    }

    fn press(c: &mut Chords, vk: u32, now: Instant) -> ChordStep {
        c.event(vk, true, now)
    }

    #[test]
    fn leader_then_keys_fires_and_nothing_leaks_to_the_app() {
        let mut c = commands().chords().unwrap();
        let t0 = Instant::now();
        assert_eq!(press(&mut c, LALT, t0), ChordStep::default());
        let lead = press(&mut c, SPACE, t0);
        assert!(lead.swallow && lead.pending == Some(true));
        assert!(press(&mut c, SPACE, t0).swallow, "auto-repeat of the leader key");
        assert!(c.event(SPACE, false, t0).swallow);
        assert!(!c.event(LALT, false, t0).swallow);
        let g = press(&mut c, G, t0);
        assert!(
            g.swallow && g.fired.is_none() && g.pending.is_none(),
            "g is a prefix"
        );
        assert!(c.event(G, false, t0).swallow, "its key-up too");
        let s = press(&mut c, S, t0);
        assert_eq!(
            s,
            ChordStep {
                swallow: true,
                fired: Some(1),
                pending: Some(false)
            }
        );
        assert!(c.event(S, false, t0).swallow);
        assert!(!c.pending());
        assert_eq!(
            press(&mut c, T, t0),
            ChordStep::default(),
            "plain typing is the app's"
        );
        assert!(!c.event(T, false, t0).swallow);
    }

    #[test]
    fn modifiers_may_stay_held_and_wrong_keys_or_esc_cancel() {
        let mut c = commands().chords().unwrap();
        let t0 = Instant::now();
        press(&mut c, LALT, t0);
        press(&mut c, SPACE, t0);
        c.event(SPACE, false, t0);
        assert_eq!(press(&mut c, T, t0).fired, Some(0), "Alt still held");
        press(&mut c, SPACE, t0);
        let wrong = press(&mut c, X, t0);
        assert!(wrong.swallow && wrong.fired.is_none() && wrong.pending == Some(false));
        c.event(SPACE, false, t0);
        press(&mut c, SPACE, t0);
        let esc = press(&mut c, ESC, t0);
        assert!(esc.swallow && esc.pending == Some(false));
        assert!(c.event(ESC, false, t0).swallow);
    }

    #[test]
    fn chords_time_out_between_keys() {
        let mut c = commands().chords().unwrap();
        let t0 = Instant::now();
        press(&mut c, LALT, t0);
        press(&mut c, SPACE, t0);
        assert!(!c.expire(t0 + Duration::from_millis(1400)));
        press(&mut c, G, t0 + Duration::from_millis(1400));
        assert!(
            !c.expire(t0 + Duration::from_millis(2800)),
            "each key restarts the clock"
        );
        assert!(c.expire(t0 + Duration::from_millis(3000)));
        assert!(!c.pending());
        assert!(!c.expire(t0 + Duration::from_secs(9)), "nothing to cancel twice");
    }

    #[test]
    fn a_lost_key_up_never_eats_a_later_real_one() {
        let mut c = commands().chords().unwrap();
        let t0 = Instant::now();
        press(&mut c, LALT, t0);
        press(&mut c, SPACE, t0);
        c.event(SPACE, false, t0);
        c.event(LALT, false, t0);
        press(&mut c, X, t0); // swallowed, cancels; its key-up never arrives
        c.down.remove(&X);
        assert!(!press(&mut c, X, t0).swallow, "the app sees this X go down");
        assert!(!c.event(X, false, t0).swallow, "and go up");
    }

    #[test]
    fn the_leader_works_again_after_a_lost_release() {
        let mut c = commands().chords().unwrap();
        let t0 = Instant::now();
        press(&mut c, LALT, t0);
        press(&mut c, SPACE, t0);
        press(&mut c, T, t0); // fires; Alt and Space key-ups are lost
        c.verify(|_| false);
        press(&mut c, LALT, t0);
        assert_eq!(press(&mut c, SPACE, t0).pending, Some(true));
    }

    #[test]
    fn corners_fire_once_per_visit_after_the_dwell() {
        let mut h = commands().hot_corners().unwrap();
        let m = ScreenRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        assert_eq!(h.tick(500, 500, m, false, t0), None);
        assert_eq!(h.tick(0, 0, m, false, ms(0)), None);
        assert_eq!(h.tick(1, 1, m, false, ms(200)), None);
        assert_eq!(h.tick(0, 0, m, false, ms(250)), Some(2));
        assert_eq!(h.tick(0, 0, m, false, ms(900)), None, "once per visit");
        h.tick(40, 40, m, false, ms(950));
        h.tick(0, 0, m, false, ms(1000));
        assert_eq!(
            h.tick(0, 0, m, false, ms(1300)),
            Some(2),
            "re-armed after leaving"
        );
        h.tick(40, 40, m, false, ms(1350));
        h.tick(0, 0, m, true, ms(1400));
        assert_eq!(h.tick(0, 0, m, false, ms(1700)), None, "dragged in: never fires");
        assert_eq!(
            h.tick(1919, 0, m, false, ms(1800)),
            None,
            "top-right is not bound"
        );
        assert_eq!(h.tick(1919, 0, m, false, ms(2200)), None);
    }

    #[test]
    fn corners_are_per_monitor_and_two_pixels_deep() {
        let m = ScreenRect {
            left: -1280,
            top: 100,
            right: 0,
            bottom: 1124,
        };
        assert_eq!(Corner::at(-1280, 100, m), Some(Corner::TopLeft));
        assert_eq!(Corner::at(-1279, 101, m), Some(Corner::TopLeft));
        assert_eq!(Corner::at(-1278, 100, m), None);
        assert_eq!(Corner::at(-1, 1123, m), Some(Corner::BottomRight));
        assert_eq!(Corner::at(-1, 100, m), Some(Corner::TopRight));
        assert_eq!(Corner::at(-1280, 1122, m), Some(Corner::BottomLeft));
        assert_eq!(Corner::at(-600, 100, m), None);
    }

    #[test]
    fn fuzzy_ranks_word_starts_and_runs_first() {
        assert_eq!(fuzzy("", "anything"), Some(0));
        assert_eq!(fuzzy("xyz", "Git status"), None);
        assert!(fuzzy("gs", "Git status") > fuzzy("gs", "Ogres"));
        assert!(fuzzy("term", "Terminal") > fuzzy("term", "Open the remote"));
        assert_eq!(fuzzy("GIT", "git status"), fuzzy("git", "Git Status"));
        let names = ["Ogres", "Git status", "Docs", "Go to settings"];
        assert_eq!(rank("gs", names), vec![1, 3, 0]);
        assert_eq!(rank("", names), vec![0, 1, 2, 3], "empty query keeps the order");
    }

    #[test]
    fn exec_sets_states_and_reports_missing_programs() {
        let dir = std::env::temp_dir().join(format!("noren-cmd-{}", std::process::id()));
        exec(&Action::State("build".into(), Some(State::Fail)), Some(&dir)).unwrap();
        assert_eq!(state::read_all(&dir).unwrap().get("build"), Some(&State::Fail));
        exec(&Action::State("build".into(), None), Some(&dir)).unwrap();
        assert!(state::read_all(&dir).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let err = exec(&Action::Run(vec!["noren-no-such-program".into()]), None).unwrap_err();
        assert!(err.to_string().contains("noren-no-such-program"), "{err}");
        assert!(exec(&Action::Palette, None).is_err());
    }
}
