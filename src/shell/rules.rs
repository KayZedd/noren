//! Declarative window rules: `[[window]]` tables in a profile, matched against every top-level
//! window, resolved to a [`Style`], applied as DWM attribute writes.
//!
//! ```toml
//! [[window]]                 # no match fields: every window
//! corners = "round"
//!
//! [[window]]
//! exe = "WindowsTerminal.exe"  # globs, case-insensitive: exe file name, class, title
//! backdrop = "acrylic"
//! border = "state:build"       # red while `noren state build fail`
//!
//! [[window]]
//! title = "*- Visual Studio Code"
//! border = "accent"            # accent | neutral | positive | warning | negative, ".N" for a step
//! caption = "neutral.2"
//! title_text = "#dcd7ba"
//! dark = true
//! opacity = 0.95
//! monitor = 2                  # 1 = primary, then left to right; moved once, when first seen
//! ```
//!
//! Later rules override earlier ones field by field. Everything here is pure; the Win32 side
//! lives in `shell::win`.

use std::collections::BTreeMap;
use std::fmt;

use glob::{MatchOptions, Pattern};
use serde::{Deserialize, Serialize};

use super::state::State;
use crate::Color;
use crate::palette::{Palette, Ramp, export::colorref};

/// `DWMWA_COLOR_DEFAULT`: let the system pick.
pub const COLOR_DEFAULT: u32 = 0xFFFF_FFFF;
/// `DWMWA_COLOR_NONE`: draw no border.
pub const COLOR_NONE: u32 = 0xFFFF_FFFE;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowRule {
    /// Glob on the executable's file name, e.g. `"Code.exe"`, `"*.exe"`.
    pub exe: Option<String>,
    /// Glob on the window class, e.g. `"CASCADIA_HOSTING_WINDOW_CLASS"`.
    pub class: Option<String>,
    /// Glob on the window title.
    pub title: Option<String>,
    pub corners: Option<Corners>,
    pub backdrop: Option<Backdrop>,
    pub border: Option<ColorSpec>,
    pub caption: Option<ColorSpec>,
    pub title_text: Option<ColorSpec>,
    /// Dark title bar and frame.
    pub dark: Option<bool>,
    /// Whole-window opacity, `0.1..=1.0`.
    pub opacity: Option<f32>,
    pub monitor: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Corners {
    Default,
    Square,
    Round,
    RoundSmall,
}

impl Corners {
    /// `DWM_WINDOW_CORNER_PREFERENCE`.
    pub fn dwm(self) -> u32 {
        match self {
            Self::Default => 0,
            Self::Square => 1,
            Self::Round => 2,
            Self::RoundSmall => 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backdrop {
    Auto,
    None,
    Mica,
    Acrylic,
    Tabbed,
}

impl Backdrop {
    /// `DWM_SYSTEMBACKDROP_TYPE`.
    pub fn dwm(self) -> u32 {
        match self {
            Self::Auto => 0,
            Self::None => 1,
            Self::Mica => 2,
            Self::Acrylic => 3,
            Self::Tabbed => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Neutral,
    Accent,
    Positive,
    Warning,
    Negative,
}

impl Role {
    fn ramp(self, p: &Palette) -> &Ramp {
        match self {
            Self::Neutral => &p.neutral,
            Self::Accent => &p.accent,
            Self::Positive => &p.positive,
            Self::Warning => &p.warning,
            Self::Negative => &p.negative,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Neutral => "neutral",
            Self::Accent => "accent",
            Self::Positive => "positive",
            Self::Warning => "warning",
            Self::Negative => "negative",
        }
    }
}

/// A color in a rule: `"default"`, `"none"`, `"#rrggbb"`, a palette role with an optional
/// step (`"accent"` is `"accent.9"`), or `"state:<name>"`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum ColorSpec {
    Default,
    None,
    Hex(Color),
    Role(Role, usize),
    State(String),
}

impl TryFrom<String> for ColorSpec {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        let bad = || {
            format!(
                "bad color {s:?}: want default, none, #rrggbb, accent|neutral|positive|warning|negative[.1-12] or state:<name>"
            )
        };
        if s == "default" {
            return Ok(Self::Default);
        }
        if s == "none" {
            return Ok(Self::None);
        }
        if s.starts_with('#') {
            return Color::from_hex(&s).map(Self::Hex).ok_or_else(bad);
        }
        if let Some(name) = s.strip_prefix("state:") {
            return if super::state::valid_name(name) {
                Ok(Self::State(name.into()))
            } else {
                Err(bad())
            };
        }
        let (role, step) = match s.split_once('.') {
            Some((r, n)) => (
                r,
                n.parse::<usize>()
                    .ok()
                    .filter(|n| (1..=12).contains(n))
                    .ok_or_else(bad)?,
            ),
            None => (s.as_str(), 9),
        };
        let role = [
            Role::Neutral,
            Role::Accent,
            Role::Positive,
            Role::Warning,
            Role::Negative,
        ]
        .into_iter()
        .find(|r| r.name() == role)
        .ok_or_else(bad)?;
        Ok(Self::Role(role, step))
    }
}

impl From<ColorSpec> for String {
    fn from(c: ColorSpec) -> String {
        c.to_string()
    }
}

impl fmt::Display for ColorSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Default => f.write_str("default"),
            Self::None => f.write_str("none"),
            Self::Hex(c) => f.write_str(&c.to_hex()),
            Self::Role(r, 9) => f.write_str(r.name()),
            Self::Role(r, n) => write!(f, "{}.{n}", r.name()),
            Self::State(n) => write!(f, "state:{n}"),
        }
    }
}

impl ColorSpec {
    /// DWM color value, or `None` for a state that is not set (the attribute is left at, or
    /// returned to, the system default).
    pub fn resolve(&self, p: &Palette, states: &BTreeMap<String, State>) -> Option<u32> {
        match self {
            Self::Default => Some(COLOR_DEFAULT),
            Self::None => Some(COLOR_NONE),
            Self::Hex(c) => Some(colorref(*c)),
            Self::Role(r, n) => Some(colorref(r.ramp(p).step(*n))),
            Self::State(name) => states.get(name).map(|s| colorref(s.color(p))),
        }
    }
}

/// What a window should look like. `None` fields are not managed: noren leaves them as the
/// app set them, or puts back the original when a rule stops applying.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Style {
    pub corners: Option<u32>,
    pub backdrop: Option<u32>,
    pub border: Option<u32>,
    pub caption: Option<u32>,
    pub title_text: Option<u32>,
    pub dark: Option<bool>,
    /// Alpha `1..=255`.
    pub opacity: Option<u8>,
    pub monitor: Option<u32>,
}

/// The facts rules match on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WindowInfo {
    /// File name of the process image, e.g. `Code.exe`; empty when it cannot be read (an
    /// elevated process seen from a normal one).
    pub exe: String,
    pub class: String,
    pub title: String,
}

struct Compiled {
    exe: Option<Pattern>,
    class: Option<Pattern>,
    title: Option<Pattern>,
}

/// Rules checked and with their globs compiled.
pub struct Rules {
    rules: Vec<WindowRule>,
    compiled: Vec<Compiled>,
}

const MATCH: MatchOptions = MatchOptions {
    case_sensitive: false,
    require_literal_separator: false,
    require_literal_leading_dot: false,
};

impl Rules {
    pub fn new(rules: &[WindowRule]) -> Result<Self, String> {
        let pattern = |i: usize, field: &str, g: &Option<String>| {
            g.as_deref()
                .map(|g| Pattern::new(g).map_err(|e| format!("window rule {}: {field} {g:?}: {e}", i + 1)))
                .transpose()
        };
        let mut compiled = Vec::with_capacity(rules.len());
        for (i, r) in rules.iter().enumerate() {
            if let Some(o) = r.opacity
                && !(0.1..=1.0).contains(&o)
            {
                return Err(format!("window rule {}: opacity {o} is outside 0.1..=1.0", i + 1));
            }
            if r.monitor == Some(0) {
                return Err(format!("window rule {}: monitors count from 1", i + 1));
            }
            compiled.push(Compiled {
                exe: pattern(i, "exe", &r.exe)?,
                class: pattern(i, "class", &r.class)?,
                title: pattern(i, "title", &r.title)?,
            });
        }
        Ok(Self {
            rules: rules.to_vec(),
            compiled,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Indices of the rules that match, in order.
    pub fn matching(&self, w: &WindowInfo) -> Vec<usize> {
        let ok = |p: &Option<Pattern>, s: &str| p.as_ref().is_none_or(|p| p.matches_with(s, MATCH));
        self.compiled
            .iter()
            .enumerate()
            .filter(|(_, c)| ok(&c.exe, &w.exe) && ok(&c.class, &w.class) && ok(&c.title, &w.title))
            .map(|(i, _)| i)
            .collect()
    }

    /// Merges every matching rule, later ones winning per field.
    pub fn resolve(&self, w: &WindowInfo, p: &Palette, states: &BTreeMap<String, State>) -> Style {
        let mut s = Style::default();
        for i in self.matching(w) {
            let r = &self.rules[i];
            let color = |spec: &Option<ColorSpec>, cur: Option<u32>| match spec {
                Some(spec) => spec.resolve(p, states),
                None => cur,
            };
            s.corners = r.corners.map(Corners::dwm).or(s.corners);
            s.backdrop = r.backdrop.map(Backdrop::dwm).or(s.backdrop);
            s.border = color(&r.border, s.border);
            s.caption = color(&r.caption, s.caption);
            s.title_text = color(&r.title_text, s.title_text);
            s.dark = r.dark.or(s.dark);
            s.opacity = r.opacity.map(|o| (o * 255.0).round() as u8).or(s.opacity);
            s.monitor = r.monitor.or(s.monitor);
        }
        s
    }

    /// True when some rule reads `state:<name>`; the runtime only watches states when needed.
    pub fn uses_states(&self) -> bool {
        self.rules.iter().any(|r| {
            [&r.border, &r.caption, &r.title_text]
                .into_iter()
                .any(|c| matches!(c, Some(ColorSpec::State(_))))
        })
    }
}

/// One attribute write. Values are the raw DWM ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Corners(u32),
    Backdrop(u32),
    Border(u32),
    Caption(u32),
    TitleText(u32),
    /// `None`: put back what the window had before noren touched it.
    Dark(Option<bool>),
    /// `None`: put back the original opacity.
    Opacity(Option<u8>),
}

/// The writes that take a window from `prev` to `next`. A field that becomes unmanaged is
/// returned to the system default, so `ops(applied, &Style::default())` undoes everything.
/// `monitor` is not here: windows are moved once, not held.
pub fn ops(prev: &Style, next: &Style) -> Vec<Op> {
    let mut out = Vec::new();
    let mut num = |a: Option<u32>, b: Option<u32>, default: u32, op: fn(u32) -> Op| {
        if a != b {
            out.push(op(b.unwrap_or(default)));
        }
    };
    num(prev.corners, next.corners, Corners::Default.dwm(), Op::Corners);
    num(prev.backdrop, next.backdrop, Backdrop::Auto.dwm(), Op::Backdrop);
    num(prev.border, next.border, COLOR_DEFAULT, Op::Border);
    num(prev.caption, next.caption, COLOR_DEFAULT, Op::Caption);
    num(prev.title_text, next.title_text, COLOR_DEFAULT, Op::TitleText);
    if prev.dark != next.dark {
        out.push(Op::Dark(next.dark));
    }
    if prev.opacity != next.opacity {
        out.push(Op::Opacity(next.opacity));
    }
    out
}

/// Screen rectangle in physical pixels, right and bottom exclusive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl ScreenRect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

/// Monitor numbering for `monitor = N`: primary is 1, the rest left to right, then top to
/// bottom. Returns indices into `monitors` (`(work area, is_primary)`).
pub fn monitor_order(monitors: &[(ScreenRect, bool)]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..monitors.len()).collect();
    idx.sort_by_key(|&i| {
        let (r, primary) = monitors[i];
        (!primary, r.left, r.top)
    });
    idx
}

/// `window` moved from work area `from` to `to`, at the same relative position, shrunk if it
/// is larger than the target and kept fully inside it.
pub fn relocate(window: ScreenRect, from: ScreenRect, to: ScreenRect) -> ScreenRect {
    let w = window.width().min(to.width());
    let h = window.height().min(to.height());
    let rel = |pos: i32, start: i32, span: i32, size: i32| {
        let free = (span - size).max(1);
        f64::from(pos - start) / f64::from(free)
    };
    let fx = rel(window.left, from.left, from.width(), window.width()).clamp(0.0, 1.0);
    let fy = rel(window.top, from.top, from.height(), window.height()).clamp(0.0, 1.0);
    let left = to.left + (f64::from(to.width() - w) * fx).round() as i32;
    let top = to.top + (f64::from(to.height() - h) * fy).round() as i32;
    ScreenRect {
        left,
        top,
        right: left + w,
        bottom: top + h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::PaletteSpec;

    fn palette() -> Palette {
        Palette::generate(&PaletteSpec::new(Color::from_hex("#7e9cd8").unwrap()))
    }

    fn info(exe: &str, class: &str, title: &str) -> WindowInfo {
        WindowInfo {
            exe: exe.into(),
            class: class.into(),
            title: title.into(),
        }
    }

    fn parse(toml_text: &str) -> Vec<WindowRule> {
        #[derive(Deserialize)]
        struct Doc {
            window: Vec<WindowRule>,
        }
        toml::from_str::<Doc>(toml_text).unwrap().window
    }

    #[test]
    fn color_specs_parse_and_print_back() {
        for s in [
            "default",
            "none",
            "#7e9cd8",
            "accent",
            "neutral.2",
            "negative.11",
            "state:build",
        ] {
            let spec = ColorSpec::try_from(s.to_string()).unwrap();
            assert_eq!(spec.to_string(), s);
        }
        assert_eq!(
            ColorSpec::try_from("accent.9".to_string()).unwrap(),
            ColorSpec::Role(Role::Accent, 9)
        );
        for bad in [
            "red",
            "accent.0",
            "accent.13",
            "accent.x",
            "state:",
            "state:../x",
            "#12",
        ] {
            assert!(ColorSpec::try_from(bad.to_string()).is_err(), "{bad}");
        }
    }

    #[test]
    fn globs_match_case_insensitively_and_all_fields_must_match() {
        let rules = Rules::new(&parse(
            r#"
            [[window]]
            corners = "round"
            [[window]]
            exe = "code.EXE"
            [[window]]
            exe = "Code.exe"
            title = "*noren*"
            "#,
        ))
        .unwrap();
        assert_eq!(
            rules.matching(&info("Code.exe", "Chrome_WidgetWin_1", "x - noren - VS")),
            [0, 1, 2]
        );
        assert_eq!(rules.matching(&info("Code.exe", "", "other")), [0, 1]);
        assert_eq!(rules.matching(&info("notepad.exe", "", "noren")), [0]);
    }

    #[test]
    fn later_rules_override_field_by_field() {
        let p = palette();
        let rules = Rules::new(&parse(
            r##"
            [[window]]
            corners = "square"
            border = "accent"
            opacity = 0.5
            [[window]]
            exe = "wt.exe"
            border = "#ff0000"
            dark = true
            "##,
        ))
        .unwrap();
        let none = BTreeMap::new();
        let s = rules.resolve(&info("wt.exe", "", ""), &p, &none);
        assert_eq!(s.corners, Some(1));
        assert_eq!(s.border, Some(0x0000_00ff), "COLORREF is 0x00BBGGRR");
        assert_eq!(s.dark, Some(true));
        assert_eq!(s.opacity, Some(128));
        let other = rules.resolve(&info("x.exe", "", ""), &p, &none);
        assert_eq!(other.border, Some(colorref(p.accent.step(9))));
        assert_eq!(other.dark, None);
    }

    #[test]
    fn state_borders_follow_the_state_and_vanish_when_cleared() {
        let p = palette();
        let rules = Rules::new(&parse("[[window]]\nborder = \"state:build\"\n")).unwrap();
        assert!(rules.uses_states());
        let w = info("a.exe", "", "");
        let mut states = BTreeMap::new();
        assert_eq!(rules.resolve(&w, &p, &states).border, None);
        states.insert("build".to_string(), State::Fail);
        let failing = rules.resolve(&w, &p, &states);
        assert_eq!(failing.border, Some(colorref(p.negative.step(9))));
        states.clear();
        let cleared = rules.resolve(&w, &p, &states);
        assert_eq!(ops(&failing, &cleared), [Op::Border(COLOR_DEFAULT)]);
    }

    #[test]
    fn ops_write_only_changes_and_undo_returns_defaults() {
        let a = Style {
            corners: Some(1),
            border: Some(5),
            dark: Some(true),
            opacity: Some(200),
            monitor: Some(2),
            ..Style::default()
        };
        assert!(ops(&a, &a).is_empty());
        assert_eq!(
            ops(&Style::default(), &a),
            [
                Op::Corners(1),
                Op::Border(5),
                Op::Dark(Some(true)),
                Op::Opacity(Some(200))
            ],
            "monitor is not an attribute write"
        );
        assert_eq!(
            ops(&a, &Style::default()),
            [
                Op::Corners(0),
                Op::Border(COLOR_DEFAULT),
                Op::Dark(None),
                Op::Opacity(None)
            ]
        );
        let b = Style { border: Some(6), ..a };
        assert_eq!(ops(&a, &b), [Op::Border(6)]);
    }

    #[test]
    fn invalid_rules_are_rejected_with_their_position() {
        let err = |t: &str| Rules::new(&parse(t)).err().unwrap();
        assert!(err("[[window]]\n[[window]]\nopacity = 1.5\n").contains("rule 2"));
        assert!(err("[[window]]\nmonitor = 0\n").contains("count from 1"));
        assert!(err("[[window]]\nexe = \"[\"\n").contains("exe"));
        #[derive(Deserialize, Debug)]
        #[allow(dead_code)]
        struct Doc {
            window: Vec<WindowRule>,
        }
        assert!(toml::from_str::<Doc>("[[window]]\ncorners = \"blob\"\n").is_err());
        assert!(toml::from_str::<Doc>("[[window]]\nborder = \"red\"\n").is_err());
        assert!(toml::from_str::<Doc>("[[window]]\ntypo = 1\n").is_err());
    }

    #[test]
    fn monitors_number_primary_first_then_left_to_right() {
        let r = |l: i32| ScreenRect {
            left: l,
            top: 0,
            right: l + 100,
            bottom: 100,
        };
        let order = monitor_order(&[(r(1920), false), (r(-1280), false), (r(0), true)]);
        assert_eq!(order, [2, 1, 0]);
    }

    #[test]
    fn relocate_keeps_relative_position_and_fits_the_target() {
        let a = ScreenRect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let b = ScreenRect {
            left: 1920,
            top: 0,
            right: 1920 + 1280,
            bottom: 680,
        };
        // Centered 800x600 stays centered.
        let w = ScreenRect {
            left: 560,
            top: 220,
            right: 1360,
            bottom: 820,
        };
        let moved = relocate(w, a, b);
        assert_eq!((moved.width(), moved.height()), (800, 600));
        assert_eq!((moved.left, moved.top), (1920 + 240, 40));
        // Too big: shrunk to the target, pinned inside.
        let huge = ScreenRect {
            left: -50,
            top: 0,
            right: 1900,
            bottom: 1040,
        };
        let fitted = relocate(huge, a, b);
        assert_eq!(fitted, b);
    }
}
