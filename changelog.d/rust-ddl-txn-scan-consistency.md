### Transactional DDL and consistent scans for the Rust server

The Rust server's storage engine now runs every namespace-level DDL —
createIndexes, dropIndexes (single and `"*"`), create, drop and rename
collection, and dropDatabase — inside the same per-statement WiredTiger
transaction machinery its CRUD path has used since the collection-locks work.
Registry rows, index entries, collection options and the DDL's oplog entry now
commit or vanish together, so a crash mid-DDL can no longer strand orphan
index-entry rows behind a missing registry row. dropDatabase commits one
transaction per collection — the same unit real mongod uses — so a huge
database can't blow the storage cache with a single monolithic transaction.

That atomicity also closes the long-standing DDL-vs-scan wobble: a lock-free
read racing a drop or rename could previously return a partial result set,
splicing rows read before the DDL with the post-DDL view. Reads now run under
a seqlock-style namespace-generation check — DDL holds the generation counter
odd for its duration, and a scan that observed an odd or moved generation
re-runs against the settled state, so every result is a point-in-time answer.
A concurrent-stress test pins the new invariant: scans racing drops and
renames observe the full collection or none of it, never a partial splice.

Two smaller items land alongside: single-document updates no longer clone the
post-image document unless the caller actually asked for it (only
`findAndModify` does — plain updates skip a full per-document clone), and the
`anyhow` dependency moved past RUSTSEC-2026-0190 in all four lockfiles.

#### Changed

- `secantus-storage`: `create_collection[_with_options]` / `drop_collection` /
  `drop_database` / `rename_collection` / `create_index` / `drop_index` /
  `drop_all_indexes` wrap their row writes in `with_statement_txn` +
  `retry_write_conflicts`; dropDatabase is per-collection transactions. DDL
  invoked inside a user (multi-document) transaction now joins it uniformly
  and rolls back with it (pinned by `tests/ddl_txn.rs`).
- `secantus-storage`: `update_matching` / `update_matching_pipeline` (and the
  `secantus-commands` storage seam's `update_matching_array_filters` /
  `update_matching_pipeline`) take a `want_post_image` flag;
  `UpdateOutcome::post_image` is captured only for `findAndModify`, sparing
  every plain single-doc update a full `Document` clone.
- `anyhow` 1.0.102 → 1.0.104 in `crates/`, `secantusdb`, `secantus-storage`
  and `secantus-storage-py` lockfiles, clearing the RUSTSEC-2026-0190
  unsoundness advisory from the cargo-audit log.

#### Fixed

- `secantus-storage`: a lock-free `find_matching_with` / `count_matching`
  racing a `renameCollection` / `dropCollection` / `dropDatabase` /
  `dropIndexes` can no longer return a partial result set. Namespace DDL runs
  under a drop-guarded seqlock generation (`ddl_generation_scope`, serialised
  by the global lock) and readers re-run a scan whose generation was odd or
  moved (bounded, so a DDL storm can't livelock a reader). Pinned by
  `tests/concurrent_reads.rs::scans_racing_namespace_ddl_are_never_partial`.
