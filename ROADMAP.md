# Roadmap

What shipped is in the [changelog](CHANGELOG.md). The issues and their milestones show the work in
progress. Each milestone is a release, and the milestones on GitHub come from
[.github/milestones.yml](.github/milestones.yml).

| Milestone | Status | Goal |
| --- | --- | --- |
| 0.1.0 | Released | First release on crates.io: the in-memory, Redis and hybrid stores behind one trait |
| 0.2.0 | Released | Stores run their own workers, and hybrid nodes detect lost invalidations |
| 0.3.0 | Released | Each hybrid store on its own backplane channel, simpler settings, a sealed store trait and a faster in-memory store |
| 0.4.0 | Released | A faster in-memory store, and fixes in every store |
| 0.5.0 | Next | Not decided yet: picked from the Later list below, as users ask for it |

## Later

Ideas that wait until users ask for them:

- A backplane on Redis Streams that replays the invalidations a node missed during a short
  disconnect, instead of clearing its local layer.
- Loading a cold key once across nodes, with an opt-in Redis lock for expensive loaders.
- A shorter TTL for "not found" values than for found ones.
- Batch reads, and loading a working set at startup.
- Serving local copies while Redis is down, trading freshness for availability.
