# Flows

How a call moves through the stores, step by step. [Architecture](01-architecture.md) explains
why each step is there.

## `get_or_set`, on every store

One loader runs per key per process. Other callers for the same key wait on its lock, up to
`lock_timeout_ms`, then read what it stored.

```mermaid
sequenceDiagram
    participant App
    participant Store
    participant Lock as Per-key lock
    participant Loader

    App->>Store: get_or_set(key, loader)
    Store->>Store: get_entry(key)
    alt live entry, not due for refresh
        Store-->>App: cached value
    else live entry past refresh_threshold
        Store->>Lock: try_lock(key)
        alt another caller is refreshing
            Store-->>App: cached value
        else got the lock
            Store->>Store: get_entry(key) again
            alt another caller refreshed it meanwhile
                Store-->>App: that value
            else still due, missing, or unreadable
                Store->>Loader: load()
                Loader-->>Store: new value, or an error
                Store->>Store: set(key, new value)
                Store-->>App: new value, or on an error the cached one if the store still holds it
            end
        end
    else missing
        Store->>Lock: lock(key), waiting up to lock_timeout_ms
        Store->>Store: get_entry(key) again
        alt another caller stored it meanwhile
            Store-->>App: that value
        else still missing, or unreadable
            Store->>Loader: load()
            Loader-->>Store: value
            Store->>Store: set(key, value)
            Store-->>App: value
        end
    end
```

If the lock wait times out, the caller loads without the lock and logs a warning. If the second
read fails, for example because a value of another type is there, the loaded value is stored over
it.

## Memory store write

```mermaid
sequenceDiagram
    participant App
    participant Store as InMemoryCacheStore
    participant Shard as Key's shard
    participant Trim as Trim, on the blocking pool

    App->>Store: set_entry(key, entry)
    Store->>Store: lifetime = remaining TTL, capped at max_ttl_secs
    Store->>Shard: insert, marked as just used
    opt more than max_entries, and no trim running
        Store-)Trim: start
    end
    Store-->>App: Ok
    Trim->>Trim: drop expired entries
    Trim->>Trim: drop least recently used, down to 90% of max_entries
```

Only one trim runs at a time, and writes go on while it runs, so the store can go over
`max_entries` meanwhile. Outside a tokio runtime the write runs the trim itself. Replaced and
dropped values are freed after the shard's lock is released, so a large value never holds up
other callers.

A read looks the key up in its shard and marks it as used. An expired entry, including one
written with no lifetime left, reads as a miss and stays until a purge, a trim or a write to its
key.

## Redis store write and read

Each key is a hash: `value` (JSON), `created_at` and optionally `ttl_ms`. Another language
writing the same keys follows these two steps; [Data formats](01-architecture.md#data-formats)
has the full contract.

```mermaid
sequenceDiagram
    participant App
    participant Store as RedisCacheStore
    participant Redis

    App->>Store: set_entry(key, entry)
    Store->>Store: encode the value as JSON
    Store->>Redis: MULTI, DEL key, HSET key value created_at ttl_ms, PEXPIRE key remaining_ms, EXEC
    Note over Store,Redis: An already expired entry only sends DEL; one that never expires gets no PEXPIRE.
    Redis-->>Store: OK
    Store-->>App: Ok

    App->>Store: get_entry(key)
    Store->>Redis: MULTI, HMGET key value created_at ttl_ms, PTTL key, EXEC
    Redis-->>Store: fields and the remaining TTL
    alt no value, or the key is gone
        Store-->>App: None
    else
        Store->>Store: decode JSON into the requested type
        Store-->>App: entry, expiring after PTTL
    end
```

A value that doesn't decode into the requested type returns an error rather than `None`, so `get`
shows a type mix-up; the misses counter still counts it. `get_or_set` loads and stores over it.

## Hybrid read

```mermaid
sequenceDiagram
    participant App
    participant Hybrid
    participant Local as Local memory
    participant Redis

    App->>Hybrid: get_entry(key)
    Hybrid->>Hybrid: note the key's generation
    alt local reads on
        Hybrid->>Local: get_entry(key)
        Local-->>Hybrid: entry or none
    end
    alt local hit
        Hybrid-->>App: entry
    else local miss, or local reads off
        Hybrid->>Redis: get_entry(key)
        Redis-->>Hybrid: entry or none
        opt found and local reads on
            Hybrid->>Local: copy the entry, if the generation is unchanged
            Note over Hybrid,Local: An invalidation waits for a copy in progress, then evicts it.
        end
        Hybrid-->>App: entry or none
    end
```

## Hybrid write and invalidation

`set`, `set_entry`, `remove`, `remove_prefix` and `clear` all take this path. The steps after
the first eviction run in their own task, so they finish even if the caller is cancelled. The call
returns once Redis has answered and the key is evicted again, or for a prefix once its keys read as
misses while the blocking pool walks them; the publish and its retries carry on in the background,
so a backplane that can't be reached doesn't slow writes down.

```mermaid
sequenceDiagram
    participant App
    participant A as Hybrid on node A
    participant LA as Local memory A
    participant Redis
    participant Bus as Backplane (pub/sub)
    participant W as Backplane worker on node B
    participant LB as Local memory B

    App->>A: set(key, value)
    A->>LA: bump generation, evict key
    A->>Redis: set_entry(key, entry)
    Redis-->>A: ok or error
    A->>LA: bump generation, evict key again
    A-->>App: the Redis result
    loop in the background, up to 3 tries, 50 then 100 ms apart
        A->>Bus: publish Invalidation::Key(key)
    end
    Bus-->>W: message, with A's trace context
    W->>W: skip A's own messages and other stores' messages
    W->>LB: bump generation, evict key
```

If every publish try fails, the write has already returned the Redis result, and a warning is
logged. Node A's next heartbeat shows node B the missing sequence number, and node B clears its
local layer.

## Backplane worker

```mermaid
sequenceDiagram
    participant W as Backplane worker
    participant Bus as Backplane
    participant Local as Local layer

    W->>Bus: subscribe
    Bus-->>W: subscribed
    W->>Local: clear, then turn local reads on
    loop while subscribed
        Bus-->>W: invalidation from another node
        W->>Local: bump generation, evict
        Bus-->>W: heartbeat from another node
        opt a number is missing, a node's heartbeats stopped, or a message can't be read
            W->>Local: clear
        end
    end
    Bus-->>W: subscription lost
    W->>Local: turn local reads off, clear
    Note over W: wait 100 ms, doubling up to 5 s
    W->>Bus: subscribe again
```

The local layer is cleared on every subscribe, because copies made before it may have missed
invalidations. While reads are off, every read goes to Redis. The wait only starts over from
100 ms after a subscription that stayed up for at least 5 s, so a connection that keeps dropping
can't clear the local layer in a tight loop. If subscribing fails, the worker waits the same way
and tries again. A Redis subscription holds up to 8,192 messages the worker hasn't read yet; a
worker that falls further behind loses its subscription the same way, instead of the messages
queueing without a bound.

The worker publishes its own heartbeat every `heartbeat_interval_ms`, subscribed or not. When its
own heartbeats stop coming back for `heartbeat_timeout_ms`, the subscription has stopped
delivering, so the worker treats it as lost.

A prefix invalidation walks every local entry, so the worker bumps the generations, leaves the walk
to the blocking pool and goes on receiving; a writer's own `remove_prefix` hands its walk over the
same way. The prefix's keys read as local misses until the walk is done. With 16 walks already
queued, another prefix clears the local layer instead. A clear empties the local layer at once and
frees what it held there too.

## Purge worker

```mermaid
sequenceDiagram
    participant W as Purge worker
    participant Store as InMemoryCacheStore

    loop every purge_interval_secs
        W->>Store: purge(), on the blocking pool
        Store->>Store: remove expired entries from every shard
    end
```

Without a purge interval the store starts no purge worker. An expired entry then stays in memory
until its key is written again, `max_entries` trims it, or the application calls `purge`; with
none of those, the entries of keys that are never written again stay for good.

## How a store runs its worker

```mermaid
sequenceDiagram
    participant App
    participant Store as Store and its clones
    participant W as Its worker

    App->>Store: first call inside a tokio runtime
    Store->>W: start once, on that runtime
    Note over W: runs until the store's last clone drops
    App->>Store: drop the last clone
    Store->>W: cancel, without waiting
```

The memory store starts its purge worker on its first write, since entries only come in there.
The hybrid store starts its backplane worker on its first read or write, or in `connect`, so a
node that only writes runs one too. A worker holds the store's entries or state but not the
store's `Worker`, so it never keeps itself running. A call outside a tokio runtime starts
nothing, and the next one inside a runtime does. When the backplane worker stops for any reason,
such as a panic, its hybrid store turns local reads off and reads Redis from then on.
