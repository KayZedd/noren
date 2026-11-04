//! The command palette: a search field over a fuzzy-ranked list. One widget, two homes: an
//! overlay in the studio (Ctrl+K) and a small window of its own (`noren-studio --palette`),
//! which `builtin = "palette"` opens from a chord or a hot corner.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Id, Key, Modifiers, Pos2, Rect, RichText, ScrollArea, Sense,
    Stroke, StrokeKind, TextEdit, Ui, Vec2, ViewportCommand,
};

use super::glass::{Motion, Theme, with_alpha};
use super::model::{Lang, Settings, icon_rgba};
use crate::anim::ease_out_cubic;
use crate::rice::Profile;
use crate::shell::command::{self, Action, Command};

pub struct Item {
    pub title: String,
    /// Right-aligned hint: a chord, a corner, a shortcut.
    pub hint: String,
}

impl Item {
    pub fn new(title: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            hint: hint.into(),
        }
    }

    /// A profile command, hinted with its chord and corner.
    pub fn of(c: &Command, leader: Option<&crate::shell::keys::Combo>) -> Self {
        let mut hint = Vec::new();
        if let (Some(l), false) = (leader, c.keys.is_empty()) {
            hint.push(format!("{l}, {}", c.keys_label()));
        }
        if let Some(k) = c.corner {
            hint.push(k.to_string());
        }
        Self::new(&c.name, hint.join("  ·  "))
    }
}

pub enum Outcome {
    None,
    Picked(usize),
    Closed,
}

const ROW: f32 = 40.0;
const ROWS: usize = 8;

pub struct PaletteUi {
    query: String,
    selected: usize,
    opened: Instant,
    focus: bool,
}

impl Default for PaletteUi {
    fn default() -> Self {
        Self {
            query: String::new(),
            selected: 0,
            opened: Instant::now(),
            focus: true,
        }
    }
}

impl PaletteUi {
    /// How far the open animation is, `0..=1`.
    pub fn shown(&self, fast: bool) -> f32 {
        let secs = if fast { 0.06 } else { 0.22 };
        ease_out_cubic((self.opened.elapsed().as_secs_f32() / secs).min(1.0))
    }

    /// The search field and the ranked list. Arrow keys move, Enter picks, Esc closes.
    pub fn show(&mut self, ui: &mut Ui, motion: &mut Motion, th: &Theme, items: &[Item], l: Lang) -> Outcome {
        let ranked = command::rank(&self.query, items.iter().map(|i| i.title.as_str()));
        let (up, down, enter, esc) = ui.input_mut(|i| {
            (
                i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::Enter),
                i.consume_key(Modifiers::NONE, Key::Escape),
            )
        });
        if esc {
            return Outcome::Closed;
        }
        if !ranked.is_empty() {
            if down {
                self.selected = (self.selected + 1) % ranked.len();
            }
            if up {
                self.selected = (self.selected + ranked.len() - 1) % ranked.len();
            }
        }
        self.selected = self.selected.min(ranked.len().saturating_sub(1));

        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::hover());
            let p = ui.painter();
            p.circle_stroke(r.center() - Vec2::splat(2.0), 6.5, Stroke::new(1.8, th.dim));
            p.line_segment(
                [r.center() + Vec2::splat(2.5), r.center() + Vec2::splat(8.0)],
                Stroke::new(2.0, th.dim),
            );
            let edit = ui.add(
                TextEdit::singleline(&mut self.query)
                    .frame(egui::Frame::NONE)
                    .font(FontId::proportional(19.0))
                    .hint_text(l.pick("Type a command", "Wpisz komendę"))
                    .desired_width(f32::INFINITY),
            );
            if self.focus {
                edit.request_focus();
                self.focus = false;
            }
            if edit.changed() {
                self.selected = 0;
            }
        });
        ui.add_space(4.0);
        let sep = ui.available_rect_before_wrap();
        ui.painter()
            .hline(sep.x_range(), sep.top(), Stroke::new(1.0, th.edge));
        ui.add_space(8.0);

        if ranked.is_empty() {
            ui.add_space(10.0);
            ui.label(RichText::new(l.pick("Nothing matches.", "Nic nie pasuje.")).color(th.dim));
            ui.add_space(10.0);
            return Outcome::None;
        }

        let mut picked = enter.then(|| ranked[self.selected]);
        let height = ROW * ranked.len().min(ROWS) as f32;
        ScrollArea::vertical()
            .max_height(height)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                let top = ui.cursor().top();
                let width = ui.available_width();
                // The highlight glides between rows instead of jumping.
                let y = motion.get(Id::unique("palette.sel"), self.selected as f32 * ROW, 140);
                let pill = Rect::from_min_size(
                    Pos2::new(ui.cursor().left(), top + y),
                    Vec2::new(width, ROW - 4.0),
                );
                ui.painter()
                    .rect_filled(pill, CornerRadius::same(10), th.accent_soft);
                ui.painter().rect_stroke(
                    pill,
                    CornerRadius::same(10),
                    Stroke::new(1.0, with_alpha(th.accent, 0.45)),
                    StrokeKind::Inside,
                );
                for (row, &i) in ranked.iter().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, ROW), Sense::click());
                    if resp.hovered() && ui.input(|i| i.pointer.delta() != Vec2::ZERO) {
                        self.selected = row;
                    }
                    if resp.clicked() {
                        picked = Some(i);
                    }
                    if row == self.selected && (up || down) {
                        resp.scroll_to_me(None);
                    }
                    let mid = rect.left_center() + Vec2::new(12.0, -2.0);
                    let p = ui.painter();
                    let color = if row == self.selected {
                        th.text
                    } else {
                        lerp_dim(th)
                    };
                    p.text(
                        mid,
                        Align2::LEFT_CENTER,
                        &items[i].title,
                        FontId::proportional(15.0),
                        color,
                    );
                    p.text(
                        rect.right_center() + Vec2::new(-12.0, -2.0),
                        Align2::RIGHT_CENTER,
                        &items[i].hint,
                        FontId::proportional(12.0),
                        th.dim,
                    );
                }
            });
        match picked {
            Some(i) => Outcome::Picked(i),
            None => Outcome::None,
        }
    }
}

fn lerp_dim(th: &Theme) -> Color32 {
    super::glass::lerp(th.dim, th.text, 0.5)
}

/// `noren-studio --palette <profile.toml>`: the palette alone, for chords and hot corners.
struct PaletteApp {
    commands: Vec<Command>,
    items: Vec<Item>,
    state_dir: Option<PathBuf>,
    ui: PaletteUi,
    motion: Motion,
    theme: Theme,
    lang: Lang,
    error: Option<String>,
    was_focused: bool,
}

impl eframe::App for PaletteApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.motion.begin_frame();
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if self.was_focused && !focused {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        self.was_focused |= focused;

        let th = self.theme.clone();
        let t = self.ui.shown(self.motion.fast);
        let full = ctx.content_rect().shrink(10.0);
        let card = full.translate(Vec2::new(0.0, -10.0 * (1.0 - t)));
        let bg = ctx.layer_painter(egui::LayerId::background());
        bg.rect_filled(card, CornerRadius::same(18), with_alpha(th.base, 0.96 * t));
        bg.rect_stroke(
            card,
            CornerRadius::same(18),
            Stroke::new(1.0, with_alpha(th.accent, 0.35 * t)),
            StrokeKind::Inside,
        );
        let mut inner = ui.new_child(egui::UiBuilder::new().max_rect(card.shrink(16.0)));
        inner.multiply_opacity(t);
        let outcome = self
            .ui
            .show(&mut inner, &mut self.motion, &th, &self.items, self.lang);
        if let Some(e) = &self.error {
            inner.label(RichText::new(e).color(th.negative));
        }
        match outcome {
            Outcome::Closed => ctx.send_viewport_cmd(ViewportCommand::Close),
            Outcome::Picked(i) => {
                let c = &self.commands[i];
                match &c.action {
                    Action::Palette => {}
                    action => match command::exec(action, self.state_dir.as_deref()) {
                        Ok(()) => ctx.send_viewport_cmd(ViewportCommand::Close),
                        Err(e) => self.error = Some(format!("{}: {e}", c.name)),
                    },
                }
            }
            Outcome::None => {}
        }
        if self.motion.busy() || t < 1.0 {
            ctx.request_repaint();
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }
}

/// Runs the palette window for the profile at `path` (a temporary file the runtime wrote; it
/// is removed once read).
pub fn run(path: &str, state_dir: Option<PathBuf>) -> ExitCode {
    let text = std::fs::read_to_string(path);
    let _ = std::fs::remove_file(path);
    let profile = match text.map_err(|e| e.to_string()).and_then(|t| Profile::parse(&t)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("noren-studio: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let commands = match profile.commands() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("noren-studio: {e}");
            return ExitCode::FAILURE;
        }
    };
    let settings = Settings::default_path()
        .map(|p| Settings::load(&p))
        .unwrap_or_default();
    let palette = profile.palette();
    let theme = Theme::new(&palette);
    // The palette opening itself is not something to pick.
    let list: Vec<Command> = commands
        .list
        .iter()
        .filter(|c| c.action != Action::Palette)
        .cloned()
        .collect();
    let items = list
        .iter()
        .map(|c| Item::of(c, commands.leader.as_ref()))
        .collect();
    let [r, g, b, _] = palette.accent.step(9).to_rgba8();
    let rows = list.len().clamp(1, ROWS) as f32;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("noren palette")
            .with_app_id("noren-palette")
            .with_inner_size([620.0, 112.0 + ROW * rows])
            .with_decorations(false)
            .with_transparent(true)
            .with_always_on_top()
            .with_resizable(false)
            .with_taskbar(false)
            .with_icon(egui::IconData {
                rgba: icon_rgba([r, g, b]),
                width: 32,
                height: 32,
            }),
        centered: true,
        ..Default::default()
    };
    let result = eframe::run_native(
        "noren palette",
        options,
        Box::new(move |cc| {
            super::app::style_text(&cc.egui_ctx);
            theme.apply(&cc.egui_ctx);
            let mut motion = Motion::default();
            motion.fast = settings.reduced_motion;
            Ok(Box::new(PaletteApp {
                commands: list,
                items,
                state_dir,
                ui: PaletteUi::default(),
                motion,
                theme,
                lang: settings.lang,
                error: None,
                was_focused: false,
            }))
        }),
    );
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("noren-studio: {e}");
            ExitCode::FAILURE
        }
    }
}
