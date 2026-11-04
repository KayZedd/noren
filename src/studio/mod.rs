//! noren studio: the GUI for everything the `noren` command line does. Edit the palette and
//! the profile, preview and apply changes, roll them back, hold the profile live from the
//! tray, watch windows and states.
//!
//! [`model`] is the pure part and always built; the egui app needs the `studio` feature.

pub mod model;

#[cfg(feature = "studio")]
mod app;
#[cfg(feature = "studio")]
mod glass;
#[cfg(all(feature = "studio", windows))]
mod tray;

#[cfg(feature = "studio")]
pub use app::main;
