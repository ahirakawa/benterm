//! Counts the locks the UI side takes of a terminal's state, for
//! measuring what a wake of the terminal costs. A terminal without a
//! meter takes its locks as before, and reads no clock.

use std::ops::Sub;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// How many locks were taken, and how long was waited for them in all.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LockCount {
    pub locks: u64,
    pub waited: Duration,
}

impl Sub for LockCount {
    type Output = LockCount;

    /// What was counted between `rhs` and `self`, `rhs` being earlier.
    fn sub(self, rhs: LockCount) -> LockCount {
        LockCount {
            locks: self.locks.saturating_sub(rhs.locks),
            waited: self.waited.saturating_sub(rhs.waited),
        }
    }
}

/// A count of locks that the terminal it is given to adds to. Clones
/// share the count.
#[derive(Clone, Debug, Default)]
pub struct LockMeter(Arc<Counts>);

#[derive(Debug, Default)]
struct Counts {
    locks: AtomicU64,
    waited_nanos: AtomicU64,
}

impl LockMeter {
    pub fn new() -> Self {
        Self::default()
    }

    /// What has been counted so far.
    pub fn count(&self) -> LockCount {
        LockCount {
            locks: self.0.locks.load(Ordering::Relaxed),
            waited: Duration::from_nanos(self.0.waited_nanos.load(Ordering::Relaxed)),
        }
    }

    /// Takes a lock with `lock`, counting it and the time it took.
    pub(crate) fn time<T>(&self, lock: impl FnOnce() -> T) -> T {
        let start = Instant::now();
        let guard = lock();
        let waited = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.0.locks.fetch_add(1, Ordering::Relaxed);
        self.0.waited_nanos.fetch_add(waited, Ordering::Relaxed);
        guard
    }
}
