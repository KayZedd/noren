//! A window switcher in place of Alt+Tab: cards with live DWM thumbnails of every window the
//! system switcher would list, in the same most-recent-first order.
//!
//! ```toml
//! [switcher]
//! key = "Alt+Tab"        # default; hold the modifiers, tap the key to move on
//! thumbnails = true      # live window previews, default true
//! ```
//!
//! Hold the modifiers and tap the key to move forward, add Shift to move back; arrows move too.
//! Releasing the modifiers or Enter switches, Esc cancels, a click picks a card. This module is
//! pure: the Win32 runtime feeds [`Switch`] key events and draws [`view`].

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::keys::{Combo, Mod};
use crate::{Element, Rect, Theme, column, container, grid, text};

const ESC: u32 = 0x1B;
const ENTER: u32 = 0x0D;
const LEFT: u32 = 0x25;
const UP: u32 = 0x26;
const RIGHT: u32 = 0x27;
const DOWN: u32 = 0x28;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SwitcherConfig {
    #[serde(default = "SwitcherConfig::default_key")]
    pub key: String,
    #[serde(default = "SwitcherConfig::default_thumbnails")]
    pub thumbnails: bool,
}

impl SwitcherConfig {
    fn default_key() -> String {
        "Alt+Tab".into()
    }

    fn default_thumbnails() -> bool {
        true
    }
}

impl Default for SwitcherConfig {
    fn default() -> Self {
        Self {
            key: Self::default_key(),
            thumbnails: true,
        }
    }
}

/// A validated `[switcher]`.
#[derive(Clone, Debug, PartialEq)]
pub struct Switcher {
    pub combo: Combo,
    pub thumbnails: bool,
}

impl Switcher {
    pub fn new(c: &SwitcherConfig) -> Result<Self, String> {
        let combo = Combo::parse(&c.key).map_err(|e| format!("[switcher] key: {e}"))?;
        if combo.key.is_none() {
            return Err(format!(
                "[switcher] key {:?} needs a key to tap, like \"Alt+Tab\"",
                c.key
            ));
        }
        if combo.mods == BTreeSet::from([Mod::Shift]) {
            return Err(format!(
                "[switcher] key {:?}: Shift alone would eat typing, add Ctrl, Alt or Win",
                c.key
            ));
        }
        Ok(Self {
            combo,
            thumbnails: c.thumbnails,
        })
    }
}

/// What the switcher does after a key event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// Show the switcher; `back` starts at the last window instead of the previous one.
    Open { back: bool },
    /// Move the selection by this many cards.
    Move(i32),
    /// Switch to the selected window and hide.
    Commit,
    /// Hide without switching.
    Cancel,
}

impl Act {
    /// Packs the act into the payload of a keyboard-hook queue entry.
    pub fn encode(self) -> isize {
        match self {
            Act::Commit => 0,
            Act::Cancel => 1,
            Act::Open { back } => 2 + isize::from(back),
            Act::Move(by) => by as isize * 8 + 4,
        }
    }

    pub fn decode(v: isize) -> Option<Act> {
        match (v, v & 7) {
            (0, _) => Some(Act::Commit),
            (1, _) => Some(Act::Cancel),
            (2 | 3, _) => Some(Act::Open { back: v == 3 }),
            (_, 4) => i32::try_from(v >> 3).ok().map(Act::Move),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SwitchStep {
    /// Keep the event from the focused app (and from the system Alt+Tab).
    pub swallow: bool,
    pub act: Option<Act>,
}

/// The switcher's keys. Modifiers always pass through, so apps never see a stuck Alt; every
/// key the switcher takes is swallowed down and up.
#[derive(Clone, Debug)]
pub struct Switch {
    combo: Combo,
    down: BTreeSet<u32>,
    open: bool,
    /// Keys whose down was swallowed: their up is swallowed too.
    eaten: BTreeSet<u32>,
}

impl Switch {
    pub fn new(combo: Combo) -> Self {
        Self {
            combo,
            down: BTreeSet::new(),
            open: false,
            eaten: BTreeSet::new(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    fn mods_held(&self) -> BTreeSet<Mod> {
        self.down.iter().filter_map(|&vk| Mod::of(vk)).collect()
    }

    /// The switcher was closed from outside (a click picked a card).
    pub fn close(&mut self) {
        self.open = false;
    }

    pub fn event(&mut self, vk: u32, down: bool) -> SwitchStep {
        let mut step = SwitchStep::default();
        if !down {
            self.down.remove(&vk);
            step.swallow = self.eaten.remove(&vk);
            let held = self.mods_held();
            if self.open && !self.combo.mods.is_subset(&held) {
                self.open = false;
                step.act = Some(Act::Commit);
            }
            return step;
        }
        self.down.insert(vk);
        let held = self.mods_held();
        let reverse = !self.combo.mods.contains(&Mod::Shift);
        let extra: BTreeSet<Mod> = held.difference(&self.combo.mods).copied().collect();
        let exact = extra.is_empty() || (reverse && extra == BTreeSet::from([Mod::Shift]));
        let act = if self.combo.key == Some(vk) && self.combo.mods.is_subset(&held) && exact {
            let back = !extra.is_empty();
            if self.open {
                Some(Act::Move(if back { -1 } else { 1 }))
            } else {
                self.open = true;
                Some(Act::Open { back })
            }
        } else if self.open {
            match vk {
                ESC => {
                    self.open = false;
                    Some(Act::Cancel)
                }
                ENTER => {
                    self.open = false;
                    Some(Act::Commit)
                }
                LEFT | UP => Some(Act::Move(-1)),
                RIGHT | DOWN => Some(Act::Move(1)),
                _ => None,
            }
        } else {
            None
        };
        if act.is_some() {
            step.swallow = true;
            self.eaten.insert(vk);
        }
        step.act = act;
        step
    }

    /// Key-ups get lost (lock screen, UAC prompt, a hook timeout). `is_down` asks the system;
    /// when the modifiers are no longer held the switcher commits, as their release would.
    pub fn verify(&mut self, is_down: impl Fn(u32) -> bool) -> Option<Act> {
        if !self.open || self.combo.mods.iter().all(|m| is_down(m.generic_vk())) {
            return None;
        }
        self.down.clear();
        self.eaten.clear();
        self.open = false;
        Some(Act::Commit)
    }
}

/// Where the selection starts: the window before the current one, or the last one going back.
pub fn initial(count: usize, back: bool) -> usize {
    match count {
        0 => 0,
        n if back => n - 1,
        n => 1.min(n - 1),
    }
}

/// `selected` moved by `by`, wrapping around.
pub fn step(selected: usize, count: usize, by: i32) -> usize {
    if count == 0 {
        return 0;
    }
    (selected as i64 + i64::from(by)).rem_euclid(count as i64) as usize
}

/// The facts the system switcher decides on, for one top-level window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Candidate {
    pub visible: bool,
    /// Cloaked by DWM: on another virtual desktop, or a suspended UWP frame.
    pub cloaked: bool,
    pub tool_window: bool,
    pub app_window: bool,
    pub no_activate: bool,
    /// The window is the one its owner chain shows (see [`owner_walk`]).
    pub shown_for_owner: bool,
    pub own_process: bool,
    pub title: String,
}

/// Whether the system Alt+Tab would list the window.
pub fn listed(c: &Candidate) -> bool {
    c.visible
        && !c.cloaked
        && !c.own_process
        && !c.title.trim().is_empty()
        && (c.app_window || (!c.tool_window && !c.no_activate))
        && (c.app_window || c.shown_for_owner)
}

/// Raymond Chen's rule for which window of an owner chain the switcher shows: start at the
/// root owner and follow the last active popup while it is hidden. The window qualifies when
/// the walk ends on it, so an owner with a visible dialog stands for both.
pub fn owner_walk<W: Copy + PartialEq>(
    window: W,
    root_owner: impl Fn(W) -> W,
    last_active_popup: impl Fn(W) -> W,
    visible: impl Fn(W) -> bool,
) -> bool {
    let mut walk = root_owner(window);
    // Bounded: a broken chain must not hang the runtime's thread.
    for _ in 0..64 {
        let next = last_active_popup(walk);
        if next == walk || visible(next) {
            break;
        }
        walk = next;
    }
    walk == window
}

/// One card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub title: String,
    pub exe: String,
}

/// Click targets in the switcher view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Card(usize),
    /// The thumbnail slot inside a card; clicks there pick the card too.
    Thumb(usize),
}

impl Pick {
    pub fn index(self) -> usize {
        match self {
            Self::Card(i) | Self::Thumb(i) => i,
        }
    }
}

/// Card size at scale 1.
pub const THUMB: (f32, f32) = (240.0, 150.0);
const PAD: f32 = 10.0;
const GAP: f32 = 12.0;
const TITLE: f32 = 40.0;
/// The view never shrinks below this, so titles stay readable; extra rows then run off.
const MIN_SCALE: f32 = 0.45;

/// How the cards are laid out on a screen area of `area` logical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grid {
    pub columns: usize,
    /// The thumbnail slot; `(w, 0)` without thumbnails.
    pub thumb: (f32, f32),
}

/// Columns and thumbnail size for `count` cards so the view fits 90% of `area`, shrinking the
/// cards when it would not.
pub fn fit_grid(count: usize, area: (f32, f32), thumbnails: bool) -> Grid {
    let count = count.max(1);
    let (aw, ah) = (area.0 * 0.9, area.1 * 0.9);
    let mut scale = 1.0f32;
    loop {
        let tw = (THUMB.0 * scale).round();
        let th = if thumbnails {
            (THUMB.1 * scale).round()
        } else {
            0.0
        };
        let card = (tw + 2.0 * PAD, th + TITLE + 2.0 * PAD);
        let fit = ((aw - 2.0 * GAP + GAP) / (card.0 + GAP)).floor().max(1.0) as usize;
        let columns = fit.min(count);
        let rows = count.div_ceil(columns) as f32;
        let height = rows * card.1 + (rows - 1.0) * GAP + 2.0 * GAP;
        if height <= ah || scale <= MIN_SCALE {
            return Grid {
                columns,
                thumb: (tw, th),
            };
        }
        scale = (scale * 0.9).max(MIN_SCALE);
    }
}

/// The switcher's content: a card per entry, the selected one highlighted.
pub fn view(entries: &[Entry], selected: usize, grid_: Grid, theme: &Theme) -> Element<Pick> {
    let (tw, th) = grid_.thumb;
    let cards = entries.iter().enumerate().map(|(i, e)| {
        let mut card = column()
            .padding(PAD)
            .gap(4.0)
            .width(tw + 2.0 * PAD)
            .radius(theme.units.corner_radius * 1.5)
            .hover_background(theme.text_alpha(0.08))
            .on_click(Pick::Card(i));
        if i == selected {
            card = card
                .background(theme.highlight.with_alpha(0.3))
                .border(theme.highlight, 2.0);
        }
        if th > 0.0 {
            card = card.child(
                container()
                    .size(tw, th)
                    .radius(theme.units.corner_radius)
                    .background(theme.text_alpha(0.05))
                    .on_click(Pick::Thumb(i)),
            );
        }
        card.child(text(e.title.clone()).elide().width(tw)).child(
            text(e.exe.clone())
                .font_size(theme.small_font_size())
                .color(theme.inactive_text)
                .elide()
                .width(tw),
        )
    });
    container()
        .padding(GAP)
        .background(theme.background)
        .child(grid(grid_.columns as u16).gap(GAP).children(cards))
}

/// The thumbnail slots of a laid-out [`view`], by entry index.
pub fn thumb_slots(frame: &crate::layout::Frame<Pick>, count: usize) -> Vec<Rect> {
    let mut slots = vec![Rect::default(); count];
    for node in &frame.nodes {
        if let Some(Pick::Thumb(i)) = node.action
            && let Some(slot) = slots.get_mut(i)
        {
            *slot = node.rect;
        }
    }
    slots
}

/// A window of `source` pixels fitted into `slot`, centered and never enlarged.
pub fn fit(source: (f32, f32), slot: Rect) -> Rect {
    if source.0 <= 0.0 || source.1 <= 0.0 {
        return slot;
    }
    let k = (slot.w / source.0).min(slot.h / source.1).min(1.0);
    let (w, h) = (source.0 * k, source.1 * k);
    Rect::new(slot.x + (slot.w - w) / 2.0, slot.y + (slot.h - h) / 2.0, w, h)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn acts_survive_the_hook_queue() {
        for act in [
            Act::Commit,
            Act::Cancel,
            Act::Open { back: false },
            Act::Open { back: true },
            Act::Move(1),
            Act::Move(-1),
            Act::Move(-7),
        ] {
            assert_eq!(Act::decode(act.encode()), Some(act), "{act:?}");
        }
        assert_eq!(Act::decode(5), None);
    }

    const LALT: u32 = 0xA4;
    const LSHIFT: u32 = 0xA0;
    const LCTRL: u32 = 0xA2;
    const TAB: u32 = 0x09;

    fn switch() -> Switch {
        Switch::new(Switcher::new(&SwitcherConfig::default()).unwrap().combo)
    }

    fn acts(s: &mut Switch, events: &[(u32, bool)]) -> Vec<(bool, Option<Act>)> {
        events
            .iter()
            .map(|&(vk, d)| {
                let st = s.event(vk, d);
                (st.swallow, st.act)
            })
            .collect()
    }

    #[test]
    fn alt_tab_opens_moves_and_commits_on_release() {
        let mut s = switch();
        let out = acts(
            &mut s,
            &[
                (LALT, true),
                (TAB, true),
                (TAB, false),
                (TAB, true),
                (LSHIFT, true),
                (TAB, true),
                (TAB, false),
                (LSHIFT, false),
                (LALT, false),
            ],
        );
        assert_eq!(
            out,
            vec![
                (false, None),
                (true, Some(Act::Open { back: false })),
                (true, None),
                (true, Some(Act::Move(1))),
                (false, None),
                (true, Some(Act::Move(-1))),
                (true, None),
                (false, None),
                (false, Some(Act::Commit)),
            ]
        );
        assert!(!s.is_open());
    }

    #[test]
    fn shift_opens_backwards_and_esc_cancels_while_alt_stays() {
        let mut s = switch();
        let out = acts(
            &mut s,
            &[
                (LALT, true),
                (LSHIFT, true),
                (TAB, true),
                (0x1B, true),
                (0x1B, false),
                (LALT, false),
            ],
        );
        assert_eq!(out[2], (true, Some(Act::Open { back: true })));
        assert_eq!(out[3], (true, Some(Act::Cancel)));
        assert_eq!(out[4], (true, None));
        assert_eq!(out[5], (false, None), "releasing Alt after Esc switches nothing");
    }

    #[test]
    fn other_keys_and_combos_pass_through() {
        let mut s = switch();
        // Tab alone, Ctrl+Alt+Tab and Alt+F4 are not the switcher's.
        assert_eq!(acts(&mut s, &[(TAB, true), (TAB, false)]), vec![(false, None); 2]);
        let out = acts(&mut s, &[(LCTRL, true), (LALT, true), (TAB, true), (TAB, false)]);
        assert!(out.iter().all(|&(sw, a)| !sw && a.is_none()), "{out:?}");
        acts(&mut s, &[(LCTRL, false), (LALT, false)]);
        let out = acts(
            &mut s,
            &[(LALT, true), (0x73, true), (0x73, false), (LALT, false)],
        );
        assert!(out.iter().all(|&(sw, a)| !sw && a.is_none()), "{out:?}");
        // While open, arrows and Enter are taken, letters pass.
        let out = acts(
            &mut s,
            &[
                (LALT, true),
                (TAB, true),
                (0x27, true),
                (0x41, true),
                (0x0D, true),
            ],
        );
        assert_eq!(out[2], (true, Some(Act::Move(1))));
        assert_eq!(out[3], (false, None));
        assert_eq!(out[4], (true, Some(Act::Commit)));
    }

    #[test]
    fn a_lost_alt_release_commits_once() {
        let mut s = switch();
        acts(&mut s, &[(LALT, true), (TAB, true)]);
        assert_eq!(s.verify(|_| true), None);
        assert_eq!(s.verify(|_| false), Some(Act::Commit));
        assert_eq!(s.verify(|_| false), None);
        // The next Alt+Tab works from scratch.
        assert_eq!(
            acts(&mut s, &[(LALT, true), (TAB, true)])[1].1,
            Some(Act::Open { back: false })
        );
    }

    #[test]
    fn keys_need_a_tap_key_and_more_than_shift() {
        let key = |k: &str| {
            Switcher::new(&SwitcherConfig {
                key: k.into(),
                thumbnails: true,
            })
        };
        assert!(key("Win+Tab").is_ok());
        assert!(key("Alt").unwrap_err().contains("needs a key to tap"));
        assert!(key("Shift+Tab").unwrap_err().contains("Shift alone"));
        assert!(key("Alt+Nope").is_err());
        // With Shift in the combo, Shift cannot also mean back.
        let mut s = Switch::new(key("Ctrl+Shift+Tab").unwrap().combo);
        let out = acts(&mut s, &[(LCTRL, true), (LSHIFT, true), (TAB, true), (TAB, true)]);
        assert_eq!(out[2].1, Some(Act::Open { back: false }));
        assert_eq!(out[3].1, Some(Act::Move(1)));
    }

    #[test]
    fn selection_starts_at_the_previous_window_and_wraps() {
        assert_eq!(initial(0, false), 0);
        assert_eq!(initial(1, false), 0);
        assert_eq!(initial(5, false), 1);
        assert_eq!(initial(5, true), 4);
        assert_eq!(step(4, 5, 1), 0);
        assert_eq!(step(0, 5, -1), 4);
        assert_eq!(step(0, 0, 1), 0);
    }

    #[test]
    fn listing_follows_the_system_switcher() {
        let app = Candidate {
            visible: true,
            shown_for_owner: true,
            title: "Doc".into(),
            ..Candidate::default()
        };
        assert!(listed(&app));
        for not in [
            Candidate {
                visible: false,
                ..app.clone()
            },
            Candidate {
                cloaked: true,
                ..app.clone()
            },
            Candidate {
                tool_window: true,
                ..app.clone()
            },
            Candidate {
                no_activate: true,
                ..app.clone()
            },
            Candidate {
                own_process: true,
                ..app.clone()
            },
            Candidate {
                shown_for_owner: false,
                ..app.clone()
            },
            Candidate {
                title: " ".into(),
                ..app.clone()
            },
        ] {
            assert!(!listed(&not), "{not:?}");
        }
        // WS_EX_APPWINDOW forces a tool or owned window in.
        assert!(listed(&Candidate {
            tool_window: true,
            shown_for_owner: false,
            app_window: true,
            ..app
        }));
    }

    #[test]
    fn the_owner_walk_lists_the_owner_of_a_visible_dialog() {
        // 1 owns visible dialog 2; 3 stands alone; 4 owns 5, hidden, which owns 6, visible.
        let root = |w: u32| match w {
            2 => 1,
            5 | 6 => 4,
            w => w,
        };
        let popup = |w: u32| match w {
            1 => 2,
            4 => 5,
            5 => 6,
            w => w,
        };
        let visible = |w: u32| w != 5;
        assert!(
            owner_walk(1, root, popup, visible),
            "the owner stands for its dialog"
        );
        assert!(!owner_walk(2, root, popup, visible));
        assert!(owner_walk(3, root, popup, visible));
        assert!(
            owner_walk(5, root, popup, visible),
            "the walk passes the hidden popup"
        );
        assert!(!owner_walk(4, root, popup, visible));
        // A popup chain that cycles ends.
        assert!(!owner_walk(7, |_| 8, |w| if w == 8 { 9 } else { 8 }, |_| false));
    }

    #[test]
    fn the_grid_fits_the_screen_and_shrinks_when_needed() {
        let g = fit_grid(3, (1920.0, 1080.0), true);
        assert_eq!(
            g,
            Grid {
                columns: 3,
                thumb: THUMB
            }
        );
        let g = fit_grid(30, (1920.0, 1080.0), true);
        assert!(g.columns >= 6 && g.thumb.0 < THUMB.0, "{g:?}");
        let rows = 30usize.div_ceil(g.columns) as f32;
        assert!(
            rows * (g.thumb.1 + TITLE + 2.0 * PAD + GAP) <= 1080.0 * 0.9 + GAP,
            "{g:?}"
        );
        let g = fit_grid(500, (800.0, 600.0), true);
        assert_eq!(g.thumb.0, (THUMB.0 * MIN_SCALE).round(), "stops shrinking");
        assert_eq!(fit_grid(4, (1920.0, 1080.0), false).thumb.1, 0.0);
        assert_eq!(fit_grid(0, (1920.0, 1080.0), true).columns, 1);
    }

    #[test]
    fn the_view_lays_out_a_slot_per_window_and_clicks_find_the_card() {
        let theme = Theme::aizome();
        let Some(mut text) = crate::text::tests::test_text() else {
            return;
        };
        let entries: Vec<Entry> = (0..5)
            .map(|i| Entry {
                title: format!("Window {i}"),
                exe: "app.exe".into(),
            })
            .collect();
        let g = Grid {
            columns: 3,
            thumb: THUMB,
        };
        let frame = crate::layout::layout(
            view(&entries, 1, g, &theme),
            None,
            None,
            &mut text,
            14.0,
            &HashMap::new(),
        );
        let slots = thumb_slots(&frame, entries.len());
        assert!(
            slots.iter().all(|r| r.w == THUMB.0 && r.h == THUMB.1),
            "{slots:?}"
        );
        assert_eq!(slots[0].y, slots[2].y);
        assert!(slots[3].y > slots[0].y && slots[3].x == slots[0].x);
        let (cx, cy) = (slots[4].x + 5.0, slots[4].y + 5.0);
        assert_eq!(frame.hit(cx, cy).map(|p| p.index()), Some(4));
        assert!(frame.size.0 > 3.0 * THUMB.0 && frame.size.1 > 2.0 * THUMB.1);
    }

    #[test]
    fn thumbnails_keep_their_shape_and_never_grow() {
        let slot = Rect::new(10.0, 10.0, 240.0, 150.0);
        let wide = fit((1920.0, 1080.0), slot);
        assert_eq!((wide.w, wide.h.round()), (240.0, 135.0));
        assert_eq!(wide.y, 10.0 + (150.0 - wide.h) / 2.0);
        let tall = fit((600.0, 1200.0), slot);
        assert_eq!((tall.w, tall.h), (75.0, 150.0));
        let small = fit((100.0, 50.0), slot);
        assert_eq!((small.w, small.h, small.x), (100.0, 50.0, 80.0));
        assert_eq!(fit((0.0, 0.0), slot), slot);
    }
}
