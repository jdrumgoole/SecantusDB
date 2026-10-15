# Rust MongoDB server: the backlog -- plan (2026-10-14)

Scope: what is still open for `secantusd-rs` against mongod 8.2.11, taken from
`tasks/backlog.md` §7 (7.064 to 7.084, measured by the probes this month,
plus the open lines of 7.05 and 7.06). Every entry of 7.00 to 7.04 and 7.5 to
7.8 is closed. The exemplar is mongod only; the Python server is never the
reference (CLAUDE.md, "Design constraints").

The plan this file held before (2026-10-06, phases 0 to 4) is DONE; it is in
git history.

**Not all of it is code.** Roughly 60% of the open lines are code work a probe
measures, about 15 items are decisions for Joe (listed at the end), and the
rest are out of scope or accepted divergences. Sizes below are read from the
backlog, not measured: reproduce each item with its probe before starting it.

**Where things stand, measured 2026-10-14** (0.5.3-beta.194 tree, mongod
8.2.11; replies that differ):

| Probe | Differ |
| --- | --- |
| `auth_enforcement` | 388 of 12,053 (324 are the five commands of Phase 0b) |
| `diagnostic_commands` | 156 of 1,148 |
| `geo_queries` | 137 of 1,278 |
| `find_order` | 97 of 6,531 |
| `explain_commands` | 72 of 791 |
| `insert_delete_count_distinct` | 31 of 905 |
| `update_commands` | 17 of 1,295 |
| `transactions` | 14 of 1,419 |
| `user_role_commands` | 7 of 399 |
| `cursor_commands` | 7 of 776 |
| `change_stream_commands` | 6 of 409 |
| `time_series` | 6 of 187 |
| `index_admin` | 5 of 90 |
| `collation` | 4 of 764 |
| `out_and_merge` | 4 of 376 |
| `ddl_admin_commands` | 2 of 809 |
| `ttl_indexes` / `bulk_write_command` | 2 / 2 |
| `views`, `capped_collections`, `validators`, `scram_auth` | 0 |

Four rows do not match the figures their backlog entries give:
`transactions` (backlog 7.072: 11, counted in scenarios), `cursor_commands`
(7.073: 5 scenarios), `change_stream_commands` (7.074: 5 results) and
`insert_delete_count_distinct` (7.078: 32). The binary tree this table was
measured on is not recorded. Re-run a probe, and record `secantusd-rs
--version`, before quoting its row anywhere.

## How each batch lands

1. Claim a branch (push it first); reproduce the items with the probe.
2. Fix them, with Rust tests pinned to mongod's answers.
3. Check: Rust gates (workspace and each WiredTiger crate), the probes and
   gauges the change can reach, and the Python suite. Every third batch, or
   before a release, run all the MongoDB probes and the Go / Node / pymongo
   gauges.
4. One PR, CI, merge, teardown. Rewrite or delete the backlog lines in the
   same PR.
5. Cut a `secantusdb-v*` release when a user would notice, then the site post
   and `invoke deploy` (a Rust release ends at the live site).

## Phase 0 -- in flight at the end of 2026-10-14

- [ ] **0a. The 0.5.3-beta.194 site post.** Released (binaries, crates.io);
      the post, the pinned binary tag and the rewritten authorization section
      of `docs-rust/security.md` are on branch `site-mdb-0.5.3-beta.194`.
      Needs a PR, a merge and `invoke deploy`.
- [ ] **0b. Five commands mongod has and the server lacked** (7.084, 324
      rows; part of 7.082's "commands not there"): `dbHash`, `logRotate`,
      `connPoolStats`, `setDefaultRWConcern`, `setFeatureCompatibilityVersion`.
      A WIP commit is on the claim branch `rust-missing-admin-commands`
      (handlers, a probe `tools/probes/admin_commands.py`, Rust tests). It was
      written by a sub-agent stopped part-way and is NOT built, tested or
      measured. Review it as code nobody has checked: read every handler
      against a mongod probe before keeping it. Then build it, run the new
      probe and `auth_enforcement`, and land.
      mongod's `dbHash` is per-collection md5s plus a top-level md5; match its
      bytes on the same data, do not approximate.

## Phase 1 -- wrong answers and lost or duplicated writes

Only items where a client can get the wrong documents back, or where a write
is lost, duplicated, or stores a document mongod could not have made.
Wording and reply-shape differences are in Phase 5.

**1a. Stores written by an older build, or by the other server.** Each of
these is a silent wrong answer from data already on disk:

- [x] A geo index built before 2026-10-13 holds no entry for a value with
      several locations, or for a point written as a document other than
      `{lng, lat}`. Queries through it miss those documents (7.081).
- [x] A collated index the Rust server built before 2026-10-10 has no
      `collationKeys` marker and keeps comparing by value (7.070).
- [x] Collated `_id`: a collection created before 2026-10-11 has no claims
      for its existing documents, so a second spelling of one of their
      `_id`s is accepted (7.070).

  For all three, detect the old index or collection when the store opens.
  Then either refuse it, as an outdated `entryFormat` is refused, or rebuild
  it, rather than leaving the user to drop it.

  **Done 2026-10-14:** rebuilt. Each server marks the collated and geo
  indexes it builds (`collationKeys` / `geoKeys`, `pythonKeys`) and rebuilds
  one without its own marker at open; an index the documents no longer allow
  refuses the open, naming it. The Rust server claims every stored `_id`
  under its collection's collation once per store, and the Python server
  clears that record when it opens a store, so the walk runs again after it.
  `tests/test_rust_store_handoff.py` hands a store Rust -> Python -> Rust.

- [x] **The format the two servers share.** The Rust server writes ICU4X
      sort keys into a collated index; the Python server writes its own
      folded strings and does not know the marker. A store handed from one
      server to the other gets wrong answers from that index (7.070). The
      Python server also takes no `_id` claim by value, so a document it
      writes can duplicate a stored `_id` (7.078). CLAUDE.md treats the
      shared on-disk format as a compatibility promise. Fix both servers, or
      have each refuse the other's collated indexes at open.

**1b. Writes.**

- [x] `bulkWrite` as a retryable write is not replayed: a driver retry runs
      it twice (7.072).
- [x] `upsertSupplied` is two steps, and the inserted document skips the
      validator (7.068).
- [x] A `$`-prefixed field inside an upserted `_id` document is stored
      (mongod: 52). That is a document mongod cannot make (7.068).
- [x] A clustered collection's `expireAfterSeconds` expires nothing (7.076).
- [x] Two transactions creating one collection: mongod fails the second
      COMMIT with 112 (7.072).
- [x] **Measure first:** what a transaction that WROTE to a collection
      commits after another session dropped that collection (7.072). The
      "schema change waits" decision below depends on the answer. If the
      commit brings the writes back or leaves orphaned rows, this is a
      Phase 1 bug, not a decision.

  **Done 2026-10-14.** A collection a transaction creates is now held until
  it commits, its `_id` index rows and unique claims under a name of the
  transaction's own, so two that create one conflict at the second commit,
  as on mongod, whatever they insert.
  `upsertSupplied` already applied the validator; what was wrong was its
  `_id` (a new ObjectId where mongod takes the query's). The measurement:
  a drop under a transaction that wrote WAITS for it here, past its own
  `maxTimeMS`, and then drops; the commit brings nothing back and leaves no
  rows. Not a Phase 1 bug: it goes to the "schema change waits" decision
  (backlog 7.072).

**1c. Reads.**

- [x] A cursor outlives a dropped index: mongod answers 175 (7.073).
- [x] No view depth limit (165) (7.066).

  **Done 2026-10-14.** A `find` cursor records the indexes its plan reads,
  and `dropIndexes` kills those cursors with mongod's 175. A chain of more
  than 20 views is refused at `create` and `collMod`, and one of 20 at read
  (both measured). An `aggregate` cursor over a dropped index is still open
  (backlog 7.073).

Size: two batches; 1a is the larger one.

## Phase 2 -- the admin and diagnostic surface (7.082, 156 rows)

- [x] Replica-set commands answered as a one-member set:
      `replSetResizeOplog`, `appendOplogNote`, `replSetInitiate`; list
      `local.oplog.rs`, `local.system.replset`, `admin.system.version`;
      `electionCandidateMetrics`. (`replSetReconfig` is RM-143.)
- [x] Commands not there: `lockInfo`, `features`, `getClusterParameter`,
      the plan-cache commands. (`$listCatalog` is below.)
- [x] `serverStatus` sections from real counters (RM-034); `locks` stays
      absent, as there is no lock manager to count.
- [x] `$indexStats` use counts; `$listCatalog` from this server's own
      catalog (decided 2026-10-15).
- [x] `notablescan` (RM-035, decided 2026-10-15): 291 for a query the
      `explain` planner answers with a collection scan.
- [x] The profiler (RM-038) and `OP_COMPRESSED` (RM-039).
- Decided 2026-10-15: `buildInfo.secantusVersion` and `serverStatus.secantus`
  stay (a scope decision in 7.082).
- Left, in the backlog: `listDatabases` lists `config` (it needs
  `config.system.sessions`, which mongod flushes sessions to);
  `connectionStatus` privileges; `collStats.wiredTiger` (no per-collection
  table to report); `validate` with `background: true` (RM-033);
  `scramIterationCount` at runtime (RM-035); the opcode and exhaust items
  found in batch 2 (RM-144 to RM-147).

  **Done 2026-10-15** in two batches (`rust-admin-diag`, then
  `rust-admin-diag-2`, built by five agents in parallel). Found on the way
  and fixed: writes to `local` were logged in the oplog; `find`'s raw
  projection path sorted the fields; `serverStatus` failed outright on a
  store opened without WiredTiger statistics.

Size: two batches.

## Phase 3 -- geo (7.081, 137 rows)

- [x] Results in index-scan order under a geo index: S2 cell order for
      `2dsphere`, geohash order for `2d` (about 100).
- [x] `2d` index options and range (18), hinted geo indexes (10).
- [x] `$centerSphere` over pi, the bounded `$geoNear` full read,
      `2dsphereIndexVersion: 2.5`, two indexes on one key in one
      `createIndexes`, `$geoNear` inside `$lookup` / `$unionWith`.

  **Done 2026-10-15** in one batch (`rust-geo-phase3`, two agents): the
  geo probe went from 137 differing replies to none. The order is mongod's
  own S2 and geohash code, transcribed, with the near stage's libc++ heap
  and the arm64 reference build's fused multiply-adds. Found on the way
  and fixed: a `2dsphere` `$geoWithin` through the index missed documents
  beyond the flat box of the polygon's vertices; `delete` / `update` with a
  `$near` that could not run changed documents. Left, with gap tests:
  RM-148 (a point exactly on an edge), RM-149 (`coarsestIndexedLevel` /
  `finestIndexedLevel`), RM-150 (a refused spec leaves its collection),
  RM-151 (geo order under `$or` and a trailing range). A Linux x86-64
  mongod's tie order was not measured.

Size: one or two batches; scan order is the hard part.

## Phase 4 -- the planner and the order (7.080: 97 -> 77; 7.075: 72 -> 35)

- [x] Ties between the scans a sort merges: mongod's priority queue.
- [x] Which of several predicate indexes is read; `$match` moved ahead of
      `$addFields`. Done by reproducing mongod's plan trial, which also
      gives `rejectedPlans`.
- [x] The storage router reads the index for `$ne`, `$nin`, `$exists`,
      regex and the other filter shapes (RM-069 closed).
- [x] `explain` of `aggregate`, classic part: `DISTINCT_SCAN`, the added
      projection, `$match` ahead of `$unwind`, per-stage counts under
      `$limit`. Not done: the slot-based plans (RM-071 / RM-072), which
      need the engine's own programs; a stand-in is Joe's call.
- Split off: exact numeric comparison (RM-152), found during this phase,
  on its own branch `rust-numeric-exact`; it merges after the Rust PG
  server's RP-040.

## Phase 5 -- smaller tails

- [ ] Moved here from Phase 1, as reply shape rather than a wrong answer:
      transaction change events lack `txnNumber` / `lsid`, and `lsid.uid`
      is not the user's hash (7.072, 7.074).
- [ ] Capped eviction rescans the collection on every insert (7.065).
      This is a performance item: keep a running size and count.
- [ ] Views (7.066): a `hint` on a view is ignored (mongod: 2);
      `$indexStats` on a view answers (40602); the wording of 48 for an
      existing view; `distinct` on a view returns values in scan order.
- [ ] TTL and clustered collections (7.068): a clustered collection
      reports an `_id` index (`numIndexesBefore: 1`, mongod 0);
      `ttlMonitorSleepSecs` reads 60 whatever the startup cadence; which
      side of the TTL cutoff an exact match falls on is not measured; a
      pipeline upsert yielding an array `_id` answers 53 (mongod 54).
- [ ] Cursors and unknown fields (7.072, 7.073): `$meta: "recordId"` /
      `"indexKey"` add nothing; `find` accepts `$hint` and other fields it
      does not know (mongod: 40415).
- [ ] 7.074: a `delete` with `limit: 0` after a `multi` update deletes in
      natural order; mongod deleted in reverse. Re-measure since 7.080.
- [ ] 7.065: a `collMod` of `cappedSize` / `cappedMax` is written to the
      oplog as `size` / `max`.
- [ ] 7.083: `rolesInfo: 1` lists stored roles by name; mongod lists them
      in creation order (2). Unlike the role-set order, this one is
      reproducible, but it needs a sequence in the role table.
- [ ] 7.084: `mapReduce` with `out` (18) answers an error where mongod
      runs it.

- [ ] Time series (7.071): sort a batch before placing it; close a bucket on
      size; the `_id` / index differences.
- [ ] Collation (7.070): `normalization: false` (ICU4X always normalises),
      the five collation types ICU4X lacks, `$densify` `bounds: "full"` across
      partitions, re-measure `$setWindowFields` partition order.
- [ ] Change streams (7.074): mongod's token-decode errors (15, 50811),
      `startAfter` a `dropDatabase` token, the unprobed items.
- [ ] `update` (7.077): 27 for `$text`, `$$CLUSTER_TIME`, the `$replaceWith`
      message. The upserted field order has no known rule.
- [ ] 7.078: `includeQueryStatsMetrics`, `distinct` over a multikey index and
      with a collation, 239 for an old `atClusterTime`.
- [ ] The `Slow query` line (7.07): `planSummary`, `keysExamined`,
      `docsExamined`, `nreturned`, `reslen`, the storage wait times.

Size: one or two batches.

## Phase 6 -- tooling and measurement

- [ ] 7.06: the binary smoke test should run the binary the provenance check
      verified; replace `timeout` in the release skill's finalize loop.
- [ ] Re-run every driver gauge against the Rust server and regenerate the
      driver panels; the `-rust-server` reports are stale (7.05).
- [ ] Re-measure throughput against mongod on the droplets: the 0.27x write
      figure (§3) is from 0.5.3-beta.160.
- [ ] Backlog entries that read as stale, to verify and close: 7.065's
      "`convertToCapped` is not implemented" (7.071 says it landed
      2026-10-11), the Go SDAM heartbeat item, the `featureCompatibilityVersion:
      "7.0"` note and the `Location91` codeName in §5.
- [ ] Backlog entries that contradict each other: 7.071 says `compact` and
      `reIndex` landed 2026-10-11 and match mongod on 51 of 51; 7.084
      (2026-10-14) says they answer an error where mongod runs them (8).
      Reproduce both before scheduling either. The unknown-hint wording is
      "left open on purpose" in 7.05 and in
      `tests/test_rust_hint_error_message.py`, but "Joe's call, not made" in
      7.078. Make the entries agree once the decision below is confirmed.
- [ ] Watch item (7.07): one writer made no progress for over 10 s in a
      droplet sweep. Not reproduced. The harness now keeps the server's log
      and store when a row fails; read them if it happens again.

## Decisions for Joe

Each unblocks rows that otherwise stay open.

| Decision | What it holds |
| --- | --- |
| Unknown-hint wording (mongod prints its query tree). Already decided: not reproduced (7.05, `tests/test_rust_hint_error_message.py`). Confirm, then mark the rows accepted in each probe | about 50 replies over six probes |
| Lazy cursors (`getMore` sees later writes; `batchSize: 0` runs nothing) | 3 replies, and memory on large results |
| Ties under a sort with a limit. mongod's order comes from its C++ library's heap, and a Linux mongod (libstdc++) was not measured. Measure it first; if it differs from macOS (libc++), accept the divergence instead of copying either | 62 |
| Index build order on disk (a catalog field the Python server must carry) | `listIndexes` order, duplicate-key index |
| A schema change waits on an open transaction (after the Phase 1b measurement) | 2 |
| The privileges for this server's own `secantusAdmin` commands, which were chosen, not measured (7.084) | |
| `explain`: mongod's plan or this server's access path | |
| Stored time-series buckets instead of derived | |
| The profiler `filter`; `system.profile` | 6 |
| Wire compression (`OP_COMPRESSED`) | 2 |
| Server-side JavaScript (`$where`) | 3 Go leaves, several probe rows |
| Gate `configureFailPoint` / `enableTestCommands` | security |
| Python-server parity for the Rust-side changes (the on-disk half is not optional: Phase 1a) | |
| The PostgreSQL crates' serde BSON decodes (7.077) | |
| Rust as the recommended default | product |

## Out of scope or accepted

- Out of scope: a second replica-set member, sharding, `text` and `hashed`
  indexes, `$meta` inside aggregation stages (it needs mongod's
  `DepsTracker`).
- Accepted: the Decimal128 transcendentals' last digit (Joe, 2026-10-06), a
  `w` tag set with several keys, the multikey sort that contradicts mongod's
  own tie-break.
- Not reproducible: the order of a set of roles (mongod keeps a hash set).

## Order

Phase 0, then Phase 1 released as soon as it lands, then Phases 2 and 3, a
release with refreshed gauges and a throughput re-measure (Phase 6), then
Phases 4 and 5. Phase 1a goes first within Phase 1, because it is the only
work here where data already on users' disks answers wrongly. About one
batch per long session: 10 to 13 batches, 2 or 3 more for the aggregate
`explain`.
