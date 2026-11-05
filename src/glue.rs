//! Widgets glued to other apps' windows: a `[[widget]]` with a `glue` table leaves the panel
//! and rides on a window instead, as a small pill pinned to one of its corners or edges.
//!
//! ```toml
//! [[widget]]
//! name = "branch"
//! run = ["git", "-C", "C:/src/noren", "branch", "--show-current"]
//! interval = 5
//! [widget.glue]
//! exe = "Code.exe"         # globs like [[window]]: exe, class, title; at least one
//! anchor = "top_right"     # top_left .. bottom_right of the window, default top_right
//! offset = [150, 6]        # from the anchored edges, logical pixels; negative goes outside
//! height = 26              # the pill's height, 16 to 96, default 26
//! ```
//!
//! The widget follows the first matching window in z-order and keeps to it while it lives. It
//! sits just above that window, so windows in front of it cover the widget too, and hides
//! while the window is minimized, on another virtual desktop or gone. A click runs the
//! widget's `on_click`; glued widgets have no popup. This module is pure; the Win32 host finds
//! the window and moves the widget with it.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::exec::{ExecWidget, Msg};
use crate::shell::rules::{Rules, ScreenRect, WindowInfo, WindowRule};
use crate::surface::{Anchor, Floating, FloatingConfig, Layer, View, ViewContext};
use crate::widget::{Command, Context, Widget};
use crate::{Element, Theme, row};

/// How far a glued widget may sit from its anchor, in logical pixels, either way.
const MAX_OFFSET: f32 = 4000.0;

/// `[widget.glue]` in a profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct GlueConfig {
    pub exe: Option<String>,
    pub class: Option<String>,
    pub title: Option<String>,
    pub anchor: Anchor,
    pub offset: (f32, f32),
    pub height: f32,
}

impl Default for GlueConfig {
    fn default() -> Self {
        Self {
            exe: None,
            class: None,
            title: None,
            anchor: Anchor::TopRight,
            offset: (8.0, 8.0),
            height: 26.0,
        }
    }
}

/// A validated glue: which window, and where on it.
pub struct Glue {
    pub config: GlueConfig,
    matcher: Rules,
}

impl Glue {
    /// `who` names the widget in errors, e.g. `widget "branch"`.
    pub fn new(who: &str, c: &GlueConfig) -> Result<Self, String> {
        let err = |what: String| format!("{who}: glue {what}");
        if c.exe.is_none() && c.class.is_none() && c.title.is_none() {
            return Err(err("needs exe, class or title to find its window".into()));
        }
        let offset_ok = |v: f32| v.is_finite() && v.abs() <= MAX_OFFSET;
        if !offset_ok(c.offset.0) || !offset_ok(c.offset.1) {
            return Err(err(format!(
                "offset {:?} is outside -{MAX_OFFSET}..={MAX_OFFSET}",
                c.offset
            )));
        }
        if !(16.0..=96.0).contains(&c.height) {
            return Err(err(format!("height {} is outside 16..=96", c.height)));
        }
        let rule = WindowRule {
            exe: c.exe.clone(),
            class: c.class.clone(),
            title: c.title.clone(),
            ..Default::default()
        };
        let matcher = Rules::new(&[rule]).map_err(|e| err(e.replace("window rule 1: ", "")))?;
        Ok(Self {
            config: c.clone(),
            matcher,
        })
    }

    pub fn matches(&self, w: &WindowInfo) -> bool {
        !self.matcher.matching(w).is_empty()
    }

    /// Top-left corner, in physical pixels, of a `size` widget on a window with frame `target`
    /// on a monitor at `scale`.
    pub fn place(&self, target: ScreenRect, size: (u32, u32), scale: f32) -> (i32, i32) {
        let area = (target.width() as f32, target.height() as f32);
        let offset = (self.config.offset.0 * scale, self.config.offset.1 * scale);
        let (x, y) = self
            .config
            .anchor
            .place(area, (size.0 as f32, size.1 as f32), offset);
        (target.left + x as i32, target.top + y as i32)
    }
}

/// A program widget drawn as a pill, for gluing onto a window.
pub struct Glued {
    widget: ExecWidget,
    height: f32,
}

impl Glued {
    pub fn new(widget: ExecWidget, height: f32) -> Self {
        Self { widget, height }
    }

    /// The widget in a window of its own, placed by the host on its target window.
    pub fn surface(self, theme: Theme) -> Floating<Self> {
        let config = FloatingConfig {
            anchor: Anchor::TopLeft,
            offset: (0.0, 0.0),
            layer: Layer::Normal,
            ..Default::default()
        };
        Floating::new(self, config, theme)
    }
}

impl View for Glued {
    type Message = Msg;

    fn poll(&mut self, now: Instant) -> bool {
        self.widget.poll(now)
    }

    fn view(&self, ctx: &ViewContext<'_>) -> Element<Msg> {
        let theme = ctx.theme;
        let inner = Context {
            theme,
            now: ctx.now,
            popup_open: false,
            panel_height: self.height,
        };
        row()
            .child(self.widget.compact(&inner))
            .center()
            .height(self.height)
            .min_width(self.height)
            .padding_xy(theme.units.small_spacing * 2.0, 0.0)
            .background(theme.background.with_alpha(theme.panel_opacity))
            .border(theme.highlight.with_alpha(0.35), 1.0)
            .radius(self.height / 2.0)
    }

    fn update(&mut self, message: Msg, now: Instant) -> Command {
        // Popups belong to the panel; a glued widget only runs its `on_click`.
        match self.widget.update(message, now) {
            Command::Quit => Command::Quit,
            _ => Command::None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::ExecConfig;
    use crate::surface::{MonitorInfo, Surface};

    fn glue(toml: &str) -> Result<Glue, String> {
        let c: GlueConfig = toml::from_str(toml).map_err(|e| e.to_string())?;
        Glue::new("widget \"w\"", &c)
    }

    fn info(exe: &str, title: &str) -> WindowInfo {
        WindowInfo {
            exe: exe.into(),
            class: "C".into(),
            title: title.into(),
        }
    }

    #[test]
    fn configs_parse_with_defaults_and_bad_values_are_named() {
        let g = glue("exe = \"Code.exe\"").unwrap();
        assert_eq!(g.config.anchor, Anchor::TopRight);
        assert_eq!(g.config.offset, (8.0, 8.0));
        assert_eq!(g.config.height, 26.0);
        assert!(g.matches(&info("code.EXE", "x")));
        assert!(!g.matches(&info("wt.exe", "x")));
        let g = glue("title = \"*build*\"\noffset = [-30, 0]\nanchor = \"left\"").unwrap();
        assert!(g.matches(&info("wt.exe", "cargo build")));
        for (bad, says) in [
            ("height = 20", "needs exe, class or title"),
            ("exe = \"a\"\nheight = 8", "height 8"),
            ("exe = \"a\"\noffset = [5000, 0]", "offset"),
            ("exe = \"[\"", "widget \"w\": glue exe"),
        ] {
            let e = glue(bad).err().unwrap();
            assert!(e.contains(says), "{bad}: {e}");
        }
        assert!(glue("exe = \"a\"\nsize = 3").err().unwrap().contains("size"));
    }

    #[test]
    fn place_pins_the_widget_to_the_window_at_scale() {
        let target = ScreenRect {
            left: 100,
            top: 50,
            right: 1100,
            bottom: 750,
        };
        // Top right, 8 logical in at 150% is 12 physical.
        let g = glue("exe = \"a\"").unwrap();
        assert_eq!(g.place(target, (90, 39), 1.5), (1100 - 12 - 90, 50 + 12));
        // A negative offset puts the widget outside the window's left edge.
        let g = glue("exe = \"a\"\nanchor = \"left\"\noffset = [-40, 0]").unwrap();
        assert_eq!(g.place(target, (40, 20), 1.0), (100 - 40, 50 + 340));
        let g = glue("exe = \"a\"\nanchor = \"bottom\"\noffset = [0, 4]").unwrap();
        assert_eq!(g.place(target, (100, 20), 1.0), (550, 750 - 4 - 20));
    }

    #[test]
    fn the_pill_shows_the_program_output_and_is_as_tall_as_configured() {
        let config = ExecConfig {
            name: "w".into(),
            run: vec!["noren-test-no-such-program".into()],
            interval: Some(60.0),
            on_click: None,
            glue: Some(GlueConfig {
                exe: Some("a".into()),
                ..Default::default()
            }),
            desktop: None,
        };
        let widget = ExecWidget::spawn(config).unwrap();
        let mut surface = Glued::new(widget, 30.0).surface(Theme::aizome());
        surface.set_monitor(MonitorInfo {
            width: 800.0,
            height: 600.0,
            scale: 1.0,
        });
        let start = Instant::now();
        let placement = loop {
            surface.tick(Instant::now(), None, false);
            let p = surface.placement(Instant::now());
            // The missing program turns the "…" placeholder into a red "!".
            if surface.view().widget.latest().is_some() || start.elapsed().as_secs() > 5 {
                break p;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!((placement.rect.x, placement.rect.y), (0.0, 0.0));
        assert_eq!(placement.rect.h, 30.0);
        // Never narrower than round, even for a one-character "!".
        assert!(placement.rect.w >= 30.0, "{placement:?}");
        assert_eq!(placement.layer, Layer::Normal);
    }
}
