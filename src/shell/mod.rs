//! The live side of a rice: things that cannot be written once to a file or the registry and
//! must be held while noren runs: window rules (`[[window]]` in a profile), named states for
//! state-colored borders, focus dimming (`[focus]`) and the x-ray hold key (`[xray]`).
//!
//! Nothing here is persistent. `noren run` applies the rules to every window, follows new
//! windows and state changes, and on exit puts every touched window back as it was.

pub mod focus;
pub mod keys;
pub mod rules;
pub mod state;
#[cfg(windows)]
mod win;

use std::io;
use std::path::PathBuf;

pub use focus::{Focus, Xray};
pub use rules::{Rules, Style, WindowInfo, WindowRule};
pub use state::State;

use crate::rice::Profile;

#[cfg(not(windows))]
fn unsupported(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, format!("{what} needs Windows"))
}

/// Everything `noren run` holds, validated.
pub struct Live {
    pub rules: Rules,
    pub focus: Option<Focus>,
    pub xray: Option<Xray>,
}

impl Live {
    pub fn new(profile: &Profile) -> Result<Self, String> {
        let live = Self {
            rules: Rules::new(&profile.window)?,
            focus: profile.focus.as_ref().map(Focus::new).transpose()?,
            xray: profile.xray.as_ref().map(Xray::new).transpose()?,
        };
        if live.rules.is_empty() && live.focus.is_none() && live.xray.is_none() {
            return Err("the profile has no [[window]] rules, [focus] or [xray], nothing to run".into());
        }
        Ok(live)
    }
}

/// Holds the profile's window rules, focus dimming and x-ray key until Ctrl+C, then restores
/// every window. States are read from `state_dir` (default [`state::default_dir`]).
pub fn run(profile: &Profile, state_dir: Option<PathBuf>) -> io::Result<()> {
    let live = Live::new(profile).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    #[cfg(windows)]
    {
        win::run(live, profile.palette(), state_dir.or_else(state::default_dir))
    }
    #[cfg(not(windows))]
    {
        let _ = (live, state_dir);
        Err(unsupported("holding a profile live"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn live(extra: &str) -> Result<Live, String> {
        Live::new(&Profile::parse(&format!(
            "schema = 1\nname = \"x\"\n[palette]\naccent = \"#7e9cd8\"\n{extra}"
        ))?)
    }

    #[test]
    fn run_needs_something_to_hold_but_not_window_rules() {
        assert!(live("").err().unwrap().contains("nothing to run"));
        assert!(live("[focus]\ndim = 0.3\n").unwrap().rules.is_empty());
        assert!(live("[xray]\nkey = \"Ctrl+Win\"\n").unwrap().xray.is_some());
        assert!(live("[[window]]\ncorners = \"round\"\n").is_ok());
    }
}
