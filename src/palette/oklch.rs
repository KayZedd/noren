//! OKLab / OKLCH (Björn Ottosson, 2020) and APCA contrast (APCA-W3 0.0.98G-4g).
//!
//! OKLCH is perceptually uniform: equal steps in `l` look like equal steps in lightness, and
//! changing `h` keeps perceived lightness and colorfulness. HSL does neither, which is why
//! ramps built in it look uneven.

use crate::Color;

/// Lightness `l` in `0..=1`, chroma `c` (`0..≈0.37` inside sRGB), hue `h` in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Oklch {
    pub l: f32,
    pub c: f32,
    pub h: f32,
}

impl Oklch {
    pub const fn new(l: f32, c: f32, h: f32) -> Self {
        Self { l, c, h }
    }

    /// Converts to sRGB. Out-of-gamut colors keep lightness and hue and lose chroma until they
    /// fit, so the result is the most colorful displayable color that looks the same lightness.
    pub fn to_color(self) -> Color {
        let l = f64::from(self.l.clamp(0.0, 1.0));
        let h = f64::from(self.h).to_radians();
        let at = |c: f64| oklab_to_linear(l, c * h.cos(), c * h.sin());
        let mut rgb = at(f64::from(self.c.max(0.0)));
        if !in_gamut(rgb) {
            let (mut lo, mut hi) = (0.0, f64::from(self.c));
            for _ in 0..32 {
                let mid = (lo + hi) / 2.0;
                if in_gamut(at(mid)) {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            rgb = at(lo);
        }
        let channel = |v: f64| linear_to_srgb(v.clamp(0.0, 1.0)) as f32;
        Color {
            r: channel(rgb[0]),
            g: channel(rgb[1]),
            b: channel(rgb[2]),
            a: 1.0,
        }
    }
}

impl Color {
    pub fn to_oklch(self) -> Oklch {
        let [l, a, b] = linear_to_oklab([
            srgb_to_linear(f64::from(self.r)),
            srgb_to_linear(f64::from(self.g)),
            srgb_to_linear(f64::from(self.b)),
        ]);
        let c = (a * a + b * b).sqrt();
        let h = if c < 1e-6 {
            0.0
        } else {
            b.atan2(a).to_degrees().rem_euclid(360.0)
        };
        Oklch::new(l as f32, c as f32, h as f32)
    }

    /// `#rrggbb`, alpha dropped.
    pub fn to_hex(self) -> String {
        let [r, g, b, _] = self.to_rgba8();
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f64) -> f64 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn in_gamut(rgb: [f64; 3]) -> bool {
    const EPS: f64 = 1e-7;
    rgb.iter().all(|v| (-EPS..=1.0 + EPS).contains(v))
}

fn linear_to_oklab([r, g, b]: [f64; 3]) -> [f64; 3] {
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    [
        0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s,
        1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s,
        0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s,
    ]
}

fn oklab_to_linear(l: f64, a: f64, b: f64) -> [f64; 3] {
    let l_ = (l + 0.3963377774 * a + 0.2158037573 * b).powi(3);
    let m_ = (l - 0.1055613458 * a - 0.0638541728 * b).powi(3);
    let s_ = (l - 0.0894841775 * a - 1.2914855480 * b).powi(3);
    [
        4.0767416621 * l_ - 3.3077115913 * m_ + 0.2309699292 * s_,
        -1.2684380046 * l_ + 2.6097574011 * m_ - 0.3413193965 * s_,
        -0.0041960863 * l_ - 0.7034186147 * m_ + 1.7076147010 * s_,
    ]
}

/// APCA lightness contrast `Lc` of `text` on `background`. Positive for dark text on light
/// backgrounds, negative for light on dark; `|Lc| ≥ 75` is body text, `≥ 60` content text,
/// `≥ 45` large text and UI glyphs, `≥ 30` non-text elements. Alpha is ignored.
pub fn apca(text: Color, background: Color) -> f32 {
    const BLACK_THRESHOLD: f64 = 0.022;
    const BLACK_CLAMP: f64 = 1.414;
    const SCALE: f64 = 1.14;
    const OFFSET: f64 = 0.027;
    const LOW_CLIP: f64 = 0.1;
    let luminance = |c: Color| {
        let y = 0.2126729 * f64::from(c.r).powf(2.4)
            + 0.7151522 * f64::from(c.g).powf(2.4)
            + 0.0721750 * f64::from(c.b).powf(2.4);
        if y < BLACK_THRESHOLD {
            y + (BLACK_THRESHOLD - y).powf(BLACK_CLAMP)
        } else {
            y
        }
    };
    let (txt, bg) = (luminance(text), luminance(background));
    if (bg - txt).abs() < 0.0005 {
        return 0.0;
    }
    let lc = if bg > txt {
        let s = (bg.powf(0.56) - txt.powf(0.57)) * SCALE;
        if s < LOW_CLIP { 0.0 } else { s - OFFSET }
    } else {
        let s = (bg.powf(0.65) - txt.powf(0.62)) * SCALE;
        if s > -LOW_CLIP { 0.0 } else { s + OFFSET }
    };
    (lc * 100.0) as f32
}

/// WCAG 2 contrast ratio, `1..=21`. Kept for tools that still audit against it.
pub fn wcag_ratio(a: Color, b: Color) -> f32 {
    let luminance = |c: Color| {
        0.2126 * srgb_to_linear(f64::from(c.r))
            + 0.7152 * srgb_to_linear(f64::from(c.g))
            + 0.0722 * srgb_to_linear(f64::from(c.b))
    };
    let (la, lb) = (luminance(a), luminance(b));
    ((la.max(lb) + 0.05) / (la.min(lb) + 0.05)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps
    }

    #[test]
    fn oklch_matches_reference_values_and_round_trips() {
        // Reference: oklch.com / CSS Color 4 for pure sRGB primaries.
        let red = Color::rgb8(255, 0, 0).to_oklch();
        assert!(close(red.l, 0.6279, 1e-3) && close(red.c, 0.2577, 1e-3) && close(red.h, 29.23, 0.1));
        let white = Color::WHITE.to_oklch();
        assert!(close(white.l, 1.0, 1e-4) && white.c < 1e-4);
        for hex in ["#7e9cd8", "#1f1f28", "#c34043", "#98bb6c", "#000000", "#ffffff"] {
            let c = Color::from_hex(hex).unwrap();
            assert_eq!(c.to_oklch().to_color().to_hex(), hex);
        }
    }

    #[test]
    fn out_of_gamut_keeps_lightness_and_hue() {
        let wild = Oklch::new(0.7, 0.4, 145.0).to_color();
        let back = wild.to_oklch();
        assert!(close(back.l, 0.7, 2e-3), "{back:?}");
        assert!(close(back.h, 145.0, 1.0), "{back:?}");
        assert!(back.c < 0.4 && back.c > 0.15, "{back:?}");
    }

    #[test]
    fn apca_matches_reference_values() {
        // Published APCA-W3 0.0.98G values.
        assert!(close(apca(Color::BLACK, Color::WHITE), 106.04, 0.05));
        assert!(close(apca(Color::WHITE, Color::BLACK), -107.88, 0.05));
        let grey = Color::rgb8(0x88, 0x88, 0x88);
        assert!(close(apca(grey, Color::WHITE), 63.06, 0.05));
        assert!(close(apca(Color::WHITE, grey), -68.54, 0.05));
        assert_eq!(apca(grey, grey), 0.0);
    }

    #[test]
    fn wcag_ratio_extremes() {
        assert!(close(wcag_ratio(Color::BLACK, Color::WHITE), 21.0, 1e-3));
        assert!(close(wcag_ratio(Color::WHITE, Color::WHITE), 1.0, 1e-6));
    }
}
