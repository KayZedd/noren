//! Widgets on the desktop: a `[[widget]]` with a `desktop` table leaves the panel and sits on
//! the wallpaper, below every window, placed where the wallpaper is calm and colored to read
//! on it.
//!
//! ```toml
//! [[widget]]
//! name = "weather"
//! run = ["curl", "-s", "wttr.in/?format=%t"]
//! interval = 600
//! [widget.desktop]
//! # anchor = "top_left"    # leave out to pick the calmest corner or edge of the wallpaper
//! offset = [32, 32]        # from the anchored edges, logical pixels, default 32
//! monitor = 1              # same numbering as window rules, default the primary
//! height = 40              # the widget's height, 16 to 200, default 40
//! text = "auto"            # "auto" reads on the wallpaper, "palette" keeps the palette's
//! ```
//!
//! With no `anchor`, the widget tries the corners, then the edges, and takes the one whose
//! wallpaper varies least (see [`crate::wallpaper`]). With `text = "auto"` the plain text
//! color becomes whichever of the palette's text and background colors has more contrast with
//! the wallpaper under the widget; status colors (ok, warn, fail) stay as they are. Both are
//! picked again when the wallpaper or the monitor changes, not while the text changes, so the
//! widget does not jump around.

use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tiny_skia::Pixmap;

use crate::exec::{ExecWidget, Msg};
use crate::surface::{
    Anchor, Floating, FloatingConfig, Layer, MonitorInfo, Placement, Response, Surface, View, ViewContext,
};
use crate::wallpaper::{AUTO_ANCHORS, Desktop, Source, readable};
use crate::widget::{Command, Context, Widget};
use crate::{Color, Element, Rect, Theme, row};

/// Where the plain text color comes from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextFrom {
    /// The palette color that reads best on the wallpaper under the widget.
    #[default]
    Auto,
    /// The palette's text color, always.
    Palette,
}

/// `[widget.desktop]` in a profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct DeskConfig {
    /// `None` picks the calmest spot on the wallpaper.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    pub offset: (f32, f32),
    /// 1-based, like window rules; `None` is the primary monitor.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub monitor: Option<u32>,
    pub height: f32,
    pub text: TextFrom,
}

impl Default for DeskConfig {
    fn default() -> Self {
        Self {
            anchor: None,
            offset: (32.0, 32.0),
            monitor: None,
            height: 40.0,
            text: TextFrom::Auto,
        }
    }
}

impl DeskConfig {
    /// `who` names the widget in errors, e.g. `widget "weather"`.
    pub fn validate(&self, who: &str) -> Result<(), String> {
        let err = |what: String| Err(format!("{who}: desktop {what}"));
        let ok = |v: f32| (0.0..=10_000.0).contains(&v);
        if !ok(self.offset.0) || !ok(self.offset.1) {
            return err(format!("offset {:?} must be 0 to 10000", self.offset));
        }
        if self.monitor == Some(0) {
            return err("monitors count from 1".into());
        }
        if !(16.0..=200.0).contains(&self.height) {
            return err(format!("height {} is outside 16..=200", self.height));
        }
        Ok(())
    }
}

/// A program widget drawn straight onto the desktop.
pub struct Desk {
    widget: ExecWidget,
    height: f32,
    /// The plain text color picked from the wallpaper, over the palette's.
    text: Option<Color>,
}

impl Desk {
    pub fn new(widget: ExecWidget, height: f32) -> Self {
        Self {
            widget,
            height,
            text: None,
        }
    }

    /// The plain text color in use, when picked from the wallpaper.
    pub fn text(&self) -> Option<Color> {
        self.text
    }
}

impl View for Desk {
    type Message = Msg;

    fn poll(&mut self, now: Instant) -> bool {
        self.widget.poll(now)
    }

    fn view(&self, ctx: &ViewContext<'_>) -> Element<Msg> {
        let mut theme = ctx.theme.clone();
        if let Some(c) = self.text {
            theme.text = c;
        }
        let inner = Context {
            theme: &theme,
            now: ctx.now,
            popup_open: false,
            panel_height: self.height,
        };
        row()
            .child(self.widget.compact(&inner))
            .center()
            .height(self.height)
            .min_width(self.height)
    }

    fn update(&mut self, message: Msg, now: Instant) -> Command {
        // Popups belong to the panel; a desktop widget only runs its `on_click`.
        match self.widget.update(message, now) {
            Command::Quit => Command::Quit,
            _ => Command::None,
        }
    }
}

/// A [`Desk`] on the desktop layer that places and colors itself from the wallpaper.
pub struct OnWallpaper {
    inner: Floating<Desk>,
    config: DeskConfig,
    choices: [Color; 2],
    source: Box<dyn Source>,
    desktop: Option<Arc<Desktop>>,
    monitor: MonitorInfo,
    /// The anchor and text color need picking again.
    stale: bool,
}

impl OnWallpaper {
    pub fn new(desk: Desk, config: DeskConfig, theme: Theme, source: Box<dyn Source>) -> Self {
        let choices = [theme.text, theme.background];
        let floating = FloatingConfig {
            anchor: config.anchor.unwrap_or(AUTO_ANCHORS[0]),
            offset: config.offset,
            layer: Layer::Desktop,
            hide_on_fullscreen: true,
            ..Default::default()
        };
        Self {
            inner: Floating::new(desk, floating, theme),
            config,
            choices,
            source,
            desktop: None,
            monitor: MonitorInfo {
                width: 1920.0,
                height: 1080.0,
                scale: 1.0,
                origin: (0, 0),
            },
            stale: true,
        }
    }

    pub fn desk(&self) -> &Desk {
        self.inner.view()
    }

    /// Picks the anchor and text color for the current size, monitor and wallpaper. `false`
    /// when there is nothing to pick from yet.
    fn pick(&mut self, now: Instant) -> bool {
        let rect = self.inner.placement(now).rect;
        let Some(shown) = self.desktop.as_ref().and_then(|d| d.shown(&self.monitor)) else {
            return false;
        };
        if rect.is_empty() {
            return false;
        }
        let screen = (self.monitor.width, self.monitor.height);
        let size = (rect.w, rect.h);
        let anchor = self
            .config
            .anchor
            .or_else(|| shown.calmest(size, self.config.offset, &AUTO_ANCHORS))
            .unwrap_or(AUTO_ANCHORS[0]);
        self.inner.set_anchor(anchor);
        if self.config.text == TextFrom::Auto {
            let (x, y) = anchor.place(screen, size, self.config.offset);
            let text = shown
                .region(Rect::new(x, y, size.0, size.1))
                .and_then(|r| readable(r.mean, &self.choices));
            self.inner.view_mut().text = text;
        }
        true
    }
}

impl Surface for OnWallpaper {
    fn set_monitor(&mut self, monitor: MonitorInfo) {
        self.monitor = monitor;
        self.stale = true;
        self.inner.set_monitor(monitor);
    }

    fn tick(&mut self, now: Instant, pointer: Option<(f32, f32)>, fullscreen: bool) -> Response {
        if let Some(d) = self.source.poll() {
            self.desktop = Some(d);
            self.stale = true;
        }
        let r = self.inner.tick(now, pointer, fullscreen);
        if self.stale && self.pick(now) {
            self.stale = false;
            // Laid out again with the new color, drawn at the new spot.
            return r
                .merge(self.inner.tick(now, pointer, fullscreen))
                .merge(Response::REDRAW);
        }
        r
    }

    fn placement(&self, now: Instant) -> Placement {
        self.inner.placement(now)
    }

    fn render(&mut self, pixmap: &mut Pixmap, now: Instant) {
        self.inner.render(pixmap, now);
    }

    fn animating(&self, now: Instant) -> bool {
        self.inner.animating(now)
    }

    fn click(&mut self, x: f32, y: f32, now: Instant) -> Response {
        self.inner.click(x, y, now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::ExecConfig;
    use crate::wallpaper::{Fit, Fixed, Hub, Screen, Wallpaper};

    fn config(toml: &str) -> Result<DeskConfig, String> {
        let c: DeskConfig = toml::from_str(toml).map_err(|e| e.to_string())?;
        c.validate("widget \"w\"")?;
        Ok(c)
    }

    #[test]
    fn configs_parse_with_defaults_and_bad_values_are_named() {
        let c = config("").unwrap();
        assert_eq!(c, DeskConfig::default());
        let c = config("anchor = \"bottom_left\"\ntext = \"palette\"\nmonitor = 2").unwrap();
        assert_eq!(c.anchor, Some(Anchor::BottomLeft));
        assert_eq!(c.text, TextFrom::Palette);
        for (bad, says) in [
            ("height = 8", "widget \"w\": desktop height 8"),
            ("monitor = 0", "count from 1"),
            ("offset = [-1, 0]", "offset"),
            ("text = \"red\"", "text"),
            ("size = 3", "size"),
        ] {
            let e = config(bad).err().unwrap();
            assert!(e.contains(says), "{bad}: {e}");
        }
    }

    /// 32x18 (16:9): one half white, the other a busy black and white checkerboard.
    fn half_white(white_left: bool) -> Arc<Wallpaper> {
        let (w, h) = (32u32, 18u32);
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let white = (x < w / 2) == white_left;
                let v = if white || (x + y) % 2 == 0 { 250 } else { 5 };
                rgba.extend_from_slice(&[v, v, v, 255]);
            }
        }
        Arc::new(Wallpaper {
            width: w,
            height: h,
            rgba,
            source: (w, h),
        })
    }

    fn bright_left() -> Option<Arc<Desktop>> {
        Some(Arc::new(Desktop::uniform(
            Some(half_white(true)),
            Fit::Fill,
            Color::BLACK,
        )))
    }

    fn monitor(origin: (i32, i32)) -> MonitorInfo {
        MonitorInfo {
            width: 1600.0,
            height: 900.0,
            scale: 1.0,
            origin,
        }
    }

    fn on(desk: DeskConfig, source: Box<dyn Source>, at: MonitorInfo) -> OnWallpaper {
        let exec = ExecConfig {
            name: "w".into(),
            run: vec!["noren-test-no-such-program".into()],
            interval: Some(60.0),
            on_click: None,
            glue: None,
            desktop: Some(desk.clone()),
        };
        let widget = ExecWidget::spawn(exec).unwrap();
        let theme = Theme::aizome();
        let mut s = OnWallpaper::new(Desk::new(widget, desk.height), desk, theme, source);
        s.set_monitor(at);
        s
    }

    fn surface(desk: DeskConfig, desktop: Option<Arc<Desktop>>) -> OnWallpaper {
        on(desk, Box::new(Fixed::new(desktop)), monitor((0, 0)))
    }

    #[test]
    fn auto_placement_takes_the_calm_side_and_dark_text_on_light() {
        let theme = Theme::aizome();
        let mut s = surface(DeskConfig::default(), bright_left());
        let now = Instant::now();
        assert!(s.tick(now, None, false).redraw);
        let p = s.placement(now);
        assert_eq!(p.layer, Layer::Desktop);
        // Top right is busy, top left is plain white: 32 in from the left.
        assert_eq!((p.rect.x, p.rect.y), (32.0, 32.0), "{p:?}");
        let text = s.desk().text().unwrap();
        // Whichever of the palette's text and background reads better on white.
        let expect = readable(Color::WHITE, &[theme.text, theme.background]).unwrap();
        assert_eq!(text, expect);
        // Nothing new: no re-pick, no redraw.
        assert!(!s.tick(now, None, false).redraw);
    }

    #[test]
    fn a_fixed_anchor_and_palette_text_are_kept() {
        let desk = config("anchor = \"bottom_right\"\ntext = \"palette\"").unwrap();
        let mut s = surface(desk, bright_left());
        let now = Instant::now();
        s.tick(now, None, false);
        let p = s.placement(now);
        assert_eq!(p.rect.x + p.rect.w, 1600.0 - 32.0, "{p:?}");
        assert_eq!(p.rect.y + p.rect.h, 900.0 - 32.0, "{p:?}");
        assert_eq!(s.desk().text(), None);
    }

    #[test]
    fn without_a_wallpaper_the_first_choice_and_palette_stay() {
        let mut s = surface(DeskConfig::default(), None);
        let now = Instant::now();
        s.tick(now, None, false);
        let p = s.placement(now);
        assert_eq!(p.rect.x + p.rect.w, 1600.0 - 32.0, "{p:?}");
        assert_eq!(s.desk().text(), None);
    }

    #[test]
    fn widgets_sharing_one_hub_read_their_own_monitors_wallpaper() {
        // Left monitor: white on the left. Right monitor: white on the right.
        let hub = Hub::new();
        let mut left = on(DeskConfig::default(), Box::new(hub.subscribe()), monitor((0, 0)));
        let mut right = on(
            DeskConfig::default(),
            Box::new(hub.subscribe()),
            monitor((1600, 0)),
        );
        let now = Instant::now();
        left.tick(now, None, false);
        assert_eq!(left.desk().text(), None, "nothing published yet");
        hub.publish(Desktop {
            screens: vec![
                Screen {
                    rect: Some((0, 0, 1600, 900)),
                    image: Some(half_white(true)),
                },
                Screen {
                    rect: Some((1600, 0, 1600, 900)),
                    image: Some(half_white(false)),
                },
            ],
            fit: Fit::Fill,
            background: Color::BLACK,
        });
        assert!(left.tick(now, None, false).redraw);
        assert!(right.tick(now, None, false).redraw);
        let (l, r) = (left.placement(now), right.placement(now));
        assert_eq!(l.rect.x, 32.0, "{l:?}");
        assert_eq!(r.rect.x + r.rect.w, 1600.0 - 32.0, "{r:?}");
        assert!(left.desk().text().is_some() && right.desk().text().is_some());
    }
}
