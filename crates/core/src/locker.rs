//! The per-key locks `get_or_set` loads under.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex, MutexGuard as StdMutexGuard},
};

use tokio::sync::{Mutex, OwnedMutexGuard};

/// One async mutex per key, created on first use and removed once nobody holds or waits on it.
///
/// `lock` calls for one key wait on each other; calls for different keys never do. `try_lock`
/// never waits.
#[derive(Debug, Default)]
pub struct Locker {
    locks: StdMutex<HashMap<String, Arc<Mutex<()>>>>,
}

/// Holds the lock for one key until dropped.
#[must_use]
pub struct KeyGuard<'a> {
    // Fields drop in order: release the mutex, then give up the claim on it.
    _guard: OwnedMutexGuard<()>,
    _claim: Claim<'a>,
}

impl std::fmt::Debug for KeyGuard<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("KeyGuard")
            .field("key", &self._claim.key)
            .finish()
    }
}

/// A share of one key's mutex. The last share to go removes the mutex from the map.
struct Claim<'a> {
    locker: &'a Locker,
    key: String,
    mutex: Option<Arc<Mutex<()>>>,
}

impl Locker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Waits for, then holds, the lock for `key` until the guard is dropped.
    pub async fn lock(&self, key: &str) -> KeyGuard<'_> {
        let claim = self.claim(key);
        let guard = claim.clone_mutex().lock_owned().await;

        KeyGuard {
            _guard: guard,
            _claim: claim,
        }
    }

    /// Takes the lock for `key` only if it is free right now.
    pub fn try_lock(&self, key: &str) -> Option<KeyGuard<'_>> {
        let claim = self.claim(key);
        let guard = claim.clone_mutex().try_lock_owned().ok()?;

        Some(KeyGuard {
            _guard: guard,
            _claim: claim,
        })
    }

    fn claim(&self, key: &str) -> Claim<'_> {
        let mutex = Arc::clone(self.lock_registry().entry(key.to_owned()).or_default());

        Claim {
            locker: self,
            key: key.to_owned(),
            mutex: Some(mutex),
        }
    }

    fn lock_registry(&self) -> StdMutexGuard<'_, HashMap<String, Arc<Mutex<()>>>> {
        self.locks.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Claim<'_> {
    fn clone_mutex(&self) -> Arc<Mutex<()>> {
        Arc::clone(self.mutex.as_ref().expect("taken only on drop"))
    }
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        drop(self.mutex.take());

        // Shares are only handed out under this lock, so a count of one here means the map's own
        // copy is the last.
        let mut locks = self.locker.lock_registry();

        if locks
            .get(&self.key)
            .is_some_and(|mutex| Arc::strong_count(mutex) == 1)
        {
            locks.remove(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

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

        assert!(locker.lock_registry().is_empty());
    }
}
