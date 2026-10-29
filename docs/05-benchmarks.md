# Benchmarks

Each release runs the benchmarks and records them on its release page: a table that compares
each one with the previous release, the machine they ran on, and Criterion's full HTML report as a
download. [The latest release](https://github.com/sisaio/sisa-caching-rs/releases/latest) has the
current numbers. v0.1.0 has no table on its page; its numbers are in the summary of
[its benchmark run](https://github.com/sisaio/sisa-caching-rs/actions/workflows/release.yml).

## What they measure

Every value is a small `User { id, name, email }`, so a read decodes and a write encodes a real
object. An OpenTelemetry meter provider is installed, so each call records its metrics as it would
in an application that exports them. Benchmarks before 0.3.0 ran without one, so their table
leaves that cost out of the previous release's column.

| Group | Benchmark | What it times |
| --- | --- | --- |
| memory | get hit, get miss, set | One call on a store with one shard |
| memory | get_or_set hit | `get_or_set` when the value is cached, so the loader never runs |
| memory 8 tasks | get hit, shards 1 and 16 | Eight tasks reading different keys at once, through one map or sixteen |
| memory 1M entries | remove_prefix, nothing matches | `remove_prefix` on a store of a million entries in one map, when no key matches |
| memory 1M entries | purge, nothing expired | A purge of that store when nothing has expired |
| memory 1M entries | purge, every entry expired | A purge that drops a million expired entries |
| memory 1M entries | set that trims | A write that takes a store past `max_entries` of a million, so it drops entries down to nine tenths of that. It runs outside a tokio runtime, where the write runs the trim itself |
| redis | get hit, get miss, set | One call, including the round trip and JSON encoding |
| hybrid | get local hit | A read served from the local layer |
| hybrid | get local miss, Redis hit | A read that goes to Redis and copies the value into the local layer |
| hybrid | set | A write: Redis and both local evictions; the publish runs in the background, so it isn't timed |

Each `memory 1M entries` call walks every entry without yielding, and calls on the map it is
walking wait for it. Inside a tokio runtime, the store runs its trims and its purge worker's purges
on the blocking pool, and a hybrid node walks another node's prefix invalidation there too, so
those hold up no async thread. `remove_prefix` and `purge` called by the application run on the
caller's thread.

## Reading the numbers

Each time is Criterion's mean, with its 95% confidence interval. Redis runs in Docker on the same
machine as the benchmark, so Redis times leave out real network latency. Compare one release with
another, not with your production network.

The previous release's benchmarks run first, in the same job on the same runner, so the two
columns compare fairly. The table calls a benchmark faster or slower only when the two 95%
intervals don't overlap and the means differ by 5% or more; anything less is "about the same". A
benchmark the previous release didn't have reads "new". The first release has nothing to compare
with, so its table shows one column.

## Running them

They need Docker for the Redis and hybrid groups.

```sh
cargo bench -p sisa-caching-benches
cargo run -p sisa-caching-benches --bin summary   # prints the Markdown table
```

To compare with a release, as the release job does:

```sh
git worktree add ../previous v0.1.0
(cd ../previous && CARGO_TARGET_DIR="$OLDPWD/target" \
    cargo bench -p sisa-caching-benches -- --save-baseline previous)
cargo bench -p sisa-caching-benches -- --save-baseline current
cargo run -p sisa-caching-benches --bin summary -- v0.1.0
```

`REDIS_IMAGE` picks the server, for example `valkey/valkey:8-alpine`. Criterion's HTML report is
in `target/criterion/report/index.html`.

The code is in [`benches`](../benches), a crate of its own, so the library crates don't depend on
Criterion.
