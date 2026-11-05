//! noren: a pure-Rust framework for customizing the Windows desktop.
//!
//! Build UI as [`Element`] trees rebuilt from state every frame (like QML bindings), put it on
//! a [`surface::Surface`]: a floating auto-hide [`Panel`] with plasmoid-style [`Widget`]s, a
//! desktop widget or overlay ([`surface::Floating`]), or your own. A [`host`] gives each
//! surface a per-pixel-alpha window.
//!
//! Stack: taffy (layout), tiny-skia (painting), cosmic-text (text), resvg (icons), raw Win32
//! (windows). All permissively licensed, no attribution UI required.

pub mod anim;
pub mod card;
pub mod color;
pub mod components;
pub mod element;
pub mod exec;
pub mod geom;
pub mod glue;
pub mod history;
pub mod host;
pub mod layout;
pub mod layouts;
pub mod media;
pub mod palette;
pub mod panel;
pub mod render;
pub mod report;
pub mod rice;
pub mod sampler;
pub mod shell;
pub mod studio;
pub mod surface;
pub mod text;
pub mod theme;
pub mod update;
pub mod widget;

pub use anim::{Animated, ease_out_cubic};
pub use color::Color;
pub use element::{Element, Paint, TextAlign, column, container, grid, row, spacer, svg, text};
pub use geom::Rect;
pub use history::History;
pub use palette::{Palette, PaletteSpec};
pub use panel::{Panel, PanelConfig};
pub use render::Canvas;
pub use sampler::Sampler;
pub use surface::{Floating, FloatingConfig, Surface, View};
pub use text::TextSystem;
pub use theme::{Theme, Units};
pub use widget::{Command, Context, Widget};

/// Re-exported so painters can draw with tiny-skia directly.
pub use tiny_skia;
