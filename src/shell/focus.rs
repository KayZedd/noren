//! Focus dimming and x-ray: profile config and the pure decisions behind them.
//!
//! ```toml
//! [focus]
//! dim = 0.35                 # how dark everything behind the focused window gets
//! color = "neutral.1"        # overlay color: hex or a palette role, default black
//! except = ["mpv.exe"]       # exe globs whose focus shows no dim
//!
//! [xray]
//! key = "Ctrl+Alt+Space"     # while held, every window turns see-through
//! opacity = 0.2
//! ```
//!
//! Dimming is one click-through overlay kept directly behind the foreground window in the
//! z-order. X-ray lowers every window's opacity through the same diff-and-restore path as
//! window rules, so releasing the key puts back exactly what was there.

use std::collections::BTreeMap;

use glob::{MatchOptions, Pattern};
use serde::{Deserialize, Serialize};

use super::keys::Combo;
use super::rules::{ColorSpec, ScreenRect, Style};
use crate::palette::Palette;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FocusConfig {
    /// Overlay opacity, `0.05..=0.9`.
    pub dim: f32,
    #[serde(default)]
    pub color: Option<ColorSpec>,
    #[serde(default)]
    pub except: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct XrayConfig {
    pub key: String,
    /// Window opacity while the key is held, `0.05..=0.9`.
    #[serde(default = "XrayConfig::default_opacity")]
    pub opacity: f32,
}

impl XrayConfig {
    fn default_opacity() -> f32 {
        0.2
    }
}

const MATCH: MatchOptions = MatchOptions {
    case_sensitive: false,
    require_literal_separator: false,
    require_literal_leading_dot: false,
};

fn alpha(table: &str, field: &str, v: f32) -> Result<u8, String> {
    if (0.05..=0.9).contains(&v) {
        Ok((v * 255.0).round() as u8)
    } else {
        Err(format!("[{table}] {field} {v} is outside 0.05..=0.9"))
    }
}

#[derive(Clone, Debug)]
pub struct Focus {
    /// Overlay alpha.
    pub alpha: u8,
    color: Option<ColorSpec>,
    except: Vec<Pattern>,
}

impl Focus {
    pub fn new(c: &FocusConfig) -> Result<Self, String> {
        let alpha = alpha("focus", "dim", c.dim)?;
        if let Some(spec) = &c.color
            && matches!(spec, ColorSpec::Default | ColorSpec::None | ColorSpec::State(_))
        {
            return Err(format!("[focus] color {spec}: use a hex color or a palette role"));
        }
        let except = c
            .except
            .iter()
            .map(|g| Pattern::new(g).map_err(|e| format!("[focus] except {g:?}: {e}")))
            .collect::<Result<_, _>>()?;
        Ok(Self {
            alpha,
            color: c.color.clone(),
            except,
        })
    }

    /// Overlay color as a COLORREF; black unless set.
    pub fn colorref(&self, p: &Palette) -> u32 {
        self.color
            .as_ref()
            .and_then(|c| c.resolve(p, &BTreeMap::new()))
            .unwrap_or(0)
    }

    /// Whether to dim behind a focused window of `exe` at `window` on a monitor whose full
    /// (not work-area) bounds are `monitor`. Fullscreen windows hide everything anyway, and
    /// an overlay behind a game or video only costs compositing.
    pub fn dims(&self, exe: &str, window: ScreenRect, monitor: ScreenRect) -> bool {
        !self.except.iter().any(|p| p.matches_with(exe, MATCH)) && !fullscreen(window, monitor)
    }
}

pub fn fullscreen(w: ScreenRect, m: ScreenRect) -> bool {
    w.left <= m.left && w.top <= m.top && w.right >= m.right && w.bottom >= m.bottom
}

#[derive(Clone, Debug)]
pub struct Xray {
    pub combo: Combo,
    pub alpha: u8,
}

impl Xray {
    pub fn new(c: &XrayConfig) -> Result<Self, String> {
        Ok(Self {
            combo: Combo::parse(&c.key).map_err(|e| format!("[xray] key: {e}"))?,
            alpha: alpha("xray", "opacity", c.opacity)?,
        })
    }

    /// `style` while x-ray is held: never more opaque than the x-ray alpha, never more
    /// opaque than a rule already made it.
    pub fn apply(&self, style: Style) -> Style {
        Style {
            opacity: Some(style.opacity.map_or(self.alpha, |o| o.min(self.alpha))),
            ..style
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Color;
    use crate::palette::PaletteSpec;
    use crate::palette::export::colorref;

    fn rect(left: i32, top: i32, right: i32, bottom: i32) -> ScreenRect {
        ScreenRect {
            left,
            top,
            right,
            bottom,
        }
    }

    fn focus(toml_text: &str) -> Result<Focus, String> {
        let c: FocusConfig = toml::from_str(toml_text).map_err(|e| e.to_string())?;
        Focus::new(&c)
    }

    #[test]
    fn focus_validates_and_resolves_color() {
        let p = Palette::generate(&PaletteSpec::new(Color::from_hex("#7e9cd8").unwrap()));
        let f = focus("dim = 0.4").unwrap();
        assert_eq!(f.alpha, 102);
        assert_eq!(f.colorref(&p), 0, "black by default");
        let f = focus("dim = 0.4\ncolor = \"neutral.1\"").unwrap();
        assert_eq!(f.colorref(&p), colorref(p.neutral.step(1)));
        assert!(focus("dim = 0.95").unwrap_err().contains("0.05..=0.9"));
        assert!(
            focus("dim = 0.3\ncolor = \"state:x\"")
                .unwrap_err()
                .contains("palette role")
        );
        assert!(
            focus("dim = 0.3\nexcept = [\"[\"]")
                .unwrap_err()
                .contains("except")
        );
        assert!(focus("dim = 0.3\nfoo = 1").is_err(), "unknown fields rejected");
    }

    #[test]
    fn no_dim_for_excluded_or_fullscreen_windows() {
        let f = focus("dim = 0.3\nexcept = [\"MPV.exe\"]").unwrap();
        let mon = rect(0, 0, 1920, 1080);
        let normal = rect(100, 100, 900, 700);
        assert!(f.dims("code.exe", normal, mon));
        assert!(!f.dims("mpv.exe", normal, mon), "globs are case-insensitive");
        assert!(!f.dims("game.exe", rect(0, 0, 1920, 1080), mon));
        assert!(
            !f.dims("game.exe", rect(-8, -8, 1928, 1088), mon),
            "borders past the edge"
        );
        assert!(
            f.dims("code.exe", rect(-8, -8, 1928, 1048), mon),
            "maximized above the taskbar"
        );
        assert!(
            f.dims("code.exe", rect(1920, 0, 3840, 1080), mon),
            "fullscreen elsewhere"
        );
    }

    #[test]
    fn xray_caps_opacity_without_raising_it() {
        let x = Xray::new(&toml::from_str("key = \"Ctrl+Alt+Space\"").unwrap()).unwrap();
        assert_eq!(x.alpha, 51, "0.2 by default");
        let ruled = Style {
            opacity: Some(30),
            corners: Some(2),
            ..Style::default()
        };
        assert_eq!(x.apply(ruled).opacity, Some(30));
        assert_eq!(x.apply(ruled).corners, Some(2));
        assert_eq!(x.apply(Style::default()).opacity, Some(51));
        let err = Xray::new(&XrayConfig {
            key: "Space".into(),
            opacity: 0.2,
        })
        .unwrap_err();
        assert!(err.starts_with("[xray] key"), "{err}");
    }
}
