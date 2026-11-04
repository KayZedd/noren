//! Now playing: a panel widget for whatever Windows media session is current (Spotify, a
//! browser tab, foobar2000...), colored from the album cover.
//!
//! ```toml
//! [media]
//! cover_colors = true     # accent from the cover art (default); false keeps the profile's
//! ```
//!
//! The session comes from the System Media Transport Controls (the same source as the volume
//! flyout), read on a background thread once a second. The cover's accent is the most
//! colorful hue that covers a fair share of the picture; a grey cover keeps the profile's
//! accent. Prev, play/pause and next are sent back to the session.

#[cfg(windows)]
mod smtc;

use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tiny_skia::Pixmap;

use crate::components::caption;
use crate::element::Paint;
use crate::palette::{Oklch, Palette, PaletteSpec};
use crate::widget::{Command, Context, Widget};
use crate::{Canvas, Color, Element, Rect, Theme, column, container, row, svg, text};

/// How often the session is read.
const POLL: Duration = Duration::from_secs(1);
/// Covers are scaled to at most this many pixels a side before they are kept.
pub const COVER_SIDE: u32 = 256;

/// `[media]` in a profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaConfig {
    #[serde(default = "yes")]
    pub cover_colors: bool,
}

fn yes() -> bool {
    true
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self { cover_colors: true }
    }
}

/// Album art as straight (not premultiplied) RGBA.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cover {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl Cover {
    /// A premultiplied pixmap for drawing.
    pub fn pixmap(&self) -> Option<Pixmap> {
        let mut p = Pixmap::new(self.width, self.height)?;
        if self.rgba.len() != p.data().len() {
            return None;
        }
        for (dst, src) in p
            .data_mut()
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(self.rgba.as_chunks::<4>().0)
        {
            let a = u16::from(src[3]);
            let pre = |c: u8| ((u16::from(c) * a + 127) / 255) as u8;
            dst.copy_from_slice(&[pre(src[0]), pre(src[1]), pre(src[2]), src[3]]);
        }
        Some(p)
    }

    /// The cover's accent: the hue with the most chroma behind it, at a lightness and chroma
    /// that work as an accent. `None` for a grey or nearly empty cover.
    pub fn accent(&self) -> Option<Color> {
        const BINS: usize = 24;
        let pixels = self.rgba.as_chunks::<4>().0.iter();
        let count = pixels.len();
        if count == 0 {
            return None;
        }
        // At most ~4096 samples.
        let stride = (count / 4096).max(1);
        let mut bins = [0f32; BINS];
        let mut samples = Vec::new();
        let mut seen = 0usize;
        for px in pixels.step_by(stride) {
            seen += 1;
            if px[3] < 128 {
                continue;
            }
            let c = Color::rgb8(px[0], px[1], px[2]).to_oklch();
            if c.c < 0.04 || !(0.2..=0.95).contains(&c.l) {
                continue;
            }
            let bin = ((c.h.rem_euclid(360.0) / 360.0) * BINS as f32) as usize % BINS;
            bins[bin] += c.c;
            samples.push((bin, c));
        }
        // Smooth over neighbors so a hue split across two bins is not outvoted.
        let score = |i: usize| bins[i] + 0.5 * (bins[(i + BINS - 1) % BINS] + bins[(i + 1) % BINS]);
        let best = (0..BINS).max_by(|&a, &b| score(a).total_cmp(&score(b)))?;
        // Colorful pixels must cover a few percent of the cover.
        let near = |b: usize| (b + BINS - best) % BINS <= 1 || (best + BINS - b) % BINS <= 1;
        let picked: Vec<Oklch> = samples
            .iter()
            .filter(|(b, _)| near(*b))
            .map(|&(_, c)| c)
            .collect();
        if (picked.len() as f32) < seen as f32 * 0.03 {
            return None;
        }
        let weight: f32 = picked.iter().map(|c| c.c).sum();
        let (mut x, mut y, mut l, mut ch) = (0f32, 0f32, 0f32, 0f32);
        for c in &picked {
            let (s, co) = c.h.to_radians().sin_cos();
            x += co * c.c;
            y += s * c.c;
            l += c.l * c.c;
            ch += c.c * c.c;
        }
        let hue = y.atan2(x).to_degrees().rem_euclid(360.0);
        let (l, ch) = (l / weight, ch / weight);
        Some(Oklch::new(l.clamp(0.6, 0.78), ch.clamp(0.09, 0.2), hue).to_color())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    Playing,
    Paused,
    #[default]
    Stopped,
}

/// What a session is playing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub status: Status,
    /// Position when the session was read, and the length; both `None` when the app does not
    /// report a timeline.
    pub position: Option<Duration>,
    pub length: Option<Duration>,
    pub cover: Option<Arc<Cover>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Previous,
    PlayPause,
    Next,
}

/// A media session source. Made and called on the widget's background thread only.
pub trait Player {
    /// The current session's track, `None` when nothing is playing.
    fn read(&mut self) -> Result<Option<Track>, String>;
    fn send(&mut self, control: Control) -> Result<(), String>;
}

/// The system's media sessions: SMTC on Windows, an error elsewhere.
pub fn system_player() -> Result<Box<dyn Player>, String> {
    #[cfg(windows)]
    {
        Ok(Box::new(smtc::Smtc::new()?))
    }
    #[cfg(not(windows))]
    {
        Err("media sessions are read on Windows".into())
    }
}

enum Update {
    Track(Option<Track>),
    Failed(String),
}

/// Reads `player` every [`POLL`] and on each control, until the widget hangs up.
fn follow(
    make: impl FnOnce() -> Result<Box<dyn Player>, String>,
    tx: Sender<Update>,
    controls: Receiver<Control>,
) {
    let mut player = match make() {
        Ok(p) => p,
        Err(e) => {
            let _ = tx.send(Update::Failed(e));
            return;
        }
    };
    let mut last: Option<Result<Option<Track>, String>> = None;
    loop {
        let now = player.read();
        if last.as_ref() != Some(&now) {
            let update = match &now {
                Ok(t) => Update::Track(t.clone()),
                Err(e) => Update::Failed(e.clone()),
            };
            if tx.send(update).is_err() {
                return;
            }
            last = Some(now);
        }
        match controls.recv_timeout(POLL) {
            Ok(c) => {
                if let Err(e) = player.send(c) {
                    let _ = tx.send(Update::Failed(e));
                }
                // Give the app a moment to change track before reading it again.
                thread::sleep(Duration::from_millis(150));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Draws a cover pixmap.
#[derive(Clone)]
struct CoverPaint(Rc<Pixmap>);

impl Paint for CoverPaint {
    fn paint(&self, canvas: &mut Canvas<'_>, rect: Rect) {
        let radius = (rect.w.min(rect.h) * 0.12).max(3.0);
        canvas.draw_image(rect, radius, &self.0);
    }
}

/// A progress bar: track and filled part.
#[derive(Clone)]
struct Progress {
    fraction: f32,
    color: Color,
    track: Color,
}

impl Paint for Progress {
    fn paint(&self, canvas: &mut Canvas<'_>, rect: Rect) {
        let r = rect.h / 2.0;
        canvas.fill_rounded_rect(rect, r, self.track);
        let w = rect.w * self.fraction.clamp(0.0, 1.0);
        if w > 0.0 {
            canvas.fill_rounded_rect(Rect::new(rect.x, rect.y, w.max(rect.h), rect.h), r, self.color);
        }
    }
}

const PREV: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M6 5h2v14H6zM20 5v14L9 12z"/></svg>"#;
const NEXT: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M16 5h2v14h-2zM4 5v14l11-7z"/></svg>"#;
const PLAY: &str =
    r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M7 4v16l13-8z"/></svg>"#;
const PAUSE: &str = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><path d="M6 4h4v16H6zM14 4h4v16h-4z"/></svg>"#;

fn icon<M>(data: &str, size: f32, color: Color) -> Element<M> {
    svg(data.as_bytes())
        .map(|e| e.size(size, size).color(color))
        .unwrap_or_else(|_| container().size(size, size))
}

/// `m:ss`.
pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

#[derive(Clone, Debug)]
pub enum Msg {
    Control(Control),
}

/// The now-playing widget.
pub struct MediaWidget {
    config: MediaConfig,
    /// Mode and contrast for the cover palette.
    spec: PaletteSpec,
    track: Option<Track>,
    /// When `track` was read, to move the position along while it plays.
    read_at: Instant,
    /// The cover as drawn, and its accent palette.
    cover: Option<(Arc<Cover>, Rc<Pixmap>)>,
    palette: Option<Palette>,
    error: Option<String>,
    /// The second shown on the progress clock, to redraw once a second.
    shown_second: Option<u64>,
    rx: Option<Receiver<Update>>,
    controls: Option<Sender<Control>>,
}

impl MediaWidget {
    /// Follows the system's media sessions. `spec` gives the mode and contrast the cover
    /// palette uses.
    pub fn new(config: MediaConfig, spec: PaletteSpec) -> Self {
        Self::with_player(config, spec, system_player)
    }

    /// Follows the player `make` returns, made and read on a background thread.
    pub fn with_player(
        config: MediaConfig,
        spec: PaletteSpec,
        make: impl FnOnce() -> Result<Box<dyn Player>, String> + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let (ctx, crx) = mpsc::channel();
        let mut w = Self::idle(config, spec);
        match thread::Builder::new()
            .name("noren-media".into())
            .spawn(move || follow(make, tx, crx))
        {
            Ok(_) => {
                w.rx = Some(rx);
                w.controls = Some(ctx);
            }
            Err(e) => w.error = Some(e.to_string()),
        }
        w
    }

    fn idle(config: MediaConfig, spec: PaletteSpec) -> Self {
        Self {
            config,
            spec,
            track: None,
            read_at: Instant::now(),
            cover: None,
            palette: None,
            error: None,
            shown_second: None,
            rx: None,
            controls: None,
        }
    }

    /// Shows `track` as if the player had just reported it.
    pub fn show(&mut self, track: Option<Track>, now: Instant) {
        self.read_at = now;
        let cover = track.as_ref().and_then(|t| t.cover.clone());
        if cover.as_ref().map(Arc::as_ptr) != self.cover.as_ref().map(|(c, _)| Arc::as_ptr(c)) {
            self.cover = cover.and_then(|c| c.pixmap().map(|p| (c, Rc::new(p))));
            self.palette = self
                .cover
                .as_ref()
                .filter(|_| self.config.cover_colors)
                .and_then(|(c, _)| c.accent())
                .map(|accent| {
                    Palette::generate(&PaletteSpec {
                        accent,
                        ..self.spec.clone()
                    })
                });
        }
        self.track = track;
        self.error = None;
    }

    pub fn track(&self) -> Option<&Track> {
        self.track.as_ref()
    }

    /// The accent in use: from the cover, or `None` for the panel's own.
    pub fn cover_accent(&self) -> Option<Color> {
        self.palette.as_ref().map(|p| p.accent.step(9))
    }

    fn position(&self, now: Instant) -> Option<Duration> {
        let t = self.track.as_ref()?;
        let at = t.position?;
        let moved = if t.status == Status::Playing {
            now.saturating_duration_since(self.read_at)
        } else {
            Duration::ZERO
        };
        Some(match t.length {
            Some(len) => (at + moved).min(len),
            None => at + moved,
        })
    }

    fn accent(&self, theme: &Theme) -> Color {
        self.palette
            .as_ref()
            .map_or(theme.highlight, |p| p.accent.step(9))
    }
}

impl Widget for MediaWidget {
    type Message = Msg;

    fn id(&self) -> &str {
        "media"
    }

    fn poll(&mut self, now: Instant) -> bool {
        let mut changed = false;
        let updates: Vec<Update> = self
            .rx
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for u in updates {
            match u {
                Update::Track(t) => self.show(t, now),
                Update::Failed(e) => self.error = Some(e),
            }
            changed = true;
        }
        let second = self.position(now).map(|p| p.as_secs());
        if second != self.shown_second {
            self.shown_second = second;
            changed = true;
        }
        changed
    }

    fn compact(&self, ctx: &Context<'_>) -> Element<Msg> {
        let theme = ctx.theme;
        let cell = ctx.panel_height - 2.0 * theme.units.small_spacing;
        let Some(t) = &self.track else {
            return row().align_center().child(caption(theme, "Nothing playing"));
        };
        let accent = self.accent(theme);
        let thumb = match &self.cover {
            Some((_, p)) => container()
                .size(cell * 0.8, cell * 0.8)
                .painter(CoverPaint(Rc::clone(p))),
            None => icon(PLAY, cell * 0.6, accent),
        };
        let label = if t.artist.is_empty() {
            t.title.clone()
        } else {
            format!("{} · {}", t.title, t.artist)
        };
        row()
            .align_center()
            .gap(theme.units.small_spacing)
            .child(thumb)
            .child(
                text(label)
                    .elide()
                    .max_width(260.0)
                    .font_size((cell * 0.4).max(9.0)),
            )
            .child(icon(
                if t.status == Status::Playing { PAUSE } else { PLAY },
                cell * 0.45,
                accent,
            ))
    }

    fn popup(&self, ctx: &Context<'_>) -> Option<Element<Msg>> {
        let theme = ctx.theme;
        let accent = self.accent(theme);
        let Some(t) = &self.track else {
            let why = self.error.clone().unwrap_or_else(|| "Nothing playing".into());
            return Some(column().min_width(220.0).child(caption(theme, &why)));
        };
        let side = 168.0;
        let art = match &self.cover {
            Some((_, p)) => container().size(side, side).painter(CoverPaint(Rc::clone(p))),
            None => container()
                .size(side, side)
                .radius(16.0)
                .background(accent.with_alpha(0.18))
                .center()
                .child(icon(PLAY, 48.0, accent)),
        };
        let mut info = column()
            .gap(2.0)
            .width(side)
            .child(text(t.title.clone()).bold().elide());
        if !t.artist.is_empty() {
            info = info.child(text(t.artist.clone()).color(accent).elide());
        }
        if !t.album.is_empty() {
            info = info.child(caption(theme, &t.album));
        }
        let mut view = column()
            .gap(theme.units.small_spacing * 2.0)
            .padding(theme.units.small_spacing)
            .child(art)
            .child(info);
        if let (Some(pos), Some(len)) = (self.position(ctx.now), t.length.filter(|l| !l.is_zero())) {
            view = view.child(
                column()
                    .gap(4.0)
                    .width(side)
                    .child(container().height(4.0).fill_width().painter(Progress {
                        fraction: pos.as_secs_f32() / len.as_secs_f32(),
                        color: accent,
                        track: theme.text_alpha(0.15),
                    }))
                    .child(
                        row()
                            .justify_between()
                            .child(caption(theme, &clock(pos)))
                            .child(caption(theme, &clock(len))),
                    ),
            );
        }
        let button = |data: &str, size: f32, c: Control, fill: bool| {
            let b = container()
                .size(40.0, 40.0)
                .center()
                .radius(20.0)
                .hover_background(theme.text_alpha(0.08))
                .on_click(Msg::Control(c));
            if fill {
                b.background(accent).child(icon(
                    data,
                    size,
                    self.palette.as_ref().map_or(theme.background, |p| p.on_accent),
                ))
            } else {
                b.child(icon(data, size, theme.text))
            }
        };
        let playing = t.status == Status::Playing;
        view = view.child(
            row()
                .width(side)
                .justify_between()
                .padding_xy(12.0, 0.0)
                .child(button(PREV, 18.0, Control::Previous, false))
                .child(button(
                    if playing { PAUSE } else { PLAY },
                    20.0,
                    Control::PlayPause,
                    true,
                ))
                .child(button(NEXT, 18.0, Control::Next, false)),
        );
        Some(view)
    }

    fn update(&mut self, message: Msg, _now: Instant) -> Command {
        match message {
            Msg::Control(c) => {
                if let Some(tx) = &self.controls {
                    let _ = tx.send(c);
                }
                Command::None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn solid(w: u32, h: u32, rgb: [u8; 3]) -> Cover {
        Cover {
            width: w,
            height: h,
            rgba: (0..w * h).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect(),
        }
    }

    fn hue(c: Color) -> f32 {
        c.to_oklch().h
    }

    #[test]
    fn a_colorful_cover_gives_its_hue() {
        let red = solid(16, 16, [200, 30, 40]).accent().unwrap();
        assert!(
            (hue(red) - hue(Color::rgb8(200, 30, 40))).abs() < 8.0,
            "{}",
            hue(red)
        );
        let teal = solid(16, 16, [20, 150, 140]).accent().unwrap();
        assert!((hue(teal) - hue(Color::rgb8(20, 150, 140))).abs() < 8.0);
    }

    #[test]
    fn accents_stay_usable() {
        // A very dark blue still gives an accent light enough to read.
        let navy = solid(8, 8, [10, 20, 90]).accent().unwrap().to_oklch();
        assert!((0.59..=0.79).contains(&navy.l), "{}", navy.l);
    }

    #[test]
    fn grey_and_empty_covers_have_no_accent() {
        assert_eq!(solid(8, 8, [128, 128, 128]).accent(), None);
        assert_eq!(solid(0, 0, [0, 0, 0]).accent(), None);
        let clear = Cover {
            rgba: vec![0; 64 * 4],
            ..solid(8, 8, [255, 0, 0])
        };
        assert_eq!(clear.accent(), None);
    }

    #[test]
    fn the_dominant_color_wins_over_a_small_splash() {
        // Mostly green with a thin red stripe.
        let mut c = solid(20, 20, [40, 160, 60]);
        for px in c.rgba.as_chunks_mut::<4>().0.iter_mut().take(40) {
            px.copy_from_slice(&[220, 20, 30, 255]);
        }
        let got = c.accent().unwrap();
        assert!(
            (hue(got) - hue(Color::rgb8(40, 160, 60))).abs() < 10.0,
            "{}",
            hue(got)
        );
    }

    #[test]
    fn a_cover_on_grey_still_counts_its_color() {
        // 90% grey, 10% orange: the orange is the accent.
        let mut c = solid(10, 10, [90, 90, 90]);
        for px in c.rgba.as_chunks_mut::<4>().0.iter_mut().take(10) {
            px.copy_from_slice(&[230, 120, 20, 255]);
        }
        assert!(c.accent().is_some());
    }

    #[test]
    fn pixmaps_premultiply() {
        let c = Cover {
            width: 1,
            height: 1,
            rgba: vec![200, 100, 0, 128],
        };
        let p = c.pixmap().unwrap();
        assert_eq!(p.data(), &[100, 50, 0, 128]);
        assert!(
            Cover {
                rgba: vec![0; 3],
                ..c
            }
            .pixmap()
            .is_none()
        );
    }

    fn track(status: Status, cover: Option<Arc<Cover>>) -> Track {
        Track {
            title: "Song".into(),
            artist: "Band".into(),
            album: "Record".into(),
            status,
            position: Some(Duration::from_secs(30)),
            length: Some(Duration::from_secs(200)),
            cover,
        }
    }

    #[test]
    fn the_cover_recolors_the_widget_unless_turned_off() {
        let now = Instant::now();
        let cover = Some(Arc::new(solid(8, 8, [200, 30, 40])));
        let mut w = MediaWidget::idle(
            MediaConfig::default(),
            PaletteSpec::new(Color::rgb8(125, 151, 212)),
        );
        w.show(Some(track(Status::Playing, cover.clone())), now);
        assert!(w.cover_accent().is_some());
        w.show(Some(track(Status::Playing, None)), now);
        assert_eq!(w.cover_accent(), None);
        let mut off = MediaWidget::idle(
            MediaConfig { cover_colors: false },
            PaletteSpec::new(Color::rgb8(125, 151, 212)),
        );
        off.show(Some(track(Status::Playing, cover)), now);
        assert_eq!(off.cover_accent(), None);
    }

    #[test]
    fn position_moves_only_while_playing() {
        let now = Instant::now();
        let mut w = MediaWidget::idle(MediaConfig::default(), PaletteSpec::new(Color::BLACK));
        w.show(Some(track(Status::Playing, None)), now);
        assert_eq!(
            w.position(now + Duration::from_secs(5)),
            Some(Duration::from_secs(35))
        );
        assert_eq!(
            w.position(now + Duration::from_secs(500)),
            Some(Duration::from_secs(200))
        );
        w.show(Some(track(Status::Paused, None)), now);
        assert_eq!(
            w.position(now + Duration::from_secs(5)),
            Some(Duration::from_secs(30))
        );
    }

    #[test]
    fn clock_formats_minutes() {
        assert_eq!(clock(Duration::from_secs(5)), "0:05");
        assert_eq!(clock(Duration::from_secs(245)), "4:05");
    }

    /// A player that records controls and plays a fixed track.
    struct Fake {
        sent: Arc<Mutex<Vec<Control>>>,
    }

    impl Player for Fake {
        fn read(&mut self) -> Result<Option<Track>, String> {
            Ok(Some(track(Status::Playing, None)))
        }
        fn send(&mut self, c: Control) -> Result<(), String> {
            self.sent.lock().unwrap().push(c);
            Ok(())
        }
    }

    #[test]
    fn the_widget_reads_the_player_and_sends_controls() {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let mut w = MediaWidget::with_player(MediaConfig::default(), PaletteSpec::new(Color::BLACK), {
            let sent = Arc::clone(&sent);
            move || Ok(Box::new(Fake { sent }) as Box<dyn Player>)
        });
        let start = Instant::now();
        while w.track().is_none() && start.elapsed() < Duration::from_secs(3) {
            w.poll(Instant::now());
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(w.track().unwrap().title, "Song");
        w.update(Msg::Control(Control::Next), Instant::now());
        let start = Instant::now();
        while sent.lock().unwrap().is_empty() && start.elapsed() < Duration::from_secs(3) {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*sent.lock().unwrap(), vec![Control::Next]);
    }

    #[test]
    fn the_popup_has_three_controls() {
        let theme = Theme::aizome();
        let now = Instant::now();
        let ctx = Context {
            theme: &theme,
            now,
            popup_open: true,
            panel_height: 36.0,
        };
        let mut w = MediaWidget::idle(MediaConfig::default(), PaletteSpec::new(Color::BLACK));
        assert!(w.popup(&ctx).is_some(), "says nothing is playing");
        w.show(Some(track(Status::Playing, None)), now);
        let popup = w.popup(&ctx).unwrap();
        let buttons = popup.child_elements().last().unwrap().child_elements();
        let sent: Vec<_> = buttons
            .iter()
            .filter_map(|b| b.click_message().cloned())
            .map(|Msg::Control(c)| c)
            .collect();
        assert_eq!(sent, vec![Control::Previous, Control::PlayPause, Control::Next]);
    }
}
