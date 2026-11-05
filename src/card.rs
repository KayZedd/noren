//! The rice card: one PNG that shows a profile at a glance, for a README, a forum post or a
//! chat. Name, palette ramps, the 16 terminal colors on a sample prompt, and what the profile
//! sets up.
//!
//! ```text
//! noren card my-rice.toml card.png
//! ```

use tiny_skia::Pixmap;

use crate::palette::{Contrast, Mode, Ramp};
use crate::rice::Profile;
use crate::{Color, Element, Palette, TextSystem, Theme, column, container, row, text};

/// Card width in logical pixels.
pub const WIDTH: f32 = 880.0;

/// What a profile sets up, one short label each, in profile order.
pub fn features(p: &Profile) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: &str| out.push(s.to_owned());
    if p.windows.is_some() {
        push("Windows accent");
    }
    if p.terminal.is_some() {
        push("Windows Terminal");
    }
    if p.vscode.is_some() {
        push("VS Code");
    }
    if p.firefox.is_some() {
        push("Firefox");
    }
    let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    if !p.window.is_empty() {
        out.push(count(p.window.len(), "window rule", "window rules"));
    }
    if p.focus.is_some() {
        out.push("focus dimming".into());
    }
    if p.xray.is_some() {
        out.push("x-ray".into());
    }
    if !p.command.is_empty() {
        out.push(count(p.command.len(), "command", "commands"));
    }
    if let Some(c) = &p.chords {
        out.push(format!("leader {}", c.leader));
    }
    if !p.glow.is_empty() {
        out.push(count(p.glow.len(), "edge glow", "edge glows"));
    }
    let on_panel = p.widget.iter().filter(|w| w.on_panel()).count();
    let glued = p.widget.iter().filter(|w| w.glue.is_some()).count();
    let desktop = p.widget.iter().filter(|w| w.desktop.is_some()).count();
    for (n, one, many) in [
        (on_panel, "panel widget", "panel widgets"),
        (glued, "glued widget", "glued widgets"),
        (desktop, "desktop widget", "desktop widgets"),
    ] {
        if n > 0 {
            out.push(count(n, one, many));
        }
    }
    if p.media.is_some() {
        out.push("now playing".into());
    }
    if !p.layout.is_empty() {
        out.push(count(p.layout.len(), "panel layout", "panel layouts"));
    }
    if let Some(s) = &p.schedule {
        out.push(format!("light {} · dark {}", s.light, s.dark));
    }
    if let Some(s) = &p.switcher {
        out.push(format!("switcher {}", s.key));
    }
    if !p.mirror.is_empty() {
        out.push(count(p.mirror.len(), "window mirror", "window mirrors"));
    }
    if p.notifications.is_some() {
        out.push("styled notifications".into());
    }
    out
}

/// A filled rounded box.
fn swatch<M>(color: Color, w: f32, h: f32, radius: f32) -> Element<M> {
    container().size(w, h).radius(radius).background(color)
}

/// Twelve steps side by side, labeled.
fn ramp<M>(theme: &Theme, label: &str, ramp: &Ramp, width: f32) -> Element<M> {
    let gap = 4.0;
    let w = (width - 11.0 * gap) / 12.0;
    column()
        .gap(6.0)
        .child(text(label).color(theme.inactive_text).font_size(12.0))
        .child(row().gap(gap).children((1..=12).map(|n| {
            // A hairline keeps the steps near the card's own background visible.
            swatch(ramp.step(n), w, 34.0, 6.0).border(theme.text_alpha(0.1), 1.0)
        })))
}

/// A prompt and two lines of output, in the profile's terminal colors.
fn terminal<M>(p: &Palette, width: f32) -> Element<M> {
    let a = &p.ansi;
    let size = 14.0;
    let span = |s: &str, c: Color| text(s).color(c).font_size(size);
    let line = || row().align_center();
    let squares = |range: std::ops::Range<usize>| {
        row()
            .gap(6.0)
            .children(range.map(|i| swatch(a[i], 22.0, 22.0, 5.0)))
    };
    container()
        .width(width)
        .padding(18.0)
        .radius(12.0)
        .background(p.neutral.step(1))
        .border(p.neutral.step(6), 1.0)
        .child(
            column()
                .gap(6.0)
                .child(
                    line()
                        .child(span("me", a[2]))
                        .child(span("@", p.neutral.step(11)))
                        .child(span("rice", a[4]))
                        .child(span(" ~/dots ", a[3]))
                        .child(span("> noren apply", p.neutral.step(12))),
                )
                .child(
                    line()
                        .child(span("ok ", a[10]))
                        .child(span("palette, terminal, vscode", p.neutral.step(11))),
                )
                .child(
                    line()
                        .child(span("warn ", a[11]))
                        .child(span("firefox restarts to pick it up", p.neutral.step(11))),
                )
                .child(
                    line()
                        .child(span("fail ", a[9]))
                        .child(span("nothing", p.neutral.step(11))),
                )
                .child(
                    column()
                        .gap(6.0)
                        .padding_edges(0.0, 10.0, 0.0, 0.0)
                        .child(squares(0..8))
                        .child(squares(8..16)),
                ),
        )
}

/// A small rounded label.
fn pill<M>(label: &str, fg: Color, bg: Color) -> Element<M> {
    container()
        .padding_xy(12.0, 6.0)
        .radius(14.0)
        .background(bg)
        .child(text(label).color(fg).font_size(13.0))
}

/// The card's element tree.
pub fn card<M>(profile: &Profile) -> Element<M> {
    let p = profile.palette();
    let theme = p.theme(Theme::aizome());
    let pad = 36.0;
    let inner = WIDTH - 2.0 * pad;
    let accent = p.accent.step(9);
    let mode = match profile.palette.mode {
        Mode::Dark => "dark",
        Mode::Light => "light",
    };
    let contrast = match profile.palette.contrast {
        Contrast::Standard => "standard contrast",
        Contrast::High => "high contrast",
    };

    let title = column()
        .gap(4.0)
        .grow()
        .child(text("noren rice").color(theme.inactive_text).font_size(13.0))
        .child(text(profile.name.clone()).bold().font_size(32.0).elide())
        .child(
            text(format!("{mode} · {contrast} · {}", accent.to_hex()))
                .color(theme.inactive_text)
                .font_size(14.0),
        );
    let badge = container()
        .size(84.0, 84.0)
        .radius(20.0)
        .background(accent)
        .center()
        .child(text("Aa").bold().font_size(30.0).color(p.on_accent));
    let header = row().align_center().gap(20.0).child(title).child(badge);

    let tones = row()
        .gap(8.0)
        .child(pill("positive", p.positive.step(11), p.positive.step(3)))
        .child(pill("warning", p.warning.step(11), p.warning.step(3)))
        .child(pill("negative", p.negative.step(11), p.negative.step(3)))
        .child(pill("accent", p.accent.step(11), p.accent.step(3)));

    let mut view = column()
        .width(WIDTH)
        .padding(pad)
        .gap(22.0)
        .child(header)
        .child(ramp(&theme, "Accent", &p.accent, inner))
        .child(ramp(&theme, "Neutral", &p.neutral, inner))
        .child(tones)
        .child(terminal(&p, inner));
    let features = features(profile);
    if !features.is_empty() {
        view = view.child(
            row()
                .gap(8.0)
                .width(inner)
                .style(|s| {
                    s.flex_wrap = taffy::FlexWrap::Wrap;
                    s.gap.height = taffy::LengthPercentage::length(8.0);
                })
                .children(
                    features
                        .iter()
                        .map(|f| pill(f, p.neutral.step(12), p.neutral.step(3))),
                ),
        );
    }
    view.child(
        row()
            .justify_end()
            .width(inner)
            .child(text("made with noren").color(p.neutral.step(10)).font_size(12.0)),
    )
}

/// The card as pixels at `scale` (2 for sharp text on high-DPI screens).
pub fn render(profile: &Profile, scale: f32) -> Result<Pixmap, String> {
    if !(0.5..=4.0).contains(&scale) {
        return Err(format!("scale {scale} is outside 0.5..4"));
    }
    let theme = profile.palette().theme(Theme::aizome());
    let mut text = TextSystem::new(&theme.font_families);
    render_with(profile, scale, &mut text)
}

fn render_with(profile: &Profile, scale: f32, text: &mut TextSystem) -> Result<Pixmap, String> {
    let p = profile.palette();
    let theme = p.theme(Theme::aizome());
    crate::render::snapshot(card::<()>(profile), &theme, text, scale, p.neutral.step(2))
        .ok_or_else(|| "the card has no size".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(extra: &str) -> Profile {
        Profile::parse(&format!(
            "schema = 1\nname = \"Night Market\"\n[palette]\naccent = \"#d27e99\"\n{extra}"
        ))
        .unwrap()
    }

    #[test]
    fn features_list_what_the_profile_sets_up() {
        assert!(features(&profile("")).is_empty());
        let p = profile(
            "[windows]\n[focus]\ndim = 0.3\n[[window]]\nexe = \"a.exe\"\nopacity = 0.9\n\
             [[window]]\nexe = \"b.exe\"\nopacity = 0.9\n[media]\n\
             [[layout]]\ndesktop = \"Music\"\nwidgets = [\"media\"]\n\
             [schedule]\nlight = \"sunrise\"\ndark = \"21:00\"\nlatitude = 52.2\nlongitude = 21.0\n\
             [switcher]\n[[mirror]]\ntitle = \"*log*\"\n[notifications]\n\
             [[widget]]\nname = \"a\"\nrun = [\"a\"]\n\
             [[widget]]\nname = \"b\"\nrun = [\"b\"]\n[widget.glue]\nexe = \"Code.exe\"\n\
             [[widget]]\nname = \"c\"\nrun = [\"c\"]\n[widget.desktop]\n",
        );
        assert_eq!(
            features(&p),
            vec![
                "Windows accent",
                "2 window rules",
                "focus dimming",
                "1 panel widget",
                "1 glued widget",
                "1 desktop widget",
                "now playing",
                "1 panel layout",
                "light sunrise · dark 21:00",
                "switcher Alt+Tab",
                "1 window mirror",
                "styled notifications",
            ]
        );
    }

    #[test]
    fn the_card_shows_the_name_and_the_palette() {
        let Some(mut text) = crate::text::tests::test_text() else {
            return;
        };
        let p = profile("[media]\n");
        let png = render_with(&p, 1.0, &mut text).unwrap();
        assert_eq!(png.width(), WIDTH as u32);
        assert!(png.height() > 400, "{}", png.height());
        let palette = p.palette();
        // Every accent ramp step is painted somewhere on the card.
        let pixels: std::collections::HashSet<[u8; 4]> = png
            .pixels()
            .iter()
            .map(|c| [c.red(), c.green(), c.blue(), c.alpha()])
            .collect();
        for n in 1..=12 {
            assert!(
                pixels.contains(&palette.accent.step(n).to_rgba8()),
                "accent step {n}"
            );
        }
        let names = crate::layout::layout(card::<()>(&p), None, None, &mut text, 13.0, &Default::default());
        assert!(names.find_text("Night Market").is_some());
        assert!(names.find_text("now playing").is_some());
    }

    #[test]
    fn scale_multiplies_the_size_and_is_bounded() {
        let Some(mut text) = crate::text::tests::test_text() else {
            return;
        };
        let p = profile("");
        let one = render_with(&p, 1.0, &mut text).unwrap();
        let two = render_with(&p, 2.0, &mut text).unwrap();
        assert_eq!(two.width(), one.width() * 2);
        assert!(render(&p, 10.0).is_err());
    }
}
