use std::io::ErrorKind;

use ::redis::{
    Client, ConnectionAddr, IntoConnectionInfo, PushInfo, PushKind, RedisError, RedisResult, Value,
    aio::{ConnectionManager, PubSubSink, PubSubStream},
    cluster::{ClusterClient, ClusterConfig},
    cluster_async::ClusterConnection,
    cluster_routing::{Route, RoutingInfo, SingleNodeRoutingInfo, SlotAddr},
    from_redis_value,
};
use futures_util::StreamExt;
use tokio::{
    sync::mpsc::{UnboundedReceiver, unbounded_channel},
    time::{Instant, Interval, MissedTickBehavior},
};

use sisa_caching_redis::{
    connection::{SentinelConnection, connection_config},
    error::{RedisCacheError, Result},
};

use crate::settings::RedisBackplaneSettings;

use super::{Backplane, BackplaneMessage, BackplaneSubscription};

/// A backplane over Redis pub/sub on one channel. Publishing shares one connection; each
/// subscription opens its own pub/sub connection.
///
/// On a cluster, use [`cluster`](Self::cluster), or [`connect`](Self::connect) to any one node:
/// classic pub/sub reaches every node. While that node is down, subscribing fails and the hybrid
/// store reads from Redis.
#[derive(Clone)]
pub struct RedisBackplane {
    server: Server,
    settings: RedisBackplaneSettings,
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
        })
    }
}

impl Backplane for RedisBackplane {
    type Error = RedisCacheError;
    type Subscription = RedisSubscription;

    async fn publish(&self, message: &BackplaneMessage) -> Result<()> {
        let command = match self.server {
            Server::Cluster { .. } => "SPUBLISH",
            _ => "PUBLISH",
        };
        let publish = ::redis::cmd(command)
            .arg(&self.settings.channel)
            .arg(serde_json::to_vec(message)?)
            .to_owned();

        match &self.server {
            Server::Standalone { conn, .. } => {
                publish.query_async::<usize>(&mut conn.clone()).await?
            }
            Server::Sentinel(sentinel) => {
                publish.query_async::<usize>(&mut sentinel.clone()).await?
            }
            Server::Cluster { conn, .. } => publish.query_async::<usize>(&mut conn.clone()).await?,
        };

        Ok(())
    }

    /// Connects and subscribes within the ping timeout.
    async fn subscribe(&self) -> Result<RedisSubscription> {
        let connection = tokio::time::timeout(self.settings.ping_timeout(), async {
            let (client, sentinel) = match &self.server {
                Server::Standalone { client, .. } => (client.clone(), None),
                Server::Sentinel(sentinel) => {
                    let client = sentinel.primary_client().await?;
                    let primary = client.get_connection_info().addr().clone();

                    (client, Some((sentinel.clone(), primary)))
                }
                Server::Cluster { client, .. } => {
                    let (sender, pushes) = unbounded_channel();
                    let mut connection = client
                        .get_async_connection_with_config(
                            ClusterConfig::new().set_push_sender(sender),
                        )
                        .await?;

                    let channel = &self.settings.channel;
                    let slot = ::redis::cmd("CLUSTER")
                        .arg("KEYSLOT")
                        .arg(channel)
                        .query_async(&mut connection)
                        .await?;
                    let owner = slot_owner(&mut connection, slot).await?;

                    connection.ssubscribe(channel).await?;

                    return Ok(Connection::Cluster {
                        connection,
                        pushes,
                        slot,
                        owner,
                    });
                }
            };
            let (mut sink, stream) = client.get_async_pubsub().await?.split();

            sink.subscribe(&self.settings.channel).await?;

            Ok::<_, RedisError>(Connection::PubSub {
                sink,
                stream,
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
            pings,
            settings: self.settings.clone(),
        })
    }
}

/// One pub/sub connection. It is pinged while waiting, so a connection that stops delivering
/// without closing ends the subscription too.
pub struct RedisSubscription {
    connection: Connection,
    pings: Interval,
    settings: RedisBackplaneSettings,
}

#[allow(
    clippy::large_enum_variant,
    reason = "one per subscription, and which variant is larger depends on redis-rs's features"
)]
enum Connection {
    /// A pub/sub connection to one server.
    PubSub {
        sink: PubSubSink,
        stream: PubSubStream,
        /// Behind Sentinel: where to ask for the primary, and the primary this subscribed on.
        sentinel: Option<(SentinelConnection, ConnectionAddr)>,
    },
    /// A cluster connection, which hands messages over as RESP3 pushes.
    Cluster {
        connection: ClusterConnection,
        pushes: UnboundedReceiver<PushInfo>,
        /// The channel's slot, and the node that owned it when subscribing.
        slot: u16,
        owner: Option<Value>,
    },
}

impl Connection {
    /// The next message's payload.
    async fn message(&mut self) -> Result<Vec<u8>> {
        let closed = || io_error(ErrorKind::ConnectionAborted, "connection closed");

        match self {
            Self::PubSub { stream, .. } => {
                let message = stream.next().await.ok_or_else(closed)?;

                Ok(message.get_payload_bytes().to_vec())
            }
            Self::Cluster { pushes, .. } => loop {
                let push = pushes.recv().await.ok_or_else(closed)?;

                match (push.kind, push.data.into_iter().nth(1)) {
                    // Garnet hands a sharded subscription's messages over as `message`.
                    (PushKind::SMessage | PushKind::Message, Some(Value::BulkString(payload))) => {
                        return Ok(payload);
                    }
                    // redis-rs subscribes again by itself, but messages sent in the gap are lost.
                    // On Redis, a slot that moves unsubscribes its channel.
                    (PushKind::Disconnection | PushKind::SUnsubscribe, _) => {
                        return Err(closed());
                    }
                    _ => {}
                }
            },
        }
    }

    /// Pings the connection, and fails once the subscription is on the wrong node: behind
    /// Sentinel, once the primary has moved (the shared Sentinel connection moves too, so the
    /// store stops reading from the demoted primary); on a cluster, once the slot has moved.
    async fn ping(&mut self, channel: &str) -> Result<()> {
        match self {
            Self::PubSub { sink, sentinel, .. } => {
                sink.ping::<Value>().await?;

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

impl std::fmt::Debug for RedisSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RedisSubscription")
            .field("channel", &self.settings.channel)
            .finish_non_exhaustive()
    }
}

impl BackplaneSubscription for RedisSubscription {
    type Error = RedisCacheError;

    async fn next(&mut self) -> Result<BackplaneMessage> {
        loop {
            tokio::select! {
                message = self.connection.message() => {
                    return Ok(serde_json::from_slice(&message?)?);
                }
                _ = self.pings.tick() => {
                    tokio::time::timeout(self.settings.ping_timeout(), self.connection.ping(&self.settings.channel))
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
