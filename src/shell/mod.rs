//! The live side of a rice: things that cannot be written once to a file or the registry and
//! must be held while noren runs. Window rules (`[[window]]` in a profile) and named states
//! for state-colored borders so far.
//!
//! Nothing here is persistent. `noren run` applies the rules to every window, follows new
//! windows and state changes, and on exit puts every touched window back as it was.

pub mod rules;
pub mod state;
#[cfg(windows)]
mod win;

use std::io;
use std::path::PathBuf;

pub use rules::{Rules, Style, WindowInfo, WindowRule};
pub use state::State;

use crate::rice::Profile;

#[cfg(not(windows))]
fn unsupported(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, format!("{what} needs Windows"))
}

/// Holds the profile's window rules until Ctrl+C, then restores every window. States are read
/// from `state_dir` (default [`state::default_dir`]).
pub fn run(profile: &Profile, state_dir: Option<PathBuf>) -> io::Result<()> {
    let rules = Rules::new(&profile.window).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    if rules.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the profile has no [[window]] rules, nothing to run",
        ));
    }
    #[cfg(windows)]
    {
        win::run(rules, profile.palette(), state_dir.or_else(state::default_dir))
    }
    #[cfg(not(windows))]
    {
        let _ = state_dir;
        Err(unsupported("running window rules"))
    }
}

/// Every top-level window rules can match, with the facts they match on.
pub fn windows() -> io::Result<Vec<WindowInfo>> {
    #[cfg(windows)]
    {
        Ok(win::list())
    }
    #[cfg(not(windows))]
    {
        Err(unsupported("listing windows"))
    }
}
