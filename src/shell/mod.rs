//! The live side of a rice: things that cannot be written once to a file or the registry and
//! must be held while noren runs: window rules (`[[window]]` in a profile), named states for
//! state-colored borders, focus dimming (`[focus]`), the x-ray hold key (`[xray]`), commands
//! on leader-key chords and hot corners (`[[command]]`, see [`command`]), and edge glows that
//! show a state (`[[glow]]`, see [`glow`]).
//!
//! Nothing here is persistent. `noren run` applies the rules to every window, follows new
//! windows and state changes, and on exit puts every touched window back as it was.

pub mod command;
pub mod focus;
pub mod glow;
#[cfg(windows)]
mod glow_win;
pub mod keys;
pub mod rules;
pub mod state;
#[cfg(windows)]
mod win;

use std::io;
use std::path::PathBuf;

pub use command::{Action, Command, Commands};
pub use focus::{Focus, Xray};
pub use glow::Glow;
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
    pub commands: Commands,
    pub glows: Vec<Glow>,
    /// The profile as TOML, handed to the command palette.
    pub source: String,
}

impl Live {
    pub fn new(profile: &Profile) -> Result<Self, String> {
        let live = Self {
            rules: Rules::new(&profile.window)?,
            focus: profile.focus.as_ref().map(Focus::new).transpose()?,
            xray: profile.xray.as_ref().map(Xray::new).transpose()?,
            commands: profile.commands()?,
            glows: glow::glows(&profile.glow)?,
            source: toml::to_string(profile).map_err(|e| e.to_string())?,
        };
        if live.rules.is_empty()
            && live.focus.is_none()
            && live.xray.is_none()
            && !live.commands.bound()
            && live.glows.is_empty()
        {
            return Err(
                "the profile has no [[window]] rules, [focus], [xray], chords, hot corners or [[glow]], nothing to run"
                    .into(),
            );
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

/// A runtime held on a background thread, for apps that keep their own UI loop (the studio).
/// Dropping it without [`Runner::stop`] leaves the thread holding windows until the process
/// exits, so stop it first.
pub struct Runner {
    #[cfg(windows)]
    tid: u32,
    thread: std::thread::JoinHandle<io::Result<()>>,
}

impl Runner {
    /// Starts holding `profile` on a new thread. Fails fast on a bad profile, and on hook or
    /// overlay errors raised before the loop starts.
    pub fn spawn(profile: &Profile, state_dir: Option<PathBuf>) -> io::Result<Self> {
        let live = Live::new(profile).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        #[cfg(windows)]
        {
            let palette = profile.palette();
            let dir = state_dir.or_else(state::default_dir);
            let (tx, rx) = std::sync::mpsc::channel();
            let thread = std::thread::Builder::new()
                .name("noren-live".into())
                .spawn(move || {
                    win::hold(live, palette, dir, |tid| {
                        let _ = tx.send(tid);
                    })
                })?;
            match rx.recv() {
                Ok(tid) => Ok(Self { tid, thread }),
                // The thread ended before its loop started: hand back its error.
                Err(_) => Err(thread
                    .join()
                    .unwrap_or_else(|_| Err(io::Error::other("live runtime panicked")))
                    .err()
                    .unwrap_or_else(|| io::Error::other("live runtime ended at start"))),
            }
        }
        #[cfg(not(windows))]
        {
            let _ = (live, state_dir);
            Err(unsupported("holding a profile live"))
        }
    }

    /// Whether the runtime thread has ended on its own (an error after start).
    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// Ends the loop, waits until every window is restored, and returns the runtime's result.
    pub fn stop(self) -> io::Result<()> {
        #[cfg(windows)]
        if !self.thread.is_finished() {
            win::stop(self.tid)?;
        }
        self.thread
            .join()
            .unwrap_or_else(|_| Err(io::Error::other("live runtime panicked")))
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
        assert_eq!(live("[[glow]]\nstate = \"ci\"\n").unwrap().glows.len(), 1);
    }
}
