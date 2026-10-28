//! A clock the test moves.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

use sisa_caching_core::clock::Clock;

/// A clock that only moves on [`advance`](Self::advance). Clones share the time, so a test keeps
/// one and gives another to a store.
#[derive(Debug, Clone)]
pub struct ManualClock {
    start: Instant,
    elapsed_ms: Arc<AtomicU64>,
    wall_jump_ms: Arc<AtomicU64>,
}

impl ManualClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            elapsed_ms: Arc::default(),
            wall_jump_ms: Arc::default(),
        }
    }

    /// Moves both clocks forward, as time passing does.
    pub fn advance(&self, by: Duration) {
        self.elapsed_ms.fetch_add(millis(by), Ordering::SeqCst);
    }

    /// Moves only the wall clock forward, as someone changing the machine's time does.
    pub fn jump_wall_clock(&self, by: Duration) {
        self.wall_jump_ms.fetch_add(millis(by), Ordering::SeqCst);
    }

    fn elapsed(&self) -> Duration {
        Duration::from_millis(self.elapsed_ms.load(Ordering::SeqCst))
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
            + Duration::from_secs(1_000_000)
            + self.elapsed()
            + Duration::from_millis(self.wall_jump_ms.load(Ordering::SeqCst))
    }

    fn instant(&self) -> Instant {
        self.start + self.elapsed()
    }
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis() as u64
}
