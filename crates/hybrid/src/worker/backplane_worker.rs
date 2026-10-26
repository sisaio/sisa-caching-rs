use std::sync::Arc;

use sisa_caching_core::{
    cache::CacheStore,
    metrics::METRICS,
    worker::{CancellationToken, WorkerService},
};
use tracing::Instrument;

use crate::{
    backplane::{Backplane, BackplaneMessage, BackplaneSubscription},
    settings::BackplaneWorkerSettings,
    store::{HybridCacheStore, state::State},
};

/// Subscribes to one hybrid store's backplane and applies other nodes' invalidations to its local
/// layer. The store reads through to the distributed store until this worker has subscribed, and
/// again whenever the subscription is lost, until it subscribes again.
pub struct BackplaneWorker<B: Backplane> {
    state: Arc<State<B>>,
    settings: BackplaneWorkerSettings,
    /// Set when `run` starts, so dropping a worker that never ran leaves the store's reads alone.
    running: bool,
}

impl<B: Backplane> std::fmt::Debug for BackplaneWorker<B> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BackplaneWorker")
            .field("store", self.state.local_store.name())
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl<B: Backplane> BackplaneWorker<B> {
    /// A worker for `store`. Register it on a `WorkerHost`.
    pub fn new<D: CacheStore + 'static>(store: &HybridCacheStore<D, B>) -> Self {
        Self {
            state: Arc::clone(store.state()),
            settings: BackplaneWorkerSettings::default(),
            running: false,
        }
    }

    /// How long the worker waits before subscribing again.
    pub fn with_settings(mut self, settings: BackplaneWorkerSettings) -> Self {
        self.settings = settings;
        self
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
}

impl<B: Backplane> WorkerService for BackplaneWorker<B> {
    fn name(&self) -> &'static str {
        "hybrid-cache-backplane-worker"
    }

    async fn run(mut self, _shutdown: CancellationToken) {
        self.running = true;

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
/// `run` owns the worker, so this runs however it ends: shutdown, a dropped host, or a panic.
impl<B: Backplane> Drop for BackplaneWorker<B> {
    fn drop(&mut self) {
        if self.running {
            self.state.disable_local_reads();
            METRICS.backplane_connected(self.state.local_store.name(), false);
        }
    }
}
