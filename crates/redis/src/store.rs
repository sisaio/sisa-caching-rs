//! The Redis store.

use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use redis::{Client, IntoConnectionInfo, aio::ConnectionManager};

use sisa_caching_core::{
    cache::{CacheEntry, CacheError, CacheStore, CacheValue, KEY_SEPARATOR, KeyPrefix},
    clock::{Clock, SystemClock},
    locker::Locker,
    metrics::{Layer, METRICS, Reads},
    settings::CacheSettings,
};

use crate::{
    codec::{CacheCodec, JsonCodec},
    connection::{RedisConnection, connection_config},
    error::RedisCacheError,
    settings::RedisCacheSettings,
};

const VALUE: &str = "value";
const CREATED_AT: &str = "created_at";
const TTL_MS: &str = "ttl_ms";

/// The longest TTL stored, in ms: half of what `PEXPIRE` takes, leaving room for the server's
/// clock. Anything longer, millions of years, is stored as no TTL.
const MAX_TTL_MS: u64 = i64::MAX as u64 / 2;

/// The JSON value and origin metadata of one entry's hash.
type StoredFields = (Option<Vec<u8>>, Option<u64>, Option<u64>);

/// A store on a Redis-family server. One store holds values of every type as JSON; `get::<User>`
/// and `set(&key, user, None)` name the type per call. Reads deserialize JSON into the requested
/// type; compatible data can be shared across languages without runtime type names in Redis.
///
/// Connect it to a single server with a `ConnectionManager`, to a Redis Cluster with a
/// `ClusterConnection`, or behind Sentinel with a
/// [`SentinelConnection`](crate::connection::SentinelConnection). On a cluster, `remove_prefix`
/// and `clear` scan every primary.
#[derive(Clone)]
pub struct RedisCacheStore<Conn = ConnectionManager>
where
    Conn: RedisConnection,
{
    conn: Conn,
    name: KeyPrefix,
    settings: RedisCacheSettings,
    locker: Arc<Locker>,
    reads: &'static Reads,
}

/// The name and the settings; never the connection or a value.
impl<Conn: RedisConnection> std::fmt::Debug for RedisCacheStore<Conn> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RedisCacheStore")
            .field("name", &self.name)
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

impl<Conn> RedisCacheStore<Conn>
where
    Conn: RedisConnection,
{
    /// A store named `name`, such as `app` or `tenant:eu`, on `conn`. Fails with
    /// [`CacheError::InvalidKey`] when the name is empty or has an empty level. A
    /// `ConnectionManager` reconnects after a lost connection.
    pub fn new(name: &str, conn: Conn) -> Result<Self, CacheError> {
        let name = KeyPrefix::new(name)?;

        Ok(Self {
            reads: METRICS.reads(&name, Layer::Redis),
            name,
            conn,
            settings: RedisCacheSettings::default(),
            locker: Arc::default(),
        })
    }

    /// Fails when a setting is out of range, such as a TTL over 365 days, so a bad value stops
    /// setup instead of a later write.
    pub fn with_settings(mut self, settings: RedisCacheSettings) -> Result<Self, CacheError> {
        settings.validate()?;
        self.settings = settings;

        Ok(self)
    }

    pub fn settings(&self) -> &RedisCacheSettings {
        &self.settings
    }

    async fn unlink(&self, prefix: Option<&str>) -> crate::error::Result<usize> {
        let scope = escape_glob(&self.name.format_scope());
        let pattern = match prefix {
            Some(p) => format!("{scope}{}{KEY_SEPARATOR}*", escape_glob(p)),
            None => format!("{scope}*"),
        };

        let mut conn = self.conn.clone();
        let mut removed = 0;
        let batch_size = self.settings.scan_batch_size();

        // `SCAN` sees one node's keys, so each primary of a cluster is walked on its own.
        for node in conn.primaries().await? {
            let mut cursor: u64 = 0;

            loop {
                let scan = redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(&pattern)
                    .arg("COUNT")
                    .arg(batch_size)
                    .to_owned();
                let reply = conn.query_node(node.as_ref(), &scan).await?;
                let (next, keys): (u64, Vec<Vec<u8>>) =
                    redis::from_redis_value(reply.extract_error()?)
                        .map_err(redis::RedisError::from)?;

                // A cluster connection splits `UNLINK` by slot itself.
                if !keys.is_empty() {
                    removed += redis::cmd("UNLINK")
                        .arg(&keys)
                        .query_async::<usize>(&mut conn)
                        .await?;
                }

                if next == 0 {
                    break;
                }

                cursor = next;
            }
        }

        Ok(removed)
    }
}

impl RedisCacheStore {
    /// Connects to one server, such as `redis://127.0.0.1:6379`, or `rediss://` for TLS with the
    /// `tls-rustls` or `tls-native-tls` feature. `info` is a URL or a `redis::ConnectionInfo`.
    /// The connection is built with [`connection_config`]: while the server is down a call fails
    /// at once, and the next one connects again. For other timeouts, or for a cluster or
    /// Sentinel, build the connection and use `new`.
    pub async fn connect(name: &str, info: impl IntoConnectionInfo) -> crate::error::Result<Self> {
        Self::from_client(name, Client::open(info)?).await
    }

    /// Like [`connect`](Self::connect), on a client built by hand, such as one that trusts a
    /// private CA or shows a client certificate:
    ///
    /// ```no_run
    /// # #[cfg(feature = "tls-rustls")]
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// use sisa_caching_redis::{
    ///     RedisCacheStore,
    ///     redis::{Client, ClientTlsConfig, TlsCertificates},
    /// };
    ///
    /// let client = Client::build_with_tls(
    ///     "rediss://cache.internal:6380",
    ///     TlsCertificates {
    ///         client_tls: Some(ClientTlsConfig {
    ///             client_cert: std::fs::read("client.crt")?,
    ///             client_key: std::fs::read("client.key")?,
    ///         }),
    ///         root_cert: Some(std::fs::read("ca.crt")?),
    ///     },
    /// )?;
    /// let cache = RedisCacheStore::from_client("app", client).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn from_client(name: &str, client: Client) -> crate::error::Result<Self> {
        KeyPrefix::validate(name)?;

        let conn = ConnectionManager::new_with_config(client, connection_config()).await?;

        Ok(Self::new(name, conn)?)
    }
}

impl<Conn> CacheStore for RedisCacheStore<Conn>
where
    Conn: RedisConnection,
{
    type Error = RedisCacheError;

    fn name(&self) -> &KeyPrefix {
        &self.name
    }

    fn cache_settings(&self) -> &CacheSettings {
        &self.settings.common
    }

    fn locker(&self) -> &Locker {
        &self.locker
    }

    #[tracing::instrument(level = "debug", name = "redis get", skip_all, fields(key = %self.name.format_key(key)), err(level = "debug"))]
    async fn get_entry<CV: CacheValue>(
        &self,
        key: &str,
    ) -> crate::error::Result<Option<CacheEntry<CV>>> {
        let redis_key = self.name.format_key(key);
        let mut conn = self.conn.clone();
        // The countdown starts before the request, so a slow reply can only shorten it.
        let requested_at = SystemClock.now();
        let ((encoded_value, created_at_ms, ttl_ms), pttl_ms): (StoredFields, i64) = redis::pipe()
            .atomic()
            .cmd("HMGET")
            .arg(&redis_key)
            .arg(VALUE)
            .arg(CREATED_AT)
            .arg(TTL_MS)
            .cmd("PTTL")
            .arg(&redis_key)
            .query_async(&mut conn)
            .await?;

        let Some(encoded_value) = encoded_value else {
            self.reads.record(false);

            return Ok(None);
        };
        // PTTL is -1 for a key without expiry and -2 for a missing one.
        let expires_at = match pttl_ms {
            -1 => None,
            ms if ms < 0 => {
                self.reads.record(false);

                return Ok(None);
            }
            // A deadline past what the platform's clock can hold never comes.
            ms => requested_at.checked_add(Duration::from_millis(ms as u64)),
        };
        let created_at =
            SystemTime::UNIX_EPOCH + Duration::from_millis(created_at_ms.unwrap_or_default());
        let value = JsonCodec.decode(&encoded_value);

        // Data that doesn't decode is a miss too: the caller gets no value.
        if value.is_err() {
            self.reads.record(false);
        }

        let entry = CacheEntry::new(value?, created_at, ttl_ms.map(Duration::from_millis))
            .with_expires_at(expires_at);

        let entry = (!entry.is_expired(SystemClock.now())).then_some(entry);
        self.reads.record(entry.is_some());

        Ok(entry)
    }

    #[tracing::instrument(level = "debug", name = "redis set", skip_all, fields(key = %self.name.format_key(key)), err(level = "debug"))]
    async fn set_entry<CV: CacheValue>(
        &self,
        key: &str,
        entry: CacheEntry<CV>,
    ) -> crate::error::Result<()> {
        let redis_key = self.name.format_key(key);
        let encoded_value = JsonCodec.encode(&entry.value)?;
        let created_at = entry
            .created_at()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        // Whole milliseconds, rounded up, so a short TTL never becomes zero.
        let to_ms = |ttl: Duration| {
            u64::try_from(ttl.as_nanos().div_ceil(1_000_000))
                .ok()
                .filter(|ms| *ms <= MAX_TTL_MS)
        };
        let ttl_ms = entry.ttl().and_then(to_ms);
        let remaining_ms = entry.remaining_ttl(SystemClock.now()).and_then(to_ms);
        let mut conn = self.conn.clone();
        let mut pipe = redis::pipe();

        pipe.atomic().cmd("DEL").arg(&redis_key).ignore();

        // An entry that has already expired only clears the key.
        if remaining_ms != Some(0) {
            let hset = pipe
                .cmd("HSET")
                .arg(&redis_key)
                .arg(VALUE)
                .arg(&encoded_value[..])
                .arg(CREATED_AT)
                .arg(created_at);

            if let Some(ttl_ms) = ttl_ms {
                hset.arg(TTL_MS).arg(ttl_ms);
            }

            hset.ignore();

            if let Some(remaining_ms) = remaining_ms {
                pipe.cmd("PEXPIRE")
                    .arg(&redis_key)
                    .arg(remaining_ms)
                    .ignore();
            }
        }

        pipe.query_async::<()>(&mut conn).await?;

        Ok(())
    }

    #[tracing::instrument(level = "debug", name = "redis remove", skip_all, fields(key = %self.name.format_key(key)), err(level = "debug"))]
    async fn remove(&self, key: &str) -> crate::error::Result<bool> {
        let removed: usize = redis::cmd("UNLINK")
            .arg(self.name.format_key(key))
            .query_async(&mut self.conn.clone())
            .await?;

        Ok(removed > 0)
    }

    #[tracing::instrument(level = "debug", name = "redis remove_prefix", skip_all, fields(store = %self.name(), %prefix), err(level = "debug"))]
    async fn remove_prefix(&self, prefix: &str) -> crate::error::Result<usize> {
        KeyPrefix::validate(prefix)?;

        self.unlink(Some(prefix)).await
    }

    #[tracing::instrument(level = "debug", name = "redis clear", skip_all, fields(store = %self.name()), err(level = "debug"))]
    async fn clear(&self) -> crate::error::Result<()> {
        self.unlink(None).await?;

        Ok(())
    }
}

/// Escapes the characters that `SCAN MATCH` reads as a pattern.
fn escape_glob(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());

    for c in text.chars() {
        if "*?[]\\".contains(c) {
            escaped.push('\\');
        }
        escaped.push(c);
    }

    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_characters_are_escaped() {
        assert_eq!(escape_glob(r"a*b?c[d]e\f"), r"a\*b\?c\[d\]e\\f");
        assert_eq!(escape_glob("app::user"), "app::user");
    }
}
