//! The connections a Redis store runs on: one server, a cluster, or a primary behind Sentinel.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use redis::{
    Client, Cmd, Pipeline, RedisError, RedisFuture, RedisResult, ServerErrorKind, Value,
    aio::{ConnectionLike, ConnectionManager, ConnectionManagerConfig, MultiplexedConnection},
    cluster_async::ClusterConnection,
    cluster_routing::{MultipleNodeRoutingInfo, RoutingInfo, SingleNodeRoutingInfo},
    from_redis_value,
    sentinel::SentinelClient,
};
use tokio::sync::Mutex;

/// The settings of every `ConnectionManager` this crate builds. While the server is down, a call
/// fails at once instead of waiting through redis-rs's default six reconnect attempts, about 9 s,
/// and the next call connects again. A cache that is down then costs a caller milliseconds, and
/// `get_or_set` goes straight to its loader. Timeouts stay redis-rs's: 500 ms for a reply, 1 s
/// for a connection attempt.
///
/// Start from it when you build a `ConnectionManager` yourself:
///
/// ```no_run
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// use std::time::Duration;
/// use sisa_caching_redis::{RedisCacheStore, connection_config, redis::{Client, aio::ConnectionManager}};
///
/// let config = connection_config().set_response_timeout(Some(Duration::from_secs(2)));
/// let conn = ConnectionManager::new_with_config(Client::open("redis://127.0.0.1:6379")?, config).await?;
/// let cache = RedisCacheStore::new("app", conn)?;
/// # Ok(())
/// # }
/// ```
pub fn connection_config() -> ConnectionManagerConfig {
    ConnectionManagerConfig::new().set_number_of_retries(0)
}

/// A node's host and port, as the cluster names it.
pub type NodeAddress = (String, u16);

/// A connection a [`RedisCacheStore`](crate::store::RedisCacheStore) runs on. Keys go wherever the
/// connection routes them; only `remove_prefix` and `clear` need more, because `SCAN` sees one
/// node's keys. They scan each node from [`primaries`](Self::primaries).
pub trait RedisConnection: ConnectionLike + Clone + Send + Sync + 'static {
    /// The nodes that hold keys. `None` is the one server this connection talks to.
    fn primaries(&mut self) -> impl Future<Output = RedisResult<Vec<Option<NodeAddress>>>> + Send {
        async { Ok(vec![None]) }
    }

    /// Sends `cmd` to `node`, or along the connection when `node` is `None`.
    fn query_node(
        &mut self,
        _node: Option<&NodeAddress>,
        cmd: &Cmd,
    ) -> impl Future<Output = RedisResult<Value>> + Send {
        async move { self.req_packed_command(cmd).await }
    }
}

impl RedisConnection for ConnectionManager {}

impl RedisConnection for MultiplexedConnection {}

impl RedisConnection for ClusterConnection {
    async fn primaries(&mut self) -> RedisResult<Vec<Option<NodeAddress>>> {
        // Asked of every primary, so each reply comes keyed by the address this connection reaches
        // its node at. One reply is enough: a node it lists with a NULL or empty host, as with
        // `cluster-preferred-endpoint-type unknown-endpoint`, is on the host the command went to.
        let every_primary = RoutingInfo::MultiNode((MultipleNodeRoutingInfo::AllMasters, None));
        let replies: HashMap<String, Value> = from_redis_value(
            self.route_command(redis::cmd("CLUSTER").arg("SLOTS").to_owned(), every_primary)
                .await?,
        )?;
        let Some((asked, ranges)) = replies.into_iter().next() else {
            return Ok(Vec::new());
        };
        let asked = asked
            .rsplit_once(':')
            .map_or(asked.as_str(), |(host, _)| host);
        // One row per slot range: [start, end, primary, replicas...], each node [host, port, ...].
        let ranges: Vec<Vec<Value>> = from_redis_value(ranges.extract_error()?)?;
        let mut primaries = Vec::new();

        for range in ranges {
            let Some(node) = range.into_iter().nth(2) else {
                continue;
            };
            let node: Vec<Value> = from_redis_value(node)?;
            let mut node = node.into_iter();
            let host: Option<String> = from_redis_value(node.next().unwrap_or(Value::Nil))?;
            let address = (
                host.filter(|host| !host.is_empty())
                    .unwrap_or_else(|| asked.to_owned()),
                from_redis_value(node.next().unwrap_or(Value::Nil))?,
            );

            if !primaries.contains(&Some(address.clone())) {
                primaries.push(Some(address));
            }
        }

        Ok(primaries)
    }

    async fn query_node(&mut self, node: Option<&NodeAddress>, cmd: &Cmd) -> RedisResult<Value> {
        let Some((host, port)) = node else {
            return self.req_packed_command(cmd).await;
        };
        let route = SingleNodeRoutingInfo::ByAddress {
            host: host.clone(),
            port: *port,
        };

        self.route_command(cmd.clone(), RoutingInfo::SingleNode(route))
            .await
    }
}

/// A connection to the primary that Redis Sentinel names. When a command fails because the old
/// primary went away or refuses writes with `READONLY`, this asks Sentinel for the primary and
/// moves there if it changed; while Sentinel names the same one, the connection stays and
/// reconnects by itself. A command that surely didn't run, refused with `READONLY` or `LOADING` or
/// never sent because the connection was refused, is sent once more. Any other failed command
/// returns its error, since it may have run. A demoted Dragonfly primary answers `LOADING` instead
/// of `READONLY` while it syncs from the new one.
///
/// It moves on an error, not on Sentinel's announcement. After a manual failover the old primary
/// keeps accepting writes until Sentinel demotes it, about ten seconds later, and those writes are
/// lost. Reads keep answering from it, as a replica, until the next write or a
/// [`primary_client`](Self::primary_client) call moves the connection.
///
/// Its connections use [`connection_config`], so while the primary is down a command fails at
/// once instead of waiting for the old primary to come back.
///
/// Clones share the connection, so one failover moves all of them.
#[derive(Clone)]
pub struct SentinelConnection {
    sentinel: Arc<Mutex<SentinelClient>>,
    primary: Arc<RwLock<Primary>>,
}

/// The connection to the current primary, and how many times it has moved, so a clone that saw a
/// command fail can tell whether another clone already moved.
#[derive(Clone)]
struct Primary {
    generation: u64,
    client: Client,
    connection: ConnectionManager,
}

impl std::fmt::Debug for SentinelConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SentinelConnection")
            .field("failovers", &self.primary().generation)
            .finish_non_exhaustive()
    }
}

impl SentinelConnection {
    /// Asks Sentinel for the primary and connects to it. Build `sentinel` with
    /// `SentinelServerType::Master`.
    pub async fn connect(mut sentinel: SentinelClient) -> RedisResult<Self> {
        let primary = Primary::connect(sentinel.async_get_client().await?, 0).await?;

        Ok(Self {
            sentinel: Arc::new(Mutex::new(sentinel)),
            primary: Arc::new(RwLock::new(primary)),
        })
    }

    /// Asks Sentinel for the primary, moves this connection there if it isn't there yet, and
    /// returns a client for it, for connections this one can't carry, such as pub/sub. A
    /// subscription opened with it and this connection's reads then use the same primary. A
    /// hybrid store's backplane calls it on each ping, so reads leave a demoted primary even when
    /// nothing fails there.
    pub async fn primary_client(&self) -> RedisResult<Client> {
        let mut sentinel = self.sentinel.lock().await;
        let client = sentinel.async_get_client().await?;

        Ok(self.move_to(client).await?.client)
    }

    fn primary(&self) -> Primary {
        self.primary
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Moves to the primary Sentinel names now, unless a clone already moved past `failed`.
    async fn follow(&self, failed: u64) -> RedisResult<ConnectionManager> {
        let mut sentinel = self.sentinel.lock().await;
        let current = self.primary();

        if current.generation != failed {
            return Ok(current.connection);
        }

        let client = sentinel.async_get_client().await?;

        Ok(self.move_to(client).await?.connection)
    }

    /// Moves to the server `client` names, unless the connection is there already: a connection
    /// to a primary that didn't move reconnects by itself. Callers hold the Sentinel lock, so
    /// clones don't move at once.
    async fn move_to(&self, client: Client) -> RedisResult<Primary> {
        let current = self.primary();

        if client.get_connection_info().addr() == current.client.get_connection_info().addr() {
            return Ok(current);
        }

        let primary = Primary::connect(client, current.generation + 1).await?;

        *self.primary.write().unwrap_or_else(|e| e.into_inner()) = primary.clone();
        Ok(primary)
    }
}

impl Primary {
    async fn connect(client: Client, generation: u64) -> RedisResult<Self> {
        let connection =
            ConnectionManager::new_with_config(client.clone(), connection_config()).await?;

        Ok(Self {
            generation,
            client,
            connection,
        })
    }
}

/// What a failover does to a command: the old primary went away, or it is a replica now.
enum Failover {
    /// The command didn't run, so it can be sent again.
    NotRun,
    /// The connection broke; the command may have run.
    MayHaveRun,
}

fn failover(error: &RedisError) -> Option<Failover> {
    // Inside a transaction `READONLY` arrives as one of the aborted commands' errors.
    let read_only = error
        .clone()
        .into_server_errors()
        .is_some_and(|errors| errors.iter().any(|(_, error)| not_run(error.kind())));

    if read_only || error.is_connection_refusal() {
        Some(Failover::NotRun)
    } else if error.is_io_error() || error.is_connection_dropped() {
        Some(Failover::MayHaveRun)
    } else {
        None
    }
}

/// Outside a transaction a server error is a reply, not an `Err`. A replica runs no writes, and a
/// server that is loading runs no commands, so a reply of `READONLY` or `LOADING` means the
/// command didn't run.
fn read_only(reply: &Value) -> bool {
    matches!(reply, Value::ServerError(error) if not_run(error.kind()))
}

fn not_run(kind: Option<ServerErrorKind>) -> bool {
    matches!(
        kind,
        Some(ServerErrorKind::ReadOnly | ServerErrorKind::BusyLoading)
    )
}

impl SentinelConnection {
    /// Sends on the current primary. After a failover it moves, and sends once more if the first
    /// try surely didn't run.
    async fn send<T, Fut>(
        &self,
        send: impl Fn(ConnectionManager) -> Fut,
        refused: impl Fn(&T) -> bool,
    ) -> RedisResult<T>
    where
        Fut: Future<Output = RedisResult<T>>,
    {
        let Primary {
            generation,
            connection,
            ..
        } = self.primary();
        let reply = send(connection).await;
        let failed = match &reply {
            Ok(reply) if refused(reply) => Some(Failover::NotRun),
            Ok(_) => None,
            Err(error) => failover(error),
        };

        match failed {
            Some(Failover::NotRun) => send(self.follow(generation).await?).await,
            Some(Failover::MayHaveRun) => {
                // The command's own error says more than a failed lookup would.
                let _ = self.follow(generation).await;
                reply
            }
            None => reply,
        }
    }
}

impl ConnectionLike for SentinelConnection {
    fn req_packed_command<'a>(&'a mut self, cmd: &'a Cmd) -> RedisFuture<'a, Value> {
        Box::pin(self.send(
            move |mut primary| async move { primary.req_packed_command(cmd).await },
            read_only,
        ))
    }

    fn req_packed_commands<'a>(
        &'a mut self,
        pipe: &'a Pipeline,
        offset: usize,
        count: usize,
    ) -> RedisFuture<'a, Vec<Value>> {
        Box::pin(self.send(
            move |mut primary| async move { primary.req_packed_commands(pipe, offset, count).await },
            |replies: &Vec<Value>| replies.iter().any(read_only),
        ))
    }

    fn get_db(&self) -> i64 {
        self.primary().connection.get_db()
    }
}

impl RedisConnection for SentinelConnection {}
