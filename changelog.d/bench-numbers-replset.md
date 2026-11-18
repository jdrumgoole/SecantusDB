### Benchmarks re-measured against a replica-set `mongod`

The published latency, concurrency and three-droplet numbers were re-measured
on 2026-11-18, with `mongod` 8.0.38 running as a one-member replica set like
`secantusd-mdb`. On both servers a default write now waits for the journal.

#### Changed

- Per-operation latency: the Rust server is at 1.2×–3.6× of `mongod`.
- Three-droplet throughput: 0.51× of `mongod`.
- Concurrency: 2.6× scaling at eight writers, against `mongod`'s 3.4×.

The earlier figures compared against a standalone `mongod`, whose default
writes do not wait for the journal, so they are not comparable with these.
