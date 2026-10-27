# Compatibility

## Rust and runtime

- Rust 1.89 or newer, edition 2024. CI builds on 1.89.
- The tokio runtime. Workers run on it, and the Redis connections use redis-rs with tokio.

## Servers

The Redis store and the hybrid store's backplane need a server that speaks the Redis protocol:
hashes, `PEXPIRE`, `MULTI`, `SCAN` and pub/sub.

| Server | One primary | Primary with replicas | Cluster | Sentinel failover | TLS |
| --- | --- | --- | --- | --- | --- |
| Redis 7.4, 8 | ✅ tested in CI | ✅ | ✅ tested in CI | ✅ tested in CI | ✅ tested in CI |
| Valkey 8 | ✅ tested in CI | ✅ | ✅ tested in CI | ✅ tested in CI | ✅ tested in CI |
| Dragonfly 2 | ✅ tested in CI | ✅, but every node must publish and subscribe on the primary | ✅ tested in CI; the backplane needs sharded pub/sub | ✅ tested in CI, watched by Redis Sentinel | ✅ tested in CI |
| Garnet 2 | ✅ tested in CI | Only in cluster mode; not tested | ✅ tested in CI | ❌ Garnet has no Sentinel | ✅ tested in CI |

`RedisCacheStore::connect`, `RedisBackplane::connect` and `HybridCacheStore::connect` take a URL
for one server.

Point every store and backplane at the primary. Reads stay on the primary too: a replica lags
behind, so a node reading from one could copy a value that was just replaced back into its local
layer.

A hybrid store on a cluster or behind Sentinel is put together with `HybridCacheStore::new`. Build
its local store with `HybridCacheSettings::default().local` to keep the 60-second cap on stale
copies; `InMemoryCacheStore::new` alone has none.

### Cluster

Give the store a `ClusterConnection`. Single-key calls go to the primary that owns the key.
`remove_prefix` and `clear` run their `SCAN` on each primary in turn, since one `SCAN` reaches one
node. A slot that moves during the walk can hide its keys from it, as keys added during a `SCAN`
can on one server.

Give the backplane the cluster client with `RedisBackplane::cluster`. Build the client with
`ProtocolVersion::RESP3`: a subscription gets its messages as RESP3 pushes. It uses sharded
pub/sub (`SPUBLISH`, `SSUBSCRIBE`), which Redis, Valkey, Garnet and Dragonfly all have: the channel
lives on the node that owns its slot, so a message goes to one node instead of all of them. When
the slot moves, each node notices within one ping interval (5 s by default), then subscribes again
on the new owner after the reconnect backoff (100 ms, doubling up to 5 s while it keeps failing).

Dragonfly's cluster mode answers `PUBLISH` and `SUBSCRIBE` with an error, so it needs this.
`RedisBackplane::connect` to any one node of a Redis, Valkey or Garnet cluster still works, with
classic pub/sub, but while that node is down nothing is delivered and nodes read from Redis.

Dragonfly's emulated cluster mode is one server; connect to it as to one primary.

### Sentinel

Connect through a `SentinelConnection` and share it between the store and the backplane
(`RedisBackplane::sentinel`). Redis Sentinel watches Redis, Valkey and Dragonfly.

- When a command fails because the primary went away or answers `READONLY` or `LOADING`, the
  connection asks Sentinel for the primary again and moves there. A command refused with either
  didn't run, so it is sent once more; one cut off by a broken connection may have run, so it
  returns its error.
- A hybrid node notices a new primary within one ping interval (5 s by default): each backplane
  ping asks Sentinel for the primary and moves the shared connection there. The subscription then
  ends, the node clears its local layer, and it subscribes on the new primary after the reconnect
  backoff (100 ms, doubling up to 5 s while it keeps failing). On a failover Sentinel also
  closes the old primary's client connections with `CLIENT KILL TYPE`, which moves Redis and
  Valkey nodes at once, but Dragonfly refuses that command. Until the node moves, it can miss
  invalidations.
- A store without a backplane keeps reading from the old primary, now a replica, until it writes
  or calls `SentinelConnection::primary_client`.
- After a manual failover the old primary keeps accepting writes until Sentinel demotes it, about
  ten seconds later, and those writes are lost. A crashed primary accepts nothing, so a failover
  after a crash loses only what async replication loses (below).

### Dragonfly

- Its replicas don't pass the primary's pub/sub messages on to their subscribers, so a backplane
  must subscribe on the primary.
- Cluster mode needs `RedisBackplane::cluster`, which uses sharded pub/sub (above).
- Over TLS without client certificates, Dragonfly refuses to start unless a password is set
  (`--requirepass`).
- A demoted primary answers `LOADING` for a moment while it syncs from the new primary, where
  Redis answers `READONLY`. The Sentinel connection takes either as a command that didn't run.

### Garnet

- It replicates only in cluster mode, so Sentinel can't watch it. A cluster's own failover
  (`CLUSTER FAILOVER`) hasn't been tested.
- It listens on 127.0.0.1 unless started with `--bind`, so a container needs `--bind 0.0.0.0`.
- It delivers a sharded subscription's messages as `message` pushes, not `smessage`. The
  cluster backplane takes either.

### Any setup with async replication

A write the primary acknowledges just before a failover can be lost. The new primary then serves
the older value until it expires, and hybrid nodes keep their copy of it for at most the local
`max_ttl_secs`.

### TLS

Turn on one TLS feature of `sisa-caching` (or of `sisa-caching-redis`) for `rediss://` URLs,
which most managed services require. Neither is on by default. A URL trusts the system's root
certificates with either.

| Feature | TLS library | Private CA or client certificate |
| --- | --- | --- |
| `tls-rustls` | rustls, with its aws-lc-rs crypto provider | Yes, through `redis::Client::build_with_tls` |
| `tls-native-tls` | The system's: OpenSSL on Linux (needs its development package, such as `libssl-dev`, to build), Schannel on Windows, Security.framework on macOS | No: redis-rs has no API for them. A private CA can go in the system's trust store, or in `SSL_CERT_FILE` on Linux |

> [!WARNING]
> Turn on only one. With both, redis-rs uses rustls and loads no system root certificates, so
> every `rediss://` URL fails with `UnknownIssuer`, whatever CA signed the server's certificate.

With `tls-rustls`:

| Need | How |
| --- | --- |
| A managed service with a public certificate | `connect("app", "rediss://...")` |
| A private CA, a client certificate (mTLS), or both | Build a client with `redis::Client::build_with_tls`; pass it to `RedisCacheStore::from_client` and `RedisBackplane::from_client` |
| Cluster | `ClusterClientBuilder::certs` |
| Sentinel | `SentinelClientBuilder::set_client_to_redis_certificates` and `set_client_to_sentinel_certificates` |

`tls-rustls` turns on rustls's aws-lc-rs crypto provider, rustls's own default. If the application
also turns on rustls's `ring` provider, rustls can't pick one, and the first TLS connection panics:
call `rustls::crypto::aws_lc_rs::default_provider().install_default()` (or ring's) at startup.
With `tls-rustls` on, redis-rs's `SentinelClient::build` takes the service name as any
`AsRef<str>`; a `String` works either way.

With `--all-features`, as CI runs it, the test suite runs the stores and a hybrid against a
TLS-only server that requires a client certificate. CI then runs the `native_tls` tests with
`tls-native-tls` alone, against a TLS-only server that asks for no client certificate.

### Timeouts

Every connection the crates build for one server or behind Sentinel, through `connect`,
`from_client`, `SentinelConnection` or `RedisBackplane::sentinel`, uses `connection_config()`: while the server is down a call fails at once, and
the next call connects again. redis-rs's own default retries the connection six times first, so
every call waits about 9 s before it fails, and a `get_or_set` whose loader works takes three times
that. Timeouts stay redis-rs's: a command fails after 500 ms without a reply, and a connection
attempt after 1 s.

A large value or a `remove_prefix` on a busy server can need longer. Start from
`connection_config()`, change the timeouts, and pass the connection to `RedisCacheStore::new`:

```rust,ignore
use std::time::Duration;
use sisa_caching::{connection_config, redis::{Client, aio::ConnectionManager}};

let config = connection_config().set_response_timeout(Some(Duration::from_secs(2)));
let conn = ConnectionManager::new_with_config(Client::open(url)?, config).await?;
let cache = RedisCacheStore::new("app", conn)?;
```

A `ClusterConnection`, and `RedisBackplane::cluster`, keep the cluster client's settings: by
default redis-rs retries a failed command up to 16 times with growing waits. Lower them with `ClusterClientBuilder::retries` and
`retry_wait_formula`.

## Data formats

Services in other languages can share the same Redis keys and invalidations. The key format, the
Redis entry and the invalidation message are in
[Architecture: data formats](01-architecture.md#data-formats).
