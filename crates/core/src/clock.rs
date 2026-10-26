//! Where stores read the time, so tests can move it.

use std::time::{Instant, SystemTime};

/// Where stores read the time. Tests swap in a clock they can move.
pub trait Clock: Send + Sync + 'static {
    /// The wall-clock time, which entries record as their creation time.
    fn now(&self) -> SystemTime;

    /// The monotonic time, which the memory store's deadlines use, so a wall-clock change
    /// doesn't move them.
    fn instant(&self) -> Instant;
}

/// The real clock, and the default for every store.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "This is the one sanctioned wall clock reader."
    )]
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    fn instant(&self) -> Instant {
        Instant::now()
    }
}
