use std::{
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use crate::{
    cache::{CacheEntry, CacheError, CacheOptions, CacheValue, KeyPrefix},
    clock::{Clock, SystemClock},
    locker::Locker,
    metrics::METRICS,
    settings::CacheSettings,
};

/// A store that can drop its expired entries on demand, as its own purge task or a scheduler does.
pub trait PurgeableStore {
    /// Drops expired entries and returns how many.
    fn purge(&self) -> usize;

    /// How often the store's own purge task calls `purge`. `None` means never.
    fn purge_interval(&self) -> Option<Duration>;
}

/// A named store that holds values of any [`CacheValue`] type under string keys. The type is
/// named on each call, so one store serves every type an application caches.
///
/// A key is any string, such as `"user:42"` or `&format!("user:v1:{id}")`; levels are separated by
/// `:`, and `remove_prefix` removes whole levels. [`CacheKey`](crate::CacheKey) builds a
/// checked one.
///
/// Since each call names its value type, the trait can't be used as `dyn CacheStore`. To pick a
/// store at startup, such as memory in development and the hybrid in production, write the code
/// that uses the cache generic over `C: CacheStore`, or wrap the stores you use in an enum.
pub trait CacheStore: Send + Sync {
    /// What the store's calls fail with. Every [`CacheError`] converts into it.
    type Error: From<CacheError> + std::error::Error + Send + Sync + 'static;

    /// The store's name, written in front of every key it stores.
    fn name(&self) -> &KeyPrefix;

    /// Shared TTL, refresh-ahead and lock settings. Composed stores reuse their distributed
    /// store's settings, so configuring that store also configures calls through the composition.
    fn cache_settings(&self) -> &CacheSettings;

    /// The per-key locks `get_or_set` loads under, which also keep whether its store calls are
    /// failing. A store holds one [`Locker::default()`] and returns it here.
    fn locker(&self) -> &Locker;

    /// Where `set` and `get_or_set` read the time.
    #[doc(hidden)]
    fn clock(&self) -> &dyn Clock {
        &SystemClock
    }

    /// The entry under `key` as a `CV`. Typed memory stores use [`CacheError::TypeMismatch`] when
    /// the stored Rust type differs. Encoded stores deserialize into `CV` and return a decoding
    /// error if the data does not fit; they may accept different types with compatible schemas.
    fn get_entry<CV: CacheValue>(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<CacheEntry<CV>>, Self::Error>> + Send;

    /// The value under `key` as a `CV`, or `None` when it is missing or expired.
    fn get<CV: CacheValue>(
        &self,
        key: &str,
    ) -> impl Future<Output = Result<Option<CV>, Self::Error>> + Send {
        async move { Ok(self.get_entry::<CV>(key).await?.map(|entry| entry.value)) }
    }

    /// `options` sets this write's TTL; `None` uses the store's default TTL.
    fn set<CV: CacheValue>(
        &self,
        key: &str,
        value: CV,
        options: impl Into<Option<CacheOptions>> + Send,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        async move {
            let options = options.into().unwrap_or_default();

            options.validate()?;

            let ttl = options.ttl().or(self.cache_settings().default_ttl());

            self.set_entry(key, CacheEntry::new(value, self.clock().now(), ttl))
                .await
        }
    }

    /// Stores `entry` as it is: its creation time and TTL are kept, and it expires when the entry
    /// does, not after this store's default TTL. A store may hold it for less, such as the memory
    /// store with `max_ttl_secs`. `get_entry` from one store and `set_entry` into another copies
    /// an entry without changing it.
    fn set_entry<CV: CacheValue>(
        &self,
        key: &str,
        entry: CacheEntry<CV>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;

    /// Loads a missing value, coordinating other loaders through this store's per-key lock.
    /// Explicit mutations are not ordered against a running loader: nothing checks for writes made
    /// while it runs, so its result can overwrite one. Stores in other processes also load
    /// independently. Use application-level versioning when those writes require strict ordering.
    ///
    /// When the key holds something it can't read as `CV`, such as another type or JSON that no
    /// longer fits after a field was added without `#[serde(default)]`, it loads and stores the
    /// loaded value over it. When a field is renamed or changes type, or a value keeps its shape
    /// but changes meaning, bump the version in its key (`user:v1` to `user:v2`, see
    /// [`CacheKey`](crate::CacheKey)); otherwise the old and new versions of a rolling deploy
    /// replace each other's entries, or misread them.
    ///
    /// A store call that fails inside it doesn't fail the call, since it returns the loader's
    /// error type: a failed read loads, and a failed store returns the loaded value without
    /// caching it. Each failure counts in `sisa.caching.errors`; the first of a run logs a
    /// warning, and the success that ends it an info line. Options over the 365-day limit fail
    /// the store the same way.
    fn get_or_set<CV, F, Fut, E>(
        &self,
        key: &str,
        load: F,
        options: impl Into<Option<CacheOptions>> + Send,
    ) -> impl Future<Output = Result<CV, E>> + Send
    where
        CV: CacheValue,
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<CV, E>> + Send,
        E: Send,
    {
        get_or_set_with_lock(self, key, load, options.into())
    }

    /// Returns whether the key was there.
    fn remove(&self, key: &str) -> impl Future<Output = Result<bool, Self::Error>> + Send;

    /// Removes every key under `prefix`, matched by whole levels (`user` removes `user:42`, not
    /// `users:42`, nor the key `user` itself), and returns how many. Fails with
    /// [`CacheError::InvalidKey`] when `prefix` is empty or has an empty level.
    ///
    /// It visits every key the store holds, not only those under `prefix`: a memory store walks
    /// all its entries, and a Redis store runs `SCAN` over the whole database on each primary. A
    /// hybrid store walks Redis once and the memory layer of every node, and reads in progress on
    /// those nodes keep no local copy. Use [`remove`](Self::remove) for keys you can name.
    fn remove_prefix(
        &self,
        prefix: &str,
    ) -> impl Future<Output = Result<usize, Self::Error>> + Send;

    /// Removes every key in this store, and nothing outside it.
    fn clear(&self) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// The body of `get_or_set`, under the store's own locks and clock.
///
/// Runs in a `cache get_or_set` debug span with the full key, and records each load's duration and
/// each failed store call in [`METRICS`]. Its warnings name the store, not the key: the span shows
/// the key once debug is on.
#[tracing::instrument(level = "debug", name = "cache get_or_set", skip_all, fields(key = %store.name().format_key(key)))]
async fn get_or_set_with_lock<S, CV, F, Fut, E>(
    store: &S,
    key: &str,
    load: F,
    options: Option<CacheOptions>,
) -> Result<CV, E>
where
    S: CacheStore + ?Sized,
    CV: CacheValue,
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = Result<CV, E>> + Send,
    E: Send,
{
    let (settings, locker, clock) = (store.cache_settings(), store.locker(), store.clock());
    // The locks go by the store's full key, built only when a call locks: a hit never needs it.
    let full_key = || store.name().format_key(key);
    // Loads, records how long it took, and stores what it got.
    let load = || async move {
        let started = Instant::now();
        let loaded = load().await;

        METRICS.load(store.name(), started.elapsed(), loaded.is_ok());

        if let Ok(value) = &loaded {
            let stored = store.set(key, value.clone(), options).await;

            record(store, "set", &locker.writes_failing, &stored);
        }

        loaded
    };

    let read = store.get_entry::<CV>(key).await;

    record(store, "get", &locker.reads_failing, &read);

    if let Ok(Some(entry)) = read {
        let due = settings
            .refresh_threshold()
            .is_some_and(|share| entry.is_refresh_due(clock.now(), share));

        // One caller refreshes; the rest are served the cached value meanwhile.
        let Some(_guard) = due.then(|| locker.try_lock(&full_key())).flatten() else {
            return Ok(entry.value);
        };

        return match load().await {
            // The cached value only while it is live: the load may have outlasted it.
            Err(_) if !entry.is_expired(clock.now()) => {
                tracing::warn!(store = %store.name(), "refresh failed; serving the cached value");

                Ok(entry.value)
            }
            loaded => loaded,
        };
    }

    let _guard = match settings.lock_timeout() {
        Some(timeout) => match tokio::time::timeout(timeout, locker.lock(&full_key())).await {
            Ok(guard) => Some(guard),
            Err(_) => {
                // Another caller's load is still running: this one loads too, without the lock.
                tracing::warn!(store = %store.name(), ?timeout, "lock wait timed out; loading without the lock");

                None
            }
        },
        None => Some(locker.lock(&full_key()).await),
    };

    // Another caller may have stored the value while this one waited.
    let read = store.get_entry::<CV>(key).await;

    record(store, "get", &locker.reads_failing, &read);

    // On a value of another type, data that doesn't decode, or a store that can't be reached, the
    // loaded value is stored over whatever is there.
    match read {
        Ok(Some(entry)) => Ok(entry.value),
        _ => load().await,
    }
}

/// Counts a failed store call in [`METRICS`], and logs the first failure of a run and the success
/// that ends it, so an outage logs two lines however many calls fail. `failing` holds whether the
/// last such call failed.
fn record<S, T>(
    store: &S,
    operation: &'static str,
    failing: &AtomicBool,
    result: &Result<T, S::Error>,
) where
    S: CacheStore + ?Sized,
{
    match result {
        Err(error) => {
            METRICS.failed(store.name(), operation);

            if !failing.swap(true, Ordering::Relaxed) {
                tracing::warn!(store = %store.name(), operation, %error, "store calls are failing; get_or_set counts each failure and logs again once one works");
            }
        }
        Ok(_) => {
            // A load first, so a success writes nothing while the store works.
            if failing.load(Ordering::Relaxed) && failing.swap(false, Ordering::Relaxed) {
                tracing::info!(store = %store.name(), operation, "store calls work again");
            }
        }
    }
}
