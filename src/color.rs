/// Straight (non-premultiplied) RGBA color, each channel in `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub r: f32,
    pub g: f32,
    pub b: f32,
    pub a: f32,
}

impl Color {
    pub const TRANSPARENT: Self = Self::rgba8(0, 0, 0, 0);
    pub const BLACK: Self = Self::rgb8(0, 0, 0);
    pub const WHITE: Self = Self::rgb8(255, 255, 255);

    pub const fn rgb8(r: u8, g: u8, b: u8) -> Self {
        Self::rgba8(r, g, b, 255)
    }

    pub const fn rgba8(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self {
            r: r as f32 / 255.0,
            g: g as f32 / 255.0,
            b: b as f32 / 255.0,
            a: a as f32 / 255.0,
        }
    }

    /// Same color with alpha replaced, like QML `Qt.rgba(c.r, c.g, c.b, alpha)`.
    pub fn with_alpha(self, alpha: f32) -> Self {
        Self {
            a: alpha.clamp(0.0, 1.0),
            ..self
        }
    }

    /// Same color with alpha multiplied, used for item `opacity`.
    pub fn fade(self, opacity: f32) -> Self {
        self.with_alpha(self.a * opacity.clamp(0.0, 1.0))
    }

    /// Parses `#rrggbb` or `#rrggbbaa`.
    pub fn from_hex(hex: &str) -> Option<Self> {
        let digits = hex.strip_prefix('#')?;
        if !matches!(digits.len(), 6 | 8) || !digits.is_ascii() {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
        let alpha = if digits.len() == 8 { byte(6)? } else { 255 };
        Some(Self::rgba8(byte(0)?, byte(2)?, byte(4)?, alpha))
    }

    pub fn to_rgba8(self) -> [u8; 4] {
        let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        [channel(self.r), channel(self.g), channel(self.b), channel(self.a)]
    }

    pub(crate) fn to_skia(self) -> tiny_skia::Color {
        let [r, g, b, a] = self.to_rgba8();
        tiny_skia::Color::from_rgba8(r, g, b, a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_parses_rgb_and_rgba_and_rejects_garbage() {
        assert_eq!(Color::from_hex("#7e9cd8"), Some(Color::rgb8(126, 156, 216)));
        assert_eq!(
            Color::from_hex("#7E9CD880"),
            Some(Color::rgba8(126, 156, 216, 128))
        );
        for bad in ["7e9cd8", "#7e9cd", "#7e9cd8f", "#zzzzzz", "#ąąą"] {
            assert_eq!(Color::from_hex(bad), None, "{bad}");
        }
    }

    #[test]
    fn alpha_helpers_clamp_and_multiply() {
        let c = Color::rgba8(10, 20, 30, 255);
        assert_eq!(c.with_alpha(0.15).a, 0.15);
        assert_eq!(c.with_alpha(2.0).a, 1.0);
        assert_eq!(c.with_alpha(0.5).fade(0.5).a, 0.25);
        assert_eq!(c.fade(-1.0).a, 0.0);
        assert_eq!(c.to_rgba8(), [10, 20, 30, 255]);
    }
}
