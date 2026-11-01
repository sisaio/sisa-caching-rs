//! Where stores read the time, so tests can move it.

use std::time::{Instant, SystemTime};

/// Where stores read the time. Tests swap in a clock they can move. A store holds its clock by
/// value and is generic over its type, so each read is a direct call. Each clone of a store holds
/// a clone of its clock, so a clock whose time can move shares that time between its clones.
pub trait Clock: Clone + Send + Sync + 'static {
    /// The wall-clock time, which entries record as their creation time.
    fn now(&self) -> SystemTime;

    /// The monotonic time, which the memory store's deadlines use, so a wall-clock change
    /// doesn't move them.
    fn instant(&self) -> Instant;
}

/// The real clock, and the default for every store.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

// The real time: the wall clock for creation times, the monotonic one for deadlines.
impl Clock for SystemClock {
    #[expect(
        clippy::disallowed_methods,
        reason = "This is the one sanctioned wall clock reader."
    )]
    // Inlined, as the memory store reads the time on every call.
    #[inline]
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }

    #[inline]
    fn instant(&self) -> Instant {
        Instant::now()
    }
}
