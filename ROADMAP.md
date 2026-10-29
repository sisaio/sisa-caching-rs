# Roadmap

What shipped is in the [changelog](CHANGELOG.md). The issues and their milestones show the work in
progress.

## Later

Ideas that wait until users ask for them:

- A backplane on Redis Streams that replays the invalidations a node missed during a short
  disconnect, instead of clearing its local layer.
- Loading a cold key once across nodes, with an opt-in Redis lock for expensive loaders.
- A shorter TTL for "not found" values than for found ones.
- Batch reads, and loading a working set at startup.
- Serving local copies while Redis is down, trading freshness for availability.
