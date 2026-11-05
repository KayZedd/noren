//! Notifications in the profile's style: `noren notify "Build" "finished in 42 s" --state ok`
//! from any script shows a card in the palette's colors on `noren panel`, stacked in a corner,
//! gone after a few seconds or on a click.
//!
//! ```toml
//! [notifications]
//! anchor = "bottom_right"   # corner or edge of the monitor, default bottom_right
//! offset = [24, 24]         # from the anchored edges, logical pixels, default 24
//! monitor = 1               # same numbering as window rules, default the primary
//! width = 360               # card width, 200 to 800, default 360
//! timeout = 6               # seconds on screen, 0 keeps a card until it is clicked
//! max = 4                   # cards shown at once, 1 to 10; the rest wait their turn
//! ```
//!
//! Each `noren notify` writes one small TOML file into the queue directory (default
//! `%LOCALAPPDATA%\noren\notifications`); the panel picks the files up in order, shows them and
//! deletes them. A file is written under a temporary name and renamed, so the panel never
//! reads half of one. Windows' own toasts are not touched: an app without package identity
//! cannot read or restyle them.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::components::caption;
use crate::shell::State;
use crate::surface::{Anchor, Floating, FloatingConfig, Layer, View, ViewContext};
use crate::widget::Command;
use crate::{Color, Element, Theme, column, row, text};

/// How often the queue directory is read.
const POLL: Duration = Duration::from_millis(250);
/// Notes queued longer ago than this (while no panel ran) are dropped unseen.
const MAX_AGE: Duration = Duration::from_secs(3600);
/// Longest title and body kept, in characters.
const MAX_TITLE: usize = 200;
const MAX_BODY: usize = 2000;

/// `[notifications]` in a profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct NotifyConfig {
    pub anchor: Anchor,
    pub offset: (f32, f32),
    /// 1-based, like window rules; `None` is the primary monitor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor: Option<u32>,
    pub width: f32,
    /// Seconds a card stays; 0 keeps it until clicked.
    pub timeout: f32,
    pub max: u32,
}

impl Default for NotifyConfig {
    fn default() -> Self {
        Self {
            anchor: Anchor::BottomRight,
            offset: (24.0, 24.0),
            monitor: None,
            width: 360.0,
            timeout: 6.0,
            max: 4,
        }
    }
}

impl NotifyConfig {
    pub fn validate(&self) -> Result<(), String> {
        let err = |what: String| Err(format!("notifications: {what}"));
        let ok = |v: f32| (0.0..=10_000.0).contains(&v);
        if !ok(self.offset.0) || !ok(self.offset.1) {
            return err(format!("offset {:?} must be 0 to 10000", self.offset));
        }
        if self.monitor == Some(0) {
            return err("monitors count from 1".into());
        }
        if !(200.0..=800.0).contains(&self.width) {
            return err(format!("width {} is outside 200..=800", self.width));
        }
        if !(self.timeout == 0.0 || (1.0..=3600.0).contains(&self.timeout)) {
            return err(format!("timeout {} must be 0 or 1 to 3600 seconds", self.timeout));
        }
        if !(1..=10).contains(&self.max) {
            return err(format!("max {} is outside 1..=10", self.max));
        }
        Ok(())
    }
}

/// One notification, as queued by `noren notify`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Note {
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// `ok`, `fail`, `warn` or `busy`: colors the card's edge; none uses the accent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Seconds on screen, over the profile's `timeout`; 0 keeps it until clicked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f32>,
}

impl Note {
    pub fn validate(&self) -> Result<(), String> {
        if self.title.trim().is_empty() {
            return Err("a notification needs a title".into());
        }
        if let Some(s) = &self.state {
            s.parse::<State>()?;
        }
        if let Some(t) = self.timeout
            && !(t == 0.0 || (1.0..=3600.0).contains(&t))
        {
            return Err(format!("timeout {t} must be 0 or 1 to 3600 seconds"));
        }
        Ok(())
    }

    /// Title and body cut to a sane length, so a runaway script cannot fill the screen.
    fn trimmed(mut self) -> Self {
        let cut = |s: &mut String, n: usize| {
            if let Some((i, _)) = s.char_indices().nth(n) {
                s.truncate(i);
                s.push('…');
            }
        };
        cut(&mut self.title, MAX_TITLE);
        cut(&mut self.body, MAX_BODY);
        self
    }
}

/// `%LOCALAPPDATA%\noren\notifications`.
pub fn default_dir() -> Option<PathBuf> {
    Some(crate::rice::data_dir()?.join("notifications"))
}

/// Queues `note` in `dir` for the panel to show. Returns the file written.
pub fn post(dir: &Path, note: &Note) -> Result<PathBuf, String> {
    note.validate()?;
    let io = |e: io::Error| format!("{}: {e}", dir.display());
    fs::create_dir_all(dir).map_err(io)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    // Sorts by time; the pid and a counter keep two posts in the same instant apart.
    let stem = format!("{nanos:024}-{:010}", std::process::id());
    let body = toml::to_string(note).map_err(|e| e.to_string())?;
    for n in 0..100u32 {
        let path = dir.join(format!("{stem}-{n:02}.toml"));
        if path.exists() {
            continue;
        }
        let tmp = dir.join(format!(".{stem}-{n:02}.tmp"));
        fs::write(&tmp, &body).map_err(io)?;
        fs::rename(&tmp, &path).map_err(io)?;
        return Ok(path);
    }
    Err(format!("{}: too many notifications at once", dir.display()))
}

/// The queued notifications in `dir`, oldest first, each file deleted once read. Files that
/// do not parse are deleted too and reported once on stderr; files older than an hour are
/// deleted unshown; temporary files are left alone.
pub fn take(dir: &Path) -> Vec<Note> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|x| x == "toml")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| !n.starts_with('.'))
        })
        .collect();
    paths.sort();
    let mut out = Vec::new();
    for path in paths {
        let stale = fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > MAX_AGE);
        if stale {
            let _ = fs::remove_file(&path);
            continue;
        }
        let parsed = fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| toml::from_str::<Note>(&t).map_err(|e| e.to_string()))
            .and_then(|n| n.validate().map(|()| n));
        let _ = fs::remove_file(&path);
        match parsed {
            Ok(n) => out.push(n.trimmed()),
            Err(e) => eprintln!("noren: notification {}: {e}", path.display()),
        }
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Dismiss(u64),
}

struct Shown {
    id: u64,
    note: Note,
    until: Option<Instant>,
}

/// The stack of cards, fed from the queue directory.
pub struct Toasts {
    config: NotifyConfig,
    dir: Option<PathBuf>,
    shown: Vec<Shown>,
    waiting: VecDeque<Note>,
    next_id: u64,
    checked: Option<Instant>,
}

impl Toasts {
    /// Reads the queue in `dir`; `None` only shows what [`Toasts::push`] hands it.
    pub fn new(config: NotifyConfig, dir: Option<PathBuf>) -> Self {
        Self {
            config,
            dir,
            shown: Vec::new(),
            waiting: VecDeque::new(),
            next_id: 0,
            checked: None,
        }
    }

    /// The stack in a window of its own, above other windows.
    pub fn surface(self, theme: Theme) -> Floating<Self> {
        let config = FloatingConfig {
            anchor: self.config.anchor,
            offset: self.config.offset,
            layer: Layer::Top,
            hide_on_fullscreen: true,
            ..Default::default()
        };
        Floating::new(self, config, theme)
    }

    pub fn push(&mut self, note: Note) {
        self.waiting.push_back(note);
    }

    /// Titles on screen, top to bottom as drawn.
    pub fn titles(&self) -> Vec<&str> {
        self.order().map(|s| s.note.title.as_str()).collect()
    }

    /// Newest nearest the anchored edge: at the bottom for bottom anchors, else on top.
    fn order(&self) -> Box<dyn Iterator<Item = &Shown> + '_> {
        let bottom = matches!(
            self.config.anchor,
            Anchor::BottomLeft | Anchor::Bottom | Anchor::BottomRight
        );
        if bottom {
            Box::new(self.shown.iter())
        } else {
            Box::new(self.shown.iter().rev())
        }
    }

    /// Expires old cards and brings waiting ones on. `true` when the stack changed.
    fn advance(&mut self, now: Instant) -> bool {
        let before = self.shown.len();
        self.shown.retain(|s| s.until.is_none_or(|u| now < u));
        let mut changed = self.shown.len() != before;
        while self.shown.len() < self.config.max as usize {
            let Some(note) = self.waiting.pop_front() else {
                break;
            };
            let secs = note.timeout.unwrap_or(self.config.timeout);
            let until = (secs > 0.0).then(|| now + Duration::from_secs_f32(secs));
            self.shown.push(Shown {
                id: self.next_id,
                note,
                until,
            });
            self.next_id += 1;
            changed = true;
        }
        changed
    }

    fn card(&self, s: &Shown, theme: &Theme) -> Element<Msg> {
        let edge = s
            .note
            .state
            .as_deref()
            .and_then(|v| v.parse::<State>().ok())
            .map_or(theme.highlight, |st| state_color(st, theme));
        let spacing = theme.units.small_spacing;
        let mut words = column()
            .gap(spacing / 2.0)
            .grow()
            .child(text(s.note.title.clone()).bold().wrap());
        if !s.note.body.is_empty() {
            words = words.child(text(s.note.body.clone()).wrap().opacity(0.85));
        }
        if s.until.is_none() {
            words = words.child(caption(theme, "click to dismiss"));
        }
        row()
            .width(self.config.width)
            .gap(spacing * 2.0)
            .padding(spacing * 2.0)
            .background(theme.background.with_alpha(theme.panel_opacity.max(0.9)))
            .border(edge.with_alpha(0.6), 1.0)
            .radius(theme.units.corner_radius * 2.0)
            .child(row().width(4.0).fill_height().background(edge).radius(2.0))
            .child(words)
            .on_click(Msg::Dismiss(s.id))
    }
}

/// The theme's status color for a state.
fn state_color(state: State, theme: &Theme) -> Color {
    match state {
        State::Ok => theme.positive,
        State::Warn => theme.neutral,
        State::Fail => theme.negative,
        State::Busy => theme.highlight,
    }
}

impl View for Toasts {
    type Message = Msg;

    fn poll(&mut self, now: Instant) -> bool {
        if let Some(dir) = &self.dir
            && self.checked.is_none_or(|t| now.duration_since(t) >= POLL)
        {
            self.checked = Some(now);
            let notes = take(dir);
            self.waiting.extend(notes);
        }
        self.advance(now)
    }

    fn view(&self, ctx: &ViewContext<'_>) -> Element<Msg> {
        let mut stack = column().gap(ctx.theme.units.small_spacing * 2.0);
        for s in self.order() {
            stack = stack.child(self.card(s, ctx.theme));
        }
        stack
    }

    fn update(&mut self, message: Msg, _now: Instant) -> Command {
        let Msg::Dismiss(id) = message;
        self.shown.retain(|s| s.id != id);
        Command::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::{MonitorInfo, Surface};

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("noren-notify-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn note(title: &str) -> Note {
        Note {
            title: title.into(),
            ..Default::default()
        }
    }

    #[test]
    fn configs_and_notes_validate_with_named_errors() {
        let c: NotifyConfig = toml::from_str("").unwrap();
        assert_eq!(c, NotifyConfig::default());
        c.validate().unwrap();
        for (bad, says) in [
            ("width = 100", "notifications: width 100"),
            ("timeout = 0.5", "timeout 0.5"),
            ("max = 0", "max 0"),
            ("monitor = 0", "count from 1"),
            ("offset = [-1, 0]", "offset"),
        ] {
            let c: NotifyConfig = toml::from_str(bad).unwrap();
            let e = c.validate().err().unwrap();
            assert!(e.contains(says), "{bad}: {e}");
        }
        assert!(toml::from_str::<NotifyConfig>("size = 1").is_err());
        assert!(note(" ").validate().unwrap_err().contains("needs a title"));
        let bad = Note {
            state: Some("red".into()),
            ..note("x")
        };
        assert!(bad.validate().unwrap_err().contains("unknown state"));
        let bad = Note {
            timeout: Some(-1.0),
            ..note("x")
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn posted_notes_come_back_in_order_once_and_junk_is_dropped() {
        let dir = temp("queue");
        assert!(take(&dir).is_empty(), "a missing directory is an empty queue");
        let first = Note {
            body: "body".into(),
            state: Some("ok".into()),
            timeout: Some(0.0),
            ..note("first")
        };
        post(&dir, &first).unwrap();
        post(&dir, &note("second")).unwrap();
        assert!(post(&dir, &note("")).is_err());
        fs::write(dir.join("zzz.toml"), "title = 3").unwrap();
        fs::write(dir.join(".half.tmp"), "title = \"x\"").unwrap();
        let notes = take(&dir);
        assert_eq!(notes, vec![first, note("second")]);
        assert!(take(&dir).is_empty());
        // The temporary file of a post in progress is left for its writer.
        assert!(dir.join(".half.tmp").exists());
        // A note left from hours ago, while no panel ran, is dropped.
        let old = post(&dir, &note("old")).unwrap();
        fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_modified(SystemTime::now() - MAX_AGE * 2)
            .unwrap();
        assert!(take(&dir).is_empty());
        assert!(!old.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn long_text_is_cut() {
        let n = Note {
            title: "t".repeat(MAX_TITLE + 50),
            body: "ż".repeat(MAX_BODY + 1),
            ..Default::default()
        }
        .trimmed();
        assert_eq!(n.title.chars().count(), MAX_TITLE + 1);
        assert!(n.body.ends_with('…'));
        assert_eq!(n.body.chars().count(), MAX_BODY + 1);
    }

    #[test]
    fn cards_stack_expire_wait_their_turn_and_dismiss_on_click() {
        let config = NotifyConfig {
            max: 2,
            timeout: 5.0,
            ..Default::default()
        };
        let mut t = Toasts::new(config, None);
        let start = Instant::now();
        t.push(note("a"));
        t.push(Note {
            timeout: Some(0.0),
            ..note("b")
        });
        t.push(note("c"));
        assert!(t.poll(start));
        // Bottom anchor: newest at the bottom; "c" waits for a free slot.
        assert_eq!(t.titles(), vec!["a", "b"]);
        assert!(!t.poll(start + Duration::from_secs(1)));
        // "a" times out, "c" comes on; "b" stays until clicked.
        assert!(t.poll(start + Duration::from_secs(6)));
        assert_eq!(t.titles(), vec!["b", "c"]);
        let b = t.shown[0].id;
        t.update(Msg::Dismiss(b), start);
        assert_eq!(t.titles(), vec!["c"]);
        // A top anchor puts the newest on top.
        let mut top = Toasts::new(
            NotifyConfig {
                anchor: Anchor::TopRight,
                ..Default::default()
            },
            None,
        );
        top.push(note("old"));
        top.push(note("new"));
        top.poll(start);
        assert_eq!(top.titles(), vec!["new", "old"]);
    }

    #[test]
    fn the_surface_hides_while_empty_and_sits_in_its_corner() {
        let dir = temp("surface");
        let mut s = Toasts::new(NotifyConfig::default(), Some(dir.clone())).surface(Theme::aizome());
        s.set_monitor(MonitorInfo {
            width: 1600.0,
            height: 900.0,
            scale: 1.0,
            origin: (0, 0),
        });
        let now = Instant::now();
        s.tick(now, None, false);
        assert!(!s.placement(now).visible);
        post(&dir, &note("Build")).unwrap();
        let later = now + POLL;
        assert!(s.tick(later, None, false).redraw);
        let p = s.placement(later);
        assert!(p.visible);
        assert_eq!(p.layer, Layer::Top);
        assert_eq!(p.rect.w, 360.0);
        assert_eq!(p.rect.x + p.rect.w, 1600.0 - 24.0);
        assert_eq!(p.rect.y + p.rect.h, 900.0 - 24.0);
        // A click on the card dismisses it.
        s.click(p.rect.x + 10.0, p.rect.y + 10.0, later);
        s.tick(later, None, false);
        assert!(!s.placement(later).visible);
        let _ = fs::remove_dir_all(&dir);
    }
}
