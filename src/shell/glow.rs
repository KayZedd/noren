//! Ambient edge glow: a thin band of light along a screen edge that shows a named state, so a
//! finished build or a waiting agent is visible without looking at any window.
//!
//! ```toml
//! [[glow]]
//! state = "ci"               # follows `noren state ci ...`; dark while the state is clear
//! edges = ["top"]            # top | bottom | left | right, default all four
//! monitor = 1                # same numbering as window rules, default every monitor
//! width = 8                  # pixels at 100% scale, 2 to 48, default 8
//! opacity = 0.85             # peak, 0.1 to 1, default 0.85
//! pulse = ["busy", "warn"]   # states that breathe instead of glowing steadily (the default)
//! settle = 6                 # ok fades out after this many seconds, default: stays on
//! ```
//!
//! Colors come from the palette (ok is positive, fail negative, warn warning, busy accent).
//! A state change crossfades, a cleared state fades out. This module is pure: the Win32
//! runtime feeds it states and the clock and draws the [`Bar`]s it returns.

use std::collections::BTreeMap;
use std::f32::consts::TAU;
use std::fmt;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::rules::ScreenRect;
use super::state::{State, valid_name};
use crate::anim::ease_out_cubic;
use crate::palette::Palette;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
}

impl Edge {
    pub const ALL: [Self; 4] = [Self::Top, Self::Bottom, Self::Left, Self::Right];

    fn horizontal(self) -> bool {
        matches!(self, Self::Top | Self::Bottom)
    }

    /// The band on `screen`'s edge, `width` pixels deep.
    pub fn rect(self, screen: ScreenRect, width: i32) -> ScreenRect {
        let ScreenRect {
            left,
            top,
            right,
            bottom,
        } = screen;
        match self {
            Self::Top => ScreenRect {
                bottom: top + width,
                ..screen
            },
            Self::Bottom => ScreenRect {
                top: bottom - width,
                ..screen
            },
            Self::Left => ScreenRect {
                right: left + width,
                ..screen
            },
            Self::Right => ScreenRect {
                left: right - width,
                ..screen
            },
        }
    }
}

impl fmt::Display for Edge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Left => "left",
            Self::Right => "right",
        })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlowConfig {
    pub state: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edges: Vec<Edge>,
    pub monitor: Option<u32>,
    #[serde(default = "GlowConfig::default_width")]
    pub width: u32,
    #[serde(default = "GlowConfig::default_opacity")]
    pub opacity: f32,
    #[serde(default = "GlowConfig::default_pulse")]
    pub pulse: Vec<String>,
    pub settle: Option<f32>,
}

impl GlowConfig {
    fn default_width() -> u32 {
        8
    }

    fn default_opacity() -> f32 {
        0.85
    }

    fn default_pulse() -> Vec<String> {
        vec!["busy".into(), "warn".into()]
    }
}

/// How long a change crossfades, and a cleared or settled glow fades out.
pub const FADE: Duration = Duration::from_millis(400);
/// One breath of a pulsing state.
pub const PERIOD: Duration = Duration::from_millis(1800);
/// The dimmest point of a breath, as a share of the peak.
const BREATH_LOW: f32 = 0.3;

/// One validated `[[glow]]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Glow {
    pub state: String,
    pub edges: Vec<Edge>,
    pub monitor: Option<u32>,
    pub width: u32,
    pub peak: f32,
    pulse: Vec<State>,
    settle: Option<Duration>,
}

impl Glow {
    pub fn new(c: &GlowConfig) -> Result<Self, String> {
        let at = format!("[[glow]] {:?}", c.state);
        if !valid_name(&c.state) {
            return Err(format!("{at}: not a valid state name"));
        }
        if !(2..=48).contains(&c.width) {
            return Err(format!("{at}: width {} is outside 2..=48", c.width));
        }
        if !(0.1..=1.0).contains(&c.opacity) {
            return Err(format!("{at}: opacity {} is outside 0.1..=1", c.opacity));
        }
        if c.monitor == Some(0) {
            return Err(format!("{at}: monitors count from 1"));
        }
        let settle = match c.settle {
            Some(s) if !(0.5..=3600.0).contains(&s) => {
                return Err(format!("{at}: settle {s} is outside 0.5..=3600 seconds"));
            }
            s => s.map(Duration::from_secs_f32),
        };
        let pulse = c
            .pulse
            .iter()
            .map(|s| s.parse().map_err(|e| format!("{at}: pulse: {e}")))
            .collect::<Result<_, _>>()?;
        let mut edges = if c.edges.is_empty() {
            Edge::ALL.to_vec()
        } else {
            c.edges.clone()
        };
        edges.sort();
        edges.dedup();
        Ok(Self {
            state: c.state.clone(),
            edges,
            monitor: c.monitor,
            width: c.width,
            peak: c.opacity,
            pulse,
            settle,
        })
    }

    fn overlaps(&self, other: &Self) -> Option<Edge> {
        let monitors = match (self.monitor, other.monitor) {
            (Some(a), Some(b)) => a == b,
            _ => true,
        };
        monitors
            .then(|| self.edges.iter().find(|e| other.edges.contains(e)).copied())
            .flatten()
    }

    /// Brightness of `state` held for `held`, `0..=1` before the peak.
    fn level(&self, state: State, held: Duration) -> f32 {
        if let (State::Ok, Some(settle)) = (state, self.settle)
            && held >= settle
        {
            let t = (held - settle).as_secs_f32() / FADE.as_secs_f32();
            return 1.0 - ease_out_cubic(t);
        }
        if self.pulse.contains(&state) {
            let phase = held.as_secs_f32() / PERIOD.as_secs_f32();
            let wave = 0.5 + 0.5 * (TAU * phase).cos();
            return BREATH_LOW + (1.0 - BREATH_LOW) * wave;
        }
        1.0
    }

    /// Whether `state` held for `held` still changes over time.
    fn moving(&self, state: State, held: Duration) -> bool {
        match (state, self.settle) {
            (State::Ok, Some(settle)) => held < settle + FADE,
            _ => self.pulse.contains(&state),
        }
    }
}

/// Every `[[glow]]`, validated together: two glows may not light the same edge of the same
/// monitor.
pub fn glows(list: &[GlowConfig]) -> Result<Vec<Glow>, String> {
    let glows: Vec<Glow> = list.iter().map(Glow::new).collect::<Result<_, _>>()?;
    for (i, a) in glows.iter().enumerate() {
        for b in &glows[i + 1..] {
            if let Some(edge) = a.overlaps(b) {
                return Err(format!(
                    "[[glow]] {:?} and {:?} both light the {edge} edge of the same monitor",
                    a.state, b.state
                ));
            }
        }
    }
    Ok(glows)
}

/// What one glow shows right now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bar {
    /// Index into the glow list.
    pub glow: usize,
    pub rgb: [u8; 3],
    /// Final opacity, `0..=1`, peak included.
    pub alpha: f32,
}

#[derive(Clone, Debug)]
struct Track {
    from: Option<State>,
    to: Option<State>,
    /// When `to` was set; `from` was set at `before`.
    since: Instant,
    before: Instant,
}

/// The glows plus their animation state.
pub struct Glows {
    pub list: Vec<Glow>,
    tracks: Vec<Track>,
}

impl Glows {
    pub fn new(list: Vec<Glow>, now: Instant) -> Self {
        let tracks = list
            .iter()
            .map(|_| Track {
                from: None,
                to: None,
                since: now,
                before: now,
            })
            .collect();
        Self { list, tracks }
    }

    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Takes the current states; a change starts a crossfade from wherever the glow is.
    pub fn update(&mut self, states: &BTreeMap<String, State>, now: Instant) {
        for (g, t) in self.list.iter().zip(&mut self.tracks) {
            let next = states.get(&g.state).copied();
            if next != t.to {
                t.from = t.to;
                t.before = t.since;
                t.to = next;
                t.since = now;
            }
        }
    }

    /// The glows that show anything, with their color and opacity at `now`.
    pub fn bars(&self, p: &Palette, now: Instant) -> Vec<Bar> {
        let mut out = Vec::new();
        for (i, (g, t)) in self.list.iter().zip(&self.tracks).enumerate() {
            let k = ease_out_cubic(now.saturating_duration_since(t.since).as_secs_f32() / FADE.as_secs_f32());
            let side = |s: Option<State>, since: Instant| {
                s.map(|s| (rgb(s, p), g.level(s, now.saturating_duration_since(since))))
            };
            let (rgb, level) = match (side(t.from, t.before), side(t.to, t.since)) {
                (None, None) => continue,
                (None, Some((c, l))) => (c, l * k),
                (Some((c, l)), None) => (c, l * (1.0 - k)),
                (Some((a, la)), Some((b, lb))) => (mix(a, b, k), la + (lb - la) * k),
            };
            let alpha = level * g.peak;
            if alpha > 0.003 {
                out.push(Bar { glow: i, rgb, alpha });
            }
        }
        out
    }

    /// Whether [`Glows::bars`] still changes without a new state: a crossfade, a breath or a
    /// pending settle. The runtime redraws every frame while this holds.
    pub fn animating(&self, now: Instant) -> bool {
        self.list.iter().zip(&self.tracks).any(|(g, t)| {
            let lit = t.from.is_some() || t.to.is_some();
            (lit && now.saturating_duration_since(t.since) < FADE)
                || t.to
                    .is_some_and(|s| g.moving(s, now.saturating_duration_since(t.since)))
        })
    }
}

fn rgb(s: State, p: &Palette) -> [u8; 3] {
    let [r, g, b, _] = s.color(p).to_rgba8();
    [r, g, b]
}

fn mix(a: [u8; 3], b: [u8; 3], k: f32) -> [u8; 3] {
    std::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * k).round() as u8)
}

/// Pixels of a band on `edge`, `len` long and `width` deep, as premultiplied BGRA rows (the
/// layout `UpdateLayeredWindow` takes) at full opacity; the runtime fades it with the blend's
/// constant alpha. Light is strongest at the screen edge and falls off quadratically inward;
/// the ends taper so the band never stops with a hard corner.
pub fn band(edge: Edge, len: u32, width: u32, rgb: [u8; 3]) -> (u32, u32, Vec<u8>) {
    let (len, width) = (len.max(1), width.max(1));
    let (w, h) = if edge.horizontal() {
        (len, width)
    } else {
        (width, len)
    };
    let taper = (len as f32 / 6.0).clamp(1.0, 160.0);
    let mut px = Vec::with_capacity(w as usize * h as usize * 4);
    for y in 0..h {
        for x in 0..w {
            // `depth`: distance from the screen edge; `along`: position on the edge.
            let (depth, along) = match edge {
                Edge::Top => (y, x),
                Edge::Bottom => (h - 1 - y, x),
                Edge::Left => (x, y),
                Edge::Right => (w - 1 - x, y),
            };
            let fall = 1.0 - depth as f32 / width as f32;
            let end = (along.min(len - 1 - along) as f32 + 0.5) / taper;
            let a = fall * fall * smoothstep(end.min(1.0));
            let a8 = (a * 255.0).round() as u8;
            let pre = |c: u8| (c as f32 * a).round() as u8;
            px.extend_from_slice(&[pre(rgb[2]), pre(rgb[1]), pre(rgb[0]), a8]);
        }
    }
    (w, h, px)
}

fn smoothstep(t: f32) -> f32 {
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::PaletteSpec;

    fn cfg(toml: &str) -> GlowConfig {
        toml::from_str(&format!("state = \"ci\"\n{toml}")).unwrap()
    }

    fn palette() -> Palette {
        Palette::generate(&toml::from_str::<PaletteSpec>("accent = \"#7e9cd8\"").unwrap())
    }

    fn states(v: &[(&str, State)]) -> BTreeMap<String, State> {
        v.iter().map(|&(n, s)| (n.to_string(), s)).collect()
    }

    #[test]
    fn defaults_light_all_four_edges_and_bad_values_say_why() {
        let g = Glow::new(&cfg("")).unwrap();
        assert_eq!(g.edges, Edge::ALL);
        assert_eq!((g.width, g.peak, g.monitor), (8, 0.85, None));
        assert_eq!(g.pulse, [State::Busy, State::Warn]);
        let err = |t: &str| Glow::new(&cfg(t)).unwrap_err();
        assert!(err("width = 1").contains("width 1"));
        assert!(err("opacity = 0").contains("opacity 0"));
        assert!(err("monitor = 0").contains("count from 1"));
        assert!(err("settle = 0.1").contains("settle"));
        assert!(err("pulse = [\"loud\"]").contains("unknown state"));
        let bad: GlowConfig = toml::from_str("state = \"../x\"").unwrap();
        assert!(Glow::new(&bad).unwrap_err().contains("valid state name"));
        assert!(toml::from_str::<GlowConfig>("state = \"ci\"\nedges = [\"middle\"]").is_err());
    }

    #[test]
    fn two_glows_may_not_light_the_same_edge_of_a_monitor() {
        let a: GlowConfig = toml::from_str("state = \"a\"\nedges = [\"top\"]").unwrap();
        let b: GlowConfig = toml::from_str("state = \"b\"\nedges = [\"bottom\"]").unwrap();
        let c: GlowConfig = toml::from_str("state = \"c\"\nedges = [\"top\"]\nmonitor = 2").unwrap();
        let d: GlowConfig = toml::from_str("state = \"d\"\nedges = [\"top\"]\nmonitor = 3").unwrap();
        assert!(glows(&[a.clone(), b.clone()]).is_ok());
        assert!(glows(&[c.clone(), d]).is_ok());
        // No monitor means every monitor, so it collides with monitor 2.
        let e = glows(&[a, b, c]).unwrap_err();
        assert!(e.contains("\"a\" and \"c\" both light the top edge"), "{e}");
    }

    #[test]
    fn a_state_fades_in_crossfades_and_fades_out() {
        let p = palette();
        let t0 = Instant::now();
        let mut g = Glows::new(vec![Glow::new(&cfg("opacity = 1.0")).unwrap()], t0);
        assert!(g.bars(&p, t0).is_empty());
        assert!(!g.animating(t0));

        g.update(&states(&[("ci", State::Ok)]), t0);
        let green = rgb(State::Ok, &p);
        let half = g.bars(&p, t0 + FADE / 4)[0];
        assert!(half.alpha > 0.0 && half.alpha < 1.0);
        let full = g.bars(&p, t0 + FADE)[0];
        assert_eq!((full.rgb, full.alpha), (green, 1.0));
        assert!(!g.animating(t0 + FADE), "a steady ok needs no frames");

        let t1 = t0 + Duration::from_secs(5);
        g.update(&states(&[("ci", State::Fail)]), t1);
        let mid = g.bars(&p, t1 + FADE / 4)[0];
        assert!(mid.rgb != green && mid.rgb != rgb(State::Fail, &p));
        assert_eq!(mid.alpha, 1.0, "a crossfade between steady states never dims");
        assert_eq!(g.bars(&p, t1 + FADE)[0].rgb, rgb(State::Fail, &p));

        let t2 = t1 + Duration::from_secs(5);
        g.update(&BTreeMap::new(), t2);
        let going = g.bars(&p, t2 + FADE / 4)[0];
        assert_eq!(going.rgb, rgb(State::Fail, &p));
        assert!(going.alpha < 1.0);
        assert!(g.bars(&p, t2 + FADE).is_empty());
        assert!(!g.animating(t2 + FADE));
    }

    #[test]
    fn pulsing_states_breathe_between_a_floor_and_the_peak() {
        let p = palette();
        let t0 = Instant::now();
        let mut g = Glows::new(vec![Glow::new(&cfg("opacity = 0.5")).unwrap()], t0 - FADE * 2);
        g.update(&states(&[("ci", State::Busy)]), t0 - FADE * 2);
        let at = |d: Duration| g.bars(&p, t0 - FADE * 2 + d)[0].alpha;
        assert!(
            (at(PERIOD) - 0.5).abs() < 1e-4,
            "a breath starts and ends at the peak"
        );
        assert!((at(PERIOD + PERIOD / 2) - 0.5 * BREATH_LOW).abs() < 1e-4);
        assert!(g.animating(t0 + Duration::from_secs(60)), "busy breathes forever");

        let quiet: GlowConfig = toml::from_str("state = \"ci\"\npulse = []").unwrap();
        let mut q = Glows::new(vec![Glow::new(&quiet).unwrap()], t0);
        q.update(&states(&[("ci", State::Busy)]), t0);
        assert!(!q.animating(t0 + Duration::from_secs(60)));
    }

    #[test]
    fn ok_settles_after_the_delay_but_other_states_stay() {
        let p = palette();
        let t0 = Instant::now();
        let mut g = Glows::new(vec![Glow::new(&cfg("settle = 2")).unwrap()], t0);
        g.update(&states(&[("ci", State::Ok)]), t0);
        assert_eq!(g.bars(&p, t0 + Duration::from_secs(1))[0].alpha, 0.85);
        assert!(g.animating(t0 + Duration::from_secs(1)), "waiting for the settle");
        assert!(g.bars(&p, t0 + Duration::from_secs(2) + FADE).is_empty());
        assert!(!g.animating(t0 + Duration::from_secs(3)));

        g.update(&states(&[("ci", State::Fail)]), t0 + Duration::from_secs(10));
        assert_eq!(g.bars(&p, t0 + Duration::from_secs(60))[0].alpha, 0.85);
    }

    #[test]
    fn bands_sit_on_their_edge() {
        let s = ScreenRect {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1080,
        };
        let r = |left, top, right, bottom| ScreenRect {
            left,
            top,
            right,
            bottom,
        };
        assert_eq!(Edge::Top.rect(s, 8), r(-1920, 0, 0, 8));
        assert_eq!(Edge::Bottom.rect(s, 8), r(-1920, 1072, 0, 1080));
        assert_eq!(Edge::Left.rect(s, 8), r(-1920, 0, -1912, 1080));
        assert_eq!(Edge::Right.rect(s, 8), r(-8, 0, 0, 1080));
    }

    #[test]
    fn band_pixels_are_brightest_at_the_edge_and_premultiplied() {
        let alpha = |px: &[u8], w: u32, x: u32, y: u32| px[((y * w + x) * 4 + 3) as usize];
        let (w, h, px) = band(Edge::Top, 600, 8, [255, 0, 0]);
        assert_eq!((w, h, px.len()), (600, 8, 600 * 8 * 4));
        assert_eq!(alpha(&px, w, 300, 0), 255);
        assert!(alpha(&px, w, 300, 4) < alpha(&px, w, 300, 1));
        assert!(alpha(&px, w, 0, 0) < 10, "the ends taper");
        // Red, premultiplied, in BGRA order.
        let i = ((4 * w + 300) * 4) as usize;
        assert_eq!((px[i], px[i + 1]), (0, 0));
        assert_eq!(px[i + 2], px[i + 3]);

        let (w, h, px) = band(Edge::Right, 400, 6, [0, 0, 255]);
        assert_eq!((w, h), (6, 400));
        assert_eq!(alpha(&px, w, 5, 200), 255, "right edge is the last column");
        assert!(alpha(&px, w, 0, 200) < 20);
        let (w, _, px) = band(Edge::Bottom, 300, 4, [1, 2, 3]);
        assert_eq!(alpha(&px, w, 150, 3), 255);
        assert_eq!(alpha(&px, w, 150, 0), 16, "(1/4)^2 of the peak");
        let (w, _, px) = band(Edge::Left, 300, 4, [1, 2, 3]);
        assert_eq!(alpha(&px, w, 0, 150), 255);
    }

    #[test]
    fn a_one_pixel_long_band_does_not_panic() {
        let (_, _, px) = band(Edge::Top, 1, 2, [9, 9, 9]);
        assert_eq!(px.len(), 8);
    }
}
