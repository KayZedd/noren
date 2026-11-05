//! A rice as a file: `profile.toml` names a palette and the targets it is pushed to. Absent
//! tables leave their target alone.
//!
//! ```toml
//! schema = 1
//! name = "my-rice"
//!
//! [palette]
//! accent = "#7d97d4"
//! mode = "dark"            # dark | light
//! contrast = "standard"    # standard | high
//!
//! [windows]                # system accent color
//! accent_on_title_bars = true
//!
//! [terminal]               # Windows Terminal: adds the scheme, makes it the default
//! scheme = "noren"
//!
//! [vscode]                 # workbench.colorCustomizations
//!
//! [firefox]                # userChrome.css + the pref that enables it
//! profile = 'C:\Users\me\AppData\Roaming\Mozilla\Firefox\Profiles\abcd.default-release'
//!
//! [[window]]               # live rules, held by `noren run` (see `shell::rules`)
//! exe = "WindowsTerminal.exe"
//! backdrop = "acrylic"
//! border = "state:build"
//!
//! [focus]                  # dim behind the focused window (see `shell::focus`)
//! dim = 0.35
//!
//! [xray]                   # hold to see through every window
//! key = "Ctrl+Alt+Space"
//!
//! [chords]                 # leader key for [[command]] keys (see `shell::command`)
//! leader = "Alt+Space"
//!
//! [[command]]              # in the palette, on a chord and/or a hot corner
//! name = "Terminal"
//! keys = "t"
//! run = ["wt.exe"]
//!
//! [[glow]]                 # light a screen edge from a state (see `shell::glow`)
//! state = "ci"
//! edges = ["top"]
//!
//! [[widget]]              # a program's output on the panel (see `exec`), `noren panel`
//! name = "weather"
//! run = ["curl", "-s", "wttr.in/?format=%t"]
//! interval = 600
//! [widget.glue]            # or on another app's window instead (see `glue`)
//! exe = "Code.exe"
//!                          # or [widget.desktop] on the wallpaper (see `desk`)
//!
//! [media]                  # now playing on the panel, colored from the cover (see `media`)
//! cover_colors = true
//!
//! [[layout]]               # panel widgets per foreground app or desktop (see `layouts`)
//! exe = ["Code.exe"]
//! widgets = ["weather"]
//!
//! [schedule]               # dark/light by the clock or the sun (see `shell::schedule`)
//! light = "sunrise"
//! dark = "21:00"
//! latitude = 52.23
//! longitude = 21.01
//!
//! [switcher]               # Alt+Tab with live thumbnails (see `shell::switcher`)
//! key = "Alt+Tab"
//!
//! [[mirror]]               # a live copy of another window (see `shell::mirror`)
//! exe = "Spotify.exe"
//! ```
//!
//! Every target accepts `path = "..."` to point at a non-default config file.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::merge::{JsonEdit, merge_jsonc, replace_block};
use super::{Change, Slot, Store};
use crate::exec::ExecConfig;
use crate::layouts::{self, LayoutConfig};
use crate::media::{MediaConfig, MediaWidget};
use crate::palette::{Palette, PaletteSpec, export};
use crate::shell::command::{ChordsConfig, CommandConfig, Commands, CornersConfig};
use crate::shell::focus::{Focus, FocusConfig, Xray, XrayConfig};
use crate::shell::glow::{self, GlowConfig};
use crate::shell::mirror::{self, MirrorConfig};
use crate::shell::schedule::ScheduleConfig;
use crate::shell::switcher::{Switcher, SwitcherConfig};
use crate::shell::{Rules, WindowRule};

/// Profile format version this build reads.
pub const SCHEMA: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub schema: u32,
    pub name: String,
    pub palette: PaletteSpec,
    pub windows: Option<WindowsTarget>,
    pub terminal: Option<TerminalTarget>,
    pub vscode: Option<FileTarget>,
    pub firefox: Option<FirefoxTarget>,
    /// Live window rules, held by `noren run` (see [`crate::shell::rules`]).
    #[serde(default)]
    pub window: Vec<WindowRule>,
    /// Dim everything behind the focused window (see [`crate::shell::focus`]).
    pub focus: Option<FocusConfig>,
    /// Hold a key to see through every window.
    pub xray: Option<XrayConfig>,
    /// The leader key for command chords.
    pub chords: Option<ChordsConfig>,
    /// Hot-corner timing.
    pub corners: Option<CornersConfig>,
    /// Named actions for the palette, chords and hot corners (see [`crate::shell::command`]).
    #[serde(default)]
    pub command: Vec<CommandConfig>,
    /// Screen edges lit by a state (see [`crate::shell::glow`]).
    #[serde(default)]
    pub glow: Vec<GlowConfig>,
    /// Programs shown as panel widgets by `noren panel` (see [`crate::exec`]).
    #[serde(default)]
    pub widget: Vec<ExecConfig>,
    /// The now-playing widget on `noren panel` (see [`crate::media`]).
    pub media: Option<MediaConfig>,
    /// Which panel widgets show per foreground app and virtual desktop (see [`crate::layouts`]).
    #[serde(default)]
    pub layout: Vec<LayoutConfig>,
    /// Dark and light by the clock or the sun, held by `noren run`.
    pub schedule: Option<ScheduleConfig>,
    /// The Alt+Tab replacement with live thumbnails, held by `noren run`.
    pub switcher: Option<SwitcherConfig>,
    /// Live copies of other windows, held by `noren run` (see [`crate::shell::mirror`]).
    #[serde(default)]
    pub mirror: Vec<MirrorConfig>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowsTarget {
    /// Accent color on title bars and window borders (Settings ▸ Colors ▸ "Show accent color on
    /// title bars").
    #[serde(default)]
    pub accent_on_title_bars: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TerminalTarget {
    #[serde(default = "TerminalTarget::default_scheme")]
    pub scheme: String,
    pub path: Option<PathBuf>,
}

impl TerminalTarget {
    fn default_scheme() -> String {
        "noren".into()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileTarget {
    pub path: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FirefoxTarget {
    /// The profile directory (about:profiles ▸ Root Directory).
    pub profile: PathBuf,
}

const DWM: &str = r"Software\Microsoft\Windows\DWM";
const ACCENT: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Accent";
const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

fn env_path(var: &str, rest: &str) -> io::Result<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(|v| PathBuf::from(v).join(rest))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("%{var}% is not set; give the target an explicit path"),
            )
        })
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

impl Profile {
    pub fn parse(text: &str) -> Result<Self, String> {
        let profile: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if profile.schema != SCHEMA {
            return Err(format!(
                "profile schema {} is not supported, this build reads schema {SCHEMA}",
                profile.schema
            ));
        }
        Rules::new(&profile.window)?;
        profile.focus.as_ref().map(Focus::new).transpose()?;
        profile.xray.as_ref().map(Xray::new).transpose()?;
        profile.commands()?;
        glow::glows(&profile.glow)?;
        crate::exec::validate(&profile.widget)?;
        layouts::validate(&profile.layout, &profile.panel_widgets())?;
        profile.switcher.as_ref().map(Switcher::new).transpose()?;
        mirror::mirrors(&profile.mirror)?;
        Ok(profile)
    }

    /// The ids of the panel's widgets, in panel order. Glued widgets are not on the panel.
    fn panel_widgets(&self) -> Vec<&str> {
        let media = self.media.as_ref().map(|_| layouts::MEDIA);
        let programs = self.widget.iter().filter(|w| w.on_panel());
        media
            .into_iter()
            .chain(programs.map(|w| w.name.as_str()))
            .collect()
    }

    /// Whether `noren panel` shows a panel: `[media]` or a `[[widget]]` without `glue` or
    /// `desktop`.
    pub fn has_panel(&self) -> bool {
        !self.panel_widgets().is_empty()
    }

    /// The `[[widget]]`s glued to other apps' windows, started, each with where it goes.
    pub fn glued(
        &self,
    ) -> Result<Vec<(crate::Floating<crate::glue::Glued>, crate::glue::GlueConfig)>, String> {
        let theme = self.palette().theme(crate::Theme::aizome());
        crate::exec::glued(&self.widget, &theme)
    }

    /// The `[[widget]]`s on the desktop, started, each with its 1-based monitor.
    pub fn desktop(&self) -> Result<Vec<(crate::desk::OnWallpaper, Option<u32>)>, String> {
        let theme = self.palette().theme(crate::Theme::aizome());
        crate::exec::desktop(&self.widget, &theme, crate::wallpaper::watch)
    }

    /// The `[[command]]`s with their `[chords]` and `[corners]` settings, validated.
    pub fn commands(&self) -> Result<Commands, String> {
        Commands::new(&self.command, self.chords.as_ref(), self.corners.as_ref())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn palette(&self) -> Palette {
        Palette::generate(&self.palette)
    }

    /// The panel `noren panel` shows: `[media]` first, then each `[[widget]]`.
    pub fn panel(&self) -> Result<crate::Panel, String> {
        if !self.has_panel() {
            return Err("the profile has no [media] or [[widget]] on the panel".into());
        }
        let theme = self.palette().theme(crate::Theme::aizome());
        let mut panel = crate::Panel::new(crate::PanelConfig::default(), theme);
        if let Some(m) = &self.media {
            panel.add(MediaWidget::new(m.clone(), self.palette.clone()));
        }
        crate::exec::add(&mut panel, &self.widget)?;
        if !self.layout.is_empty() {
            panel.set_switcher(layouts::Switcher::system(self.layout.clone()));
        }
        Ok(panel)
    }

    /// The desired state of every targeted slot. Config files are read through `store` so
    /// edits merge into what is there.
    pub fn changes(&self, store: &dyn Store) -> io::Result<Vec<Change>> {
        let p = self.palette();
        let mut out = Vec::new();
        if let Some(w) = &self.windows {
            let a = export::windows_accent(&p);
            out.push(Change::binary(
                "accent palette",
                ACCENT,
                "AccentPalette",
                &a.accent_palette,
            ));
            out.push(Change::dword(
                "accent",
                ACCENT,
                "AccentColorMenu",
                a.accent_color_menu,
            ));
            out.push(Change::dword(
                "start accent",
                ACCENT,
                "StartColorMenu",
                a.start_color_menu,
            ));
            out.push(Change::dword("DWM accent", DWM, "AccentColor", a.dwm_accent));
            out.push(Change::dword(
                "colorization",
                DWM,
                "ColorizationColor",
                a.colorization,
            ));
            out.push(Change::dword(
                "afterglow",
                DWM,
                "ColorizationAfterglow",
                a.colorization,
            ));
            out.push(Change::dword(
                "accent on title bars",
                DWM,
                "ColorPrevalence",
                u32::from(w.accent_on_title_bars),
            ));
            let light = u32::from(self.palette.mode == crate::palette::Mode::Light);
            out.push(Change::dword(
                "apps theme",
                PERSONALIZE,
                "AppsUseLightTheme",
                light,
            ));
            out.push(Change::dword(
                "system theme",
                PERSONALIZE,
                "SystemUsesLightTheme",
                light,
            ));
        }
        if let Some(t) = &self.terminal {
            let path = match &t.path {
                Some(p) => p.clone(),
                None => env_path(
                    "LOCALAPPDATA",
                    r"Packages\Microsoft.WindowsTerminal_8wekyb3d8bbwe\LocalState\settings.json",
                )?,
            };
            let edits = [
                JsonEdit::Upsert {
                    path: vec!["schemes".into()],
                    key: "name".into(),
                    value: export::windows_terminal_scheme(&p, &t.scheme),
                },
                JsonEdit::Set {
                    path: vec!["profiles".into(), "defaults".into(), "colorScheme".into()],
                    value: json!(t.scheme),
                },
            ];
            out.push(json_change("Windows Terminal", store, path, &edits)?);
        }
        if let Some(v) = &self.vscode {
            let path = match &v.path {
                Some(p) => p.clone(),
                None => env_path("APPDATA", r"Code\User\settings.json")?,
            };
            let edits: Vec<JsonEdit> = export::vscode_colors(&p)
                .into_iter()
                .map(|(k, c)| JsonEdit::Set {
                    path: vec!["workbench.colorCustomizations".into(), k],
                    value: Value::String(c),
                })
                .collect();
            out.push(json_change("VS Code", store, path, &edits)?);
        }
        if let Some(f) = &self.firefox {
            let css = f.profile.join("chrome").join("userChrome.css");
            let body = export::firefox_user_chrome(&p);
            let text = text_of(store, &css)?;
            out.push(Change::file(
                "Firefox userChrome.css",
                &css,
                replace_block(&text, "/* noren:begin */", "/* noren:end */", &body),
            ));
            let prefs = f.profile.join("user.js");
            let text = text_of(store, &prefs)?;
            let pref = "user_pref(\"toolkit.legacyUserProfileCustomizations.stylesheets\", true);";
            out.push(Change::file(
                "Firefox user.js",
                &prefs,
                replace_block(&text, "// noren:begin", "// noren:end", pref),
            ));
        }
        Ok(out)
    }
}

fn text_of(store: &dyn Store, path: &Path) -> io::Result<String> {
    let bytes = store.read(&Slot::File { path: path.into() })?.unwrap_or_default();
    String::from_utf8(bytes).map_err(|_| invalid(format!("{} is not UTF-8", path.display())))
}

fn json_change(label: &str, store: &dyn Store, path: PathBuf, edits: &[JsonEdit]) -> io::Result<Change> {
    let text = text_of(store, &path)?;
    // Windows Terminal writes a UTF-8 BOM; keep it.
    let (bom, body) = match text.strip_prefix('\u{feff}') {
        Some(rest) => ("\u{feff}", rest),
        None => ("", text.as_str()),
    };
    let merged = merge_jsonc(body, edits).map_err(|e| invalid(format!("{}: {e}", path.display())))?;
    Ok(Change::file(label, path, format!("{bom}{merged}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rice::MemStore;

    const FULL: &str = r##"
schema = 1
name = "my-rice"

[palette]
accent = "#7e9cd8"

[windows]
accent_on_title_bars = true

[terminal]
path = "wt.json"

[vscode]
path = "code.json"

[firefox]
profile = "ff"
"##;

    #[test]
    fn full_profile_produces_changes_for_every_target() {
        let profile = Profile::parse(FULL).unwrap();
        let mut store = MemStore::default();
        let wt = Slot::File {
            path: "wt.json".into(),
        };
        store.set(&wt, Some("\u{feff}{\"schemes\": [], // user\n\"x\": 1}".into()));
        let changes = profile.changes(&store).unwrap();
        let labels: Vec<&str> = changes.iter().map(|c| c.label.as_str()).collect();
        assert!(
            labels.contains(&"accent palette") && labels.contains(&"VS Code"),
            "{labels:?}"
        );
        let wt_text = changes
            .iter()
            .find(|c| c.slot == wt)
            .unwrap()
            .value
            .clone()
            .unwrap();
        let wt_text = String::from_utf8(wt_text).unwrap();
        assert!(
            wt_text.starts_with('\u{feff}') && wt_text.contains("// user"),
            "{wt_text}"
        );
        assert!(wt_text.contains("\"colorScheme\": \"noren\""), "{wt_text}");
        let prevalence = changes
            .iter()
            .find(|c| c.label == "accent on title bars")
            .unwrap();
        assert_eq!(prevalence.value, Some(1u32.to_le_bytes().to_vec()));
    }

    #[test]
    fn minimal_profile_touches_nothing_but_parses() {
        let p = Profile::parse("schema = 1\nname = \"x\"\n[palette]\naccent = \"#ff0000\"\n").unwrap();
        assert!(p.changes(&MemStore::default()).unwrap().is_empty());
    }

    #[test]
    fn wrong_schema_unknown_keys_and_bad_colors_are_rejected() {
        let base = "name = \"x\"\n[palette]\naccent = \"#ff0000\"\n";
        assert!(
            Profile::parse(&format!("schema = 2\n{base}"))
                .unwrap_err()
                .contains("schema 2")
        );
        assert!(Profile::parse(&format!("schema = 1\ntypo = 1\n{base}")).is_err());
        let bad = Profile::parse("schema = 1\nname = \"x\"\n[palette]\naccent = \"red\"\n");
        assert!(bad.unwrap_err().contains("#rrggbb"));
    }

    #[test]
    fn window_rules_parse_and_are_validated_with_the_profile() {
        let base = "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n";
        let p = Profile::parse(&format!(
            "{base}[[window]]\nexe = \"Code.exe\"\nborder = \"state:build\"\n[[window]]\ncorners = \"square\"\n"
        ))
        .unwrap();
        assert_eq!(p.window.len(), 2);
        assert!(
            p.changes(&MemStore::default()).unwrap().is_empty(),
            "window rules are live, never written by apply"
        );
        let bad = Profile::parse(&format!("{base}[[window]]\nopacity = 0.0\n")).unwrap_err();
        assert!(bad.contains("opacity"), "{bad}");
    }

    #[test]
    fn mirrors_parse_and_are_validated_with_the_profile() {
        let base = "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n";
        let p = Profile::parse(&format!("{base}[[mirror]]\nexe = \"Spotify.exe\"\nwidth = 400\n")).unwrap();
        assert_eq!(p.mirror.len(), 1);
        assert_eq!(p.mirror[0].width, 400.0);
        let bad = Profile::parse(&format!("{base}[[mirror]]\nwidth = 400\n")).unwrap_err();
        assert!(bad.contains("mirror 1"), "{bad}");
    }

    #[test]
    fn the_switcher_parses_with_defaults_and_is_validated() {
        let base = "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n";
        let p = Profile::parse(&format!("{base}[switcher]\n")).unwrap();
        assert_eq!(p.switcher, Some(SwitcherConfig::default()));
        let p = Profile::parse(&format!(
            "{base}[switcher]\nkey = \"Win+Tab\"\nthumbnails = false\n"
        ))
        .unwrap();
        assert!(!p.switcher.unwrap().thumbnails);
        let bad = Profile::parse(&format!("{base}[switcher]\nkey = \"Alt\"\n")).unwrap_err();
        assert!(bad.contains("[switcher] key"), "{bad}");
        let bad = Profile::parse(&format!("{base}[switcher]\nkeys = \"Alt+Tab\"\n")).unwrap_err();
        assert!(bad.contains("keys"), "{bad}");
    }

    #[test]
    fn focus_and_xray_parse_and_are_validated_with_the_profile() {
        let base = "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n";
        let p = Profile::parse(&format!("{base}[focus]\ndim = 0.3\n[xray]\nkey = \"Ctrl+Win\"\n")).unwrap();
        assert_eq!(p.focus.unwrap().dim, 0.3);
        assert_eq!(p.xray.unwrap().opacity, 0.2);
        assert!(p.window.is_empty());
        let bad = Profile::parse(&format!("{base}[xray]\nkey = \"Space\"\n")).unwrap_err();
        assert!(bad.contains("[xray] key"), "{bad}");
        let bad = Profile::parse(&format!("{base}[focus]\ndim = 1.0\n")).unwrap_err();
        assert!(bad.contains("[focus] dim"), "{bad}");
    }

    #[test]
    fn widgets_parse_and_survive_a_round_trip() {
        let text = "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n\
                    [[widget]]\nname = \"w\"\nrun = [\"curl\", \"-s\"]\ninterval = 60\n\
                    on_click = [\"explorer.exe\"]\n";
        let p = Profile::parse(text).unwrap();
        assert_eq!(p.widget.len(), 1);
        assert_eq!(p.widget[0].interval, Some(60.0));
        assert_eq!(Profile::parse(&toml::to_string(&p).unwrap()).unwrap(), p);
        assert!(Profile::parse(&text.replace("interval", "every")).is_err());
    }

    #[test]
    fn layouts_parse_and_name_only_panel_widgets() {
        let base = "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n[media]\n\
                    [[widget]]\nname = \"git\"\nrun = [\"git\"]\n";
        let text = format!(
            "{base}[[layout]]\nexe = [\"Code.exe\"]\nwidgets = [\"git\", \"media\"]\n\
             [[layout]]\ndesktop = \"Music\"\nwidgets = [\"media\"]\n"
        );
        let p = Profile::parse(&text).unwrap();
        assert_eq!(p.layout.len(), 2);
        assert_eq!(p.layout[1].desktop.as_deref(), Some("Music"));
        assert_eq!(Profile::parse(&toml::to_string(&p).unwrap()).unwrap(), p);
        let e = Profile::parse(&format!(
            "{base}[[layout]]\nexe = [\"a.exe\"]\nwidgets = [\"cpu\"]\n"
        ))
        .unwrap_err();
        assert!(e.contains("layout 1: no widget named \"cpu\""), "{e}");
        let e = Profile::parse(&format!("{base}[[layout]]\nwidgets = [\"git\"]\n")).unwrap_err();
        assert!(e.contains("set exe, desktop or both"), "{e}");
        let no_media = base.replace("[media]\n", "");
        assert!(
            Profile::parse(&format!(
                "{no_media}[[layout]]\nexe = [\"a.exe\"]\nwidgets = [\"media\"]\n"
            ))
            .is_err()
        );
    }

    #[test]
    fn glued_widgets_leave_the_panel_and_bad_glue_fails_the_load() {
        let base = "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n\
                    [[widget]]\nname = \"git\"\nrun = [\"git\"]\n[widget.glue]\nexe = \"Code.exe\"\n";
        let p = Profile::parse(base).unwrap();
        assert!(!p.has_panel());
        assert!(p.panel().err().unwrap().contains("on the panel"));
        assert_eq!(Profile::parse(&toml::to_string(&p).unwrap()).unwrap(), p);
        let e = Profile::parse(&format!(
            "{base}[[layout]]\nexe = [\"a.exe\"]\nwidgets = [\"git\"]\n"
        ))
        .unwrap_err();
        assert!(e.contains("no widget named \"git\""), "{e}");
        let e = Profile::parse(&base.replace("exe = \"Code.exe\"", "height = 200")).unwrap_err();
        assert!(e.contains("widget \"git\": glue"), "{e}");
        let both = format!("{base}[media]\n");
        assert!(Profile::parse(&both).unwrap().has_panel());
    }

    #[test]
    fn desktop_widgets_leave_the_panel_and_round_trip() {
        let base = "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n\
                    [[widget]]\nname = \"cpu\"\nrun = [\"x\"]\n[widget.desktop]\nanchor = \"left\"\n";
        let p = Profile::parse(base).unwrap();
        assert!(!p.has_panel());
        assert_eq!(Profile::parse(&toml::to_string(&p).unwrap()).unwrap(), p);
        let e = Profile::parse(&base.replace("anchor = \"left\"", "monitor = 0")).unwrap_err();
        assert!(e.contains("widget \"cpu\": desktop monitors count from 1"), "{e}");
    }

    #[test]
    fn media_parses_and_makes_a_panel_on_its_own() {
        let base = "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n";
        let none = Profile::parse(base).unwrap();
        assert!(none.panel().err().unwrap().contains("[media]"));
        let p = Profile::parse(&format!("{base}[media]\n")).unwrap();
        assert_eq!(p.media, Some(MediaConfig { cover_colors: true }));
        assert_eq!(Profile::parse(&toml::to_string(&p).unwrap()).unwrap(), p);
        let off = Profile::parse(&format!("{base}[media]\ncover_colors = false\n")).unwrap();
        assert!(!off.media.unwrap().cover_colors);
        let mut panel = p.panel().unwrap();
        panel.set_viewport(1280.0, 720.0, 1.0);
        panel.reveal_now();
        panel.tick(std::time::Instant::now(), Some((640.0, 10.0)), false);
        assert!(panel.cell_rect("media").is_some());
        assert!(Profile::parse(&format!("{base}[media]\ncolors = 1\n")).is_err());
    }

    #[test]
    fn commands_parse_are_validated_and_survive_a_round_trip() {
        let base = "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n";
        let text = format!(
            "{base}[chords]\nleader = \"Alt+Space\"\n[corners]\ndwell = 0.1\n\
             [[command]]\nname = \"Terminal\"\nkeys = \"t\"\nrun = [\"wt.exe\"]\n\
             [[command]]\nname = \"Palette\"\ncorner = \"top-left\"\nbuiltin = \"palette\"\n"
        );
        let p = Profile::parse(&text).unwrap();
        assert_eq!(p.commands().unwrap().list.len(), 2);
        assert!(
            p.windows.is_none() && p.window.is_empty(),
            "commands are live only"
        );
        let again = Profile::parse(&toml::to_string(&p).unwrap()).unwrap();
        assert_eq!(again, p, "the palette gets the profile as TOML");
        let bad = Profile::parse(&format!(
            "{base}[[command]]\nname = \"t\"\nkeys = \"t\"\nrun = [\"x\"]\n"
        ))
        .unwrap_err();
        assert!(bad.contains("no [chords] leader"), "{bad}");
    }

    #[test]
    fn glows_parse_are_validated_and_survive_a_round_trip() {
        let base = "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n";
        let p = Profile::parse(&format!(
            "{base}[[glow]]\nstate = \"ci\"\nedges = [\"top\"]\nsettle = 5\n\
             [[glow]]\nstate = \"agent\"\nedges = [\"bottom\"]\npulse = [\"warn\"]\n"
        ))
        .unwrap();
        assert_eq!(p.glow.len(), 2);
        let again = Profile::parse(&toml::to_string(&p).unwrap()).unwrap();
        assert_eq!(again, p);
        let bad = Profile::parse(&format!(
            "{base}[[glow]]\nstate = \"a\"\n[[glow]]\nstate = \"b\"\nedges = [\"left\"]\n"
        ))
        .unwrap_err();
        assert!(bad.contains("both light the left edge"), "{bad}");
    }

    #[test]
    fn applying_twice_is_a_no_op() {
        let profile = Profile::parse(
            "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n[vscode]\npath = \"c.json\"\n[firefox]\nprofile = \"ff\"\n",
        )
        .unwrap();
        let mut store = MemStore::default();
        for change in profile.changes(&store).unwrap() {
            store.set(&change.slot, change.value);
        }
        let again = profile.changes(&store).unwrap();
        assert!(crate::rice::plan(&store, &again).unwrap().is_empty());
    }
}
