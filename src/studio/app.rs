//! The studio window: a sidebar of pages over the drifting aurora, glass cards for content.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, Frame, Id, Layout, Margin, Pos2, Rect, RichText, ScrollArea,
    Sense, Stroke, StrokeKind, TextEdit, Ui, Vec2, ViewportCommand, epaint::Shape,
};

use super::glass::{
    self, Motion, Theme, accent_button, c32, chip, glass, mode_switch, pulse_dot, segmented, toggle,
    with_alpha,
};
use super::model::{
    AIZOME, Lang, NEW_PROFILE, Page, Settings, ToastKind, Toasts, ago, default_spec, icon_rgba, set_palette,
};
use super::palette::{Item, Outcome, PaletteUi};
use super::updates::{self, Updater};
use crate::Color;
use crate::anim::{ease_in_out, ease_out_cubic};
use crate::palette::{Contrast, Mode, Palette, PaletteSpec, apca};
use crate::rice::{self, Entry, Journal, Profile, Status, Transaction, describe};
use crate::shell::command::{self, Action, Command};
use crate::shell::{self, Rules, Runner, State, WindowInfo, state};

impl Page {
    /// A small line icon centered on `c`. Drawn, not typed: egui's bundled fonts carry no
    /// symbol glyphs.
    fn paint_icon(self, p: &egui::Painter, c: Pos2, color: Color32) {
        let st = Stroke::new(1.6, color);
        let v = |x: f32, y: f32| c + Vec2::new(x, y);
        match self {
            Page::Palette => {
                for (x, y) in [(-3.5, 2.5), (3.5, 2.5), (0.0, -3.5)] {
                    p.circle_filled(v(x, y), 3.2, color);
                }
            }
            Page::Profile => {
                p.rect_stroke(
                    Rect::from_center_size(c, Vec2::new(11.0, 14.0)),
                    2.0,
                    st,
                    StrokeKind::Middle,
                );
                p.line_segment([v(-2.5, -2.0), v(2.5, -2.0)], st);
                p.line_segment([v(-2.5, 1.5), v(2.5, 1.5)], st);
            }
            Page::Apply => {
                p.add(Shape::line(
                    vec![v(-5.0, 0.5), v(-1.5, 4.0), v(5.5, -4.0)],
                    Stroke::new(2.0, color),
                ));
            }
            Page::Live => {
                p.add(Shape::convex_polygon(
                    vec![v(-3.5, -5.5), v(5.5, 0.0), v(-3.5, 5.5)],
                    color,
                    Stroke::NONE,
                ));
            }
            Page::Windows => {
                let r = Rect::from_center_size(c, Vec2::new(14.0, 12.0));
                p.rect_stroke(r, 2.0, st, StrokeKind::Middle);
                p.line_segment([v(-7.0, -2.5), v(7.0, -2.5)], st);
            }
            Page::States => {
                p.circle_stroke(c, 6.0, st);
                p.circle_filled(c, 2.8, color);
            }
            Page::History => {
                p.circle_stroke(c, 6.5, st);
                p.add(Shape::line(vec![v(0.0, -4.0), v(0.0, 0.0), v(3.0, 2.0)], st));
            }
            Page::Settings => {
                for i in 0..6 {
                    let a = i as f32 * std::f32::consts::TAU / 6.0;
                    let d = Vec2::angled(a);
                    p.line_segment([c + d * 4.0, c + d * 7.0], Stroke::new(2.2, color));
                }
                p.circle_stroke(c, 4.2, st);
            }
        }
    }
}

/// How long a palette or mode change takes to glide.
const LOOK_MS: f32 = 520.0;

/// Palette and theme easing from one spec to the next.
struct Look {
    from: (Palette, Theme),
    to: (Palette, Theme),
    since: Option<Instant>,
    /// The eased theme still needs applying to egui (the change's last frame included).
    dirty: bool,
    /// Where a mode switch's reveal wave starts; None for a plain crossfade.
    origin: Option<Pos2>,
}

impl Look {
    fn new(spec: &PaletteSpec) -> Self {
        let p = Palette::generate(spec);
        let th = Theme::new(&p);
        Self {
            from: (p.clone(), th.clone()),
            to: (p, th),
            since: None,
            dirty: false,
            origin: None,
        }
    }

    fn set(&mut self, spec: &PaletteSpec, now: Instant) {
        if self.to.0.spec == *spec {
            return;
        }
        self.from = (self.palette_at(now), self.theme(now));
        let p = Palette::generate(spec);
        self.to = (p.clone(), Theme::new(&p));
        self.since = Some(now);
        self.dirty = true;
        self.origin = None;
    }

    /// Linear progress of the current change, `0..=1`.
    fn linear(&self, now: Instant) -> f32 {
        self.since.map_or(1.0, |s| {
            (now.saturating_duration_since(s).as_secs_f32() * 1000.0 / LOOK_MS).min(1.0)
        })
    }

    /// Eased progress: slow in, slow out, so a dark-to-light swap has no visible stop.
    fn progress(&self, now: Instant) -> f32 {
        ease_in_out(self.linear(now))
    }

    fn animating(&self, now: Instant) -> bool {
        self.linear(now) < 1.0
    }

    fn theme(&self, now: Instant) -> Theme {
        self.from.1.blend(&self.to.1, self.progress(now))
    }

    /// The displayed palette: target ramps with colors eased from the previous ones.
    fn palette_at(&self, now: Instant) -> Palette {
        let t = self.progress(now);
        if t >= 1.0 {
            return self.to.0.clone();
        }
        let mix = |a: Color, b: Color| {
            let (a, b) = (a.to_rgba8(), b.to_rgba8());
            let m = |i: usize| (f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t).round() as u8;
            Color::rgba8(m(0), m(1), m(2), 255)
        };
        let mut p = self.to.0.clone();
        for (to, from) in [
            (&mut p.neutral, &self.from.0.neutral),
            (&mut p.accent, &self.from.0.accent),
            (&mut p.positive, &self.from.0.positive),
            (&mut p.warning, &self.from.0.warning),
            (&mut p.negative, &self.from.0.negative),
        ] {
            for i in 0..12 {
                to.0[i] = mix(from.0[i], to.0[i]);
            }
        }
        for i in 0..16 {
            p.ansi[i] = mix(self.from.0.ansi[i], p.ansi[i]);
        }
        p
    }
}

pub struct Studio {
    settings: Settings,
    settings_path: Option<PathBuf>,
    page: Page,
    page_since: Instant,
    motion: Motion,
    look: Look,
    spec: PaletteSpec,
    hex: String,
    start: Instant,
    /// The Windows acrylic backdrop, when DWM gave us one.
    backdrop: Option<Backdrop>,
    toasts: Toasts,

    path: Option<PathBuf>,
    text: String,
    saved: String,
    parsed: Result<Profile, String>,

    runner: Option<Runner>,
    live_text: String,

    diff: Option<(Instant, Result<Vec<Entry>, String>)>,
    force: bool,

    windows: Option<Result<Vec<WindowInfo>, String>>,
    filter: String,

    states: BTreeMap<String, State>,
    states_polled: Option<Instant>,
    new_state: String,

    journal: Option<Result<Vec<Transaction>, String>>,

    /// The Ctrl+K command palette, while open.
    palette: Option<PaletteUi>,

    /// Update check and install.
    updater: Updater,
    /// The check made on start reports failures quietly: being offline is not news.
    update_quiet: bool,
    update_dismissed: bool,
    /// The compatibility report, while shown.
    report: Option<crate::report::Report>,

    #[cfg(windows)]
    tray: Option<super::tray::Tray>,
    quitting: bool,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Studio {
    fn new(cc: &eframe::CreationContext<'_>, page: Page) -> Self {
        let settings_path = Settings::default_path();
        let settings = settings_path.as_deref().map(Settings::load).unwrap_or_default();
        let (path, text) = match &settings.profile {
            Some(p) => match std::fs::read_to_string(p) {
                Ok(t) => (Some(p.clone()), t),
                Err(_) => (None, NEW_PROFILE.to_string()),
            },
            None => (None, NEW_PROFILE.to_string()),
        };
        let parsed = Profile::parse(&text);
        let spec = parsed
            .as_ref()
            .map_or_else(|_| default_spec(), |p| p.palette.clone());
        let look = Look::new(&spec);
        style_text(&cc.egui_ctx);
        look.to.1.apply(&cc.egui_ctx);
        let mut motion = Motion::default();
        motion.fast = settings.reduced_motion;
        let now = Instant::now();
        #[cfg(windows)]
        let backdrop = Backdrop::new(cc, spec.mode == Mode::Dark);
        #[cfg(not(windows))]
        let backdrop = None;
        let mut s = Self {
            hex: spec.accent.to_hex(),
            settings_path,
            page,
            page_since: now,
            motion,
            look,
            spec,
            start: now,
            backdrop,
            toasts: Toasts::default(),
            saved: if path.is_some() {
                text.clone()
            } else {
                String::new()
            },
            path,
            text,
            parsed,
            runner: None,
            live_text: String::new(),
            diff: None,
            force: false,
            windows: None,
            filter: String::new(),
            states: BTreeMap::new(),
            states_polled: None,
            new_state: String::new(),
            journal: None,
            palette: None,
            updater: Updater::default(),
            update_quiet: true,
            update_dismissed: false,
            report: None,
            #[cfg(windows)]
            tray: None,
            quitting: false,
            settings,
        };
        #[cfg(windows)]
        {
            let [r, g, b, _] = s.look.to.0.accent.step(9).to_rgba8();
            match super::tray::Tray::new(&cc.egui_ctx, s.settings.lang, [r, g, b]) {
                Ok(t) => s.tray = Some(t),
                Err(e) => s.toast(ToastKind::Error, format!("tray: {e}")),
            }
        }
        if s.settings.live_on_start {
            s.start_live();
        }
        // Releases are published for Windows only.
        if cfg!(windows) && !s.settings.skip_update_check {
            let ctx = cc.egui_ctx.clone();
            s.updater.check(move || ctx.request_repaint());
        }
        s
    }

    fn check_updates(&mut self, ctx: &egui::Context) {
        self.update_quiet = false;
        self.update_dismissed = false;
        let ctx = ctx.clone();
        self.updater.check(move || ctx.request_repaint());
    }

    fn install_update(&mut self, ctx: &egui::Context) {
        let ctx = ctx.clone();
        self.updater.install(move || ctx.request_repaint());
    }

    /// Toasts for finished update work.
    fn poll_updates(&mut self) {
        if !self.updater.poll() {
            return;
        }
        let l = self.lang();
        match self.updater.state.clone() {
            updates::State::UpToDate if !self.update_quiet => {
                self.toast(
                    ToastKind::Ok,
                    l.pick("noren is up to date", "noren jest aktualny"),
                );
            }
            updates::State::Installed(r) => self.toast(
                ToastKind::Ok,
                format!(
                    "{} {}. {}",
                    l.pick("Updated to noren", "Zaktualizowano do noren"),
                    r.version,
                    l.pick(
                        "Restart the studio to use it.",
                        "Uruchom studio ponownie, żeby go użyć."
                    )
                ),
            ),
            updates::State::Failed(e) if !self.update_quiet || matches!(self.page, Page::Settings) => {
                self.toast(
                    ToastKind::Error,
                    format!("{}: {e}", l.pick("Update", "Aktualizacja")),
                );
            }
            _ => {}
        }
    }

    /// A strip above the page while a newer release waits.
    fn update_banner(&mut self, ui: &mut Ui, th: &Theme) {
        let updates::State::Available(release) = &self.updater.state else {
            return;
        };
        if self.update_dismissed {
            return;
        }
        let (version, page) = (release.version, release.page.clone());
        let l = self.lang();
        let ctx = ui.ctx().clone();
        glass(ui, th, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "{} {version} {}",
                        l.pick("noren", "noren"),
                        l.pick("is out", "jest już dostępny")
                    ))
                    .strong(),
                );
                ui.add_space(8.0);
                if accent_button(ui, &mut self.motion, th, l.pick("Install", "Zainstaluj"), true).clicked() {
                    self.install_update(&ctx);
                }
                if ui.button(l.pick("Release notes", "Lista zmian")).clicked()
                    && let Err(e) = command::open(&page)
                {
                    self.toast(ToastKind::Error, e.to_string());
                }
                if ui.button(l.pick("Later", "Później")).clicked() {
                    self.update_dismissed = true;
                }
            });
        });
        ui.add_space(8.0);
    }

    fn lang(&self) -> Lang {
        self.settings.lang
    }

    fn now(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn toast(&mut self, kind: ToastKind, text: impl Into<String>) {
        let now = self.now();
        self.toasts.push(kind, text, now);
    }

    fn save_settings(&mut self) {
        if let Some(p) = &self.settings_path
            && let Err(e) = self.settings.save(p)
        {
            let msg = format!("{}: {e}", p.display());
            self.toast(ToastKind::Error, msg);
        }
    }

    fn go(&mut self, page: Page) {
        if self.page != page {
            self.page = page;
            self.page_since = Instant::now();
        }
    }

    /// The text changed: parse again and follow its palette.
    fn reparse(&mut self) {
        self.parsed = Profile::parse(&self.text);
        if let Ok(p) = &self.parsed
            && p.palette != self.spec
        {
            self.spec = p.palette.clone();
            self.hex = self.spec.accent.to_hex();
            self.look.set(&self.spec, Instant::now());
        }
        self.diff = None;
    }

    /// The palette controls changed: retheme, and write the spec into the profile text.
    fn palette_edited(&mut self) {
        self.look.set(&self.spec, Instant::now());
        self.hex = self.spec.accent.to_hex();
        if let Ok(t) = set_palette(&self.text, &self.spec) {
            self.text = t;
            self.parsed = Profile::parse(&self.text);
            self.diff = None;
        }
    }

    fn load(&mut self, path: PathBuf) {
        match std::fs::read_to_string(&path) {
            Ok(t) => {
                self.text = t.clone();
                self.saved = t;
                self.settings.profile = Some(path.clone());
                self.path = Some(path);
                self.reparse();
                self.save_settings();
                let l = self.lang();
                self.toast(ToastKind::Ok, l.pick("Profile opened", "Otwarto profil"));
            }
            Err(e) => self.toast(ToastKind::Error, format!("{}: {e}", path.display())),
        }
    }

    fn save(&mut self, as_new: bool) {
        let path = match (&self.path, as_new) {
            (Some(p), false) => p.clone(),
            _ => {
                let name = self.parsed.as_ref().map_or("profile".into(), |p| p.name.clone());
                let Some(p) = rfd::FileDialog::new()
                    .add_filter("noren profile", &["toml"])
                    .set_file_name(format!("{name}.toml"))
                    .save_file()
                else {
                    return;
                };
                p
            }
        };
        match std::fs::write(&path, &self.text) {
            Ok(()) => {
                self.saved = self.text.clone();
                self.settings.profile = Some(path.clone());
                self.path = Some(path);
                self.save_settings();
                let l = self.lang();
                self.toast(ToastKind::Ok, l.pick("Saved", "Zapisano"));
            }
            Err(e) => self.toast(ToastKind::Error, format!("{}: {e}", path.display())),
        }
    }

    /// Renders the rice card (see [`crate::card`]) to a PNG the user picks.
    fn save_card(&mut self) {
        let Ok(profile) = &self.parsed else {
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .add_filter("PNG", &["png"])
            .set_file_name(format!("{}.png", profile.name))
            .save_file()
        else {
            return;
        };
        let saved =
            crate::card::render(profile, 2.0).and_then(|png| png.save_png(&path).map_err(|e| e.to_string()));
        let l = self.lang();
        match saved {
            Ok(()) => self.toast(
                ToastKind::Ok,
                format!("{}: {}", l.pick("Card saved", "Zapisano kartę"), path.display()),
            ),
            Err(e) => self.toast(ToastKind::Error, format!("{}: {e}", path.display())),
        }
    }

    fn start_live(&mut self) {
        let l = self.lang();
        let profile = match &self.parsed {
            Ok(p) => p.clone(),
            Err(e) => {
                let msg = format!("{}: {e}", l.pick("Profile has errors", "Profil ma błędy"));
                self.toast(ToastKind::Error, msg);
                return;
            }
        };
        match Runner::spawn(&profile, None) {
            Ok(r) => {
                self.runner = Some(r);
                self.live_text = self.text.clone();
                self.toast(
                    ToastKind::Ok,
                    l.pick("Holding the profile live", "Profil trzymany na żywo"),
                );
            }
            Err(e) => self.toast(ToastKind::Error, e.to_string()),
        }
        self.sync_tray();
    }

    fn stop_live(&mut self) {
        if let Some(r) = self.runner.take() {
            let l = self.lang();
            match r.stop() {
                Ok(()) => self.toast(
                    ToastKind::Info,
                    l.pick("Stopped; windows restored", "Zatrzymano; okna przywrócone"),
                ),
                Err(e) => self.toast(ToastKind::Error, e.to_string()),
            }
        }
        self.sync_tray();
    }

    fn sync_tray(&self) {
        #[cfg(windows)]
        if let Some(t) = &self.tray {
            t.set_live(self.runner.is_some());
        }
    }

    /// A runtime that ended by itself failed after start: report why.
    fn check_runner(&mut self) {
        if self.runner.as_ref().is_some_and(Runner::is_finished) {
            let r = self.runner.take().expect("checked");
            let msg = match r.stop() {
                Ok(()) => self
                    .lang()
                    .pick("Live runtime ended", "Runtime zakończył się")
                    .to_string(),
                Err(e) => e.to_string(),
            };
            self.toast(ToastKind::Error, msg);
            self.sync_tray();
        }
    }

    #[cfg(windows)]
    fn quit(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        self.stop_live();
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Close);
    }

    #[cfg(windows)]
    fn show_window(ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
    }

    // ---- frame -----------------------------------------------------------------------

    fn sidebar(&mut self, ui: &mut Ui, th: &Theme) {
        let l = self.lang();
        ui.add_space(22.0);
        ui.horizontal(|ui| {
            ui.add_space(14.0);
            ui.label(RichText::new("noren").size(26.0).strong().color(th.accent));
            ui.label(RichText::new("studio").size(15.0).color(th.dim));
        });
        ui.add_space(18.0);
        let pill = ui.painter().add(Shape::Noop);
        let mut selected = None;
        for page in Page::ALL {
            let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click());
            let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
            if resp.clicked() {
                self.go(page);
            }
            let on = page == self.page;
            if on {
                selected = Some(rect);
            }
            let hover = self
                .motion
                .get(resp.id, if resp.hovered() && !on { 1.0 } else { 0.0 }, 160);
            let lift = self
                .motion
                .get(resp.id.with("on"), if on { 1.0 } else { 0.0 }, 260);
            let r = rect.shrink2(Vec2::new(10.0, 2.0));
            if hover > 0.0 {
                ui.painter()
                    .rect_filled(r, CornerRadius::same(12), with_alpha(th.text, 0.06 * hover));
            }
            let x = r.left() + 16.0 + 4.0 * hover + 2.0 * lift;
            let color = glass::lerp(th.dim, th.text, hover.max(lift));
            page.paint_icon(
                ui.painter(),
                Pos2::new(x + 7.0, r.center().y),
                glass::lerp(color, th.accent, lift),
            );
            ui.painter().text(
                Pos2::new(x + 28.0, r.center().y),
                Align2::LEFT_CENTER,
                page.title(l),
                egui::FontId::proportional(14.5),
                color,
            );
        }
        if let Some(rect) = selected {
            let r = rect.shrink2(Vec2::new(10.0, 2.0));
            let y = self.motion.get(Id::unique("nav-pill"), r.top(), 320);
            let pill_rect = Rect::from_min_size(Pos2::new(r.left(), y), r.size());
            let bar = Rect::from_min_size(
                Pos2::new(pill_rect.left() + 4.0, pill_rect.top() + 10.0),
                Vec2::new(3.0, pill_rect.height() - 20.0),
            );
            ui.painter().set(
                pill,
                Shape::Vec(vec![
                    Shape::rect_filled(pill_rect, CornerRadius::same(12), th.accent_soft),
                    Shape::rect_stroke(
                        pill_rect,
                        CornerRadius::same(12),
                        Stroke::new(1.0, with_alpha(th.accent, 0.35)),
                        StrokeKind::Inside,
                    ),
                    Shape::rect_filled(bar, CornerRadius::same(2), th.accent),
                ]),
            );
        }
        // Live status at the bottom of the sidebar.
        let t = self.start.elapsed().as_secs_f32();
        let live = self.runner.is_some();
        ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
            ui.add_space(18.0);
            ui.horizontal(|ui| {
                ui.add_space(22.0);
                pulse_dot(
                    ui,
                    if live { th.positive } else { th.dim },
                    live && !self.settings.reduced_motion,
                    t,
                );
                ui.label(
                    RichText::new(if live {
                        l.pick("live", "na żywo")
                    } else {
                        l.pick("idle", "bezczynne")
                    })
                    .size(12.5)
                    .color(th.dim),
                );
            });
            if let Some(p) = &self.path {
                ui.horizontal(|ui| {
                    ui.add_space(22.0);
                    let name = p
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let dirty = if self.text != self.saved { " *" } else { "" };
                    ui.label(RichText::new(format!("{name}{dirty}")).size(12.0).color(th.dim));
                });
            }
        });
    }

    fn header(&self, ui: &mut Ui, th: &Theme) {
        let l = self.lang();
        ui.label(
            RichText::new(self.page.title(l))
                .size(30.0)
                .strong()
                .color(th.text),
        );
        ui.label(RichText::new(self.page.subtitle(l)).size(14.0).color(th.dim));
        ui.add_space(14.0);
    }

    fn toasts_ui(&mut self, ctx: &egui::Context, th: &Theme) {
        let now = self.now();
        let items: Vec<(String, ToastKind, f32)> = self
            .toasts
            .visible(now)
            .into_iter()
            .map(|(t, v)| (t.text.clone(), t.kind, v))
            .collect();
        if items.is_empty() {
            return;
        }
        egui::Area::new(Id::unique("toasts"))
            .anchor(Align2::RIGHT_BOTTOM, Vec2::new(-20.0, -20.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                ui.with_layout(Layout::bottom_up(Align::Max), |ui| {
                    for (text, kind, v) in items.iter().rev() {
                        let v = ease_out_cubic(*v);
                        let color = match kind {
                            ToastKind::Info => th.accent,
                            ToastKind::Ok => th.positive,
                            ToastKind::Error => th.negative,
                        };
                        ui.horizontal(|ui| {
                            ui.add_space(40.0 * (1.0 - v));
                            ui.multiply_opacity(v);
                            Frame::new()
                                .fill(with_alpha(th.base, 0.92))
                                .stroke(Stroke::new(1.0, with_alpha(color, 0.6)))
                                .corner_radius(CornerRadius::same(14))
                                .inner_margin(Margin::symmetric(14, 10))
                                .shadow(egui::Shadow {
                                    offset: [0, 6],
                                    blur: 20,
                                    spread: 0,
                                    color: Color32::from_black_alpha(60),
                                })
                                .show(ui, |ui| {
                                    ui.set_max_width(360.0);
                                    ui.horizontal(|ui| {
                                        let (r, _) =
                                            ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
                                        ui.painter().circle_filled(r.center(), 4.0, color);
                                        ui.label(RichText::new(text).size(13.5).color(th.text));
                                    });
                                });
                        });
                    }
                });
            });
    }

    // ---- pages -----------------------------------------------------------------------

    fn page_palette(&mut self, ui: &mut Ui, th: &Theme) {
        let l = self.lang();
        let now = Instant::now();
        let shown = self.look.palette_at(now);
        ui.columns(2, |cols| {
            glass(&mut cols[0], th, |ui| {
                ui.set_min_height(176.0);
                ui.label(RichText::new(l.pick("Accent", "Akcent")).size(16.0).strong());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let [r, g, b, _] = self.spec.accent.to_rgba8();
                    let mut rgb = [r, g, b];
                    if ui.color_edit_button_srgb(&mut rgb).changed() {
                        self.spec.accent = Color::rgba8(rgb[0], rgb[1], rgb[2], 255);
                        self.palette_edited();
                    }
                    let resp = ui.add(
                        TextEdit::singleline(&mut self.hex)
                            .desired_width(90.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    if resp.lost_focus()
                        && let Some(c) = Color::from_hex(self.hex.trim())
                    {
                        self.spec.accent = c;
                        self.palette_edited();
                    }
                });
                ui.add_space(8.0);
                ui.label(RichText::new("Aizome").size(12.5).color(th.dim));
                ui.horizontal_wrapped(|ui| {
                    for (name, hex) in AIZOME {
                        let c = Color::from_hex(hex).expect("preset");
                        let (rect, resp) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
                        let hover = self
                            .motion
                            .get(resp.id, if resp.hovered() { 1.0 } else { 0.0 }, 150);
                        let on = self.spec.accent.to_hex() == hex;
                        let p = ui.painter();
                        if on {
                            p.circle_stroke(rect.center(), 14.5, Stroke::new(2.0, th.text));
                        }
                        p.circle_filled(rect.center(), 10.0 + 2.5 * hover, c32(c));
                        if resp.on_hover_text(name).clicked() {
                            self.spec.accent = c;
                            self.palette_edited();
                        }
                    }
                });
            });
            glass(&mut cols[1], th, |ui| {
                ui.set_min_height(176.0);
                ui.label(RichText::new(l.pick("Mode", "Tryb")).size(16.0).strong());
                ui.add_space(6.0);
                let mut light = self.spec.mode == Mode::Light;
                let labels = [l.pick("Dark", "Ciemny"), l.pick("Light", "Jasny")];
                if let Some(at) =
                    mode_switch(ui, &mut self.motion, th, Id::unique("mode"), labels, &mut light)
                {
                    self.spec.mode = if light { Mode::Light } else { Mode::Dark };
                    self.palette_edited();
                    if !self.settings.reduced_motion {
                        self.look.origin = Some(at);
                    }
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let mut high = self.spec.contrast == Contrast::High;
                    if toggle(ui, &mut self.motion, th, &mut high).changed() {
                        self.spec.contrast = if high { Contrast::High } else { Contrast::Standard };
                        self.palette_edited();
                    }
                    ui.label(l.pick("High contrast", "Wysoki kontrast"));
                });
                if let Err(e) = &self.parsed {
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!(
                            "{} {e}",
                            l.pick(
                                "Profile has errors; palette edits preview only:",
                                "Profil ma błędy; zmiany palety tylko w podglądzie:"
                            )
                        ))
                        .size(12.0)
                        .color(th.warning),
                    );
                }
            });
        });
        ui.add_space(4.0);
        glass(ui, th, |ui| {
            ui.label(RichText::new(l.pick("Ramps", "Rampy")).size(16.0).strong());
            ui.add_space(4.0);
            let ramps = [
                ("neutral", &shown.neutral),
                ("accent", &shown.accent),
                ("positive", &shown.positive),
                ("warning", &shown.warning),
                ("negative", &shown.negative),
            ];
            let w = ((ui.available_width() - 90.0) / 12.0).max(16.0);
            for (name, ramp) in ramps {
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [80.0, 30.0],
                        egui::Label::new(RichText::new(name).size(12.5).color(th.dim)),
                    );
                    ui.spacing_mut().item_spacing.x = 0.0;
                    for (i, c) in ramp.0.iter().enumerate() {
                        let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 30.0), Sense::click());
                        let hover = self
                            .motion
                            .get(resp.id, if resp.hovered() { 1.0 } else { 0.0 }, 140);
                        let r = rect
                            .shrink(1.5 - 2.5 * hover)
                            .translate(Vec2::new(0.0, -2.0 * hover));
                        ui.painter()
                            .rect_filled(r, CornerRadius::same(6 + (hover * 2.0) as u8), c32(*c));
                        let hex = c.to_hex();
                        if resp
                            .on_hover_text(format!("{name}.{}  {hex}", i + 1))
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            ui.ctx().copy_text(hex.clone());
                            self.toast(
                                ToastKind::Info,
                                format!("{} {hex}", l.pick("Copied", "Skopiowano")),
                            );
                        }
                    }
                });
            }
        });
        ui.add_space(4.0);
        ui.columns(2, |cols| {
            glass(&mut cols[0], th, |ui| {
                ui.label(
                    RichText::new(l.pick("Contrast (APCA Lc)", "Kontrast (APCA Lc)"))
                        .size(16.0)
                        .strong(),
                );
                ui.add_space(4.0);
                let p = &self.look.to.0;
                let rows = [
                    (
                        l.pick("Body text", "Tekst"),
                        p.neutral.step(12),
                        p.neutral.step(1),
                        75.0,
                    ),
                    (
                        l.pick("Muted text", "Tekst przygaszony"),
                        p.neutral.step(11),
                        p.neutral.step(2),
                        60.0,
                    ),
                    (
                        l.pick("On accent", "Na akcencie"),
                        p.on_accent,
                        p.accent.step(9),
                        60.0,
                    ),
                ];
                for (name, fg, bg, need) in rows {
                    let lc = apca(fg, bg).abs();
                    ui.horizontal(|ui| {
                        Frame::new()
                            .fill(c32(bg))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(10, 4))
                            .show(ui, |ui| ui.label(RichText::new("Aa").color(c32(fg)).strong()));
                        ui.label(name);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let ok = lc >= need;
                            chip(
                                ui,
                                &format!("Lc {lc:.0}"),
                                if ok { th.positive } else { th.warning },
                            );
                        });
                    });
                }
            });
            glass(&mut cols[1], th, |ui| {
                ui.label(RichText::new(l.pick("Preview", "Podgląd")).size(16.0).strong());
                ui.add_space(4.0);
                let p = &shown;
                Frame::new()
                    .fill(c32(p.neutral.step(1)))
                    .stroke(Stroke::new(2.0, c32(p.accent.step(7))))
                    .corner_radius(CornerRadius::same(10))
                    .inner_margin(Margin::same(10))
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.label(
                            RichText::new("~/rice $ noren apply my-rice.toml")
                                .monospace()
                                .color(c32(p.neutral.step(12))),
                        );
                        ui.label(
                            RichText::new("applied #4 (7 changes)")
                                .monospace()
                                .color(c32(p.positive.step(11))),
                        );
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 3.0;
                            for c in p.ansi {
                                let (r, _) = ui.allocate_exact_size(Vec2::new(16.0, 12.0), Sense::hover());
                                ui.painter().rect_filled(r, CornerRadius::same(3), c32(c));
                            }
                        });
                    });
            });
        });
    }

    fn page_profile(&mut self, ui: &mut Ui, th: &Theme) {
        let l = self.lang();
        ui.horizontal(|ui| {
            if ui.button(l.pick("New", "Nowy")).clicked() {
                self.text = NEW_PROFILE.to_string();
                self.saved.clear();
                self.path = None;
                self.reparse();
            }
            if ui.button(l.pick("Open…", "Otwórz…")).clicked()
                && let Some(p) = rfd::FileDialog::new()
                    .add_filter("noren profile", &["toml"])
                    .pick_file()
            {
                self.load(p);
            }
            let dirty = self.text != self.saved;
            if accent_button(ui, &mut self.motion, th, l.pick("Save", "Zapisz"), dirty).clicked() {
                self.save(false);
            }
            if ui.button(l.pick("Save as…", "Zapisz jako…")).clicked() {
                self.save(true);
            }
            if ui
                .add_enabled(
                    self.parsed.is_ok(),
                    egui::Button::new(l.pick("Share card…", "Karta do udostępnienia…")),
                )
                .clicked()
            {
                self.save_card();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| match &self.parsed {
                Ok(p) => {
                    chip(ui, &p.name, th.positive);
                }
                Err(_) => {
                    chip(ui, l.pick("invalid", "błędny"), th.negative);
                }
            });
        });
        if let Err(e) = &self.parsed {
            ui.label(RichText::new(e).size(12.5).color(th.negative));
        }
        if let Some(p) = &self.path {
            ui.label(RichText::new(p.display().to_string()).size(12.0).color(th.dim));
        }
        ui.add_space(4.0);
        glass(ui, th, |ui| {
            ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let resp = ui.add(
                    TextEdit::multiline(&mut self.text)
                        .code_editor()
                        .frame(Frame::NONE)
                        .desired_width(f32::INFINITY)
                        .desired_rows(28)
                        .lock_focus(true),
                );
                if resp.changed() {
                    self.reparse();
                }
            });
        });
        if ui.input_mut(|i| {
            i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND,
                egui::Key::S,
            ))
        }) {
            self.save(false);
        }
    }

    fn needs_windows(&self, ui: &mut Ui, th: &Theme) -> bool {
        if cfg!(windows) {
            return false;
        }
        let l = self.lang();
        glass(ui, th, |ui| {
            ui.label(
                RichText::new(l.pick("Needs Windows", "Wymaga Windows"))
                    .size(16.0)
                    .strong(),
            );
            ui.label(
                RichText::new(l.pick(
                    "This page reads and writes the Windows desktop. Everything else in the studio works here.",
                    "Ta strona czyta i zapisuje pulpit Windows. Reszta studio działa tutaj.",
                ))
                .color(th.dim),
            );
        });
        true
    }

    fn page_apply(&mut self, ui: &mut Ui, th: &Theme) {
        let l = self.lang();
        if self.needs_windows(ui, th) {
            return;
        }
        let profile = self.parsed.clone();
        ui.horizontal(|ui| {
            if ui.button(l.pick("Preview changes", "Pokaż zmiany")).clicked() {
                self.diff = Some((Instant::now(), diff(&profile)));
            }
            let can = self
                .diff
                .as_ref()
                .is_some_and(|(_, d)| d.as_ref().is_ok_and(|e| !e.is_empty()));
            if accent_button(ui, &mut self.motion, th, l.pick("Apply", "Zastosuj"), can).clicked() {
                let msg = apply(&profile);
                self.toast(
                    if msg.is_ok() {
                        ToastKind::Ok
                    } else {
                        ToastKind::Error
                    },
                    msg.unwrap_or_else(|e| e),
                );
                self.diff = Some((Instant::now(), diff(&profile)));
                self.journal = None;
            }
            ui.add_space(12.0);
            if ui.button(l.pick("Roll back", "Cofnij")).clicked() {
                let (kind, msg) = rollback(self.force, l);
                self.toast(kind, msg);
                self.diff = Some((Instant::now(), diff(&profile)));
                self.journal = None;
            }
            toggle(ui, &mut self.motion, th, &mut self.force);
            ui.label(RichText::new(l.pick("force", "wymuś")).color(th.dim))
                .on_hover_text(l.pick(
                    "Also restore values changed by someone else since the apply.",
                    "Przywróć też wartości zmienione przez kogoś innego od zastosowania.",
                ));
        });
        ui.add_space(6.0);
        let Some((at, result)) = &self.diff else {
            ui.label(
                RichText::new(l.pick("Nothing previewed yet.", "Nic jeszcze nie pokazano.")).color(th.dim),
            );
            return;
        };
        let age = at.elapsed().as_secs_f32();
        match result {
            Err(e) => {
                ui.label(RichText::new(e).color(th.negative));
            }
            Ok(entries) if entries.is_empty() => {
                glass(ui, th, |ui| {
                    ui.label(
                        RichText::new(l.pick("Up to date", "Wszystko aktualne"))
                            .size(16.0)
                            .color(th.positive),
                    );
                });
            }
            Ok(entries) => {
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for (i, e) in entries.iter().enumerate() {
                        let v = ease_out_cubic(((age - i as f32 * 0.04) / 0.3).clamp(0.0, 1.0));
                        ui.horizontal(|ui| {
                            ui.add_space(24.0 * (1.0 - v));
                            ui.multiply_opacity(v);
                            ui.vertical(|ui| {
                                glass(ui, th, |ui| {
                                    ui.set_min_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(&e.label).strong());
                                        ui.label(RichText::new(e.slot.to_string()).size(12.0).color(th.dim));
                                    });
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(
                                            RichText::new(describe(&e.slot, e.before.as_deref()))
                                                .monospace()
                                                .color(th.dim),
                                        );
                                        ui.label(RichText::new("->").color(th.accent));
                                        ui.label(
                                            RichText::new(describe(&e.slot, e.after.as_deref())).monospace(),
                                        );
                                    });
                                });
                            });
                        });
                    }
                });
                if age < 0.3 + entries.len() as f32 * 0.04 {
                    ui.ctx().request_repaint();
                }
            }
        }
    }

    fn page_live(&mut self, ui: &mut Ui, th: &Theme) {
        let l = self.lang();
        if self.needs_windows(ui, th) {
            return;
        }
        let live = self.runner.is_some();
        let t = self.start.elapsed().as_secs_f32();
        glass(ui, th, |ui| {
            ui.horizontal(|ui| {
                pulse_dot(
                    ui,
                    if live { th.positive } else { th.dim },
                    live && !self.settings.reduced_motion,
                    t,
                );
                let name = self.parsed.as_ref().map_or("?".into(), |p| p.name.clone());
                ui.label(
                    RichText::new(if live {
                        format!("{} {name}", l.pick("Holding", "Trzymam"))
                    } else {
                        l.pick("Stopped", "Zatrzymane").to_string()
                    })
                    .size(20.0)
                    .strong(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let label = if live {
                        l.pick("Stop", "Zatrzymaj")
                    } else {
                        l.pick("Start", "Uruchom")
                    };
                    if accent_button(ui, &mut self.motion, th, label, live || self.parsed.is_ok()).clicked() {
                        if live {
                            self.stop_live();
                        } else {
                            self.start_live();
                        }
                    }
                    if live
                        && self.text != self.live_text
                        && ui
                            .button(l.pick("Restart with edits", "Restart ze zmianami"))
                            .clicked()
                    {
                        self.stop_live();
                        self.start_live();
                    }
                });
            });
        });
        ui.add_space(4.0);
        let Ok(p) = &self.parsed else { return };
        ui.columns(4, |cols| {
            glass(&mut cols[0], th, |ui| {
                ui.set_min_height(80.0);
                ui.label(
                    RichText::new(p.window.len().to_string())
                        .size(28.0)
                        .strong()
                        .color(th.accent),
                );
                ui.label(RichText::new(l.pick("window rules", "reguły okien")).color(th.dim));
            });
            glass(&mut cols[1], th, |ui| {
                ui.set_min_height(80.0);
                let v = p
                    .focus
                    .as_ref()
                    .map_or("off".into(), |f| format!("{:.0}%", f.dim * 100.0));
                ui.label(RichText::new(v).size(28.0).strong().color(th.accent));
                ui.label(RichText::new(l.pick("focus dimming", "przyciemnianie")).color(th.dim));
            });
            glass(&mut cols[2], th, |ui| {
                ui.set_min_height(80.0);
                let v = p.xray.as_ref().map_or("off".into(), |x| x.key.clone());
                ui.label(RichText::new(v).size(22.0).strong().color(th.accent));
                ui.label(RichText::new(l.pick("x-ray hold key", "klawisz x-ray")).color(th.dim));
            });
            glass(&mut cols[3], th, |ui| {
                ui.set_min_height(80.0);
                ui.label(
                    RichText::new(p.glow.len().to_string())
                        .size(28.0)
                        .strong()
                        .color(th.accent),
                );
                ui.label(RichText::new(l.pick("edge glows", "poświaty krawędzi")).color(th.dim));
            });
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if toggle(ui, &mut self.motion, th, &mut self.settings.live_on_start).changed() {
                self.save_settings();
            }
            ui.label(l.pick(
                "Start holding when the studio opens",
                "Uruchamiaj przy starcie studio",
            ));
        });
    }

    fn page_windows(&mut self, ui: &mut Ui, th: &Theme) {
        let l = self.lang();
        if self.needs_windows(ui, th) {
            return;
        }
        ui.horizontal(|ui| {
            if ui.button(l.pick("Refresh", "Odśwież")).clicked() || self.windows.is_none() {
                self.windows = Some(shell::windows().map_err(|e| e.to_string()));
            }
            ui.add(
                TextEdit::singleline(&mut self.filter)
                    .hint_text(l.pick("filter", "filtr"))
                    .desired_width(240.0),
            );
        });
        let rules = self.parsed.as_ref().ok().and_then(|p| Rules::new(&p.window).ok());
        let f = self.filter.to_lowercase();
        match &self.windows {
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(th.negative));
            }
            Some(Ok(list)) => {
                glass(ui, th, |ui| {
                    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                        for w in list.iter().filter(|w| {
                            f.is_empty()
                                || [&w.exe, &w.class, &w.title]
                                    .iter()
                                    .any(|s| s.to_lowercase().contains(&f))
                        }) {
                            ui.horizontal(|ui| {
                                ui.add_sized(
                                    [170.0, 22.0],
                                    egui::Label::new(RichText::new(&w.exe).strong()).truncate(),
                                );
                                ui.add_sized(
                                    [200.0, 22.0],
                                    egui::Label::new(
                                        RichText::new(&w.class).monospace().size(12.0).color(th.dim),
                                    )
                                    .truncate(),
                                );
                                if let Some(r) = &rules {
                                    for i in r.matching(w) {
                                        chip(ui, &format!("#{}", i + 1), th.accent);
                                    }
                                }
                                ui.add(egui::Label::new(&w.title).truncate());
                            });
                        }
                    });
                });
            }
            None => {}
        }
    }

    fn page_states(&mut self, ui: &mut Ui, th: &Theme) {
        let l = self.lang();
        let Some(dir) = state::default_dir() else {
            ui.label(RichText::new(l.pick("No data directory", "Brak katalogu danych")).color(th.negative));
            return;
        };
        if self
            .states_polled
            .is_none_or(|t| t.elapsed() > Duration::from_secs(1))
        {
            self.states = state::read_all(&dir).unwrap_or_default();
            self.states_polled = Some(Instant::now());
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));
        let palette = self.look.to.0.clone();
        let mut set: Option<(String, Option<State>)> = None;
        let choices = [State::Ok, State::Warn, State::Fail, State::Busy];
        glass(ui, th, |ui| {
            ui.set_min_width(ui.available_width());
            if self.states.is_empty() {
                ui.label(RichText::new(l.pick("No states set.", "Brak ustawionych stanów.")).color(th.dim));
            }
            for (name, s) in &self.states {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                    ui.painter()
                        .circle_filled(r.center(), 5.5, c32(s.color(&palette)));
                    ui.add_sized(
                        [200.0, 24.0],
                        egui::Label::new(RichText::new(name).strong()).truncate(),
                    );
                    for c in choices {
                        let on = c == *s;
                        let text =
                            RichText::new(c.as_str()).color(if on { c32(c.color(&palette)) } else { th.dim });
                        if ui.selectable_label(on, text).clicked() && !on {
                            set = Some((name.clone(), Some(c)));
                        }
                    }
                    if ui.button(l.pick("clear", "usuń")).clicked() {
                        set = Some((name.clone(), None));
                    }
                });
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.add(
                    TextEdit::singleline(&mut self.new_state)
                        .hint_text(l.pick("new state name", "nazwa stanu"))
                        .desired_width(200.0),
                );
                let valid = state::valid_name(self.new_state.trim());
                for c in choices {
                    if ui.add_enabled(valid, egui::Button::new(c.as_str())).clicked() {
                        set = Some((self.new_state.trim().to_string(), Some(c)));
                    }
                }
            });
        });
        if let Some((name, value)) = set {
            match state::set(&dir, &name, value) {
                Ok(()) => {
                    self.states_polled = None;
                    self.new_state.clear();
                }
                Err(e) => self.toast(ToastKind::Error, e.to_string()),
            }
        }
    }

    fn page_history(&mut self, ui: &mut Ui, th: &Theme) {
        let l = self.lang();
        ui.horizontal(|ui| {
            if ui.button(l.pick("Refresh", "Odśwież")).clicked() || self.journal.is_none() {
                self.journal = Some(
                    Journal::default_dir()
                        .ok_or_else(|| "no data directory".to_string())
                        .and_then(|d| Journal::open(d).map_err(|e| e.to_string()))
                        .and_then(|j| j.list().map_err(|e| e.to_string())),
                );
            }
        });
        let now = unix_now();
        match &self.journal {
            Some(Err(e)) => {
                ui.label(RichText::new(e).color(th.negative));
            }
            Some(Ok(list)) if list.is_empty() => {
                glass(ui, th, |ui| {
                    ui.set_min_width(ui.available_width());
                    ui.label(
                        RichText::new(l.pick("Nothing applied yet", "Nic jeszcze nie zastosowano")).strong(),
                    );
                    ui.label(
                        RichText::new(l.pick(
                            "Every apply lands here, and any of them can be rolled back.",
                            "Każde zastosowanie trafi tutaj i każde da się cofnąć.",
                        ))
                        .color(th.dim),
                    );
                });
            }
            Some(Ok(list)) => {
                ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    for t in list.iter().rev() {
                        glass(ui, th, |ui| {
                            ui.set_min_width(ui.available_width());
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("#{}", t.id))
                                        .size(18.0)
                                        .strong()
                                        .color(th.accent),
                                );
                                ui.label(RichText::new(&t.profile).strong());
                                let (name, color) = match t.status {
                                    Status::Applied => (l.pick("applied", "zastosowane"), th.positive),
                                    Status::RolledBack => (l.pick("rolled back", "cofnięte"), th.dim),
                                    Status::Pending => (l.pick("interrupted", "przerwane"), th.warning),
                                };
                                chip(ui, name, color);
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(RichText::new(ago(now, t.created_unix, l)).color(th.dim));
                                });
                            });
                            egui::CollapsingHeader::new(format!(
                                "{} {}",
                                t.entries.len(),
                                l.pick("changes", "zmian")
                            ))
                            .id_salt(("tx", t.id))
                            .show(ui, |ui| {
                                for e in &t.entries {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(RichText::new(&e.label).strong());
                                        ui.label(RichText::new(e.slot.to_string()).size(12.0).color(th.dim));
                                    });
                                }
                            });
                        });
                    }
                });
            }
            None => {}
        }
    }

    fn page_settings(&mut self, ui: &mut Ui, th: &Theme) {
        let l = self.lang();
        glass(ui, th, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(l.pick("Language", "Język")).strong());
                ui.add_space(12.0);
                let mut i = usize::from(self.settings.lang == Lang::Pl);
                if segmented(
                    ui,
                    &mut self.motion,
                    th,
                    Id::unique("lang"),
                    &["English", "Polski"],
                    &mut i,
                ) {
                    self.settings.lang = if i == 1 { Lang::Pl } else { Lang::En };
                    self.save_settings();
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if toggle(ui, &mut self.motion, th, &mut self.settings.reduced_motion).changed() {
                    self.motion.fast = self.settings.reduced_motion;
                    self.save_settings();
                }
                ui.label(l.pick("Reduce motion", "Ogranicz animacje"));
            });
            ui.add_space(8.0);
            let backdrop = if self.backdrop.is_some() {
                l.pick("Windows acrylic backdrop: on", "Tło akrylowe Windows: włączone")
            } else {
                l.pick(
                    "Windows acrylic backdrop: unavailable",
                    "Tło akrylowe Windows: niedostępne",
                )
            };
            ui.label(RichText::new(backdrop).color(th.dim));
            if let Some(p) = &self.settings_path {
                ui.label(RichText::new(p.display().to_string()).size(12.0).color(th.dim));
            }
        });
        ui.add_space(4.0);
        self.updates_ui(ui, th);
        ui.add_space(4.0);
        self.report_ui(ui, th);
    }

    fn updates_ui(&mut self, ui: &mut Ui, th: &Theme) {
        let l = self.lang();
        let ctx = ui.ctx().clone();
        glass(ui, th, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(format!("noren {}", env!("CARGO_PKG_VERSION"))).strong());
            ui.label(
                RichText::new(l.pick(
                    "Closing the window keeps the studio in the tray; Quit there restores every window.",
                    "Zamknięcie okna zostawia studio w zasobniku; Zakończ tam przywraca wszystkie okna.",
                ))
                .color(th.dim),
            );
            ui.add_space(8.0);
            let status = match &self.updater.state {
                updates::State::Idle => String::new(),
                updates::State::Checking => l.pick("Checking…", "Sprawdzanie…").into(),
                updates::State::UpToDate => l
                    .pick("This is the newest release.", "To najnowsza wersja.")
                    .into(),
                updates::State::Available(r) => {
                    format!("{} {}", l.pick("Available:", "Dostępna:"), r.version)
                }
                updates::State::Installing(r) => {
                    format!("{} {}…", l.pick("Installing", "Instalowanie"), r.version)
                }
                updates::State::Installed(r) => format!(
                    "{} {}",
                    r.version,
                    l.pick(
                        "is installed; restart the studio to use it.",
                        "zainstalowana; uruchom studio ponownie."
                    )
                ),
                updates::State::Failed(e) => e.clone(),
            };
            ui.horizontal(|ui| {
                let idle = !self.updater.busy();
                if ui
                    .add_enabled(
                        idle && cfg!(windows),
                        egui::Button::new(l.pick("Check for updates", "Sprawdź aktualizacje")),
                    )
                    .clicked()
                {
                    self.check_updates(&ctx);
                }
                if matches!(self.updater.state, updates::State::Available(_))
                    && accent_button(ui, &mut self.motion, th, l.pick("Install", "Zainstaluj"), true)
                        .clicked()
                {
                    self.install_update(&ctx);
                }
                ui.label(RichText::new(status).color(th.dim));
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let mut on = !self.settings.skip_update_check;
                if toggle(ui, &mut self.motion, th, &mut on).changed() {
                    self.settings.skip_update_check = !on;
                    self.save_settings();
                }
                ui.label(l.pick(
                    "Check for updates when the studio opens",
                    "Sprawdzaj aktualizacje przy otwarciu studia",
                ));
            });
            ui.label(
                RichText::new(l.pick(
                    "Updates come from GitHub Releases and install only when their signature checks out.",
                    "Aktualizacje pochodzą z GitHub Releases i instalują się tylko z poprawnym podpisem.",
                ))
                .size(12.0)
                .color(th.dim),
            );
        });
    }

    fn report_ui(&mut self, ui: &mut Ui, th: &Theme) {
        let l = self.lang();
        glass(ui, th, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(RichText::new(l.pick("Compatibility report", "Raport kompatybilności")).strong());
            ui.label(
                RichText::new(l.pick(
                    "Versions, monitors and what the profile sets up, without names or paths. \
                     It opens as a GitHub issue that you read and submit yourself; nothing is sent before that.",
                    "Wersje, monitory i to, co ustawia profil, bez nazw i ścieżek. \
                     Otwiera się jako zgłoszenie na GitHubie, które sam czytasz i wysyłasz; wcześniej nic nie wychodzi.",
                ))
                .color(th.dim),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = if self.report.is_some() {
                    l.pick("Hide report", "Ukryj raport")
                } else {
                    l.pick("Show report", "Pokaż raport")
                };
                if ui.button(label).clicked() {
                    self.report = match self.report {
                        Some(_) => None,
                        None => {
                            let transactions = journal().and_then(|j| j.list().map_err(|e| e.to_string()));
                            Some(crate::report::Report::collect(
                                self.parsed.as_ref().ok(),
                                &transactions.unwrap_or_default(),
                            ))
                        }
                    };
                }
                let mut failed = None;
                if let Some(report) = &self.report
                    && accent_button(
                        ui,
                        &mut self.motion,
                        th,
                        l.pick("Open as GitHub issue", "Otwórz jako zgłoszenie na GitHubie"),
                        true,
                    )
                    .clicked()
                    && let Err(e) = command::open(&report.issue_url())
                {
                    failed = Some(e.to_string());
                }
                if let Some(e) = failed {
                    self.toast(ToastKind::Error, e);
                }
            });
            if let Some(report) = &self.report {
                ui.add_space(6.0);
                let mut text = format!("{}\n\n{}", report.title(), report.markdown());
                ui.add(
                    TextEdit::multiline(&mut text)
                        .code_editor()
                        .interactive(false)
                        .desired_width(f32::INFINITY),
                );
            }
        });
    }
}

fn diff(profile: &Result<Profile, String>) -> Result<Vec<Entry>, String> {
    let p = profile.as_ref().map_err(Clone::clone)?;
    let store = rice::SystemStore::default();
    let changes = p.changes(&store).map_err(|e| e.to_string())?;
    rice::plan(&store, &changes).map_err(|e| e.to_string())
}

fn journal() -> Result<Journal, String> {
    let dir = Journal::default_dir().ok_or("no data directory")?;
    Journal::open(&dir).map_err(|e| format!("{}: {e}", dir.display()))
}

fn apply(profile: &Result<Profile, String>) -> Result<String, String> {
    let p = profile.as_ref().map_err(Clone::clone)?;
    let mut store = rice::SystemStore::default();
    let changes = p.changes(&store).map_err(|e| e.to_string())?;
    match rice::apply(&mut store, &journal()?, &p.name, &changes).map_err(|e| e.to_string())? {
        Some(t) => Ok(format!("applied #{} ({} changes)", t.id, t.entries.len())),
        None => Ok("up to date".into()),
    }
}

fn rollback(force: bool, l: Lang) -> (ToastKind, String) {
    let result = journal().and_then(|j| {
        rice::rollback(&mut rice::SystemStore::default(), &j, force).map_err(|e| e.to_string())
    });
    match result {
        Err(e) => (ToastKind::Error, e),
        Ok(r) => match r.transaction {
            None => (
                ToastKind::Info,
                l.pick("Nothing to roll back", "Nic do cofnięcia").into(),
            ),
            Some(id) if r.conflicts.is_empty() => {
                (ToastKind::Ok, format!("#{id}: {} restored", r.restored.len()))
            }
            Some(id) => (
                ToastKind::Error,
                format!(
                    "#{id}: {} restored, {} {}",
                    r.restored.len(),
                    r.conflicts.len(),
                    l.pick(
                        "kept (changed since apply; use force)",
                        "zostawione (zmienione od zastosowania; użyj wymuś)"
                    )
                ),
            ),
        },
    }
}

pub(super) fn style_text(ctx: &egui::Context) {
    use egui::{FontFamily, FontId, TextStyle};
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Heading, FontId::new(24.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.5, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.5, FontFamily::Monospace)),
        ]
        .into();
    });
}

/// The acrylic system backdrop (Windows 11 22H2+) and which mode DWM tints it for.
struct Backdrop {
    #[cfg_attr(not(windows), allow(dead_code))]
    hwnd: isize,
    dark: bool,
}

impl Backdrop {
    /// Asks DWM for acrylic. None where unsupported; the studio then paints its own opaque
    /// background.
    #[cfg(windows)]
    fn new(cc: &eframe::CreationContext<'_>, dark: bool) -> Option<Self> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Dwm::{DWMWA_SYSTEMBACKDROP_TYPE, DwmSetWindowAttribute};
        let RawWindowHandle::Win32(h) = cc.window_handle().ok()?.as_raw() else {
            return None;
        };
        let mut b = Self {
            hwnd: h.hwnd.get(),
            dark: !dark,
        };
        b.set_dark(dark);
        // DWMSBT_TRANSIENTWINDOW: acrylic.
        let kind = 3i32;
        // SAFETY: hwnd is this process's live top-level window; the value is a 4-byte i32.
        let ok = unsafe {
            DwmSetWindowAttribute(
                HWND(b.hwnd as *mut _),
                DWMWA_SYSTEMBACKDROP_TYPE,
                (&raw const kind).cast(),
                4,
            )
        };
        ok.is_ok().then_some(b)
    }

    /// Retints the backdrop and title bar for dark or light.
    fn set_dark(&mut self, dark: bool) {
        if self.dark == dark {
            return;
        }
        self.dark = dark;
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::HWND;
            use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
            let dark = i32::from(dark);
            // SAFETY: hwnd is this process's live top-level window; the value is a 4-byte i32.
            unsafe {
                let _ = DwmSetWindowAttribute(
                    HWND(self.hwnd as *mut _),
                    DWMWA_USE_IMMERSIVE_DARK_MODE,
                    (&raw const dark).cast(),
                    4,
                );
            }
        }
    }
}

/// How much of the theme's base covers the acrylic backdrop. It rises to opaque halfway
/// through a mode change, where DWM swaps the backdrop's tint, so that swap never shows.
fn backdrop_cover(linear: f32) -> f32 {
    let bump = (std::f32::consts::PI * linear.clamp(0.0, 1.0)).sin();
    0.55 + 0.45 * bump
}

impl eframe::App for Studio {
    fn logic(&mut self, #[allow(unused_variables)] ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(windows)]
        if let Some(actions) = self.tray.as_ref().map(super::tray::Tray::actions) {
            use super::tray::Action;
            for a in actions {
                match a {
                    Action::Open => Self::show_window(ctx),
                    Action::ToggleLive => {
                        if self.runner.is_some() {
                            self.stop_live();
                        } else {
                            self.start_live();
                        }
                    }
                    Action::Quit => self.quit(ctx),
                }
            }
        }
        self.check_runner();
        self.poll_updates();
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let now = Instant::now();
        self.motion.begin_frame();

        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && cfg!(windows) && self.has_tray()
        {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(ViewportCommand::Visible(false));
            return;
        }

        let theming = self.look.animating(now);
        let th = self.look.theme(now);
        if self.look.dirty {
            th.apply(&ctx);
            self.look.dirty = theming;
        }
        let cover = match &mut self.backdrop {
            Some(b) => {
                // Mode flips where the cover is opaque.
                b.set_dark(th.dark);
                backdrop_cover(self.look.linear(now))
            }
            None => 1.0,
        };

        let t = if self.settings.reduced_motion {
            0.0
        } else {
            self.start.elapsed().as_secs_f32()
        };
        let full = ctx.content_rect();
        let bg = ctx.layer_painter(egui::LayerId::background());
        match self.look.origin.filter(|_| theming) {
            Some(origin) => {
                let (from, to) = (&self.look.from.1, &self.look.to.1);
                glass::paint_reveal(&bg, full, origin, self.look.linear(now), from, to, cover);
                glass::paint_aurora(&bg, full, &th, t, None);
            }
            None => glass::paint_aurora(&bg, full, &th, t, Some(cover)),
        }

        egui::Panel::left("nav")
            .exact_size(220.0)
            .resizable(false)
            .show_separator_line(false)
            .frame(
                Frame::new()
                    .fill(with_alpha(th.base, 0.35))
                    .stroke(Stroke::new(1.0, th.edge)),
            )
            .show(ui, |ui| self.sidebar(ui, &th));

        let page_t = ease_out_cubic(
            (self.page_since.elapsed().as_secs_f32()
                / if self.settings.reduced_motion { 0.08 } else { 0.32 })
            .min(1.0),
        );
        egui::CentralPanel::no_frame()
            .frame(Frame::new().inner_margin(Margin {
                left: 28,
                right: 28,
                top: 24,
                bottom: 16,
            }))
            .show(ui, |ui| {
                ui.multiply_opacity(page_t);
                ui.add_space(14.0 * (1.0 - page_t));
                self.header(ui, &th);
                self.update_banner(ui, &th);
                match self.page {
                    Page::Palette => {
                        ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| self.page_palette(ui, &th));
                    }
                    Page::Profile => self.page_profile(ui, &th),
                    Page::Apply => self.page_apply(ui, &th),
                    Page::Live => self.page_live(ui, &th),
                    Page::Windows => self.page_windows(ui, &th),
                    Page::States => self.page_states(ui, &th),
                    Page::History => self.page_history(ui, &th),
                    Page::Settings => self.page_settings(ui, &th),
                }
            });

        self.toasts_ui(&ctx, &th);

        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::K)) {
            self.palette = match self.palette {
                Some(_) => None,
                None => Some(PaletteUi::default()),
            };
        }
        let palette_open = self.palette.is_some();
        if palette_open {
            self.palette_ui(&ctx, &th);
        }

        // Repaint pacing: immediately while something eases, slower for the idle drift.
        let busy = self.motion.busy() || theming || page_t < 1.0 || !self.toasts.is_empty() || palette_open;
        if busy {
            ctx.request_repaint();
        } else if !self.settings.reduced_motion || self.runner.is_some() {
            let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
            ctx.request_repaint_after(Duration::from_millis(if focused { 33 } else { 100 }));
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.backdrop.is_some() {
            [0.0; 4]
        } else {
            // The eased base, not the target: the target would flash in under the glass.
            self.look.theme(Instant::now()).base.to_normalized_gamma_f32()
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        // Restores every window the live runtime touched.
        self.stop_live();
        self.save_settings();
    }
}

/// What a studio palette entry does.
enum Pick {
    Page(Page),
    Live,
    Save,
    Command(Command),
}

impl Studio {
    fn palette_entries(&self) -> Vec<(Item, Pick)> {
        let l = self.lang();
        let mut out = Vec::new();
        if let Ok(p) = &self.parsed
            && let Ok(cmds) = p.commands()
        {
            for c in cmds.list.iter().filter(|c| c.action != Action::Palette) {
                out.push((Item::of(c, cmds.leader.as_ref()), Pick::Command(c.clone())));
            }
        }
        let live = if self.runner.is_some() {
            l.pick("Stop holding the profile live", "Przestań trzymać profil na żywo")
        } else {
            l.pick("Hold the profile live", "Trzymaj profil na żywo")
        };
        out.push((Item::new(live, ""), Pick::Live));
        out.push((
            Item::new(l.pick("Save the profile", "Zapisz profil"), "Ctrl+S"),
            Pick::Save,
        ));
        for page in Page::ALL {
            let title = format!("{} {}", l.pick("Go to", "Idź do"), page.title(l));
            out.push((Item::new(title, ""), Pick::Page(page)));
        }
        out
    }

    /// The Ctrl+K overlay: the window dims, the palette drops in from above.
    fn palette_ui(&mut self, ctx: &egui::Context, th: &Theme) {
        let entries = self.palette_entries();
        let items: Vec<Item> = entries
            .iter()
            .map(|(i, _)| Item::new(&i.title, &i.hint))
            .collect();
        let Some(mut pal) = self.palette.take() else {
            return;
        };
        let t = pal.shown(self.settings.reduced_motion);
        let full = ctx.content_rect();
        let mut close = false;
        egui::Area::new(Id::unique("palette-scrim"))
            .order(egui::Order::Foreground)
            .fixed_pos(full.min)
            .show(ctx, |ui| {
                let (r, resp) = ui.allocate_exact_size(full.size(), Sense::click());
                ui.painter()
                    .rect_filled(r, 0.0, Color32::from_black_alpha((110.0 * t) as u8));
                close |= resp.clicked();
            });
        let width = 600.0_f32.min(full.width() - 48.0);
        let lang = self.lang();
        let mut outcome = Outcome::None;
        egui::Area::new(Id::unique("palette"))
            .order(egui::Order::Tooltip)
            .fixed_pos(Pos2::new(
                full.center().x - width / 2.0,
                full.top() + 90.0 - 16.0 * (1.0 - t),
            ))
            .show(ctx, |ui| {
                ui.multiply_opacity(t);
                ui.set_width(width);
                glass::card(th).fill(with_alpha(th.base, 0.96)).show(ui, |ui| {
                    ui.set_width(width - 36.0);
                    outcome = pal.show(ui, &mut self.motion, th, &items, lang);
                });
            });
        match outcome {
            Outcome::Closed => close = true,
            Outcome::Picked(i) => {
                close = true;
                match entries.into_iter().nth(i).map(|(_, p)| p) {
                    Some(Pick::Page(p)) => self.go(p),
                    Some(Pick::Live) => {
                        if self.runner.is_some() {
                            self.stop_live();
                        } else {
                            self.start_live();
                        }
                    }
                    Some(Pick::Save) => self.save(false),
                    Some(Pick::Command(c)) => match command::exec(&c.action, None) {
                        Ok(()) => self.toast(ToastKind::Ok, c.name),
                        Err(e) => self.toast(ToastKind::Error, format!("{}: {e}", c.name)),
                    },
                    None => {}
                }
            }
            Outcome::None => {}
        }
        if !close {
            self.palette = Some(pal);
        }
    }

    fn has_tray(&self) -> bool {
        #[cfg(windows)]
        {
            self.tray.is_some()
        }
        #[cfg(not(windows))]
        {
            false
        }
    }
}

pub fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(i) = args.iter().position(|a| a == "--palette") {
        let Some(file) = args.get(i + 1) else {
            eprintln!("noren-studio: --palette needs a profile file");
            return ExitCode::from(2);
        };
        let state_dir = args
            .iter()
            .position(|a| a == "--state-dir")
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from);
        return super::palette::run(file, state_dir);
    }
    let page = match Page::from_args(&args) {
        Ok(p) => p.unwrap_or(Page::Palette),
        Err(e) => {
            eprintln!("noren-studio: {e}");
            return ExitCode::from(2);
        }
    };
    let accent = Palette::generate(&default_spec()).accent.step(9).to_rgba8();
    let icon = egui::IconData {
        rgba: icon_rgba([accent[0], accent[1], accent[2]]),
        width: 32,
        height: 32,
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("noren studio")
            .with_app_id("noren-studio")
            .with_inner_size([1160.0, 760.0])
            .with_min_inner_size([860.0, 580.0])
            .with_transparent(true)
            .with_icon(icon),
        renderer: super::renderer::chosen(std::env::var(super::renderer::ENV).ok().as_deref()),
        ..Default::default()
    };
    super::renderer::finish(eframe::run_native(
        "noren studio",
        options,
        Box::new(|cc| Ok(Box::new(Studio::new(cc, page)))),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(mode: Mode) -> PaletteSpec {
        PaletteSpec {
            mode,
            ..default_spec()
        }
    }

    #[test]
    fn a_mode_switch_glides_without_a_stop() {
        let t0 = Instant::now();
        let mut look = Look::new(&spec(Mode::Dark));
        let dark = look.theme(t0).base;
        look.set(&spec(Mode::Light), t0);
        assert!(look.dirty && look.animating(t0));
        assert_eq!(look.theme(t0).base, dark, "starts where it was");
        // Brightness rises every step; the old ease-out did 87% of the way in half the time.
        let at = |ms: u64| look.theme(t0 + Duration::from_millis(ms)).base.r();
        let steps: Vec<u8> = (0..=13).map(|i| at(i * 40)).collect();
        assert!(steps.windows(2).all(|w| w[1] >= w[0]), "{steps:?}");
        let half = at(LOOK_MS as u64 / 2);
        let (lo, hi) = (f32::from(steps[0]), f32::from(*steps.last().unwrap()));
        assert!(
            (f32::from(half) - (lo + hi) / 2.0).abs() < (hi - lo) * 0.1,
            "{half} {lo} {hi}"
        );
        let end = t0 + Duration::from_millis(LOOK_MS as u64 + 1);
        assert!(!look.animating(end));
        assert_eq!(look.theme(end).base, look.to.1.base);
        assert_eq!(look.theme(end).light, 1.0);
    }

    #[test]
    fn the_backdrop_is_covered_when_its_tint_swaps() {
        assert!((backdrop_cover(0.0) - 0.55).abs() < 1e-6);
        assert!((backdrop_cover(0.5) - 1.0).abs() < 1e-6);
        assert!((backdrop_cover(1.0) - 0.55).abs() < 1e-5);
    }
}
