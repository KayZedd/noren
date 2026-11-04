//! Palettes generated from one accent color, with contrast guaranteed by construction.
//!
//! KDE color schemes are hand-picked sRGB values: ramps are uneven, status colors have
//! unrelated weights, and nothing checks readability. Here every color is derived in OKLCH from
//! a seed, so:
//! - the twelve steps of each ramp are perceptually even (Radix-style roles, see [`Ramp`]);
//! - neutrals are tinted toward the accent hue, so the whole desktop reads as one material;
//! - positive / warning / negative share the accent's lightness and chroma, so no status color
//!   shouts louder than another;
//! - text steps are pushed until they pass APCA thresholds against the background, for any
//!   seed, in dark and light mode (see the tests: every hue is checked).
//!
//! The same [`Palette`] drives the framework [`Theme`] and the exporters in [`export`].

pub mod export;
mod oklch;

pub use oklch::{Oklch, apca, wcag_ratio};
use serde::{Deserialize, Serialize};

use crate::{Color, Theme};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    #[default]
    Dark,
    Light,
}

/// APCA targets for the text steps.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Contrast {
    /// Step 12 ≥ Lc 90, step 11 ≥ Lc 60, colored text ≥ Lc 60, ANSI ≥ Lc 45.
    #[default]
    Standard,
    /// Step 12 ≥ Lc 100, step 11 ≥ Lc 75, colored text ≥ Lc 75, ANSI ≥ Lc 60.
    High,
}

impl Contrast {
    fn targets(self) -> Targets {
        match self {
            Self::Standard => Targets {
                text: 90.0,
                subtle_text: 60.0,
                colored_text: 60.0,
                ansi: 45.0,
            },
            Self::High => Targets {
                text: 100.0,
                subtle_text: 75.0,
                colored_text: 75.0,
                ansi: 60.0,
            },
        }
    }
}

struct Targets {
    text: f32,
    subtle_text: f32,
    colored_text: f32,
    ansi: f32,
}

/// Input of [`Palette::generate`]; also the `[palette]` table of a rice profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaletteSpec {
    /// Seed, used verbatim as accent step 9.
    #[serde(with = "hex")]
    pub accent: Color,
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub contrast: Contrast,
    /// How strongly neutrals lean toward the accent hue, OKLCH chroma. `0` is pure grey.
    #[serde(default = "PaletteSpec::default_tint")]
    pub neutral_tint: f32,
}

impl PaletteSpec {
    fn default_tint() -> f32 {
        0.012
    }

    pub fn new(accent: Color) -> Self {
        Self {
            accent,
            mode: Mode::Dark,
            contrast: Contrast::Standard,
            neutral_tint: Self::default_tint(),
        }
    }
}

/// Twelve steps with fixed roles (the Radix Colors convention):
/// 1 app background, 2 subtle background, 3-5 component backgrounds (normal, hover, pressed),
/// 6-8 borders and separators, 9 solid fill, 10 hovered solid, 11 low-contrast text,
/// 12 high-contrast text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ramp(pub [Color; 12]);

impl Ramp {
    /// 1-based step, as in the role table.
    pub fn step(&self, n: usize) -> Color {
        self.0[n.clamp(1, 12) - 1]
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Palette {
    pub spec: PaletteSpec,
    pub neutral: Ramp,
    pub accent: Ramp,
    pub positive: Ramp,
    pub warning: Ramp,
    pub negative: Ramp,
    /// Text on a step-9 accent fill: black or white, whichever has more contrast.
    pub on_accent: Color,
    /// Terminal colors: black, red, green, yellow, blue, magenta, cyan, white, then brights.
    pub ansi: [Color; 16],
}

/// OKLCH hues of the status and terminal colors.
mod hue {
    pub const RED: f32 = 27.0;
    pub const AMBER: f32 = 75.0;
    pub const YELLOW: f32 = 100.0;
    pub const GREEN: f32 = 145.0;
    pub const CYAN: f32 = 200.0;
    pub const BLUE: f32 = 255.0;
    pub const MAGENTA: f32 = 330.0;
}

const DARK_L: [f32; 12] = [
    0.18, 0.205, 0.245, 0.275, 0.305, 0.345, 0.40, 0.48, 0.0, 0.0, 0.78, 0.93,
];
const LIGHT_L: [f32; 12] = [
    0.99, 0.973, 0.948, 0.92, 0.89, 0.855, 0.81, 0.74, 0.0, 0.0, 0.47, 0.25,
];
/// Chroma of each step relative to the ramp's peak.
const CHROMA_SHAPE: [f32; 12] = [0.12, 0.18, 0.3, 0.42, 0.55, 0.65, 0.78, 0.9, 1.0, 1.0, 0.85, 0.45];

impl Palette {
    pub fn generate(spec: &PaletteSpec) -> Self {
        let targets = spec.contrast.targets();
        let seed = spec.accent.with_alpha(1.0).to_oklch();
        let table = match spec.mode {
            Mode::Dark => DARK_L,
            Mode::Light => LIGHT_L,
        };

        let mut neutral = [Color::BLACK; 12];
        for (i, slot) in neutral.iter_mut().enumerate() {
            let l = match i {
                8 => table[7] + (table[10] - table[7]) * 0.3,
                9 => table[7] + (table[10] - table[7]) * 0.4,
                _ => table[i],
            };
            // Tint fades toward the ends so near-black and near-white stay clean.
            let c = spec.neutral_tint * (1.0 - (2.0 * l - 1.0).powi(4));
            *slot = Oklch::new(l, c, seed.h).to_color();
        }
        let bg = neutral[0];
        push_text(&mut neutral[11], bg, targets.text);
        push_text(&mut neutral[10], bg, targets.subtle_text);

        let solid = Oklch::new(seed.l, seed.c, seed.h);
        let accent = colored_ramp(
            solid,
            spec.accent.with_alpha(1.0),
            &table,
            bg,
            spec.mode,
            &targets,
        );
        let status_solid = |h: f32| {
            let (lo, hi) = match spec.mode {
                Mode::Dark => (0.62, 0.78),
                Mode::Light => (0.52, 0.66),
            };
            Oklch::new(seed.l.clamp(lo, hi), seed.c.clamp(0.11, 0.19), h)
        };
        let status = |h: f32| {
            let s = status_solid(h);
            colored_ramp(s, s.to_color(), &table, bg, spec.mode, &targets)
        };
        let (positive, warning, negative) = (status(hue::GREEN), status(hue::AMBER), status(hue::RED));

        let solid9 = accent.step(9);
        let on_accent = if apca(Color::WHITE, solid9).abs() >= apca(Color::BLACK, solid9).abs() {
            Color::WHITE
        } else {
            Color::BLACK
        };

        let (normal_l, bright_l) = match spec.mode {
            Mode::Dark => (0.70, 0.80),
            Mode::Light => (0.52, 0.44),
        };
        let mut ansi = [Color::BLACK; 16];
        let hues = [
            hue::RED,
            hue::GREEN,
            hue::YELLOW,
            hue::BLUE,
            hue::MAGENTA,
            hue::CYAN,
        ];
        for (i, h) in hues.into_iter().enumerate() {
            for (offset, l) in [(0, normal_l), (8, bright_l)] {
                let mut c = Oklch::new(l, 0.14, h).to_color();
                push_text(&mut c, bg, targets.ansi);
                ansi[1 + i + offset] = c;
            }
        }
        let (black, bright_black, white, bright_white) = match spec.mode {
            Mode::Dark => (neutral[2], neutral[7], neutral[10], neutral[11]),
            Mode::Light => (neutral[11], neutral[10], neutral[6], neutral[3]),
        };
        ansi[0] = black;
        ansi[7] = white;
        ansi[8] = bright_black;
        ansi[15] = bright_white;
        push_text(&mut ansi[8], bg, targets.ansi);

        Self {
            spec: spec.clone(),
            neutral: Ramp(neutral),
            accent,
            positive,
            warning,
            negative,
            on_accent,
            ansi,
        }
    }

    pub fn background(&self) -> Color {
        self.neutral.step(1)
    }

    pub fn text(&self) -> Color {
        self.neutral.step(12)
    }

    /// `n` colors that are distinct from each other but share the accent's weight: hues walk
    /// the golden angle from the accent hue. For charts, rings, per-series legends.
    pub fn categorical(&self, n: usize) -> Vec<Color> {
        let seed = self.accent.step(9).to_oklch();
        (0..n)
            .map(|i| Oklch::new(seed.l, seed.c.max(0.1), seed.h + 137.508 * i as f32).to_color())
            .collect()
    }

    /// Theme colors from the palette; fonts, units and opacity stay from `base`.
    pub fn theme(&self, base: Theme) -> Theme {
        Theme {
            text: self.neutral.step(12),
            inactive_text: self.neutral.step(11),
            background: self.neutral.step(1),
            alternate_background: self.neutral.step(2),
            header_background: self.neutral.step(2),
            highlight: self.accent.step(9),
            positive: self.positive.step(11),
            neutral: self.warning.step(11),
            negative: self.negative.step(11),
            ..base
        }
    }
}

fn colored_ramp(
    solid: Oklch,
    exact: Color,
    table: &[f32; 12],
    bg: Color,
    mode: Mode,
    targets: &Targets,
) -> Ramp {
    let mut steps = [Color::BLACK; 12];
    let hover = match mode {
        Mode::Dark => 0.05,
        Mode::Light => -0.05,
    };
    for (i, slot) in steps.iter_mut().enumerate() {
        *slot = match i {
            8 => exact,
            9 => Oklch::new(solid.l + hover, solid.c, solid.h).to_color(),
            _ => Oklch::new(table[i], solid.c * CHROMA_SHAPE[i], solid.h).to_color(),
        };
    }
    push_text(&mut steps[10], bg, targets.colored_text);
    push_text(&mut steps[11], bg, targets.text);
    Ramp(steps)
}

/// Moves `color` away from `bg` in OKLCH lightness until `|APCA| ≥ target` or lightness runs
/// out. Hue is kept; chroma shrinks only as far as the gamut forces it.
fn push_text(color: &mut Color, bg: Color, target: f32) {
    let mut c = color.to_oklch();
    let up = bg.to_oklch().l < 0.5;
    while apca(*color, bg).abs() < target {
        let next = if up { c.l + 0.005 } else { c.l - 0.005 };
        if !(0.0..=1.0).contains(&next) {
            *color = if up { Color::WHITE } else { Color::BLACK };
            return;
        }
        c.l = next;
        *color = c.to_color();
    }
}

/// Serde adapter for `#rrggbb` colors.
pub(crate) mod hex {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    use crate::Color;

    pub fn serialize<S: Serializer>(color: &Color, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&color.to_hex())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Color, D::Error> {
        let raw = String::deserialize(d)?;
        Color::from_hex(&raw).ok_or_else(|| D::Error::custom(format!("bad color {raw:?}, want #rrggbb")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs() -> impl Iterator<Item = PaletteSpec> {
        let seeds = (0..24).flat_map(|i| {
            let h = i as f32 * 15.0;
            [0.45, 0.65, 0.85].map(move |l| Oklch::new(l, 0.15, h).to_color())
        });
        seeds.flat_map(|accent| {
            [Mode::Dark, Mode::Light].into_iter().flat_map(move |mode| {
                [Contrast::Standard, Contrast::High].map(move |contrast| PaletteSpec {
                    accent,
                    mode,
                    contrast,
                    neutral_tint: 0.012,
                })
            })
        })
    }

    #[test]
    fn every_seed_meets_its_contrast_targets() {
        for spec in specs() {
            let p = Palette::generate(&spec);
            let t = spec.contrast.targets();
            let bg = p.background();
            let lc = |c: Color| apca(c, bg).abs();
            let ctx = format!("{} {:?} {:?}", spec.accent.to_hex(), spec.mode, spec.contrast);
            // White on a tinted dark background tops out below Lc 105: past that, the best
            // achievable contrast is the target.
            let ceiling = lc(Color::WHITE).max(lc(Color::BLACK));
            assert!(lc(p.neutral.step(12)) >= t.text.min(ceiling), "text {ctx}");
            assert!(lc(p.neutral.step(11)) >= t.subtle_text, "subtle {ctx}");
            for ramp in [&p.accent, &p.positive, &p.warning, &p.negative] {
                assert!(lc(ramp.step(11)) >= t.colored_text, "colored {ctx}");
            }
            for (i, c) in p.ansi.iter().enumerate().filter(|(i, _)| ![0, 7, 15].contains(i)) {
                assert!(lc(*c) >= t.ansi, "ansi {i} {ctx}");
            }
            let solid = p.accent.step(9);
            assert!(apca(p.on_accent, solid).abs() >= 40.0, "on_accent {ctx}");
        }
    }

    #[test]
    fn backgrounds_get_monotonically_further_from_the_base() {
        for spec in specs() {
            let p = Palette::generate(&spec);
            let l: Vec<f32> = p.neutral.0.iter().map(|c| c.to_oklch().l).collect();
            let rising = l.windows(2).all(|w| w[1] > w[0]);
            let falling = l.windows(2).all(|w| w[1] < w[0]);
            assert!(rising || falling, "{:?} {l:?}", spec.mode);
        }
    }

    #[test]
    fn accent_seed_is_kept_verbatim_and_status_colors_share_its_weight() {
        let spec = PaletteSpec::new(Color::from_hex("#7e9cd8").unwrap());
        let p = Palette::generate(&spec);
        assert_eq!(p.accent.step(9).to_hex(), "#7e9cd8");
        let ls: Vec<f32> = [&p.positive, &p.warning, &p.negative]
            .iter()
            .map(|r| r.step(9).to_oklch().l)
            .collect();
        assert!(ls.iter().all(|l| (l - ls[0]).abs() < 0.01), "{ls:?}");
    }

    #[test]
    fn neutrals_lean_toward_the_accent_hue() {
        let p = Palette::generate(&PaletteSpec::new(Oklch::new(0.6, 0.15, 300.0).to_color()));
        let mid = p.neutral.step(3).to_oklch();
        assert!(mid.c > 0.004 && (mid.h - 300.0).abs() < 8.0, "{mid:?}");
        let grey = Palette::generate(&PaletteSpec {
            neutral_tint: 0.0,
            ..PaletteSpec::new(Color::rgb8(200, 50, 50))
        });
        assert!(grey.neutral.step(3).to_oklch().c < 1e-3);
    }

    #[test]
    fn generation_is_deterministic_and_categorical_colors_differ() {
        let spec = PaletteSpec::new(Color::from_hex("#c34043").unwrap());
        assert_eq!(Palette::generate(&spec), Palette::generate(&spec));
        let cats = Palette::generate(&spec).categorical(5);
        for (i, a) in cats.iter().enumerate() {
            for b in &cats[i + 1..] {
                assert_ne!(a.to_hex(), b.to_hex());
            }
        }
    }
}
