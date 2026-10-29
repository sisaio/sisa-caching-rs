use std::sync::Arc;

use sisa_caching_core::{cache::CacheStore, metrics::METRICS};
use tracing::Instrument;

use crate::{
    backplane::{Backplane, BackplaneMessage, BackplaneSubscription},
    settings::BackplaneWorkerSettings,
    store::state::State,
};

/// Subscribes to one hybrid store's backplane and applies other nodes' invalidations to its local
/// layer. The store starts it once. It holds the store's state but not its `Worker`, so it stops
/// when the store's last clone drops. The store reads through to the distributed store until this
/// worker has subscribed, and again whenever the subscription is lost, until it subscribes again.
pub(crate) struct BackplaneWorker<B: Backplane> {
    state: Arc<State<B>>,
    settings: BackplaneWorkerSettings,
}

impl<B: Backplane> BackplaneWorker<B> {
    pub(crate) fn new(state: Arc<State<B>>, settings: BackplaneWorkerSettings) -> Self {
        Self { state, settings }
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
        let mut delay = self.settings.reconnect_delay();

        // 0 until the first subscribe, so a backplane that never connects still reports.
        METRICS.backplane_connected(store, false);

        loop {
            match state.backplane.subscribe().await {
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

                    // Only a subscription that stayed up starts the backoff over. One that breaks
                    // right away keeps doubling the wait, so it can't resubscribe and clear the
                    // local layer in a tight loop.
                    if subscribed_at.elapsed() >= self.settings.max_reconnect_delay() {
                        delay = self.settings.reconnect_delay();
                    }
                }
                Err(error) => {
                    tracing::warn!(%store, %error, "could not subscribe to the backplane; retrying");
                }
            }

            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(self.settings.max_reconnect_delay());
        }
    }
}

/// A stopped worker can't apply invalidations, so the store must stop trusting its local layer.
/// `run` owns the worker, so this runs however it ends: the store's last clone dropping, its
/// runtime shutting down, or a panic. A store runs one worker, so no other worker's reads are
/// turned off.
impl<B: Backplane> Drop for BackplaneWorker<B> {
    fn drop(&mut self) {
        self.state.disable_local_reads();
        METRICS.backplane_connected(self.state.local_store.name(), false);
    }
}

#[cfg(test)]
mod tests {
    use sisa_caching_memory::store::InMemoryCacheStore;
    use sisa_caching_testing::eventually;

    use super::*;

    /// A backplane whose subscriptions stay up and never deliver.
    struct Quiet;

    struct Silent;

    impl BackplaneSubscription for Silent {
        type Error = std::io::Error;

        async fn next(&mut self) -> std::io::Result<BackplaneMessage> {
            std::future::pending().await
        }
    }

    impl Backplane for Quiet {
        type Error = std::io::Error;
        type Subscription = Silent;

        async fn publish(&self, _: &BackplaneMessage) -> std::io::Result<()> {
            Ok(())
        }

        async fn subscribe(&self) -> std::io::Result<Silent> {
            Ok(Silent)
        }
    }

    #[tokio::test]
    async fn local_reads_go_off_when_the_worker_stops() {
        let state = Arc::new(State::new(InMemoryCacheStore::new("users").unwrap(), Quiet));
        let worker = BackplaneWorker::new(Arc::clone(&state), BackplaneWorkerSettings::default());
        let running = tokio::spawn(worker.run());

        eventually("the worker subscribes", || async {
            state.local_reads_enabled()
        })
        .await;
        running.abort();
        _ = running.await;

        assert!(!state.local_reads_enabled());
    }
}
