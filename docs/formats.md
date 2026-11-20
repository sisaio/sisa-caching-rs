# Formats

What goes into Redis and over the backplane. Services in other languages can share a Redis store
and its invalidations by following these two formats. Both change only with a version bump that
the changelog calls out. How each crate uses them is in its [spec](specs/README.md).

The memory store has no format of its own: its values stay Rust values inside the process and
never leave it as bytes. Every store shares the same entry model, a value with the time it was
written and an optional TTL.

## Keys

A stored key is `{store}::{key}`, such as `app::user:42`.

| Part | Rule | Example |
| --- | --- | --- |
| Store name | One or more levels joined by `:`, none empty | `app`, `tenant:eu` |
| Key | Any string; `:` separates its levels | `user:v1:42`, `42`, `user:eu:ada@example.com` |
| Prefix | One or more levels joined by `:`, none empty; matches the keys below it, by whole levels: `user` matches `user:42`, not `users:42` or `user` | `user`, `user:v1`, `user:eu` |

The store name never holds `::`, so the first `::` always ends it, and stores `a` and `a:b` never
share keys.

Put the version of what a key holds right after its name, such as `user:v1:42` or `users:count:v1`,
so it works the same for structs, scalars and lists. Bump it when a field is renamed or changes
type, or a value keeps its shape but changes meaning; adding or removing a field needs no bump.
Bump every key that holds the type, such as a list of users along with each user. Once no old node
is left, `remove_prefix("user:v1")` drops the old entries (run earlier, old nodes write them back),
while `remove_prefix("users")` still drops every version under `users`.

> [!WARNING]
> Nothing checks for the version: a key without one works, and so does a forgotten bump. Deciding
> when to bump is the application's job.

## Redis entry

Each key is a Redis hash:

| Field | Required | Value |
| --- | --- | --- |
| `value` | Yes | The value as UTF-8 JSON, such as `{"id":42,"name":"Ada"}` |
| `created_at` | Yes | When it was written, in Unix milliseconds |
| `ttl_ms` | No | Its full TTL in milliseconds; leave it out when it never expires (Rust also leaves out a TTL over `i64::MAX / 2` ms) |

To write, replace the whole hash and set its expiry in one transaction. `PEXPIRE` takes the time
left, which is `ttl_ms` for a fresh entry:

```text
MULTI
DEL     app::user:42
HSET    app::user:42 value '{"id":42,"name":"Ada"}' created_at 1760940000000 ttl_ms 300000
PEXPIRE app::user:42 300000
EXEC
```

Leave out `ttl_ms` and `PEXPIRE` for an entry that never expires. To read, run
`HMGET app::user:42 value created_at ttl_ms` and `PTTL app::user:42` together. A missing `value`,
or a `PTTL` of `-2`, is a miss. The entry expires when `PTTL` says, so readers never compare
clocks. A `value` that isn't valid JSON for the requested type fails the read with a decode
error.

## Invalidation message

After each write, a hybrid node publishes one JSON message on its store's pub/sub channel,
`<store>:backplane`, such as `app:backplane` for a store named `app`. The channel names the store,
so the message doesn't. A writer in another language publishes on the same channel:

| Field | Required | Value |
| --- | --- | --- |
| `v` | Yes | The format version, `1` |
| `source` | Yes | An id unique to the writing process; nodes skip their own messages |
| `seq` | No | On an invalidation, its number in the writer's sequence, from 1 up; on a heartbeat, the number up to which the writer has finished publishing; on `stopping`, the last number it handed out |
| `kind` | Yes | `key`, `prefix`, `clear`, `heartbeat`, or `stopping` |
| `target` | For `key` and `prefix` | The key or prefix, without the store name |
| `trace` | No | Trace propagation fields, such as `traceparent` |

```json
{"v":1,"source":"node-7","seq":41,"kind":"key","target":"user:42"}
{"v":1,"source":"node-7","seq":42,"kind":"prefix","target":"user"}
{"v":1,"source":"node-7","seq":43,"kind":"clear"}
{"v":1,"source":"node-7","seq":43,"kind":"heartbeat"}
{"v":1,"source":"node-7","seq":43,"kind":"stopping"}
```

Each store's worker publishes a heartbeat every `heartbeat_interval_ms`. A heartbeat only counts
publishes that are over, so on one channel it arrives after the invalidations it covers. A node
that received fewer of a writer's numbers than the heartbeat accounts for, or that hears no
heartbeat from a writer for `heartbeat_timeout_ms`, clears its local layer. A node tracks a
writer from the first message it gets from it. Nothing says which of the writer's earlier numbers
came before the subscribe, so the writer's first heartbeat clears the local layer unless every
number up to it arrived: after each subscribe, a node clears once more for every writer that
published before, which covers an invalidation lost in between, as during a failover.

`HybridCacheStore::shutdown` stops the store's heartbeats and waits up to two seconds for the
writes still running, even those whose caller gave up, to publish their invalidations. Then it
publishes `stopping`: a last heartbeat that counts every number the writer handed out, which the
other nodes check like any other and then forget the writer, instead of clearing their local layers
`heartbeat_timeout_ms` later. No heartbeat follows it. Only then does it stop the store's worker.

A service in another language that writes to Redis publishes the matching message after each
write, so hybrid nodes drop their copies. It can leave out `seq` and the heartbeats, and nodes
then apply its messages without tracking them. Keep messages exact: a node that receives one it
can't read, an unknown `v` included, clears its local layer, logs a warning and counts it, and
stays subscribed.
