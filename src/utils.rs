//! Utilities

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::task::yield_now;

#[derive(Debug, Clone)]
#[repr(transparent)]
/// Idle guard
pub(crate) struct IdleHandler {
    idle_since: Arc<Mutex<Option<Instant>>>,
}

impl IdleHandler {
    #[inline]
    /// Create a new [`IdleHandler`]
    pub(crate) fn new() -> Self {
        Self {
            idle_since: Arc::new(Mutex::new(Some(Instant::now()))),
        }
    }

    /// When idle, wait up to `max_dur` time.
    pub(crate) async fn wait_max_idle(self, max_dur: Option<Duration>) {
        let max_dur = max_dur.unwrap_or(Duration::from_secs(15));

        loop {
            if self
                .idle_since
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some_and(|start| start.elapsed() > max_dur)
            {
                break;
            }

            tokio::time::sleep(Duration::from_secs(1)).await;

            yield_now().await;
        }
    }

    #[inline]
    /// `NOT idle` guard
    pub(crate) fn idle_guard(&self) -> IdleGuard<'_> {
        *self.idle_since.lock().unwrap_or_else(|e| e.into_inner()) = None;
        IdleGuard {
            idle_since: &self.idle_since,
        }
    }
}

#[repr(transparent)]
/// Idle guard
pub(crate) struct IdleGuard<'g> {
    idle_since: &'g Mutex<Option<Instant>>,
}

impl Drop for IdleGuard<'_> {
    fn drop(&mut self) {
        *self.idle_since.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }
}
