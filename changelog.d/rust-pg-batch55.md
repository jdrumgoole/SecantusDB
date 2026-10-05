### Rust PostgreSQL server: SIGTERM, long READ COMMITTED blocks, sequences read live

#### Fixed

- `secantusd-pg` started with SIGTERM blocked, for example as a background job, could not be stopped by SIGTERM. It now unblocks and resets SIGINT and SIGTERM at startup. This is POSIX only.
- A block's `SELECT last_value` on a sequence missed another session's `nextval`. Sequences are now read live unless the block wrote them.
- READ COMMITTED blocks with more than 256 writes no longer keep their first snapshot. A block of 400 serial INSERTs alternating with SELECTs went from 2.54 s to 0.44 s. The block now moves to a fresh snapshot only for commits that can change its answer.

#### Performance

- Correlated subqueries with collated-text ordering filters, or with numeric `min` / `max` under a filter, are hashed instead of run per row. At 2,000 × 2,000 rows they take 0.03-0.04 s, against 0.12-0.68 s before.

#### Tooling

- The PostgreSQL differential probe compares against a `C`-collation reference database when the reference cluster's default collation is not `C`.
