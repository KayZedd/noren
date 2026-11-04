//! Colors, fonts and spacing units, modelled on `Kirigami.Theme` and `Kirigami.Units`.

use std::time::Duration;

use crate::Color;

/// `Kirigami.Units` equivalents, in logical pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct Units {
    pub small_spacing: f32,
    pub large_spacing: f32,
    pub grid_unit: f32,
    pub corner_radius: f32,
    pub icon_small: f32,
    pub icon_medium: f32,
    pub short_duration: Duration,
    pub long_duration: Duration,
}

impl Default for Units {
    fn default() -> Self {
        Self {
            small_spacing: 4.0,
            large_spacing: 8.0,
            grid_unit: 18.0,
            corner_radius: 5.0,
            icon_small: 16.0,
            icon_medium: 32.0,
            short_duration: Duration::from_millis(100),
            long_duration: Duration::from_millis(200),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub text: Color,
    pub inactive_text: Color,
    pub background: Color,
    pub alternate_background: Color,
    pub header_background: Color,
    pub highlight: Color,
    pub positive: Color,
    pub neutral: Color,
    pub negative: Color,
    /// Tried in order; the first installed family wins.
    pub font_families: Vec<String>,
    pub font_point_size: f32,
    pub small_font_point_size: f32,
    pub units: Units,
    /// Opacity of the panel and popup backgrounds.
    pub panel_opacity: f32,
}

impl Theme {
    /// KDE Breeze Dark, the Plasma default.
    pub fn breeze_dark() -> Self {
        Self {
            text: Color::rgb8(252, 252, 252),
            inactive_text: Color::rgb8(161, 169, 177),
            background: Color::rgb8(32, 35, 38),
            alternate_background: Color::rgb8(41, 44, 48),
            header_background: Color::rgb8(41, 44, 48),
            highlight: Color::rgb8(61, 174, 233),
            positive: Color::rgb8(39, 174, 96),
            neutral: Color::rgb8(246, 116, 0),
            negative: Color::rgb8(218, 68, 83),
            font_families: vec!["Noto Sans".into(), "Segoe UI".into()],
            font_point_size: 10.0,
            small_font_point_size: 8.0,
            units: Units::default(),
            panel_opacity: 0.94,
        }
    }

    /// Kanagawa Wave color scheme with accent `#7e9cd8`.
    pub fn kanagawa_wave() -> Self {
        Self {
            text: Color::rgb8(220, 215, 186),
            inactive_text: Color::rgb8(84, 84, 109),
            background: Color::rgb8(31, 31, 40),
            alternate_background: Color::rgb8(22, 22, 29),
            header_background: Color::rgb8(22, 22, 29),
            highlight: Color::rgb8(126, 156, 216),
            positive: Color::rgb8(152, 187, 108),
            neutral: Color::rgb8(220, 165, 97),
            negative: Color::rgb8(195, 64, 67),
            ..Self::breeze_dark()
        }
    }

    /// Builds a theme from a KDE `kdeglobals` file (copy `~/.config/kdeglobals` from the
    /// Plasma machine). Missing keys keep their Breeze Dark values. `[General] AccentColor`
    /// overrides the selection color, as Plasma does.
    pub fn from_kdeglobals(source: &str) -> Result<Self, String> {
        let mut theme = Self::breeze_dark();
        let mut selection = None;
        let mut accent = None;
        let mut header = None;
        let mut section = String::new();
        for (index, raw) in source.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
                continue;
            }
            if line.starts_with('[') {
                // Nested groups such as `[Colors:Header][Inactive]` are not the active palette.
                section = line.to_owned();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let at = |what: &str| format!("kdeglobals line {}: {what}", index + 1);
            let color = || parse_kde_color(value).ok_or_else(|| at(&format!("bad color {value:?}")));
            match (section.as_str(), key) {
                ("[Colors:Window]", "BackgroundNormal") => theme.background = color()?,
                ("[Colors:Window]", "BackgroundAlternate") => theme.alternate_background = color()?,
                ("[Colors:Window]", "ForegroundNormal") => theme.text = color()?,
                ("[Colors:Window]", "ForegroundInactive") => theme.inactive_text = color()?,
                ("[Colors:Window]", "ForegroundPositive") => theme.positive = color()?,
                ("[Colors:Window]", "ForegroundNeutral") => theme.neutral = color()?,
                ("[Colors:Window]", "ForegroundNegative") => theme.negative = color()?,
                ("[Colors:Header]", "BackgroundNormal") => header = Some(color()?),
                ("[Colors:Selection]", "BackgroundNormal") => selection = Some(color()?),
                ("[General]", "AccentColor") => accent = Some(color()?),
                ("[General]", "font") => {
                    let (family, size) = parse_kde_font(value).ok_or_else(|| at("bad font"))?;
                    theme.font_families.insert(0, family);
                    theme.font_point_size = size;
                }
                ("[General]", "smallestReadableFont") => {
                    let (_, size) = parse_kde_font(value).ok_or_else(|| at("bad font"))?;
                    theme.small_font_point_size = size;
                }
                _ => {}
            }
        }
        theme.highlight = accent.or(selection).unwrap_or(theme.highlight);
        theme.header_background = header.unwrap_or(theme.alternate_background);
        theme.font_families.dedup();
        Ok(theme)
    }

    /// `Kirigami.Theme.defaultFont.pixelSize` in logical pixels.
    pub fn font_size(&self) -> f32 {
        self.font_point_size * 96.0 / 72.0
    }

    /// `Kirigami.Theme.smallFont.pixelSize` in logical pixels.
    pub fn small_font_size(&self) -> f32 {
        self.small_font_point_size * 96.0 / 72.0
    }

    /// Plasma widget level colors: 0 normal (highlight), 1 warning, 2 critical.
    pub fn level_color(&self, level: u8) -> Color {
        match level {
            0 => self.highlight,
            1 => self.neutral,
            _ => self.negative,
        }
    }

    /// Text color with alpha, the QML idiom for tracks, separators and card fills.
    pub fn text_alpha(&self, alpha: f32) -> Color {
        self.text.with_alpha(alpha)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::breeze_dark()
    }
}

fn parse_kde_color(value: &str) -> Option<Color> {
    if value.starts_with('#') {
        return Color::from_hex(value);
    }
    let parts: Vec<u8> = value
        .split(',')
        .map(|p| p.trim().parse().ok())
        .collect::<Option<_>>()?;
    match parts[..] {
        [r, g, b] => Some(Color::rgb8(r, g, b)),
        [r, g, b, a] => Some(Color::rgba8(r, g, b, a)),
        _ => None,
    }
}

/// `Noto Sans,10,-1,5,400,0,...` → family and point size.
fn parse_kde_font(value: &str) -> Option<(String, f32)> {
    let mut parts = value.split(',');
    let family = parts.next()?.trim();
    let size: f32 = parts.next()?.trim().parse().ok()?;
    (!family.is_empty() && size.is_finite() && size > 0.0).then(|| (family.to_owned(), size))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
[ColorEffects:Disabled]
Color=56,56,56

[Colors:Selection]
BackgroundNormal=97,118,163

[Colors:Window]
BackgroundAlternate=22,22,29
BackgroundNormal=31,31,40
ForegroundInactive=84,84,109
ForegroundNegative=195,64,67
ForegroundNeutral=220,165,97
ForegroundNormal=220,215,186
ForegroundPositive=152,187,108

[Colors:Window][Inactive]
BackgroundNormal=1,2,3

[General]
AccentColor=126,156,216
font=Inter,11,-1,5,400,0,0,0,0,0,0,0,0,0,0,1
smallestReadableFont=Inter,9,-1,5,400,0,0,0,0,0,0,0,0,0,0,1
";

    #[test]
    fn kdeglobals_maps_window_palette_accent_and_fonts() {
        let theme = Theme::from_kdeglobals(SAMPLE).unwrap();
        let kanagawa = Theme::kanagawa_wave();
        assert_eq!(theme.background, kanagawa.background);
        assert_eq!(theme.alternate_background, kanagawa.alternate_background);
        assert_eq!(theme.text, kanagawa.text);
        assert_eq!(theme.highlight, Color::rgb8(126, 156, 216));
        assert_eq!(theme.negative, kanagawa.negative);
        assert_eq!(theme.header_background, kanagawa.alternate_background);
        assert_eq!(theme.font_families[0], "Inter");
        assert_eq!(theme.font_point_size, 11.0);
        assert_eq!(theme.small_font_point_size, 9.0);
    }

    #[test]
    fn selection_is_used_without_accent_and_inactive_groups_are_ignored() {
        let source = SAMPLE.replace("AccentColor=126,156,216\n", "");
        let theme = Theme::from_kdeglobals(&source).unwrap();
        assert_eq!(theme.highlight, Color::rgb8(97, 118, 163));
        assert_eq!(theme.background, Color::rgb8(31, 31, 40));
    }

    #[test]
    fn malformed_values_report_the_line() {
        let error = Theme::from_kdeglobals("[Colors:Window]\nBackgroundNormal=1,2\n").unwrap_err();
        assert!(error.contains("line 2"), "{error}");
        assert!(Theme::from_kdeglobals("[General]\nfont=,10\n").is_err());
        assert!(Theme::from_kdeglobals("").is_ok());
    }

    #[test]
    fn units_and_font_sizes_match_kirigami_defaults() {
        let theme = Theme::breeze_dark();
        assert!((theme.font_size() - 13.333).abs() < 0.01);
        assert!((theme.small_font_size() - 10.666).abs() < 0.01);
        assert_eq!(theme.units.grid_unit, 18.0);
        assert_eq!(theme.level_color(0), theme.highlight);
        assert_eq!(theme.level_color(1), theme.neutral);
        assert_eq!(theme.level_color(7), theme.negative);
    }
}
