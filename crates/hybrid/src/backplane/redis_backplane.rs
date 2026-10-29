use std::{io::ErrorKind, sync::Arc};

use ::redis::{
    AsyncConnectionConfig, Client, ConnectionAddr, IntoConnectionInfo, ProtocolVersion, PushInfo,
    PushKind, RedisError, RedisResult, Value,
    aio::{ConnectionManager, MultiplexedConnection},
    cluster::{ClusterClient, ClusterConfig},
    cluster_async::ClusterConnection,
    cluster_routing::{Route, RoutingInfo, SingleNodeRoutingInfo, SlotAddr},
    from_redis_value,
};
use tokio::{
    sync::broadcast::{self, Receiver, error::RecvError},
    time::{Instant, Interval, MissedTickBehavior},
};

use sisa_caching_core::cache::KeyPrefix;
use sisa_caching_redis::{
    connection::{SentinelConnection, connection_config},
    error::{RedisCacheError, Result},
};

use crate::settings::RedisBackplaneSettings;

use super::BackplaneMessage;

/// How many unread messages a subscription holds. The connection keeps reading, so Redis's limit
/// on slow subscribers never applies. A worker that falls further behind loses its subscription
/// instead, so it clears its local layer and subscribes again, rather than the messages queueing
/// without a bound or getting lost without a sign.
const QUEUE_LIMIT: usize = 8192;

/// The channel every store shared before 0.3. Each node also publishes there for one release, so
/// that 0.2 nodes still get its invalidations and heartbeats during a rolling upgrade.
// TODO(0.4): stop publishing on it.
const SHARED_CHANNEL: &str = "sisa-caching:backplane";

/// A backplane over Redis pub/sub. Each hybrid store has its own channel, `<store>:backplane`, such
/// as `catalog:backplane`, so a node gets only its own stores' messages. Publishing shares one
/// connection. Each subscription opens its own connection, on RESP3, and holds up to 8,192
/// messages its worker hasn't read: a worker that falls further behind loses its subscription, so
/// the node clears its local layer and subscribes again.
///
/// Redis delivers a message to subscribers on every database, so two deployments that share a
/// server need different store names, such as `prod:catalog` and `staging:catalog`, even on
/// separate databases.
///
/// On a cluster, use [`cluster`](Self::cluster), or [`connect`](Self::connect) to any one node:
/// classic pub/sub reaches every node. While that node is down, subscribing fails and the hybrid
/// store reads from Redis.
#[derive(Clone)]
pub struct RedisBackplane {
    server: Server,
    pub(crate) settings: RedisBackplaneSettings,
    /// The store's channel. [`for_store`](Self::for_store) sets it when the store is built.
    channel: Arc<str>,
}

/// The server the backplane publishes and subscribes on.
#[derive(Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "one per backplane, and which variant is larger depends on redis-rs's features"
)]
enum Server {
    /// One server, with or without replicas. A node of a cluster also works, with classic pub/sub.
    Standalone {
        client: Client,
        conn: ConnectionManager,
    },
    /// The primary that Sentinel names when the backplane publishes or subscribes.
    Sentinel(SentinelConnection),
    /// A cluster, which routes each command to the node it belongs on.
    Cluster {
        client: ClusterClient,
        conn: ClusterConnection,
    },
}

impl std::fmt::Debug for RedisBackplane {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let server = match self.server {
            Server::Standalone { .. } => "standalone",
            Server::Sentinel(_) => "sentinel",
            Server::Cluster { .. } => "cluster",
        };

        formatter
            .debug_struct("RedisBackplane")
            .field("server", &server)
            .field("channel", &self.channel)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl RedisBackplane {
    /// Opens the publish connection to one server or any node of a cluster, built with
    /// [`connection_config`]. `info` is a URL, such as `redis://127.0.0.1:6379`, or a
    /// `redis::ConnectionInfo`. Each subscription opens its own connection later.
    pub async fn connect(
        info: impl IntoConnectionInfo,
        settings: RedisBackplaneSettings,
    ) -> Result<Self> {
        Self::from_client(Client::open(info)?, settings).await
    }

    /// Like [`connect`](Self::connect), on a client built by hand, such as one from
    /// `redis::Client::build_with_tls` that trusts a private CA or shows a client certificate.
    pub async fn from_client(client: Client, settings: RedisBackplaneSettings) -> Result<Self> {
        let conn = ConnectionManager::new_with_config(client.clone(), connection_config()).await?;

        Ok(Self {
            server: Server::Standalone { client, conn },
            settings,
            channel: Arc::from(""),
        })
    }

    /// Publishes and subscribes on the primary Sentinel names. Publishing shares the store's
    /// connection, which moves after a failover. Each ping also asks Sentinel for the primary and
    /// moves the shared connection there ([`SentinelConnection::primary_client`]). A subscription
    /// on a server that is no longer the primary ends, so the node subscribes again on the new
    /// one. Redis Sentinel closes client connections on a failover itself, but Dragonfly refuses
    /// the command it uses (`CLIENT KILL TYPE`).
    pub fn sentinel(sentinel: SentinelConnection, settings: RedisBackplaneSettings) -> Self {
        Self {
            server: Server::Sentinel(sentinel),
            settings,
            channel: Arc::from(""),
        }
    }

    /// Publishes and subscribes through a cluster client, with sharded pub/sub (`SPUBLISH`,
    /// `SSUBSCRIBE`). Build `client` with `ProtocolVersion::RESP3`: a subscription on a cluster
    /// connection gets its messages as RESP3 pushes.
    ///
    /// The channel lives on the node that owns its slot. Each ping also asks the cluster who owns
    /// it, and a subscription ends once the slot has moved, so the node subscribes again on the
    /// new owner and clears its local layer. Redis and Valkey end it
    /// themselves; Garnet keeps it on the old node, where it gets nothing. Dragonfly's cluster
    /// mode needs sharded pub/sub: it has no classic pub/sub.
    pub async fn cluster(client: ClusterClient, settings: RedisBackplaneSettings) -> Result<Self> {
        let conn = client.get_async_connection().await?;

        Ok(Self {
            server: Server::Cluster { client, conn },
            settings,
            channel: Arc::from(""),
        })
    }
}

impl RedisBackplane {
    /// Moves the backplane to `store`'s channel.
    pub(crate) fn for_store(mut self, store: &KeyPrefix) -> Self {
        self.channel = Arc::from(format!("{store}:backplane"));
        self
    }

    /// Sends the message to all subscribers, including this node's own, and returns how many the
    /// server delivered it to. It also goes to 0.2 nodes on the shared channel, at the same time;
    /// a failure there is left to their gap detection.
    pub(crate) async fn publish(&self, message: &BackplaneMessage) -> Result<usize> {
        let payload = serde_json::to_vec(message)?;
        let (subscribers, shared) = tokio::join!(
            self.publish_on(&self.channel, &payload),
            self.publish_on(SHARED_CHANNEL, &payload),
        );

        Ok(subscribers? + shared.unwrap_or(0))
    }

    async fn publish_on(&self, channel: &str, payload: &[u8]) -> Result<usize> {
        let command = match self.server {
            Server::Cluster { .. } => "SPUBLISH",
            _ => "PUBLISH",
        };
        let publish = ::redis::cmd(command).arg(channel).arg(payload).to_owned();

        let subscribers = match &self.server {
            Server::Standalone { conn, .. } => {
                publish.query_async::<usize>(&mut conn.clone()).await?
            }
            Server::Sentinel(sentinel) => {
                publish.query_async::<usize>(&mut sentinel.clone()).await?
            }
            Server::Cluster { conn, .. } => publish.query_async::<usize>(&mut conn.clone()).await?,
        };

        Ok(subscribers)
    }

    /// Opens a subscription that receives the messages published from now on. It connects and
    /// subscribes within the ping timeout.
    pub(crate) async fn subscribe(&self) -> Result<RedisSubscription> {
        // Messages arrive as RESP3 pushes. A broadcast channel takes them without making the
        // connection wait, keeps the last `QUEUE_LIMIT`, and tells the reader it fell behind.
        let (sender, pushes) = broadcast::channel(QUEUE_LIMIT);
        let connection = tokio::time::timeout(self.settings.ping_timeout(), async move {
            let (client, sentinel) = match &self.server {
                Server::Standalone { client, .. } => (client.clone(), None),
                Server::Sentinel(sentinel) => {
                    let client = sentinel.primary_client().await?;
                    let primary = client.get_connection_info().addr().clone();

                    (client, Some((sentinel.clone(), primary)))
                }
                Server::Cluster { client, .. } => {
                    let mut connection = client
                        .get_async_connection_with_config(
                            ClusterConfig::new().set_push_sender(sender),
                        )
                        .await?;

                    let channel = &*self.channel;
                    let slot = ::redis::cmd("CLUSTER")
                        .arg("KEYSLOT")
                        .arg(channel)
                        .query_async(&mut connection)
                        .await?;
                    let owner = slot_owner(&mut connection, slot).await?;

                    connection.ssubscribe(channel).await?;

                    return Ok(Connection::Cluster {
                        connection,
                        slot,
                        owner,
                    });
                }
            };
            let info = client.get_connection_info().clone();
            let resp3 = info
                .redis_settings()
                .clone()
                .set_protocol(ProtocolVersion::RESP3);
            // The ping timeout bounds subscribing and each ping.
            let config = AsyncConnectionConfig::new()
                .set_connection_timeout(None)
                .set_response_timeout(None)
                .set_push_sender(sender);
            let mut connection = Client::open(info.set_redis_settings(resp3))?
                .get_multiplexed_async_connection_with_config(&config)
                .await?;

            connection.subscribe(&*self.channel).await?;

            Ok::<_, RedisError>(Connection::Server {
                connection,
                sentinel,
            })
        })
        .await
        .map_err(|_| io_error(ErrorKind::TimedOut, "subscribing timed out"))??;

        let mut pings = tokio::time::interval_at(
            Instant::now() + self.settings.ping_interval(),
            self.settings.ping_interval(),
        );

        pings.set_missed_tick_behavior(MissedTickBehavior::Delay);

        Ok(RedisSubscription {
            connection,
            pushes,
            pings,
            settings: self.settings.clone(),
            channel: Arc::clone(&self.channel),
        })
    }
}

/// One subscribed connection. It is pinged while waiting, so a connection that stops delivering
/// without closing ends the subscription too.
pub(crate) struct RedisSubscription {
    connection: Connection,
    /// The connection's pushes, messages among them, up to `QUEUE_LIMIT`.
    pushes: Receiver<PushInfo>,
    pings: Interval,
    settings: RedisBackplaneSettings,
    channel: Arc<str>,
}

#[allow(
    clippy::large_enum_variant,
    reason = "one per subscription, and which variant is larger depends on redis-rs's features"
)]
enum Connection {
    /// A connection to one server.
    Server {
        connection: MultiplexedConnection,
        /// Behind Sentinel: where to ask for the primary, and the primary this subscribed on.
        sentinel: Option<(SentinelConnection, ConnectionAddr)>,
    },
    /// A cluster connection.
    Cluster {
        connection: ClusterConnection,
        /// The channel's slot, and the node that owned it when subscribing.
        slot: u16,
        owner: Option<Value>,
    },
}

/// The next message's payload.
async fn message(pushes: &mut Receiver<PushInfo>) -> Result<Vec<u8>> {
    let closed = || io_error(ErrorKind::ConnectionAborted, "connection closed");

    loop {
        let push = match pushes.recv().await {
            Ok(push) => push,
            Err(RecvError::Lagged(_)) => {
                return Err(io_error(ErrorKind::Other, "the worker fell too far behind"));
            }
            Err(RecvError::Closed) => return Err(closed()),
        };

        match (push.kind, push.data.into_iter().nth(1)) {
            // Garnet hands a sharded subscription's messages over as `message`.
            (PushKind::Message | PushKind::SMessage, Some(Value::BulkString(payload))) => {
                return Ok(payload);
            }
            // A cluster connection subscribes again by itself, but messages sent in the gap are
            // lost. On Redis, a slot that moves unsubscribes its channel.
            (PushKind::Disconnection | PushKind::SUnsubscribe, _) => return Err(closed()),
            _ => {}
        }
    }
}

impl Connection {
    /// Pings the connection, and fails once the subscription is on the wrong node: behind
    /// Sentinel, once the primary has moved (the shared Sentinel connection moves too, so the
    /// store stops reading from the demoted primary); on a cluster, once the slot has moved.
    async fn ping(&mut self, channel: &str) -> Result<()> {
        match self {
            Self::Server {
                connection,
                sentinel,
            } => {
                ::redis::cmd("PING")
                    .query_async::<Value>(connection)
                    .await?;

                if let Some((sentinel, subscribed)) = sentinel
                    && sentinel
                        .primary_client()
                        .await?
                        .get_connection_info()
                        .addr()
                        != subscribed
                {
                    return Err(io_error(ErrorKind::ConnectionReset, "the primary moved"));
                }
            }
            // The node holding the subscription is the one that owns the channel's slot.
            Self::Cluster {
                connection,
                slot,
                owner,
                ..
            } => {
                let node = Route::with_key(channel, SlotAddr::Master);
                connection
                    .route_command(
                        ::redis::cmd("PING"),
                        RoutingInfo::SingleNode(SingleNodeRoutingInfo::SpecificNode(node)),
                    )
                    .await?;

                // Garnet keeps a sharded subscription on the old node after its slot moves,
                // where it gets nothing, so the subscription ends instead.
                if slot_owner(connection, *slot).await? != *owner {
                    return Err(io_error(ErrorKind::ConnectionReset, "the slot moved"));
                }
            }
        }

        Ok(())
    }
}

/// The primary that owns `slot`, as `CLUSTER SLOTS` names it.
async fn slot_owner(connection: &mut ClusterConnection, slot: u16) -> RedisResult<Option<Value>> {
    // One row per slot range: [start, end, primary, replicas...].
    let ranges: Vec<Vec<Value>> = ::redis::cmd("CLUSTER")
        .arg("SLOTS")
        .query_async(connection)
        .await?;

    Ok(ranges.into_iter().find_map(|range| {
        let mut range = range.into_iter();
        let first: u16 = from_redis_value(range.next()?).ok()?;
        let last: u16 = from_redis_value(range.next()?).ok()?;

        (first..=last)
            .contains(&slot)
            .then(|| range.next())
            .flatten()
    }))
}

impl RedisSubscription {
    /// The next message's payload, in order, with none missing. An error means messages may have
    /// been lost and this subscription is done; open a new one. Dropping an unfinished call loses
    /// no message.
    pub(crate) async fn next(&mut self) -> Result<Vec<u8>> {
        loop {
            tokio::select! {
                message = message(&mut self.pushes) => return message,
                _ = self.pings.tick() => {
                    tokio::time::timeout(self.settings.ping_timeout(), self.connection.ping(&self.channel))
                        .await
                        .map_err(|_| io_error(ErrorKind::TimedOut, "ping timed out"))??;
                }
            }
        }
    }
}

fn io_error(kind: ErrorKind, reason: &'static str) -> RedisCacheError {
    RedisError::from(std::io::Error::new(kind, reason)).into()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sisa_caching_core::cache::KeyPrefix;
    use sisa_caching_testing::{Redis, RedisCluster};

    use super::*;
    use crate::backplane::{Invalidation, NodeId};

    /// The channel of the `users` store, which the test messages belong to.
    const USERS_CHANNEL: &str = "users:backplane";

    fn users() -> KeyPrefix {
        KeyPrefix::new("users").unwrap()
    }

    async fn connect(redis: &Redis, settings: RedisBackplaneSettings) -> RedisBackplane {
        RedisBackplane::connect(redis.url(), settings)
            .await
            .unwrap()
            .for_store(&users())
    }

    /// Pings every 50 ms and gives up after 100 ms, so a stall shows quickly.
    fn quick_pings() -> RedisBackplaneSettings {
        RedisBackplaneSettings {
            ping_interval_ms: 50,
            ping_timeout_ms: 100,
        }
    }

    fn message(invalidation: Invalidation) -> BackplaneMessage {
        BackplaneMessage::new(NodeId::new("node-a"), users(), invalidation)
    }

    async fn next(subscription: &mut RedisSubscription) -> BackplaneMessage {
        let payload = tokio::time::timeout(Duration::from_secs(5), subscription.next())
            .await
            .expect("a message arrives")
            .unwrap();

        serde_json::from_slice(&payload).unwrap()
    }

    #[tokio::test]
    async fn every_subscription_receives_the_messages_of_its_store() {
        let redis = Redis::start().await;
        let backplane = connect(&redis, RedisBackplaneSettings::default()).await;
        let elsewhere = connect(&redis, RedisBackplaneSettings::default())
            .await
            .for_store(&KeyPrefix::new("orders").unwrap());
        let mut first = backplane.subscribe().await.unwrap();
        let mut second = backplane.subscribe().await.unwrap();
        let sent = [
            message(Invalidation::Key("user:1".to_owned())),
            message(Invalidation::Prefix(KeyPrefix::new("user").unwrap())),
            message(Invalidation::Clear),
        ];

        elsewhere
            .publish(&message(Invalidation::Clear))
            .await
            .unwrap();
        for message in &sent {
            backplane.publish(message).await.unwrap();
        }

        for subscription in [&mut first, &mut second] {
            for message in &sent {
                assert_eq!(&next(subscription).await, message);
            }
        }
    }

    #[tokio::test]
    async fn a_message_also_reaches_nodes_on_the_shared_channel() {
        let redis = Redis::start().await;
        let backplane = connect(&redis, RedisBackplaneSettings::default()).await;
        let mut subscription = backplane.subscribe().await.unwrap();
        // A 0.2 node, subscribed to the channel every store used to share.
        let (sender, mut pushes) = tokio::sync::mpsc::unbounded_channel();
        let info = Client::open(redis.url())
            .unwrap()
            .get_connection_info()
            .clone();
        let resp3 = info
            .redis_settings()
            .clone()
            .set_protocol(ProtocolVersion::RESP3);
        let config = AsyncConnectionConfig::new().set_push_sender(sender);
        let mut legacy = Client::open(info.set_redis_settings(resp3))
            .unwrap()
            .get_multiplexed_async_connection_with_config(&config)
            .await
            .unwrap();
        let sent = message(Invalidation::Clear);

        legacy.subscribe(SHARED_CHANNEL).await.unwrap();
        let subscribers = backplane.publish(&sent).await.unwrap();

        assert_eq!(subscribers, 2);
        assert_eq!(next(&mut subscription).await, sent);
        let push = loop {
            let push = tokio::time::timeout(Duration::from_secs(5), pushes.recv())
                .await
                .expect("the shared channel gets the message")
                .unwrap();
            if push.kind == PushKind::Message {
                break push;
            }
        };
        let payload: Vec<u8> = from_redis_value(push.data[1].clone()).unwrap();
        assert_eq!(
            serde_json::from_slice::<BackplaneMessage>(&payload).unwrap(),
            sent
        );
    }

    #[tokio::test]
    async fn a_subscription_that_falls_too_far_behind_ends() {
        let redis = Redis::start().await;
        let backplane = connect(&redis, RedisBackplaneSettings::default()).await;
        let mut subscription = backplane.subscribe().await.unwrap();
        let payload = serde_json::to_vec(&message(Invalidation::Clear)).unwrap();
        let mut connection = redis.connection().await;
        let mut publish = redis::pipe();

        for _ in 0..1_000 {
            publish
                .cmd("PUBLISH")
                .arg(USERS_CHANNEL)
                .arg(&payload)
                .ignore();
        }
        // More than the 8,192 messages a subscription holds, all read off the connection before
        // anything reads the subscription. Each batch answers well within the connection's 500 ms
        // response timeout, even on a slow server.
        for _ in 0..20 {
            publish.query_async::<()>(&mut connection).await.unwrap();
        }
        tokio::time::sleep(Duration::from_secs(1)).await;

        let error = tokio::time::timeout(Duration::from_secs(5), subscription.next())
            .await
            .expect("the subscription ends")
            .unwrap_err();

        assert!(error.to_string().contains("fell too far behind"), "{error}");
    }

    #[tokio::test]
    async fn subscribing_to_a_server_that_does_not_answer_times_out() {
        let redis = Redis::start().await;
        let backplane = connect(&redis, quick_pings()).await;

        redis.pause().await;

        let attempt = tokio::time::timeout(Duration::from_secs(2), backplane.subscribe()).await;

        redis.unpause().await;
        assert!(attempt.expect("the ping timeout ends the attempt").is_err());
    }

    #[tokio::test]
    async fn a_sharded_subscription_ends_only_when_its_channel_slot_moves() {
        let cluster = RedisCluster::start().await;
        let backplane = RedisBackplane::cluster(
            cluster.client(),
            RedisBackplaneSettings {
                ping_interval_ms: 100,
                ..RedisBackplaneSettings::default()
            },
        )
        .await
        .unwrap()
        .for_store(&users());
        let message = message(Invalidation::Clear);
        let mut subscription = backplane.subscribe().await.unwrap();

        // Waiting through a few pings while the slot stays put, the subscription still delivers.
        let (_, next) = tokio::join!(
            async {
                tokio::time::sleep(Duration::from_millis(350)).await;
                backplane.publish(&message).await.unwrap();
            },
            tokio::time::timeout(Duration::from_secs(5), subscription.next()),
        );

        next.expect("the message arrives").unwrap();

        cluster.move_slot(USERS_CHANNEL).await;
        backplane.publish(&message).await.unwrap();

        // Delivering the message is fine too; waiting on a node that no longer gets it is not.
        let next = tokio::time::timeout(Duration::from_secs(5), subscription.next())
            .await
            .expect("the subscription delivers the message or ends");
        if next.is_err() {
            let mut subscription = backplane.subscribe().await.unwrap();

            backplane.publish(&message).await.unwrap();
            tokio::time::timeout(Duration::from_secs(5), subscription.next())
                .await
                .expect("a new subscription gets the message")
                .unwrap();
        }
    }

    #[tokio::test]
    async fn a_sharded_subscription_that_falls_too_far_behind_ends() {
        let cluster = RedisCluster::start().await;
        let backplane =
            RedisBackplane::cluster(cluster.client(), RedisBackplaneSettings::default())
                .await
                .unwrap()
                .for_store(&users());
        let mut subscription = backplane.subscribe().await.unwrap();
        let payload = serde_json::to_vec(&message(Invalidation::Clear)).unwrap();
        let mut publish = redis::pipe();

        // More than the 8,192 messages a subscription holds, all read off the connection before
        // anything reads the subscription.
        for _ in 0..20_000 {
            publish
                .cmd("SPUBLISH")
                .arg(USERS_CHANNEL)
                .arg(&payload)
                .ignore();
        }
        publish
            .query_async::<()>(&mut cluster.connection().await)
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_secs(1)).await;

        let error = tokio::time::timeout(Duration::from_secs(5), subscription.next())
            .await
            .expect("the subscription ends")
            .unwrap_err();

        assert!(error.to_string().contains("fell too far behind"), "{error}");
    }
}
