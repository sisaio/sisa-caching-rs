use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
    time::Duration,
};

use sisa_caching_core::{cache::CacheStore, metrics::METRICS};
use tokio::{
    task::JoinSet,
    time::{Instant, MissedTickBehavior},
};
use tracing::Instrument;

use crate::{
    backplane::{BackplaneMessage, Invalidation, NodeId, RedisBackplane},
    settings::BackplaneWorkerSettings,
    store::state::State,
};

/// Subscribes to one hybrid store's backplane and applies other nodes' invalidations to its local
/// layer. The store starts it once. It holds the store's state but not its `Worker`, so it stops
/// when the store's last clone drops. The store reads through to the distributed store until this
/// worker has subscribed, and again whenever the subscription is lost, until it subscribes again.
///
/// The worker also publishes a heartbeat every `heartbeat_interval_ms`. It clears the local layer
/// when a message can't be read, when another node's heartbeat shows a sequence number this node
/// never received, or when that node's heartbeats stop for `heartbeat_timeout_ms`. When its own
/// heartbeats stop arriving, it treats the subscription as lost.
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

    /// Handles one payload: applies another node's invalidation of this store and tracks that
    /// node's sequence numbers, and clears the local layer when the payload can't be read or a
    /// number went missing.
    async fn receive(&self, payload: &[u8], peers: &mut Peers) {
        let store = self.state.local_store.name();
        let message: BackplaneMessage = match serde_json::from_slice(payload) {
            Ok(message) => message,
            Err(error) => {
                METRICS.backplane_unreadable(store);
                tracing::warn!(%store, %error, "could not read a backplane message; clearing the local layer");
                return self.state.resync(true).await;
            }
        };

        if &message.store != store {
            return;
        }
        if message.source == self.state.node_id {
            peers.own_heard = Instant::now();
            return;
        }
        if message.invalidation != Invalidation::Heartbeat {
            self.apply(&message).await;
        }

        let Some(gap) = peers.receive(&message) else {
            return;
        };

        match gap {
            Gap::Missing => {
                tracing::warn!(%store, node = %message.source, "an invalidation never arrived; clearing the local layer");
            }
            Gap::Unknown => {
                tracing::debug!(%store, node = %message.source, "a node's first heartbeat counts invalidations this subscription never got; clearing the local layer");
            }
        }
        self.state.resync(true).await;
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
            .apply(&message.invalidation)
            .instrument(span)
            .await;
    }

    pub(crate) async fn run(self) {
        let state = &self.state;
        let store = state.local_store.name();
        let timeout = self.settings.heartbeat_timeout();
        let mut backoff = Backoff::new(&self.settings);
        // Sent while unsubscribed too, so other nodes don't take this one for gone. Dropping the
        // set stops them.
        let mut heartbeats = JoinSet::new();

        heartbeats.spawn(send_heartbeats(
            Arc::clone(state),
            Arc::clone(&self.backplane),
            self.settings.heartbeat_interval(),
        ));

        // 0 until the first subscribe, so a backplane that never connects still reports.
        METRICS.backplane_connected(store, false);

        loop {
            match self.backplane.subscribe().await {
                Ok(mut subscription) => {
                    let subscribed_at = Instant::now();

                    // Copies made before this subscription may have missed invalidations.
                    state.resync(true).await;
                    METRICS.backplane_connected(store, true);
                    tracing::info!(%store, "backplane subscribed; local reads on");

                    let mut peers = Peers::new();
                    let mut checks = tokio::time::interval(self.settings.heartbeat_interval());

                    checks.set_missed_tick_behavior(MissedTickBehavior::Delay);

                    let error = loop {
                        tokio::select! {
                            payload = subscription.next() => match payload {
                                Ok(payload) => self.receive(&payload, &mut peers).await,
                                Err(error) => break error.to_string(),
                            },
                            _ = checks.tick() => {
                                if peers.own_heard.elapsed() >= timeout {
                                    break "this node's own heartbeats stopped arriving".to_owned();
                                }

                                let gone = peers.expire(timeout);

                                if !gone.is_empty() {
                                    tracing::warn!(%store, nodes = ?gone, "heartbeats stopped; clearing the local layer");
                                    state.resync(true).await;
                                }
                            }
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

/// Publishes a heartbeat every `interval`, one at a time, so they arrive in order. The first goes
/// out after one interval.
async fn send_heartbeats(state: Arc<State>, backplane: Arc<RedisBackplane>, interval: Duration) {
    let store = state.local_store.name();
    let mut ticks = tokio::time::interval_at(Instant::now() + interval, interval);

    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        ticks.tick().await;

        let heartbeat = BackplaneMessage::heartbeat(
            state.node_id.clone(),
            store.clone(),
            state.sequence.finished(),
        );

        if let Err(error) = backplane.publish(&heartbeat).await {
            tracing::debug!(%store, %error, "could not publish a heartbeat");
        }
    }
}

/// What one subscription has heard from the other nodes of its store. A node is tracked from the
/// first message this subscription gets from it. Nothing says which of its earlier numbers were
/// sent before the subscribe, so its first heartbeat clears the local layer unless every number up
/// to it arrived. That covers an invalidation lost between the subscribe and that heartbeat, as
/// during a failover, at the cost of one more clear for each node that published before.
struct Peers {
    /// When this node last heard itself.
    own_heard: Instant,
    nodes: HashMap<NodeId, Peer>,
}

struct Peer {
    /// The `seq` of its last heartbeat: every number up to it was received or is accounted for.
    /// `None` until its first heartbeat.
    finished: Option<u64>,
    /// The numbers received above `finished`.
    received: BTreeSet<u64>,
    /// When its last heartbeat arrived, or its first message before that.
    heard: Instant,
}

/// What a heartbeat shows that clears the local layer.
#[derive(Debug, PartialEq)]
enum Gap {
    /// A number it counts never arrived.
    Missing,
    /// It is the node's first, and counts numbers this subscription never got: sent before the
    /// subscribe, or lost since.
    Unknown,
}

impl Peers {
    fn new() -> Self {
        Self {
            own_heard: Instant::now(),
            nodes: HashMap::new(),
        }
    }

    /// Records another node's message, and returns the gap it shows: a heartbeat that counts a
    /// number this subscription never got. A message without a `seq` is not tracked.
    fn receive(&mut self, message: &BackplaneMessage) -> Option<Gap> {
        let seq = message.seq?;
        let peer = self
            .nodes
            .entry(message.source.clone())
            .or_insert_with(|| Peer {
                finished: None,
                received: BTreeSet::new(),
                heard: Instant::now(),
            });

        if message.invalidation != Invalidation::Heartbeat {
            if peer.finished.is_none_or(|finished| seq > finished) {
                peer.received.insert(seq);
            }

            return None;
        }

        peer.heard = Instant::now();

        let finished = match peer.finished {
            Some(finished) if seq <= finished => return None,
            finished => finished,
        };
        let above = peer.received.split_off(&(seq + 1));
        let received = std::mem::replace(&mut peer.received, above).len() as u64;

        peer.finished = Some(seq);

        if received == seq - finished.unwrap_or(0) {
            return None;
        }

        Some(if finished.is_some() {
            Gap::Missing
        } else {
            Gap::Unknown
        })
    }

    /// Forgets the nodes whose heartbeats stopped for `timeout`, and returns them.
    fn expire(&mut self, timeout: Duration) -> Vec<NodeId> {
        let mut gone = Vec::new();

        self.nodes.retain(|node, peer| {
            let alive = peer.heard.elapsed() < timeout;

            if !alive {
                gone.push(node.clone());
            }

            alive
        });

        gone
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
    use sisa_caching_core::cache::KeyPrefix;

    use super::*;

    /// A 1 ms first wait and a 50 ms cap.
    fn settings() -> BackplaneWorkerSettings {
        BackplaneWorkerSettings {
            reconnect_delay_ms: 1,
            max_reconnect_delay_ms: 50,
            ..BackplaneWorkerSettings::default()
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

    fn from_peer(seq: u64, invalidation: Invalidation) -> BackplaneMessage {
        let mut message = BackplaneMessage::new(
            NodeId::new("peer"),
            KeyPrefix::new("users").unwrap(),
            invalidation,
        );

        message.seq = Some(seq);
        message
    }

    fn heartbeat(seq: u64) -> BackplaneMessage {
        BackplaneMessage::heartbeat(NodeId::new("peer"), KeyPrefix::new("users").unwrap(), seq)
    }

    fn key(seq: u64) -> BackplaneMessage {
        from_peer(seq, Invalidation::Key(seq.to_string()))
    }

    #[test]
    fn numbers_in_any_order_and_repeats_are_no_gap() {
        let mut peers = Peers::new();

        assert_eq!(peers.receive(&heartbeat(0)), None);

        for message in [key(2), key(1), key(2), key(4)] {
            assert_eq!(peers.receive(&message), None);
        }

        assert_eq!(peers.receive(&heartbeat(2)), None);
        assert_eq!(
            peers.receive(&heartbeat(1)),
            None,
            "an older heartbeat changes nothing"
        );
        assert_eq!(peers.receive(&key(3)), None);
        assert_eq!(peers.receive(&heartbeat(4)), None);
    }

    #[test]
    fn a_number_missing_below_a_heartbeat_is_a_gap() {
        let mut peers = Peers::new();

        peers.receive(&heartbeat(0));
        peers.receive(&key(1));
        peers.receive(&key(3));

        assert_eq!(peers.receive(&heartbeat(3)), Some(Gap::Missing));
        assert_eq!(peers.receive(&key(4)), None);
        assert_eq!(
            peers.receive(&heartbeat(4)),
            None,
            "the gap is reported once"
        );
    }

    #[test]
    fn a_node_is_checked_from_its_first_number() {
        let mut started_after = Peers::new();

        started_after.receive(&key(1));
        started_after.receive(&key(2));
        assert_eq!(started_after.receive(&heartbeat(2)), None);

        let mut started_before = Peers::new();

        started_before.receive(&key(5));
        assert_eq!(
            started_before.receive(&heartbeat(5)),
            Some(Gap::Unknown),
            "1 to 4 came before the subscribe, or were lost since"
        );
        assert_eq!(started_before.receive(&heartbeat(6)), Some(Gap::Missing));
    }

    #[test]
    fn a_node_heard_again_after_it_expired_is_checked_from_its_first_number() {
        let mut peers = Peers::new();

        peers.receive(&heartbeat(0));
        assert_eq!(peers.expire(Duration::ZERO), [NodeId::new("peer")]);
        assert_eq!(
            peers.receive(&heartbeat(1)),
            Some(Gap::Unknown),
            "1 was sent while it was gone"
        );
    }

    #[test]
    fn messages_without_a_sequence_number_are_not_tracked() {
        let mut peers = Peers::new();
        let mut unnumbered = key(1);

        unnumbered.seq = None;
        peers.receive(&heartbeat(0));
        peers.receive(&unnumbered);

        assert_eq!(
            peers.receive(&heartbeat(1)),
            Some(Gap::Missing),
            "a message without a number fills none"
        );
    }
}
