//! Live mirrors: a small always-on-top copy of another app's window, drawn by DWM as it
//! changes, so a build log, a chat or a video stays in sight while other windows cover it.
//!
//! ```toml
//! [[mirror]]
//! exe = "Spotify.exe"        # globs like [[window]]: exe, class, title; at least one
//! anchor = "bottom_right"    # top_left .. bottom_right, default bottom_right
//! offset = [24, 24]          # from the anchored edges, logical pixels, default 24
//! monitor = 1                # same numbering as window rules, default the primary
//! width = 320                # logical pixels, 48 to 4000, default 320
//! max_height = 240           # the height follows the window's shape up to this, default 240
//! opacity = 1.0              # 0.1 to 1, fades into the palette background, default 1
//! client_only = true         # leave out the title bar and frame, default true
//! hide_when_focused = true   # step aside while the window itself is in front, default true
//! ```
//!
//! The mirror follows the first matching window in z-order. A click brings that window to
//! the front. It hides while nothing matches, the window is minimized or a fullscreen app has
//! its monitor. This module is pure; the Win32 runtime finds the windows and places the mirrors.

use serde::{Deserialize, Serialize};

use super::rules::{Rules, ScreenRect, WindowInfo, WindowRule};
use crate::surface::Anchor;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct MirrorConfig {
    pub exe: Option<String>,
    pub class: Option<String>,
    pub title: Option<String>,
    pub anchor: Anchor,
    pub offset: (f32, f32),
    pub monitor: Option<u32>,
    pub width: f32,
    pub max_height: f32,
    pub opacity: f32,
    pub client_only: bool,
    pub hide_when_focused: bool,
}

impl Default for MirrorConfig {
    fn default() -> Self {
        Self {
            exe: None,
            class: None,
            title: None,
            anchor: Anchor::BottomRight,
            offset: (24.0, 24.0),
            monitor: None,
            width: 320.0,
            max_height: 240.0,
            opacity: 1.0,
            client_only: true,
            hide_when_focused: true,
        }
    }
}

/// A validated `[[mirror]]`.
pub struct Mirror {
    pub config: MirrorConfig,
    matcher: Rules,
}

impl Mirror {
    pub fn new(n: usize, c: &MirrorConfig) -> Result<Self, String> {
        let err = |what: String| format!("mirror {n}: {what}");
        if c.exe.is_none() && c.class.is_none() && c.title.is_none() {
            return Err(err("needs exe, class or title to find its window".into()));
        }
        if !(48.0..=4000.0).contains(&c.width) {
            return Err(err(format!("width {} is outside 48..=4000", c.width)));
        }
        if !(48.0..=4000.0).contains(&c.max_height) {
            return Err(err(format!("max_height {} is outside 48..=4000", c.max_height)));
        }
        if !(0.1..=1.0).contains(&c.opacity) {
            return Err(err(format!("opacity {} is outside 0.1..=1.0", c.opacity)));
        }
        if c.monitor == Some(0) {
            return Err(err("monitors count from 1".into()));
        }
        if !(0.0..=10_000.0).contains(&c.offset.0) || !(0.0..=10_000.0).contains(&c.offset.1) {
            return Err(err(format!("offset {:?} must be 0 or more", c.offset)));
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

    /// The mirror's box in physical pixels on a monitor with work area `work` and `scale`,
    /// for a source window of `source` pixels. `None` for an empty source.
    pub fn place(&self, source: (i32, i32), work: ScreenRect, scale: f32) -> Option<ScreenRect> {
        let (w, h) = size(source, (self.config.width, self.config.max_height))?;
        let (pw, ph) = ((w * scale).round(), (h * scale).round());
        let area = (work.width() as f32, work.height() as f32);
        let offset = (self.config.offset.0 * scale, self.config.offset.1 * scale);
        let (x, y) = self.config.anchor.place(area, (pw, ph), offset);
        let (left, top) = (work.left + x.round() as i32, work.top + y.round() as i32);
        Some(ScreenRect {
            left,
            top,
            right: left + pw as i32,
            bottom: top + ph as i32,
        })
    }
}

pub fn mirrors(configs: &[MirrorConfig]) -> Result<Vec<Mirror>, String> {
    configs
        .iter()
        .enumerate()
        .map(|(i, c)| Mirror::new(i + 1, c))
        .collect()
}

/// `source` scaled to fit `bound` with its shape kept, in whole pixels; it may grow.
pub fn size(source: (i32, i32), bound: (f32, f32)) -> Option<(f32, f32)> {
    let (sw, sh) = (source.0 as f32, source.1 as f32);
    if sw < 1.0 || sh < 1.0 {
        return None;
    }
    let k = (bound.0 / sw).min(bound.1 / sh);
    Some(((sw * k).round().max(1.0), (sh * k).round().max(1.0)))
}

/// For each mirror, the first window in `windows` (z-order, front first) it matches.
pub fn pick<W: Copy>(mirrors: &[Mirror], windows: &[(W, WindowInfo)]) -> Vec<Option<W>> {
    mirrors
        .iter()
        .map(|m| windows.iter().find(|(_, info)| m.matches(info)).map(|(w, _)| *w))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(exe: &str, title: &str) -> WindowInfo {
        WindowInfo {
            exe: exe.into(),
            class: "C".into(),
            title: title.into(),
        }
    }

    fn mirror(toml: &str) -> Result<Mirror, String> {
        let c: MirrorConfig = toml::from_str(toml).map_err(|e| e.to_string())?;
        Mirror::new(1, &c)
    }

    #[test]
    fn defaults_parse_and_bad_values_are_named() {
        let m = mirror("exe = \"Spotify.exe\"").unwrap();
        assert_eq!(m.config.anchor, Anchor::BottomRight);
        assert_eq!(m.config.width, 320.0);
        assert!(m.config.client_only && m.config.hide_when_focused);
        let m = mirror("title = \"*log*\"\nanchor = \"top_left\"\noffset = [0, 8]").unwrap();
        assert_eq!(m.config.anchor, Anchor::TopLeft);
        assert_eq!(m.config.offset, (0.0, 8.0));
        for (bad, says) in [
            ("width = 300", "needs exe, class or title"),
            ("exe = \"a\"\nwidth = 10", "width 10"),
            ("exe = \"a\"\nmax_height = 9000", "max_height 9000"),
            ("exe = \"a\"\nopacity = 0", "opacity 0"),
            ("exe = \"a\"\nmonitor = 0", "count from 1"),
            ("exe = \"a\"\noffset = [-1, 0]", "offset"),
            ("exe = \"[\"", "mirror 1: exe"),
        ] {
            let e = mirror(bad).err().unwrap();
            assert!(e.contains(says), "{bad}: {e}");
        }
        assert!(mirror("exe = \"a\"\nsize = 3").err().unwrap().contains("size"));
    }

    #[test]
    fn the_first_matching_window_in_z_order_is_mirrored() {
        let ms = mirrors(&[
            MirrorConfig {
                exe: Some("spotify.EXE".into()),
                ..Default::default()
            },
            MirrorConfig {
                title: Some("*build*".into()),
                ..Default::default()
            },
            MirrorConfig {
                exe: Some("nothing.exe".into()),
                ..Default::default()
            },
        ])
        .unwrap();
        let windows = [
            (1, info("Code.exe", "build log")),
            (2, info("Spotify.exe", "Song")),
            (3, info("wt.exe", "cargo build")),
        ];
        assert_eq!(pick(&ms, &windows), vec![Some(2), Some(1), None]);
    }

    #[test]
    fn size_keeps_the_shape_inside_the_bound() {
        assert_eq!(size((1600, 900), (320.0, 240.0)), Some((320.0, 180.0)));
        assert_eq!(size((900, 1600), (320.0, 240.0)), Some((135.0, 240.0)));
        assert_eq!(size((100, 50), (320.0, 240.0)), Some((320.0, 160.0)));
        assert_eq!(size((0, 900), (320.0, 240.0)), None);
    }

    #[test]
    fn place_anchors_inside_the_work_area_at_scale() {
        let m = mirror("exe = \"a\"").unwrap();
        let work = ScreenRect {
            left: 1920,
            top: 0,
            right: 3840,
            bottom: 1040,
        };
        // 320x180 logical at 150% is 480x270, 24 logical in is 36.
        let r = m.place((1600, 900), work, 1.5).unwrap();
        assert_eq!((r.left, r.top, r.right, r.bottom), (3324, 734, 3804, 1004));
        let m = mirror("exe = \"a\"\nanchor = \"top_left\"\noffset = [0, 0]").unwrap();
        let r = m.place((1600, 900), work, 1.0).unwrap();
        assert_eq!((r.left, r.top, r.right, r.bottom), (1920, 0, 2240, 180));
        assert!(m.place((0, 0), work, 1.0).is_none());
    }
}
