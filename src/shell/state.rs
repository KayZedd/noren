//! Named states that scripts set and window rules read: `noren state build fail` from a build
//! script turns every window with `border = "state:build"` red.
//!
//! Each state is a file `<dir>/<name>` holding `ok`, `fail`, `warn` or `busy`. Files keep it
//! scriptable from anything (a git hook can `echo fail > ...`) and survive restarts of the
//! runtime.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::Color;
use crate::palette::Palette;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Ok,
    Fail,
    Warn,
    Busy,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Fail => "fail",
            Self::Warn => "warn",
            Self::Busy => "busy",
        }
    }

    /// The palette's solid step for the state; busy uses the accent.
    pub fn color(self, p: &Palette) -> Color {
        match self {
            Self::Ok => p.positive.step(9),
            Self::Fail => p.negative.step(9),
            Self::Warn => p.warning.step(9),
            Self::Busy => p.accent.step(9),
        }
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for State {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim() {
            "ok" => Ok(Self::Ok),
            "fail" => Ok(Self::Fail),
            "warn" => Ok(Self::Warn),
            "busy" => Ok(Self::Busy),
            other => Err(format!("unknown state {other:?}, want ok|fail|warn|busy")),
        }
    }
}

/// `%LOCALAPPDATA%\noren\state` (see [`crate::rice::data_dir`]).
pub fn default_dir() -> Option<PathBuf> {
    Some(crate::rice::data_dir()?.join("state"))
}

/// Letters, digits, `.`, `_`, `-`; 1 to 64 characters; no leading dot. Keeps names usable as
/// file names everywhere and out of other directories.
pub fn valid_name(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

fn check_name(name: &str) -> io::Result<()> {
    if valid_name(name) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("bad state name {name:?}: use letters, digits, '.', '_' or '-' (max 64)"),
        ))
    }
}

/// Sets `name` to `state`; `None` clears it. Written to a temporary file and renamed, so a
/// reader never sees a half-written value.
pub fn set(dir: &Path, name: &str, state: Option<State>) -> io::Result<()> {
    check_name(name)?;
    let path = dir.join(name);
    match state {
        Some(s) => {
            fs::create_dir_all(dir)?;
            let tmp = dir.join(format!(".{name}.tmp"));
            fs::write(&tmp, s.as_str())?;
            fs::rename(tmp, path)
        }
        None => match fs::remove_file(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        },
    }
}

/// Every valid state in `dir`. A missing directory is no states; files with bad names or
/// contents are skipped, so a stray file cannot stop the runtime.
pub fn read_all(dir: &Path) -> io::Result<BTreeMap<String, State>> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(e),
    };
    let mut out = BTreeMap::new();
    for entry in entries {
        let entry = entry?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !valid_name(&name) {
            continue;
        }
        if let Ok(state) = fs::read_to_string(entry.path())
            .map_err(|e| e.to_string())
            .and_then(|t| t.parse())
        {
            out.insert(name, state);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::PaletteSpec;

    fn temp(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("noren-state-{tag}-{}", std::process::id()))
    }

    #[test]
    fn set_read_clear_round_trip() {
        let dir = temp("rt");
        assert!(read_all(&dir).unwrap().is_empty(), "missing dir reads as empty");
        set(&dir, "build", Some(State::Fail)).unwrap();
        set(&dir, "git.dirty", Some(State::Warn)).unwrap();
        set(&dir, "build", Some(State::Ok)).unwrap();
        let all = read_all(&dir).unwrap();
        assert_eq!(all.get("build"), Some(&State::Ok));
        assert_eq!(all.get("git.dirty"), Some(&State::Warn));
        assert_eq!(all.len(), 2, "no temp files left behind: {all:?}");
        set(&dir, "build", None).unwrap();
        set(&dir, "build", None).unwrap();
        assert_eq!(read_all(&dir).unwrap().len(), 1);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn junk_files_are_skipped_and_hand_written_values_are_trimmed() {
        let dir = temp("junk");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("ci"), "busy\n").unwrap();
        fs::write(dir.join("broken"), "maybe").unwrap();
        fs::write(dir.join(".hidden"), "ok").unwrap();
        let all = read_all(&dir).unwrap();
        assert_eq!(
            all.into_iter().collect::<Vec<_>>(),
            [("ci".to_string(), State::Busy)]
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn names_cannot_escape_the_directory() {
        for bad in ["", "..", "../x", "a/b", r"a\b", ".x", "a b", &"x".repeat(65)] {
            assert!(!valid_name(bad), "{bad:?}");
            assert!(set(&temp("bad"), bad, Some(State::Ok)).is_err());
        }
        assert!(valid_name("build-main_2.ci"));
    }

    #[test]
    fn states_map_to_palette_status_colors() {
        let p = Palette::generate(&PaletteSpec::new(Color::from_hex("#7e9cd8").unwrap()));
        assert_eq!(State::Fail.color(&p), p.negative.step(9));
        assert_eq!(State::Ok.color(&p), p.positive.step(9));
        assert_eq!("warn".parse::<State>(), Ok(State::Warn));
        assert!("nope".parse::<State>().is_err());
    }
}
