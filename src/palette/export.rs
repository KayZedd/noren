//! A [`Palette`] rendered into other programs' color formats. Pure functions: text and values
//! only. Writing them to disk, merging into existing configs and undoing it is
//! [`crate::rice`]'s job.

use serde_json::{Value, json};

use super::{Mode, Palette};
use crate::Color;

/// A Windows Terminal `schemes[]` entry.
pub fn windows_terminal_scheme(p: &Palette, name: &str) -> Value {
    let a = |i: usize| Value::String(p.ansi[i].to_hex());
    json!({
        "name": name,
        "background": p.neutral.step(1).to_hex(),
        "foreground": p.neutral.step(12).to_hex(),
        "cursorColor": p.accent.step(9).to_hex(),
        "selectionBackground": p.accent.step(5).to_hex(),
        "black": a(0), "red": a(1), "green": a(2), "yellow": a(3),
        "blue": a(4), "purple": a(5), "cyan": a(6), "white": a(7),
        "brightBlack": a(8), "brightRed": a(9), "brightGreen": a(10), "brightYellow": a(11),
        "brightBlue": a(12), "brightPurple": a(13), "brightCyan": a(14), "brightWhite": a(15),
    })
}

/// VS Code `workbench.colorCustomizations` keys, sorted.
pub fn vscode_colors(p: &Palette) -> Vec<(String, String)> {
    let (n, ac) = (&p.neutral, &p.accent);
    let colors = vec![
        ("activityBar.background", n.step(2)),
        ("activityBar.foreground", n.step(12)),
        ("activityBar.inactiveForeground", n.step(11)),
        ("activityBarBadge.background", ac.step(9)),
        ("activityBarBadge.foreground", p.on_accent),
        ("badge.background", ac.step(9)),
        ("badge.foreground", p.on_accent),
        ("button.background", ac.step(9)),
        ("button.foreground", p.on_accent),
        ("button.hoverBackground", ac.step(10)),
        ("editor.background", n.step(1)),
        ("editor.foreground", n.step(12)),
        ("editor.lineHighlightBackground", n.step(2)),
        ("editor.selectionBackground", ac.step(5)),
        ("editorCursor.foreground", ac.step(9)),
        ("editorError.foreground", p.negative.step(11)),
        ("editorGroupHeader.tabsBackground", n.step(2)),
        ("editorLineNumber.activeForeground", n.step(12)),
        ("editorLineNumber.foreground", n.step(8)),
        ("editorWarning.foreground", p.warning.step(11)),
        ("errorForeground", p.negative.step(11)),
        ("focusBorder", ac.step(8)),
        ("foreground", n.step(12)),
        ("gitDecoration.addedResourceForeground", p.positive.step(11)),
        ("gitDecoration.deletedResourceForeground", p.negative.step(11)),
        ("gitDecoration.modifiedResourceForeground", p.warning.step(11)),
        ("input.background", n.step(3)),
        ("input.border", n.step(6)),
        ("list.activeSelectionBackground", ac.step(5)),
        ("list.hoverBackground", n.step(4)),
        ("panel.background", n.step(1)),
        ("panel.border", n.step(6)),
        ("sideBar.background", n.step(2)),
        ("sideBar.foreground", n.step(11)),
        ("statusBar.background", n.step(2)),
        ("statusBar.foreground", n.step(11)),
        ("tab.activeBackground", n.step(1)),
        ("tab.activeBorderTop", ac.step(9)),
        ("tab.inactiveBackground", n.step(2)),
        ("tab.inactiveForeground", n.step(11)),
        ("terminal.background", n.step(1)),
        ("terminal.foreground", n.step(12)),
        ("textLink.foreground", ac.step(11)),
        ("titleBar.activeBackground", n.step(2)),
        ("titleBar.activeForeground", n.step(12)),
        ("titleBar.inactiveBackground", n.step(2)),
        ("titleBar.inactiveForeground", n.step(11)),
    ];
    let ansi = [
        "Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White",
    ];
    let mut out: Vec<(String, String)> = colors
        .into_iter()
        .map(|(k, c)| (k.to_owned(), c.to_hex()))
        .collect();
    for (i, name) in ansi.iter().enumerate() {
        out.push((format!("terminal.ansi{name}"), p.ansi[i].to_hex()));
        out.push((format!("terminal.ansiBright{name}"), p.ansi[i + 8].to_hex()));
    }
    out.sort();
    out
}

/// CSS custom properties (`--noren-neutral-1` … `--noren-negative-12`, `--noren-ansi-0` …)
/// inside `selector`. Used for Firefox `userChrome.css` and web dashboards.
pub fn css_variables(p: &Palette, selector: &str) -> String {
    let mut css = format!("{selector} {{\n");
    for (name, ramp) in [
        ("neutral", &p.neutral),
        ("accent", &p.accent),
        ("positive", &p.positive),
        ("warning", &p.warning),
        ("negative", &p.negative),
    ] {
        for (i, c) in ramp.0.iter().enumerate() {
            css += &format!("  --noren-{name}-{}: {};\n", i + 1, c.to_hex());
        }
    }
    for (i, c) in p.ansi.iter().enumerate() {
        css += &format!("  --noren-ansi-{i}: {};\n", c.to_hex());
    }
    css += &format!("  --noren-on-accent: {};\n}}\n", p.on_accent.to_hex());
    css
}

/// Firefox chrome restyle built on [`css_variables`]: toolbars, tabs, URL bar, menus.
pub fn firefox_user_chrome(p: &Palette) -> String {
    let mut css = css_variables(p, ":root");
    css += r#":root {
  --toolbar-bgcolor: var(--noren-neutral-2) !important;
  --toolbar-color: var(--noren-neutral-12) !important;
  --lwt-accent-color: var(--noren-neutral-1) !important;
  --lwt-text-color: var(--noren-neutral-12) !important;
  --tab-selected-bgcolor: var(--noren-neutral-3) !important;
  --toolbar-field-background-color: var(--noren-neutral-3) !important;
  --toolbar-field-focus-background-color: var(--noren-neutral-4) !important;
  --toolbar-field-color: var(--noren-neutral-12) !important;
  --focus-outline-color: var(--noren-accent-8) !important;
  --arrowpanel-background: var(--noren-neutral-2) !important;
  --arrowpanel-color: var(--noren-neutral-12) !important;
  --arrowpanel-border-color: var(--noren-neutral-6) !important;
}
#TabsToolbar, #navigator-toolbox { background: var(--noren-neutral-1) !important; }
.tab-background[selected] { border-top: 2px solid var(--noren-accent-9) !important; }
"#;
    css
}

/// KDE `kdeglobals` color groups, so the Linux side of a dual-boot rice matches.
pub fn kdeglobals(p: &Palette) -> String {
    let kde = |c: Color| {
        let [r, g, b, _] = c.to_rgba8();
        format!("{r},{g},{b}")
    };
    let n = &p.neutral;
    let mut out = format!("[General]\nAccentColor={}\n", kde(p.accent.step(9)));
    out += &format!(
        "ColorScheme=Noren{}\n",
        if p.spec.mode == Mode::Dark {
            "Dark"
        } else {
            "Light"
        }
    );
    for (group, bg, alt) in [
        ("Window", n.step(1), n.step(2)),
        ("View", n.step(1), n.step(2)),
        ("Button", n.step(3), n.step(4)),
        ("Header", n.step(2), n.step(3)),
        ("Tooltip", n.step(2), n.step(3)),
        ("Complementary", n.step(2), n.step(3)),
    ] {
        out += &format!(
            "\n[Colors:{group}]\nBackgroundNormal={}\nBackgroundAlternate={}\nForegroundNormal={}\n\
             ForegroundInactive={}\nForegroundActive={}\nForegroundLink={}\nForegroundPositive={}\n\
             ForegroundNeutral={}\nForegroundNegative={}\nDecorationFocus={}\nDecorationHover={}\n",
            kde(bg),
            kde(alt),
            kde(n.step(12)),
            kde(n.step(11)),
            kde(p.accent.step(11)),
            kde(p.accent.step(11)),
            kde(p.positive.step(11)),
            kde(p.warning.step(11)),
            kde(p.negative.step(11)),
            kde(p.accent.step(9)),
            kde(p.accent.step(8)),
        );
    }
    out += &format!(
        "\n[Colors:Selection]\nBackgroundNormal={}\nForegroundNormal={}\n",
        kde(p.accent.step(9)),
        kde(p.on_accent)
    );
    out
}

/// Registry values Windows reads for the system accent (Settings ▸ Personalization ▸ Colors).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WindowsAccent {
    /// `Explorer\Accent\AccentPalette`: 8 RGBA entries, light-3 … dark-3 then a spare.
    pub accent_palette: [u8; 32],
    /// `Explorer\Accent\AccentColorMenu` and `StartColorMenu`, `0xAABBGGRR`.
    pub accent_color_menu: u32,
    pub start_color_menu: u32,
    /// `DWM\AccentColor`, `0xAABBGGRR`.
    pub dwm_accent: u32,
    /// `DWM\ColorizationColor` and `ColorizationAfterglow`, `0xAARRGGBB`.
    pub colorization: u32,
}

pub fn windows_accent(p: &Palette) -> WindowsAccent {
    let base = p.accent.step(9).to_oklch();
    // Windows ships the accent with three lighter and three darker shades for its UI.
    let shades: Vec<Color> = [0.24, 0.16, 0.08, 0.0, -0.08, -0.16, -0.24, -0.32]
        .iter()
        .map(|d| super::Oklch::new((base.l + d).clamp(0.05, 0.97), base.c, base.h).to_color())
        .collect();
    let mut accent_palette = [0u8; 32];
    for (i, c) in shades.iter().enumerate() {
        accent_palette[i * 4..i * 4 + 4].copy_from_slice(&c.with_alpha(1.0).to_rgba8());
    }
    let abgr = |c: &Color| {
        let [r, g, b, _] = c.to_rgba8();
        u32::from_le_bytes([r, g, b, 0xff])
    };
    let [r, g, b, _] = shades[3].to_rgba8();
    WindowsAccent {
        accent_palette,
        accent_color_menu: abgr(&shades[3]),
        start_color_menu: abgr(&shades[5]),
        dwm_accent: abgr(&shades[3]),
        colorization: u32::from_be_bytes([0xc4, r, g, b]),
    }
}

/// DWM `COLORREF` (`0x00BBGGRR`) for `DWMWA_BORDER_COLOR` / `DWMWA_CAPTION_COLOR` /
/// `DWMWA_TEXT_COLOR`.
pub fn colorref(c: Color) -> u32 {
    let [r, g, b, _] = c.to_rgba8();
    u32::from_le_bytes([r, g, b, 0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::PaletteSpec;

    fn palette() -> Palette {
        Palette::generate(&PaletteSpec::new(Color::from_hex("#7e9cd8").unwrap()))
    }

    #[test]
    fn terminal_scheme_has_every_field_windows_terminal_needs() {
        let s = windows_terminal_scheme(&palette(), "noren");
        for key in [
            "name",
            "background",
            "foreground",
            "cursorColor",
            "selectionBackground",
            "black",
            "red",
            "green",
            "yellow",
            "blue",
            "purple",
            "cyan",
            "white",
            "brightBlack",
            "brightRed",
            "brightGreen",
            "brightYellow",
            "brightBlue",
            "brightPurple",
            "brightCyan",
            "brightWhite",
        ] {
            let v = s[key].as_str().unwrap_or_else(|| panic!("{key}"));
            assert!(key == "name" || Color::from_hex(v).is_some(), "{key}={v}");
        }
        assert_eq!(s["cursorColor"], "#7e9cd8");
    }

    #[test]
    fn vscode_keys_are_unique_sorted_and_include_terminal_colors() {
        let colors = vscode_colors(&palette());
        assert!(colors.windows(2).all(|w| w[0].0 < w[1].0));
        assert!(colors.iter().any(|(k, _)| k == "terminal.ansiBrightMagenta"));
        assert!(colors.iter().all(|(_, v)| Color::from_hex(v).is_some()));
    }

    #[test]
    fn kdeglobals_export_reads_back_as_the_same_theme() {
        let p = palette();
        let theme = crate::Theme::from_kdeglobals(&kdeglobals(&p)).unwrap();
        let expected = p.theme(crate::Theme::breeze_dark());
        assert_eq!(theme.background.to_hex(), expected.background.to_hex());
        assert_eq!(theme.text.to_hex(), expected.text.to_hex());
        assert_eq!(theme.highlight.to_hex(), expected.highlight.to_hex());
        assert_eq!(theme.negative.to_hex(), expected.negative.to_hex());
    }

    #[test]
    fn windows_accent_byte_orders() {
        let a = windows_accent(&palette());
        // Base shade is entry 3 of the palette and must equal the seed.
        assert_eq!(&a.accent_palette[12..16], &[0x7e, 0x9c, 0xd8, 0xff]);
        assert_eq!(a.dwm_accent, 0xffd89c7e);
        assert_eq!(a.colorization, 0xc47e9cd8);
        assert_eq!(colorref(Color::rgb8(1, 2, 3)), 0x0003_0201);
    }

    #[test]
    fn css_lists_all_ramps() {
        let css = css_variables(&palette(), ":root");
        assert!(css.contains("--noren-accent-9: #7e9cd8;"));
        assert_eq!(css.matches("--noren-").count(), 5 * 12 + 16 + 1);
    }
}
