# Crate: sisa-caching-testing

Status: **Implemented** · Published: no · Label: `area:testing` · Commit scope: `testing`

## Purpose

The test helpers the store crates' tests share: a test clock, a server in Docker for one test,
and a way to wait for another task. Never published, so it has no public API to keep.

## Public contract

| Item | Gives |
| --- | --- |
| `ManualClock` | A clock that moves only when the test moves it |
| `Redis` | A fresh Redis server in Docker for one test, removed when the test ends |
| `RedisCluster`, `RedisSentinel` | The same for a three-primary cluster, and for a primary and replica behind Sentinel |
| `RedisTls` | A server that only speaks TLS, with or without asking for a client certificate |
| `Proxy` | A TCP proxy in front of a server that a test can cut, as a network split would |
| `eventually` | Waits for something another task does, instead of sleeping |
| `Logs` | The info and higher events logged on the test's thread |
| `Metrics` | The metrics the stores record |
| `SlowDrop` | A value whose drop waits until the test lets it finish |

## Behaviour

Every test starts a server of its own, so tests run side by side and share nothing.
`REDIS_IMAGE` picks the server for all of them: Redis, Valkey, Dragonfly or Garnet, with the
default [CONTRIBUTING.md](../../CONTRIBUTING.md#tests) gives.

## Guarantees and limits

The suite needs Docker and nothing else.

## Errors

None: a helper that can't start its server panics, which fails the test.

## Formats

None.
