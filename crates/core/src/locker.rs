//! The per-key locks `get_or_set` loads under.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard, PoisonError, atomic::AtomicBool},
};

use tokio::sync::{Mutex, OwnedMutexGuard};

/// A share of one key's mutex. The last share to go removes the mutex from the map.
struct Claim<'a> {
    locker: &'a Locker,
    key: String,
    /// `None` once dropped.
    mutex: Option<Arc<Mutex<()>>>,
}

// Gives up the share, and removes the key's mutex if it was the last one.
impl Drop for Claim<'_> {
    fn drop(&mut self) {
        // A caller takes its first share under this lock and gives up its claim, its last share,
        // under it too, so a count of two, the map's and this one, means nobody else holds or
        // waits on the mutex. This share goes before the lock is released: given up after it,
        // two last claims dropped at once could each count the other's share and both leave the
        // mutex in the map. It is the map's own `Arc`, so the count needs no second lookup.
        let mut locks = self.locker.lock_registry();
        let mutex = self.mutex.take();

        if mutex.is_some_and(|mutex| Arc::strong_count(&mutex) == 2) {
            locks.remove(&self.key);
        }
    }
}

/// Holds the lock for one key until dropped.
#[must_use]
pub struct KeyGuard<'a> {
    // Fields drop in order: release the mutex, then give up the claim on it.
    _guard: OwnedMutexGuard<()>,
    claim: Claim<'a>,
}

// Shows the key held; the mutex guard has nothing to show.
impl std::fmt::Debug for KeyGuard<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeyGuard")
            .field("key", &self.claim.key)
            .finish()
    }
}

/// One async mutex per key, created on first use and removed once nobody holds or waits on it.
///
/// `lock` calls for one key wait on each other; calls for different keys never do. `try_lock`
/// never waits.
///
/// It also keeps whether the store's last read and last write inside `get_or_set` failed, so a
/// run of failures logs once.
#[derive(Debug, Default)]
pub struct Locker {
    locks: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
    pub(crate) reads_failing: AtomicBool,
    pub(crate) writes_failing: AtomicBool,
}

impl Locker {
    /// An empty locker, the same as `Locker::default()`.
    pub fn new() -> Self {
        Self::default()
    }

    /// Waits for, then holds, the lock for `key` until the guard is dropped.
    pub async fn lock(&self, key: &str) -> KeyGuard<'_> {
        let (claim, mutex) = self.claim(key);
        let guard = mutex.lock_owned().await;

        KeyGuard {
            _guard: guard,
            claim,
        }
    }

    /// Takes the lock for `key` only if it is free right now.
    pub fn try_lock(&self, key: &str) -> Option<KeyGuard<'_>> {
        let (claim, mutex) = self.claim(key);
        let guard = mutex.try_lock_owned().ok()?;

        Some(KeyGuard {
            _guard: guard,
            claim,
        })
    }

    /// A share of `key`'s mutex, and the mutex to lock.
    fn claim(&self, key: &str) -> (Claim<'_>, Arc<Mutex<()>>) {
        let mutex = Arc::clone(self.lock_registry().entry(key.to_owned()).or_default());
        let claim = Claim {
            locker: self,
            key: key.to_owned(),
            mutex: Some(Arc::clone(&mutex)),
        };

        (claim, mutex)
    }

    fn lock_registry(&self) -> StdMutexGuard<'_, HashMap<String, Arc<Mutex<()>>>> {
        self.locks.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::{sync::Barrier, time::Duration};

    use super::*;

    #[tokio::test]
    async fn a_key_is_held_by_one_caller_at_a_time() {
        let locker = Locker::new();
        let held = locker.lock("a").await;

        assert!(locker.try_lock("a").is_none());
        assert!(locker.try_lock("b").is_some(), "other keys stay free");

        drop(held);

        assert!(locker.try_lock("a").is_some());
    }

    #[tokio::test]
    async fn a_waiting_caller_gets_the_lock_once_it_is_released() {
        let locker = Locker::new();
        let held = locker.lock("a").await;
        let mut waiting = Box::pin(locker.lock("a"));

        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );

        drop(held);

        let _now_held = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .expect("the lock was released");
    }

    #[tokio::test]
    async fn a_lock_nobody_holds_is_removed() {
        let locker = Locker::new();

        drop(locker.lock("a").await);
        drop(locker.try_lock("b"));
        // A caller that stops waiting gives up its claim too.
        let held = locker.lock("c").await;
        assert!(
            tokio::time::timeout(Duration::from_millis(10), locker.lock("c"))
                .await
                .is_err()
        );
        drop(held);

        assert!(locker.lock_registry().is_empty());
    }

    #[test]
    fn the_last_two_claims_given_up_at_once_remove_the_lock() {
        let locker = Locker::new();
        let keys: Vec<String> = (0..10_000).map(|id| id.to_string()).collect();
        let claim_all = || {
            keys.iter()
                .map(|key| locker.claim(key).0)
                .collect::<Vec<_>>()
        };
        let (first, second) = (claim_all(), claim_all());
        let together = Barrier::new(2);

        // Each key's two claims go at the same moment, on two threads.
        std::thread::scope(|scope| {
            for claims in [first, second] {
                scope.spawn(|| {
                    for claim in claims {
                        together.wait();
                        drop(claim);
                    }
                });
            }
        });

        assert_eq!(locker.lock_registry().len(), 0, "locks left in the map");
    }
}
