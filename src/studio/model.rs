//! The studio's pure side: settings, profile text edits, theme colors and toasts. No GUI
//! types here, so all of it is covered by the plain test run.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Table, value};

use crate::Color;
use crate::palette::{Contrast, Mode, Oklch, Palette, PaletteSpec};
use crate::rice::Profile;

/// Aizome's indigo: the studio's look when no profile is open, and the accent of a new
/// profile.
pub const DEFAULT_ACCENT: &str = "#7d97d4";

pub fn default_spec() -> PaletteSpec {
    PaletteSpec::new(Color::from_hex(DEFAULT_ACCENT).expect("valid hex"))
}

/// Starting point for "New profile": the Aizome palette and one gentle window rule.
pub const NEW_PROFILE: &str = r##"schema = 1
name = "my-rice"

[palette]
accent = "#7d97d4"   # every other color is derived from this one
mode = "dark"
contrast = "standard"

[terminal]
scheme = "noren"

[vscode]

[[window]]
corners = "round"
border = "accent.7"

# [focus]
# dim = 0.3

# [xray]
# key = "Ctrl+Alt+Space"

# [[glow]]               # light the top edge from `noren state ci ok|fail|warn|busy`
# state = "ci"
# edges = ["top"]

# [[widget]]             # any program printing a line or waybar JSON, shown by `noren panel`
# name = "weather"
# run = ["curl", "-s", "wttr.in/?format=%t"]
# interval = 600
# [widget.glue]          # optional: ride on an app's window instead of the panel
# exe = "Code.exe"
# anchor = "top_right"
# [widget.desktop]       # or sit on the wallpaper, placed and colored to read on it
# text = "auto"

# [media]                # now playing on `noren panel`, colored from the album cover
# cover_colors = true

# [[layout]]             # which panel widgets show while an app is in front or on a desktop
# exe = ["Code.exe"]
# widgets = ["weather"]

# [schedule]             # light from sunrise, dark from 21:00 while Live holds the profile
# light = "sunrise"
# dark = "21:00"
# latitude = 52.23
# longitude = 21.01

# [switcher]             # Alt+Tab with live window thumbnails while Live holds the profile
# key = "Alt+Tab"

# [[mirror]]             # a live copy of a window, kept in the corner while others cover it
# exe = "Spotify.exe"
# anchor = "bottom_right"
"##;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    En,
    Pl,
}

impl Lang {
    pub fn pick<'a>(self, en: &'a str, pl: &'a str) -> &'a str {
        match self {
            Self::En => en,
            Self::Pl => pl,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub profile: Option<PathBuf>,
    pub lang: Lang,
    /// Stops the background drift and shortens transitions.
    pub reduced_motion: bool,
    /// Start holding the profile as soon as the studio opens.
    pub live_on_start: bool,
    /// Do not ask GitHub for a newer release when the studio opens.
    pub skip_update_check: bool,
}

impl Settings {
    /// `%LOCALAPPDATA%\noren\studio.json`.
    pub fn default_path() -> Option<PathBuf> {
        Some(crate::rice::data_dir()?.join("studio.json"))
    }

    /// Missing or unreadable settings are defaults: a broken file must not keep the studio
    /// from opening.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(
            &tmp,
            serde_json::to_string_pretty(self).map_err(io::Error::other)?,
        )?;
        std::fs::rename(tmp, path)
    }
}

/// Rewrites the `[palette]` table of a profile's text to `spec`, keeping every comment and
/// every other table as written. The result is checked as a whole profile.
pub fn set_palette(text: &str, spec: &PaletteSpec) -> Result<String, String> {
    let mut doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| e.to_string())?;
    let table = doc
        .entry("palette")
        .or_insert_with(|| Item::Table(Table::new()))
        .as_table_like_mut()
        .ok_or("[palette] is not a table")?;
    let set = |t: &mut dyn toml_edit::TableLike, key: &str, v: &str| match t.get_mut(key) {
        // Keeps the trailing comment on the line.
        Some(Item::Value(old)) => {
            let decor = old.decor().clone();
            *old = v.into();
            *old.decor_mut() = decor;
        }
        _ => {
            t.insert(key, value(v));
        }
    };
    set(table, "accent", &spec.accent.to_hex());
    set(
        table,
        "mode",
        match spec.mode {
            Mode::Dark => "dark",
            Mode::Light => "light",
        },
    );
    set(
        table,
        "contrast",
        match spec.contrast {
            Contrast::Standard => "standard",
            Contrast::High => "high",
        },
    );
    let out = doc.to_string();
    Profile::parse(&out)?;
    Ok(out)
}

/// Background aurora: the accent and three hue-shifted companions, softened. With the
/// default accent they land near Aizome's fuji, sora and asagi.
pub fn aurora(p: &Palette) -> [Color; 4] {
    let seed = p.accent.step(9).to_oklch();
    let dark = p.spec.mode == Mode::Dark;
    let l = if dark { 0.62 } else { 0.80 };
    let c = seed.c.clamp(0.04, 0.11);
    [0.0, 40.0, -40.0, -85.0].map(|dh| {
        Oklch::new(
            if dh == 0.0 { seed.l.min(l + 0.05) } else { l },
            c,
            (seed.h + dh).rem_euclid(360.0),
        )
        .to_color()
    })
}

/// The studio's pages, in sidebar order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Palette,
    Profile,
    Apply,
    Live,
    Windows,
    States,
    History,
    Settings,
}

impl Page {
    pub const ALL: [Page; 8] = [
        Page::Palette,
        Page::Profile,
        Page::Apply,
        Page::Live,
        Page::Windows,
        Page::States,
        Page::History,
        Page::Settings,
    ];

    /// The name `--page` takes: the English title, lowercase.
    pub fn slug(self) -> &'static str {
        match self {
            Page::Palette => "palette",
            Page::Profile => "profile",
            Page::Apply => "apply",
            Page::Live => "live",
            Page::Windows => "windows",
            Page::States => "states",
            Page::History => "history",
            Page::Settings => "settings",
        }
    }

    /// The page to open from the command line: `noren-studio --page live`.
    pub fn from_args(args: &[String]) -> Result<Option<Self>, String> {
        let Some(i) = args.iter().position(|a| a == "--page") else {
            return Ok(None);
        };
        let name = args.get(i + 1).ok_or("--page needs a name")?;
        Self::ALL
            .into_iter()
            .find(|p| p.slug().eq_ignore_ascii_case(name))
            .map(Some)
            .ok_or_else(|| {
                let all: Vec<_> = Self::ALL.iter().map(|p| p.slug()).collect();
                format!("unknown page {name:?}, want one of {}", all.join(", "))
            })
    }

    pub fn title(self, l: Lang) -> &'static str {
        match self {
            Page::Palette => l.pick("Palette", "Paleta"),
            Page::Profile => l.pick("Profile", "Profil"),
            Page::Apply => l.pick("Apply", "Zastosuj"),
            Page::Live => l.pick("Live", "Na żywo"),
            Page::Windows => l.pick("Windows", "Okna"),
            Page::States => l.pick("States", "Stany"),
            Page::History => l.pick("History", "Historia"),
            Page::Settings => l.pick("Settings", "Ustawienia"),
        }
    }

    pub fn subtitle(self, l: Lang) -> &'static str {
        match self {
            Page::Palette => l.pick(
                "One accent, every color. Edits land in the profile's [palette].",
                "Jeden akcent, wszystkie kolory. Zmiany trafiają do [palette] profilu.",
            ),
            Page::Profile => l.pick(
                "The profile as TOML, checked as you type.",
                "Profil jako TOML, sprawdzany w trakcie pisania.",
            ),
            Page::Apply => l.pick(
                "See what would change, apply it as one transaction, roll it back.",
                "Zobacz co się zmieni, zastosuj jako jedną transakcję, cofnij.",
            ),
            Page::Live => l.pick(
                "Window rules, focus dimming, x-ray, chords, hot corners and edge glows, held while the studio runs.",
                "Reguły okien, przyciemnianie, x-ray, akordy, gorące rogi i poświaty, trzymane póki działa studio.",
            ),
            Page::Windows => l.pick(
                "Every window rules can match.",
                "Każde okno, które mogą złapać reguły.",
            ),
            Page::States => l.pick(
                "Named states that color window borders and edge glows.",
                "Nazwane stany, które kolorują ramki okien i poświaty krawędzi.",
            ),
            Page::History => l.pick(
                "Every apply, newest first.",
                "Każde zastosowanie, od najnowszego.",
            ),
            Page::Settings => l.pick("How the studio behaves.", "Jak zachowuje się studio."),
        }
    }
}

/// Aizome, noren's own palette: accents named after Japanese dyes, offered as one-click
/// presets.
pub const AIZOME: [(&str, &str); 8] = [
    ("ai, indigo", "#7d97d4"),
    ("fuji, wisteria", "#9884bd"),
    ("sora, sky", "#7db1c9"),
    ("asagi, pale teal", "#77a89c"),
    ("matcha", "#9bb96f"),
    ("yamabuki, kerria gold", "#e2c187"),
    ("sakura", "#d3829b"),
    ("shu, vermilion", "#c84f48"),
];

/// "5 min ago" for a journal entry written at `then` (Unix seconds).
pub fn ago(now: u64, then: u64, lang: Lang) -> String {
    let s = now.saturating_sub(then);
    let (n, en, pl) = match s {
        0..60 => return lang.pick("just now", "przed chwilą").into(),
        60..3600 => (s / 60, "min", "min"),
        3600..86_400 => (s / 3600, "h", "godz."),
        _ => (s / 86_400, "d", "dni"),
    };
    match lang {
        Lang::En => format!("{n} {en} ago"),
        Lang::Pl => format!("{n} {pl} temu"),
    }
}

/// The app and tray icon, 32x32 RGBA: a rounded square in `accent` with a white "n". Drawn
/// in code so the repo carries no image files.
pub fn icon_rgba(accent: [u8; 3]) -> Vec<u8> {
    const S: usize = 32;
    let corner = |x: f32, y: f32| {
        let r = 8.0;
        let (cx, cy) = (x.clamp(r, S as f32 - r), y.clamp(r, S as f32 - r));
        (r - ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() + 0.5).clamp(0.0, 1.0)
    };
    let seg = |x: f32, y: f32, a: (f32, f32), b: (f32, f32)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let t = (((x - a.0) * dx + (y - a.1) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
        ((x - a.0 - t * dx).powi(2) + (y - a.1 - t * dy).powi(2)).sqrt()
    };
    let glyph = |x: f32, y: f32| {
        let arch = if y <= 16.0 {
            (((x - 16.0).powi(2) + (y - 16.0).powi(2)).sqrt() - 5.5).abs()
        } else {
            f32::MAX
        };
        let d = seg(x, y, (10.5, 10.5), (10.5, 23.0))
            .min(seg(x, y, (21.5, 16.0), (21.5, 23.0)))
            .min(arch);
        (2.4 - d).clamp(0.0, 1.0)
    };
    let mut px = Vec::with_capacity(S * S * 4);
    for y in 0..S {
        for x in 0..S {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let w = glyph(fx, fy);
            let mix = |c: u8| (f32::from(c) * (1.0 - w) + 255.0 * w).round() as u8;
            px.extend([
                mix(accent[0]),
                mix(accent[1]),
                mix(accent[2]),
                (corner(fx, fy) * 255.0) as u8,
            ]);
        }
    }
    px
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Ok,
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    born: f64,
}

/// Toasts slide in, stay, and fade out. Times are seconds on any monotonic clock.
#[derive(Clone, Debug, Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

pub const TOAST_IN: f64 = 0.25;
pub const TOAST_OUT: f64 = 0.4;

impl Toasts {
    /// Errors stay longer: they usually need reading.
    fn life(kind: ToastKind) -> f64 {
        match kind {
            ToastKind::Error => 8.0,
            _ => 4.0,
        }
    }

    pub fn push(&mut self, kind: ToastKind, text: impl Into<String>, now: f64) {
        self.items.push(Toast {
            text: text.into(),
            kind,
            born: now,
        });
        // Five on screen at most; the oldest go first.
        let excess = self.items.len().saturating_sub(5);
        self.items.drain(..excess);
    }

    /// Live toasts with their visibility `0..=1` (fade and slide), dropping expired ones.
    pub fn visible(&mut self, now: f64) -> Vec<(&Toast, f32)> {
        self.items.retain(|t| now - t.born < Self::life(t.kind));
        self.items
            .iter()
            .map(|t| {
                let age = now - t.born;
                let left = Self::life(t.kind) - age;
                let v = (age / TOAST_IN).min(left / TOAST_OUT).clamp(0.0, 1.0);
                (t, v as f32)
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noren-studio-{tag}-{}", std::process::id()))
    }

    #[test]
    fn new_profile_template_is_a_valid_default_profile() {
        let p = Profile::parse(NEW_PROFILE).unwrap();
        assert_eq!(p.palette, default_spec());
        assert_eq!(p.window.len(), 1);
    }

    #[test]
    fn set_palette_keeps_comments_and_other_tables() {
        let spec = PaletteSpec {
            mode: Mode::Light,
            contrast: Contrast::High,
            ..PaletteSpec::new(Color::from_hex("#957fb8").unwrap())
        };
        let out = set_palette(NEW_PROFILE, &spec).unwrap();
        assert!(
            out.contains("accent = \"#957fb8\"   # every other color is derived from this one"),
            "{out}"
        );
        assert!(out.contains("mode = \"light\""));
        assert!(out.contains("contrast = \"high\""));
        assert!(out.contains("# [xray]") && out.contains("[[window]]"));
        assert_eq!(Profile::parse(&out).unwrap().palette, spec);
        // A profile without contrast gets it added, and nothing else moves.
        let bare = "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n";
        let out = set_palette(bare, &spec).unwrap();
        assert!(out.starts_with("schema = 1\nname = \"x\"\n[palette]\naccent = \"#957fb8\"\n"));
        assert!(set_palette("schema = 1\nname = ", &spec).is_err());
        assert!(
            set_palette("schema = 9\nname = \"x\"\n", &spec)
                .unwrap_err()
                .contains("schema")
        );
    }

    #[test]
    fn settings_round_trip_and_survive_junk() {
        let path = temp("settings").join("studio.json");
        assert_eq!(Settings::load(&path), Settings::default());
        let s = Settings {
            profile: Some("C:/rice/my-rice.toml".into()),
            lang: Lang::Pl,
            reduced_motion: true,
            live_on_start: true,
            skip_update_check: true,
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path), Settings::default());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        assert_eq!(Lang::Pl.pick("Apply", "Zastosuj"), "Zastosuj");
    }

    #[test]
    fn default_aurora_leans_toward_aizome_hues() {
        let p = Palette::generate(&default_spec());
        let [a, b, c, d] = aurora(&p).map(|c| c.to_oklch().h);
        let near = |h: f32, want: &str| {
            let w = Color::from_hex(want).unwrap().to_oklch().h;
            let d = (h - w).rem_euclid(360.0);
            d.min(360.0 - d) < 25.0
        };
        assert!(near(a, DEFAULT_ACCENT));
        assert!(near(b, AIZOME[1].1), "fuji, got {b}");
        assert!(near(c, AIZOME[2].1), "sora, got {c}");
        assert!(near(d, AIZOME[3].1), "asagi, got {d}");
    }

    #[test]
    fn toasts_fade_in_hold_fade_out_and_expire() {
        let mut t = Toasts::default();
        t.push(ToastKind::Ok, "applied #3", 10.0);
        t.push(ToastKind::Error, "access denied", 10.0);
        assert_eq!(t.visible(10.0)[0].1, 0.0);
        assert_eq!(t.visible(10.125)[0].1, 0.5);
        assert_eq!(t.visible(12.0)[0].1, 1.0);
        assert!((t.visible(13.8)[0].1 - 0.5).abs() < 1e-3, "fading out");
        let left = t.visible(14.5);
        assert_eq!(left.len(), 1, "ok expired, error stays");
        assert_eq!(left[0].0.kind, ToastKind::Error);
        assert!(t.visible(18.0).is_empty() && t.is_empty());
        for i in 0..7 {
            t.push(ToastKind::Info, format!("{i}"), 20.0);
        }
        let v = t.visible(20.1);
        assert_eq!(v.len(), 5);
        assert_eq!(v[0].0.text, "2");
    }
    #[test]
    fn presets_parse_and_time_reads_naturally() {
        for (name, hex) in AIZOME {
            assert!(Color::from_hex(hex).is_some(), "{name}");
        }
        assert_eq!(AIZOME[0].1, DEFAULT_ACCENT);
        assert_eq!(ago(100, 100, Lang::En), "just now");
        assert_eq!(
            ago(100, 200, Lang::En),
            "just now",
            "clock skew is not negative time"
        );
        assert_eq!(ago(3599 + 100, 100, Lang::En), "59 min ago");
        assert_eq!(ago(7200, 0, Lang::Pl), "2 godz. temu");
        assert_eq!(ago(3 * 86_400 + 5, 0, Lang::En), "3 d ago");
    }

    #[test]
    fn icon_is_32px_rgba_with_clear_corners_and_a_white_glyph() {
        let px = icon_rgba([0x7e, 0x9c, 0xd8]);
        assert_eq!(px.len(), 32 * 32 * 4);
        let at = |x: usize, y: usize| &px[(y * 32 + x) * 4..(y * 32 + x) * 4 + 4];
        assert_eq!(at(0, 0)[3], 0, "rounded corner is transparent");
        assert_eq!(
            at(16, 28),
            [0x7e, 0x9c, 0xd8, 255],
            "plain accent below the glyph"
        );
        assert_eq!(at(10, 18), [255, 255, 255, 255], "stem of the n");
    }

    #[test]
    fn page_flag_picks_a_page_by_name() {
        let args = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(Page::from_args(&args(&[])), Ok(None));
        assert_eq!(Page::from_args(&args(&["--page", "Live"])), Ok(Some(Page::Live)));
        assert!(Page::from_args(&args(&["--page"])).is_err());
        assert!(
            Page::from_args(&args(&["--page", "nope"]))
                .unwrap_err()
                .contains("palette, profile")
        );
        for p in Page::ALL {
            assert_eq!(Page::from_args(&args(&["--page", p.slug()])), Ok(Some(p)));
            assert_ne!(p.title(Lang::Pl), "");
        }
    }
}
