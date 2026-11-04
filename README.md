# noren

A pure-Rust framework for customizing the Windows desktop. It covers top panels with popups, desktop widgets, overlays and HUDs. You describe UI as declarative element trees. Everything is rendered in software into per-pixel-alpha windows. The license is MIT, and so is every dependency's license.

It started as the Windows port of a set of KDE Plasma 6 widgets (`rice`). It reproduces their look and behavior: Kirigami units, the Plasma panel and popup chrome, and color schemes read from `kdeglobals`.

## Why not an existing UI toolkit

| Option | Why it was not used |
|---|---|
| Slint | Royalty-free license requires attribution in the shipped app |
| egui / iced | Built around one application window. Layered, click-through, AppBar and desktop-layer windows fight the event loop. |
| WinUI / XAML | Not Rust-first, heavy runtime |

Desktop customization needs things those toolkits don't model:
- Windows whose transparent pixels pass clicks through to the desktop.
- Several of these windows on several monitors, driven by one loop.
- Auto-hide edges.
- A fullscreen-app detector.

The rendering stack is small and permissive:
- [tiny-skia](https://github.com/linebender/tiny-skia) for paint
- [cosmic-text](https://github.com/pop-os/cosmic-text) for text
- [taffy](https://github.com/DioxusLabs/taffy) for flexbox and grid layout
- [resvg](https://github.com/linebender/resvg) for icons
- [windows](https://github.com/microsoft/windows-rs) for Win32

## Architecture

```
Surface (trait)            one transparent window on one monitor
├── Panel                  auto-hide top bar + popups, hosts Widgets
│   └── Widget (trait)     compact view in the bar, optional popup, messages
└── Floating<V: View>      element tree anchored to a monitor corner/edge
                           Layer::Desktop (widgets), Normal, Top (overlays)

Element<M>                 declarative tree: row/column/grid/text/svg/painter
layout → Frame             taffy layout, QML-like defaults (no implicit shrink)
render                     tiny-skia painting, hover, clipping, scroll areas

host::run                  Win32: layered windows, UpdateLayeredWindow, AppBar,
                           monitor/DPI tracking, fullscreen detection
host::preview::run         winit window acting as a virtual monitor (any OS)
host::sim::Simulator       headless desktop: compositing, input routing, PNGs
```

Surfaces work in logical pixels relative to their monitor. Hosts convert to and from physical screen coordinates using per-monitor DPI v2. The same `Surface` runs under all three hosts, so most behavior is testable on Linux CI without a display.

## Usage

```rust,ignore
use noren::host::{self, Entry, SurfaceOptions};
use noren::{Panel, PanelConfig, Theme};

let mut panel = Panel::new(PanelConfig::default(), Theme::kanagawa_wave());
panel.add(MyWidget::new()); // impl Widget
host::run(vec![Entry::new(panel, SurfaceOptions::panel())])?;
```

Desktop widget pinned to the bottom-right corner, below all windows:

```rust,ignore
use noren::surface::{Anchor, FloatingConfig, Layer};
use noren::Floating;

let config = FloatingConfig { anchor: Anchor::BottomRight, layer: Layer::Desktop, ..Default::default() };
let clock = Floating::new(Clock::default(), config, Theme::kanagawa_wave()); // impl View
host::run(vec![Entry::new(clock, SurfaceOptions::default())])?;
```

`examples/pulse.rs` is a complete widget. It ports the `systempulse` plasmoid: CPU, RAM and GPU rings, temperature and network, with a popup of stat cards and sparklines.

```sh
cargo run --release --example pulse                          # Windows: native panel
cargo run --example pulse --features preview -- --preview     # any OS: dev window
cargo run --example pulse -- --screenshot pulse.png --popup   # headless PNG
```

On Windows, GPU load comes from the `GPU Engine` performance counters, the same source Task Manager uses. Temperature reads "n/a" when no sensor is exposed to user mode, as in the plasmoid.

## Palettes

`noren::palette` builds a whole color system from one accent color, in OKLCH:
- Each ramp has twelve steps with fixed roles: backgrounds, borders, solid fills, text.
- Neutrals are tinted toward the accent hue.
- Positive, warning and negative colors share the accent's lightness and chroma.
- 16 ANSI colors are included.

Text steps are pushed until they pass APCA contrast targets against the background. The tests check this for every hue, in dark and light mode, at both contrast levels.

```sh
noren palette '#7e9cd8'                     # swatches in the terminal
noren palette '#7e9cd8' --light --high      # light mode, high contrast
noren palette '#7e9cd8' --export terminal   # also: vscode, css, kde, firefox
```

## Transactional ricing

A rice is a `profile.toml`: a palette plus the targets it is pushed to. Supported targets:
- Windows accent and title bars (per-user registry)
- Windows Terminal
- VS Code
- Firefox

```sh
noren diff kanagawa.toml       # what would change
noren apply kanagawa.toml      # one transaction
noren rollback                 # exact undo of the newest transaction
noren log
```

Every write is recorded in a journal with the previous bytes **before** it happens. A failed apply undoes what it already wrote. Rollback restores a value only if it still holds what noren wrote. Anything you changed afterwards is reported and left alone, unless you pass `--force`.

JSON configs are edited in place, so comments, key order and formatting survive. CSS and `user.js` get a marked `noren:begin` / `noren:end` block, and everything around it stays yours.

The profile format is documented in [`src/rice/profile.rs`](src/rice/profile.rs).

## Fonts

Text uses the theme's font families and falls back to system fonts. No fonts are bundled. To ship one, call `TextSystem::load_fonts(path)` and check its license.

## Development

```sh
cargo test --all-targets                                       # gate tests, < 1 s
cargo clippy --all-targets --features preview
cargo clippy --target x86_64-pc-windows-msvc --all-targets     # Win32 host from Linux
```

Text-rendering tests run only when Noto Sans or DejaVu Sans is installed. Without either they pass trivially.

## License

MIT, see [LICENSE](LICENSE).
