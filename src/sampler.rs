use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

/// Runs a collector on a background thread at a fixed interval and keeps only the newest
/// result. Dropping the sampler wakes the thread and joins it, so no work outlives the widget.
pub struct Sampler<T> {
    shared: Arc<Shared<T>>,
    thread: Option<JoinHandle<()>>,
}

struct Shared<T> {
    state: Mutex<State<T>>,
    wake: Condvar,
}

struct State<T> {
    latest: Option<T>,
    stop: bool,
}

impl<T: Send + 'static> Sampler<T> {
    /// `collect` runs immediately, then once per `interval` until the sampler is dropped.
    pub fn spawn(interval: Duration, mut collect: impl FnMut() -> T + Send + 'static) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                latest: None,
                stop: false,
            }),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("noren-sampler".into())
            .spawn(move || {
                loop {
                    let value = collect();
                    let state = worker.state.lock().unwrap_or_else(PoisonError::into_inner);
                    if state.stop {
                        break;
                    }
                    let mut state = state;
                    state.latest = Some(value);
                    let (state, _) = worker
                        .wake
                        .wait_timeout_while(state, interval, |s| !s.stop)
                        .unwrap_or_else(PoisonError::into_inner);
                    if state.stop {
                        break;
                    }
                }
            })
            .expect("spawn sampler thread");
        Self {
            shared,
            thread: Some(thread),
        }
    }

    /// Newest sample since the last call, if any.
    pub fn take(&self) -> Option<T> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .latest
            .take()
    }
}

impl<T> Drop for Sampler<T> {
    fn drop(&mut self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .stop = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Instant;

    #[test]
    fn delivers_first_sample_and_drop_wakes_a_long_sleep() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let sampler = Sampler::spawn(Duration::from_secs(3600), move || {
            counter.fetch_add(1, Ordering::SeqCst) + 1
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        let first = loop {
            if let Some(v) = sampler.take() {
                break v;
            }
            assert!(Instant::now() < deadline, "no sample");
            std::thread::yield_now();
        };
        assert_eq!(first, 1);
        assert_eq!(sampler.take(), None);
        let started = Instant::now();
        drop(sampler);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
