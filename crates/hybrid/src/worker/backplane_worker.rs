use std::{sync::Arc, time::Duration};

use sisa_caching_core::{cache::CacheStore, metrics::METRICS};
use tracing::Instrument;

use crate::{
    backplane::{BackplaneMessage, RedisBackplane},
    settings::BackplaneWorkerSettings,
    store::state::State,
};

/// Subscribes to one hybrid store's backplane and applies other nodes' invalidations to its local
/// layer. The store starts it once. It holds the store's state but not its `Worker`, so it stops
/// when the store's last clone drops. The store reads through to the distributed store until this
/// worker has subscribed, and again whenever the subscription is lost, until it subscribes again.
pub(crate) struct BackplaneWorker {
    state: Arc<State>,
    backplane: Arc<RedisBackplane>,
    settings: BackplaneWorkerSettings,
}

impl BackplaneWorker {
    pub(crate) fn new(
        state: Arc<State>,
        backplane: Arc<RedisBackplane>,
        settings: BackplaneWorkerSettings,
    ) -> Self {
        Self {
            state,
            backplane,
            settings,
        }
    }

    /// Whether the message should run against this store's local layer: not its own, and for
    /// this store.
    fn should_run(&self, message: &BackplaneMessage) -> bool {
        message.source != self.state.node_id && &message.store == self.state.local_store.name()
    }

    /// Removes the local copies a message names, in a child of the writer's span, so the trace
    /// crosses nodes.
    async fn apply(&self, message: &BackplaneMessage) {
        let store = self.state.local_store.name();
        let span = {
            let _parent = message.parent_context().attach();

            tracing::info_span!("hybrid apply_invalidation", %store, kind = message.invalidation.kind())
        };

        tracing::debug!(parent: &span, invalidation = ?message.invalidation, "applying an invalidation");
        METRICS.invalidation_received(store);
        self.state
            .invalidate(&message.invalidation)
            .instrument(span)
            .await;
    }

    pub(crate) async fn run(self) {
        let state = &self.state;
        let store = state.local_store.name();
        let mut backoff = Backoff::new(&self.settings);

        // 0 until the first subscribe, so a backplane that never connects still reports.
        METRICS.backplane_connected(store, false);

        loop {
            match self.backplane.subscribe().await {
                Ok(mut subscription) => {
                    let subscribed_at = tokio::time::Instant::now();

                    // Copies made before this subscription may have missed invalidations.
                    state.resync(true).await;
                    METRICS.backplane_connected(store, true);
                    tracing::info!(%store, "backplane subscribed; local reads on");

                    let error = loop {
                        match subscription.next().await {
                            Ok(message) if self.should_run(&message) => self.apply(&message).await,
                            Ok(_) => {}
                            Err(error) => break error,
                        }
                    };

                    tracing::warn!(%store, %error, "backplane subscription lost; reading through until it is back");

                    state.resync(false).await;
                    METRICS.backplane_connected(store, false);
                    backoff.lost(subscribed_at.elapsed());
                }
                Err(error) => {
                    tracing::warn!(%store, %error, "could not subscribe to the backplane; retrying");
                }
            }

            tokio::time::sleep(backoff.wait()).await;
        }
    }
}

/// The wait before each subscribe: `reconnect_delay_ms`, doubling after each try up to
/// `max_reconnect_delay_ms`. Only a subscription that stayed up that long starts it over, so one
/// that breaks right away can't resubscribe and clear the local layer in a tight loop.
struct Backoff<'a> {
    settings: &'a BackplaneWorkerSettings,
    next: Duration,
}

impl<'a> Backoff<'a> {
    fn new(settings: &'a BackplaneWorkerSettings) -> Self {
        Self {
            settings,
            next: settings.reconnect_delay(),
        }
    }

    /// Starts over after a subscription that stayed up for `up`.
    fn lost(&mut self, up: Duration) {
        if up >= self.settings.max_reconnect_delay() {
            self.next = self.settings.reconnect_delay();
        }
    }

    fn wait(&mut self) -> Duration {
        let wait = self.next;

        self.next = (wait * 2).min(self.settings.max_reconnect_delay());
        wait
    }
}

/// A stopped worker can't apply invalidations, so the store must stop trusting its local layer.
/// `run` owns the worker, so this runs however it ends: the store's last clone dropping, its
/// runtime shutting down, or a panic. A store runs one worker, so no other worker's reads are
/// turned off.
impl Drop for BackplaneWorker {
    fn drop(&mut self) {
        self.state.disable_local_reads();
        METRICS.backplane_connected(self.state.local_store.name(), false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1 ms first wait and a 50 ms cap.
    fn settings() -> BackplaneWorkerSettings {
        BackplaneWorkerSettings {
            reconnect_delay_ms: 1,
            max_reconnect_delay_ms: 50,
        }
    }

    #[test]
    fn a_subscription_that_keeps_breaking_waits_longer_each_time() {
        let settings = settings();
        let mut backoff = Backoff::new(&settings);
        let waits: Vec<u128> = (0..8)
            .map(|_| {
                backoff.lost(Duration::ZERO);
                backoff.wait().as_millis()
            })
            .collect();

        assert_eq!(waits, [1, 2, 4, 8, 16, 32, 50, 50]);
    }

    #[test]
    fn a_subscription_that_stayed_up_starts_the_wait_over() {
        let settings = settings();
        let mut backoff = Backoff::new(&settings);

        for _ in 0..6 {
            backoff.wait();
        }
        backoff.lost(Duration::from_millis(49));
        assert_eq!(backoff.wait().as_millis(), 50, "49 ms is not long enough");

        backoff.lost(Duration::from_millis(50));
        assert_eq!(backoff.wait().as_millis(), 1);
    }
}
