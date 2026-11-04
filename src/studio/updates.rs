//! The studio's update check and install, run off the UI thread.

use std::sync::mpsc::{Receiver, channel};

use crate::update::{self, Release};

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    Installing(Release),
    /// Installed; the new version runs after a restart.
    Installed(Release),
    Failed(String),
}

enum Msg {
    Checked(Result<Option<Release>, String>),
    Installed(Result<Vec<String>, String>),
}

pub struct Updater {
    pub state: State,
    rx: Option<Receiver<Msg>>,
}

impl Default for Updater {
    fn default() -> Self {
        Self {
            state: State::Idle,
            rx: None,
        }
    }
}

impl Updater {
    pub fn busy(&self) -> bool {
        matches!(self.state, State::Checking | State::Installing(_))
    }

    /// Asks GitHub for the newest release. `wake` runs when the answer is in.
    pub fn check(&mut self, wake: impl Fn() + Send + 'static) {
        if !self.busy() {
            self.state = State::Checking;
            self.spawn(move || Msg::Checked(update::check()), wake);
        }
    }

    /// Installs the release found by [`Self::check`].
    pub fn install(&mut self, wake: impl Fn() + Send + 'static) {
        if let State::Available(release) = &self.state {
            let release = release.clone();
            self.state = State::Installing(release.clone());
            self.spawn(move || Msg::Installed(update::install(&release)), wake);
        }
    }

    fn spawn(&mut self, work: impl FnOnce() -> Msg + Send + 'static, wake: impl Fn() + Send + 'static) {
        let (tx, rx) = channel();
        self.rx = Some(rx);
        std::thread::Builder::new()
            .name("noren-update".into())
            .spawn(move || {
                let _ = tx.send(work());
                wake();
            })
            .expect("spawning the update thread");
    }

    /// Takes a finished answer, if any. True when the state changed.
    pub fn poll(&mut self) -> bool {
        let Some(msg) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) else {
            return false;
        };
        self.rx = None;
        self.state = next(std::mem::replace(&mut self.state, State::Idle), msg);
        true
    }
}

fn next(state: State, msg: Msg) -> State {
    match (state, msg) {
        (_, Msg::Checked(Ok(None))) => State::UpToDate,
        (_, Msg::Checked(Ok(Some(r)))) => State::Available(r),
        (State::Installing(r), Msg::Installed(Ok(_))) => State::Installed(r),
        (_, Msg::Checked(Err(e)) | Msg::Installed(Err(e))) => State::Failed(e),
        (state, Msg::Installed(Ok(_))) => state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::Version;

    fn release() -> Release {
        Release {
            version: Version(9, 0, 0),
            tag: "v9.0.0".into(),
            page: "https://example.invalid".into(),
            assets: Default::default(),
        }
    }

    #[test]
    fn answers_move_the_state() {
        assert_eq!(next(State::Checking, Msg::Checked(Ok(None))), State::UpToDate);
        assert_eq!(
            next(State::Checking, Msg::Checked(Ok(Some(release())))),
            State::Available(release())
        );
        assert_eq!(
            next(
                State::Installing(release()),
                Msg::Installed(Ok(vec!["noren.exe".into()]))
            ),
            State::Installed(release())
        );
        assert_eq!(
            next(
                State::Installing(release()),
                Msg::Installed(Err("bad sum".into()))
            ),
            State::Failed("bad sum".into())
        );
    }

    #[test]
    fn poll_takes_the_answer_once() {
        let mut u = Updater {
            state: State::Checking,
            ..Updater::default()
        };
        assert!(u.busy());
        assert!(!u.poll());
        u.spawn(|| Msg::Checked(Ok(Some(release()))), || {});
        while !u.poll() {
            std::thread::yield_now();
        }
        assert_eq!(u.state, State::Available(release()));
        assert!(!u.poll());
        assert!(!u.busy());
    }
}
