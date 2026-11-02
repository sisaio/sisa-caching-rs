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
| memory | get hit, get miss, set, remove | One call on a store with one shard. `set` builds a new value and writes it over the same key each time; `remove` takes out a key written just before it, untimed |
| memory | get_or_set hit | `get_or_set` when the value is cached, so the loader never runs |
| memory | get_or_set miss | `get_or_set` when the key is missing: a read, the key's lock, a second read, the loader and the write. The key is removed before each call, untimed |
| memory 8 tasks | get hit, shards 1 and 16 | Eight tasks reading different keys at once, through one map or sixteen |
| memory 1M entries | remove_prefix, nothing matches | `remove_prefix` on a store of a million entries in one map, when no key matches |
| memory 1M entries | purge, nothing expired | A purge of that store when nothing has expired |
| memory 1M entries | purge, every entry expired | A purge that drops a million expired entries |
| memory 1M entries | set that trims | A write that takes a store past `max_entries` of a million, so it drops entries down to nine tenths of that. It runs outside a tokio runtime, where the write runs the trim itself |
| redis | get hit, get miss, set, remove | One call, including the round trip and JSON encoding |
| redis | get_or_set hit, get_or_set miss | As for memory: a miss makes three round trips, two reads and the write |
| hybrid | get local hit | A read served from the local layer |
| hybrid | get local miss, Redis hit | A read that goes to Redis and copies the value into the local layer |
| hybrid | get miss | A read that misses both layers |
| hybrid | set, remove | A write: Redis and both local evictions; the publish runs in the background, so it isn't timed |
| hybrid | get_or_set local hit, get_or_set miss | As for memory; a miss reads both layers twice and writes as `set` does |

Every store times the same calls: `get` hit and miss, `set`, `remove`, and `get_or_set` hit and
miss. `remove_prefix` is timed on a large memory store only: on Redis it scans every key on the
server, so its time says more about what else the server holds than about the store. `clear` isn't
timed.

Each `memory 1M entries` call walks every entry without yielding, and calls on the map it is
walking wait for it. Inside a tokio runtime, the store runs its trims and its purge worker's purges
on the blocking pool, and a hybrid node walks every prefix invalidation there too, its own or
another node's, so those hold up no async thread. A memory store's `remove_prefix` and `purge`
called by the application walk on the caller's thread; inside a runtime, `remove_prefix` frees
what it removed on the blocking pool.

`set that trims` runs on the default 16 maps, so it is not comparable with releases before 0.3.0,
which kept one map by default: 0.3.0 timed it 17% slower than 0.2.0. Over 16 maps the trim does
less work but touches more memory, as it walks the maps one at a time.

## Reading the numbers

Redis runs in Docker on the same machine as the benchmark, so Redis times leave out real network
latency. Compare one release with another, not with your production network.

The previous release's benchmarks run in the same job on the same runner, in alternating rounds:
the previous release, then this one, three times over. A shared runner drifts by 10 to 20%
between runs of the same code, more than most changes, and alternating lets that drift slow both
columns alike. Both versions are built with the same function alignment, so moving a function
doesn't move a time.

Each time is the median of the three rounds' Criterion means, and the range is this release's
lowest to highest round. The table calls a benchmark faster or slower only when no round of one
version overlaps the rounds of the other and the medians differ by 5% or more; anything less is
"about the same". A benchmark the previous release didn't have reads "new". The first release has
nothing to compare with, so its table shows one run's mean and its 95% confidence interval.

## Running them

They need Docker for the Redis and hybrid groups.

```sh
cargo bench -p sisa-caching-benches
cargo run -p sisa-caching-benches --bin summary   # prints the Markdown table
```

To compare with a release, as the release job does, with that release's tag for `v0.X.Y`:

```sh
export RUSTFLAGS="-C llvm-args=-align-all-functions=6 -C llvm-args=-align-all-nofallthru-blocks=5"
export CRITERION_HOME="$PWD/target/criterion"
git worktree add ../previous v0.X.Y
for round in 1 2 3; do
    (cd ../previous && CARGO_TARGET_DIR="$OLDPWD/target-previous" \
        cargo bench -p sisa-caching-benches -- --save-baseline "previous-$round")
    cargo bench -p sisa-caching-benches -- --save-baseline "current-$round"
done
cargo run -p sisa-caching-benches --bin summary -- v0.X.Y
```

Add a filter after `--`, such as `-- 'memory 1M' --save-baseline ...`, to compare a few benchmarks
only.

`REDIS_IMAGE` picks the server, for example `valkey/valkey:8-alpine`. Criterion's HTML report is
in `target/criterion/report/index.html`.

The code is in [`benches`](../benches), a crate of its own, so the library crates don't depend on
Criterion.
