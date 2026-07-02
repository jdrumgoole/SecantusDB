# Backlog: stubs, stopgaps, and deferred work

A living list of things SecantusDB does not yet implement faithfully. Update when you stub something, when you defer a slice, or when you discover a limitation in production code. Don't add items here that already have a fix in flight — those belong in tasks/todo.md.

Each item should have enough context for a future session to pick it up cold: what's there now, what's missing, why it was deferred.

---

## 1. Stubs (canned responses, no real semantics)

These commands accept the request and return a wire-valid response, but the response is fabricated — they do no real work.

(None currently — `commitTransaction` / `abortTransaction` were the last true stubs; real multi-document transactions shipped via `secantus.transactions` + WT-native per-transaction sessions. Remaining transaction divergences are in §3.4.)

## 2. Stopgaps (functional but with significant limitations)

These work end-to-end but cut corners.

- [ ] **`_id` numeric type bridge** — works for finite int/float/Decimal128. `bool` is deliberately not numeric. NaN and infinity `_id` values fall through to the BSON-blob path; behavior is unspecified.
- [ ] **`top` counters are always zero** — the command returns the mongod shape (one `totals` entry per namespace, `total`/`readLock`/`writeLock`/per-op sections) and mongotop renders it like an idle server, but SecantusDB doesn't instrument per-namespace operation timing, so every `{time, count}` is `0`. Real counters would need per-ns accounting in `Metrics` threaded through dispatch.
- [ ] **`$geoNear` is a brute-force COLLSCAN on both servers** — `aggregate._stage_geo_near` (Python) and `secantus-commands::aggregate::apply_geo_near` (Rust) both walk *every* doc and compute the distance, even when a `2d` / `2dsphere` index exists and a `maxDistance` bound is set. Results are correct and the two servers match; this is purely a missing perf optimization. **Fix the Python server first** (it's the oracle): when `$geoNear` is the leading stage with a `maxDistance` (and a usable geo index for `key`), lift the `near` + `maxDistance` into a `$geoWithin: {$centerSphere|$center: [...]}` candidate query and serve it through `Storage.find_matching` (the same geo-index path `$near`/`$geoWithin` already use — `_pick_geo_index_for_filter` / `_try_geo_index_id_keys`), then compute exact distances + sort over just the candidates. Without `maxDistance`, `$geoNear` must return all docs in distance order, so a full scan is unavoidable — the optimization only applies to the bounded case. Once Python ships it, mirror in the Rust aggregate's leading-stage initial-fetch (the same place a leading `$match` is lifted into the index filter). Output (docs + order + attached `distanceField`) must stay identical to the brute-force path — only the candidate set shrinks.
- ~~**`renameCollection` cross-process safety**~~ structurally guaranteed by WiredTiger (b34). Within-process atomicity is the storage `RLock`. Cross-process exclusion is `WiredTiger.lock` — a second `wiredtiger_open` on the same path fails with ``WT_ERROR Resource busy`` before any state is touched, so concurrent writers across processes / worktrees can't exist in the first place. See `tests/test_storage_exclusion.py`.
- ~~**`createIndexes` collation**~~ shipped (single-field b25 + compound b27). `sortkey.encode_value_directed` takes a `collation` kwarg; index entries are written under the index's stored collation; single-field equality / range / `$in` (`_find_leading_field_index`), compound bare-equality (`_pick_compound_eq_index`), and compound prefix + trailing-operator (`_pick_compound_range_index`) all thread collation through and gate by exact match. Unique-probe path reads each index's stored collation too. Strength 1/2/3 + `caseLevel` work uniformly across single- and compound-field indexes; `numericOrdering` still falls back to COLLSCAN at every level (would need a length-prefixed digit-run encoding to stay byte-sortable). See `docs/indexes.md` "Per-index collation".

## 3. Deferred work (skipped from a slice, ready to come back)

Specific items that were left out of the slice that introduced their feature area.

- [ ] **Admin UI saved-connections / settings page**: Slice 11 of the admin UI shipped schema sampler / logs viewer / geo viewer but skipped the planned `/settings` page with saved Mongo URIs and a manual dark/light toggle. The CLI today takes a single `--uri` per launch, so saved connections are bookmark-only (you can't switch targets after start). When the launcher gains hot-swap support, revisit this page — it's likely a small SQLite-backed list reusing the existing `~/.secantus/admin.db` store.

### 3.1 Authentication

SCRAM-SHA-256 is implemented end-to-end. The wire-protocol shape (saslStart/saslContinue, `hello.saslSupportedMechs`, per-connection auth state, `--auth` gating) is conformant for pymongo and mongo-go-driver. The remaining gaps are mostly orthogonal:

- ~~**MONGODB-X509**~~ — shipped on top of the b22 mTLS slice. Users with `mechanisms: ["MONGODB-X509"]` on `$external` (or `admin`) auth via cert-subject-DN-as-username; no password. The legacy `authenticate` command path (what pymongo / Java / Go / Node use for X509) is wired up alongside `saslStart`. See `docs/authentication.md` "MONGODB-X509" section.
- [ ] **LDAP / Kerberos / GSSAPI / MONGODB-AWS / MONGODB-OIDC** — remaining alternative auth mechanisms. Out of scope for now (each one is its own slice with its own external-system dependency).
- [ ] **Internal cluster auth (keyfile / x509)** — only meaningful with replica sets / sharding, both out of scope.
- ~~**`system.users` collection visibility**~~ shipped (b31). `admin.system.users` is a synthetic read-only view onto the `secantus_users` table; `find` / `aggregate` / `count` surface the stored records with their existing mongod-shaped fields (`_id`, `user`, `db`, `credentials`, `roles`, `mechanisms`). Writes are rejected with code 13 Unauthorized — mutate via `createUser` / `updateUser` / `dropUser`. Other dbs' `system.users` returns empty (matches mongod). See `storage._find_system_users` + `_count_system_users`.
- ~~**`system.version` `authSchema`**~~ shipped (b33). `admin.system.version` is a synthetic read-only view returning a single hard-coded doc: `{_id: "authSchema", currentVersion: 5}` (SCRAM-SHA-256 baseline, MongoDB 4.0+). Tools that read the auth-schema version on startup now get an honest answer. Writes rejected (code 13). See `storage._find_system_version`.

### 3.2 Change-stream limitations

Single-node change streams are implemented and conformant for typical pymongo `watch()` flows, but the following are deferred or intentionally diverge from real `mongod`:

- [x] **Rust server: DDL change-stream events (`showExpandedEvents`)** — DONE
  (0.5.3-beta.22). `create_index` / `drop_index` now emit an oplog `op: "c"`
  entry (`{createIndexes, indexes:[{v,key,name}]}` / `{dropIndexes, index}`), and
  `collMod` routes through a new `Storage::coll_mod` that writes the options AND
  emits `{collMod, …}` (plain `set_collection_options` stays oplog-silent so
  `create`'s internal option writes don't fire a spurious `modify`). The
  storage-side projector already gated `createIndexes` / `dropIndexes` behind
  `showExpandedEvents`; added the `modify` branch. A `showExpandedEvents` watch
  now sees `createIndexes` / `dropIndexes` / `modify`, and a default watch
  suppresses them.
- [ ] **Read concern / write concern semantics** — accepted on the wire for compatibility, otherwise ignored.
- [ ] **C-driver (`libmongoc`) change-stream gauge tests excluded** — the C gauge's `include_paths.py` deliberately omits the `/change_stream` and `/change_streams` suites. libmongoc's test fixture bootstraps every change-stream test through `test_framework_replset_member_count()`, which now sees `replSetGetStatus` → `NoReplicationEnabled` (member count 0) and therefore *skips* those tests as "standalone". They no longer abort the run (that was the pre-`replSetGetStatus` behaviour), but they wouldn't actually exercise the change-stream path either, so they're left out. To gauge change streams through the C driver, `replSetGetStatus` would need to report ≥1 live member (a fuller fake-replset reply) — a larger emulation change than the standalone error we ship.
- [ ] **Resume-token cross-server identity** — tokens are opaque to pymongo and round-trip fine, but the inner layout is `{s, t, n, k}` (BSON-encoded, hex-stringed) rather than mongod's keystring format. Tokens minted by SecantusDB cannot be presented to a real `mongod`, and vice versa.

### 3.3 MongoDB CLI / tool conformance tests

Every connectable tool in the MongoDB toolchain is now covered by tests in the
default suite (each starts an embedded `SecantusDBServer` and drives the real
binary; all skip gracefully when the tool isn't on PATH):

- `mongosh` — `tests/test_mongosh.py` (two-direction round-trip).
- `mongodump` / `mongorestore` / `bsondump` — `tests/test_mongodump_restore.py`
  (dump-restore round-trip incl. indexes; bsondump pins the extended-JSON dump
  format).
- `mongoimport` / `mongoexport` — `tests/test_mongoimport_export.py` (NDJSON +
  CSV round-trips, `--query`/`--fields`, `--drop`, canonical-extended-JSON type
  fidelity for ObjectId / datetime / Decimal128 / Int64 / Binary).
- `mongostat` / `mongotop` — `tests/test_mongostat_mongotop.py` (single
  iteration each; mongostat needed `serverStatus.mem`, mongotop needed the
  `top` command — both fixed on the cli-tools slice).
- `mongofiles` (GridFS) — `tests/test_mongofiles.py` (put/get/list/delete,
  cross-checked against pymongo's gridfs).

CI installs mongosh + Database Tools on the Linux and macOS `test.yml` cells
(apt repo / mongodb/brew tap) so the tool tests run continuously there.
Windows is intentionally uncovered: the mongosh tests skip on win32 by design
(PowerShell `--eval` quoting), and a choco database-tools install is slow and
flake-prone for the marginal remainder. Per-tool caveats live in the test
files' docstrings.

Compass is covered headlessly in `tests/test_compass_commands.py`: the full
command surface the GUI issues (instance probes, `$collStats` / `$sample` /
`$indexStats`, explain at both verbosities, performance-tab polls,
`atlasVersion` → CommandNotFound) is pinned without driving Electron. Driving
the actual GUI stays out of scope.

### 3.4 Multi-document transaction limitations

Real transactions shipped (`secantus.transactions` registry + per-transaction
WT sessions in `Storage`; statements run with the transaction's session swapped
into the thread-local, oplog entries buffered until commit). Conformance:
`tests/test_transactions.py` (pymongo-driven), `tests/test_transaction_registry.py`,
`tests/test_storage_user_txn.py`. Known divergences, all deliberate:

- [ ] **Non-transactional writers don't block until the transaction ends** —
  mongod parks a plain writer that hits a transaction's uncommitted write until
  commit/abort. SecantusDB retries in a bounded backoff loop
  (`storage._retry_write_conflicts`, ~5s deadline) and then surfaces 112
  `WriteConflict`. A plain writer can therefore fail against a long-lived open
  transaction where mongod would have waited the full
  `transactionLifetimeLimitSeconds`.
- [ ] **Cross-transaction unique-index enforcement can leak** — index-entry
  keys embed the doc's id_key, so two different docs violating the same unique
  constraint from a transaction + a concurrent writer don't collide on a WT key
  and both commits can succeed. mongod prevents this with prepared conflicts.
  Same-key (`_id`) conflicts ARE caught (WT write-write conflict → 112).
- [ ] **Failpoint-injected errors on in-transaction statements don't abort the
  transaction** — the `failCommand` short-circuit runs before transaction
  resolution so retryable-commit tests (inject once, retry succeeds) work. If a
  unified test asserts transient labels on injected in-txn statement errors,
  the label must come from the failpoint's own `errorLabels` data.
- [ ] **No `recoveryToken` / mongos pinning, no prepared transactions, no
  `maxCommitTimeMS`, no `serverStatus.transactions` metrics, no
  `afterClusterTime` enforcement** — multi-node machinery; out of scope.
  (`$clusterTime` / `operationTime` ARE gossiped on every reply so drivers
  can send `afterClusterTime`; the value is accepted and ignored.)
- [ ] **Expected-red in the pymongo transactions gauge**: the three
  secondary-read-preference unified tests
  (`TestUnifiedRunCommand::test_run_command_fails_with_*secondary*`,
  `TestUnifiedReadPref::test_secondary_readPreference`) fail with a
  client-side 30s `ServerSelectionTimeoutError` — the fictional
  single-node replica set has no secondary to select, and the asserted
  server error ("read preference in a transaction must be primary")
  can only come from a server that received the command. Unfixable on
  a single node.
- [ ] **readConcern levels inside transactions are accept-and-ignore** — every
  in-transaction read runs against the transaction's pinned WT snapshot
  regardless of level (`snapshot` is exactly that; `local`/`majority` are
  indistinguishable on a single node).

### 3.4 Honest-gauge remainder (post-94.8% triage)

The 2026-06-13 gauge-gaps slice fixed projection `_id`/array semantics,
`maxBsonObjectSize` enforcement, snapshot readConcern + `$$NOW`, and most of
the change-stream batch. Still open, precisely characterized:

- [ ] **Cursor/collection misc from the 64-list** (task #14 of the slice).
  Fixed in the gauge-misc slice (2026-06-13): embedded-document equality is
  now order-sensitive + exact (real matcher correctness bug — `query_embedded`
  / `query_array` examples), the `validate` command is implemented
  (clean mongod-shaped result + full/background rejection), and upsert with a
  `None` `_id` reports `did_upsert` correctly (real bug — `None` was the
  "no upsert" sentinel). Still open:
  - timeseries `insertMany` bulk path (`test_collection_management` timeseries).
  - the 3 `test_dbref.py` execnet failures: gauge-harness artifact (xdist
    can't serialize ObjectId in subtest reports — runner-side, not server).
  - `test_maxtime_ms_message` / `test_to_list_csot_applied`: pymongo CSOT
    client-side timeout formatting — not server-side.
  Out-of-scope rejections to keep: `$where`, text/hashed indexes (CLAUDE.md).
  Expected-red (single-node topology): the 3 `test_transactions_unified`
  secondary-readPreference tests.

### 3.5 Point-in-time recovery (PITR) — v2 and limitations

PITR shipped for the Python server: `secantus.oplog_replay` replays an oplog
source (a backup archive or a stopped data dir) into a fresh store, stopping at a
target `ts` / wall time; surfaced as `secantusdb restore` (CLI) and
`secantusAdmin.restoreToTimestamp` (wire). **v2 also shipped** (`secantus.pitr_archive`):
with `oplog_archive_dir` set, `prune_oplog` archives soon-to-be-dropped oplog rows
to durable segment files; `Storage.archive_base_snapshot` / `secantusAdmin.archiveBaseSnapshot`
take base snapshots; and `restore_from_archive_dir` (a directory `source` on the
CLI/wire) picks the newest base ≤ T and stitches archived oplog forward onto it,
lifting the genesis-intact restriction. `--preserve-oplog` carries the replayed
oplog for change-stream resume continuity, and collection options replay too.
Native Rust server PITR (Phase R) is **complete** (branch `rust-pitr`):
`secantusAdmin.backupArchive` + `secantusdb restore` (v1 + v2 archive-dir
sources, `--preserve-oplog`), `secantusAdmin.archiveBaseSnapshot`,
`--oplog-archive-dir` server flag, the `secantus_storage::{replay,pitr_archive}`
modules, the reverse `apply_update_description` in `secantus-core`, collection-
options carry, and `set_index_expiry`. Cross-server restore is byte-faithful both
ways (`tests/test_rust_pitr_cross_server.py`, `tests/test_rust_binary_pitr.py`).
Plan/history: `tasks/rust-pitr-phase-r-plan.md`.

## 4. Out of scope (intentional, with reasoning)

These are explicit non-goals. Don't add them without a reason.

- **Real replica sets / sharding** — depend on cluster topology and cross-node consistency. SecantusDB advertises `setName: "secantus"` to satisfy pymongo's change-stream topology check, but the topology is fictional — there are no other members, no elections, no cross-node oplog. Change streams are still in scope (single-node, oplog-backed); see `## 3. Deferred work / Change-stream limitations`.
- ~~Authentication (SCRAM-SHA-256)~~ — implemented. `--auth` (CLI) / `require_auth=True` (constructor) gates non-handshake commands behind a successful `saslStart`/`saslContinue` round-trip. Provision users via `createUser`; manage with `dropUser` / `usersInfo`. The remaining auth gaps are tracked under `## 3. Deferred work / Authentication` below.
- ~~TLS / SSL~~ — implemented in b21+b22. `[tls] cert_file` + `[tls] key_file` enable server-side TLS; `[tls] ca_file` + `[tls] require_client_cert` add mTLS (transport-layer client verification). MONGODB-X509 cert-as-username auth landed in a later slice (see `docs/authentication.md`).
- **`OP_COMPRESSED`** — compression negotiation. Clients can be told the server doesn't support compression; nothing to do.
- **Text search** (`$text`, `$meta: "textScore"`, text indexes) — would need a full-text index implementation.
- **Geo — complete and shipped.** Operators (`$geoWithin` / `$geoIntersects` / `$near` / `$nearSphere`) + `$geoNear` aggregation stage (auto-infer `key`, `includeLocs`), `2dsphere` (S2 cell coverings + ancestors) and `2d` (bit-interleaved geohash with quadtree-decomposed Z-order range covering) index acceleration, compound geo+scalar indexes (geo cell scan + verifier-step filter on trailing scalars), legacy mongod sibling-form `$maxDistance` / `$minDistance` for `$near` / `$nearSphere`, and write-time input validation (out-of-range coordinates / unparseable shapes reject with mongod's documented code 16572 across insert / update / upsert / createIndex). See `src/secantus/geo.py` + `src/secantus/geo_index.py`. **Validation surface**: 79 in-tree pymongo tests in `tests/test_geo*.py`; 3 cross-driver smoke tests through mongosh, mongo-node-driver, and mongo-go-driver in `tests/test_geo_cross_driver.py` (all pass — wire-protocol geo path is clean across drivers); the pymongo conformance gauge keeps `test_collection.py`'s built-in geo tests at 100%; mongo-java-driver's `GeoJsonFiltersFunctionalSpecification` + `GeoFiltersFunctionalSpecification` upstream specs both pass 10/10 in the java gauge's `:driver-core:test` module. **Out of scope**: exact mongod error-string matching (chase work without a clear payoff unless a driver test pins exact wording).
- **`$where`** — runs JavaScript. We don't ship a JS runtime.
- **`$function`, `$accumulator`** (aggregation expressions) — same reason: both evaluate user-supplied JavaScript and need an embedded JS engine + sandbox + BSON↔JS shim layer. Would also require a `--javascriptEnabled` gate (mongod gates JS behind this; many prod deployments disable it). Adding the runtime would let us implement these three operators + `mapReduce` together for ~600–1000 LOC + a heavyweight binary dep (PythonMonkey / QuickJS / V8 — each with maintenance trade-offs). Most pipelines that reach for `$function` can be expressed in the existing aggregation expression library — `$cond` / `$switch` / `$let` / `$map` / `$reduce` / `$filter` / `$regexFind` / `$concat` / `$substrCP` / `$dateToString` / etc. — which is more expressive than people often realise. **`lang: "python"` as a SecantusDB extension was considered and rejected (May 2026)**: would break the conformance contract (CLAUDE.md "pymongo cannot tell SecantusDB apart from mongod" — pipelines that work locally would explode in production with `Unrecognized lang value: 'python'`), and CPython's sandboxing story is actually worse than embedded JS engines (no per-context isolation primitives, no reliable cross-platform CPU-time interrupt, `RestrictedPython` is an AST rewriter not a true sandbox). If a real user need ever surfaces, the right escape hatch is a server-side trusted-plugin registry — `{$secantusFunction: {name: "<pre-registered>", args: [...]}}` — not user code at query time.
- **`mapReduce`** — same JS-runtime dependency as `$where` / `$function` / `$accumulator`. Also explicitly deprecated by MongoDB (removed from the Stable API in 5.0; recommended migration path is aggregation pipelines). `commands._map_reduce` recognises the canonical `emit(this.<field>, 1)` + `values.length` "count by field" pattern (the shape mongo-java-driver's `testMapReduceWithGenerics` test exercises) and translates it to an equivalent `$group` aggregation. Non-canonical map / reduce bodies return `{results: [], ok: 1}` so wire-shape probes pass, and `out: "<coll>"` (non-inline) is rejected with FailedToParse. Anything that genuinely needs JS evaluation needs a real `mongod`.
- ~~Capped collections~~ — implemented. `create capped: true, size, max` accepted; `Storage.insert` and `Storage.update_matching` enforce FIFO eviction by walking the doc table in natural order and evicting oldest non-fresh docs while bounds are exceeded. `listCollections` surfaces `options.{capped,size,max}`. Eviction emits oplog `op:"d"` entries (and pre-images when enabled) so change streams observe the deletes. **Known limitation (Python server only):** Python's `_enforce_capped_bounds_locked` evicts in `_id_key` order, which equals insertion order only when `_id` is monotonic (the default `ObjectId`); with user-supplied non-monotonic `_id`s the wrong docs are evicted (not strict FIFO). The Python natural-order index (`_scan_docs_natural`) exists and is used for `find`/`$natural` — the fix is to route eviction through it too (one-line: `_scan_docs` → `_scan_docs_natural` in `_enforce_capped_bounds_locked`). **The Rust server already does true FIFO** (beta.92, `scan_docs_natural` — see §7.3).
- ~~Profiling~~ — implemented. `profile` command (-1 / 0 / 1 / 2 with `slowms` + `sampleRate`) sets per-database state in `secantus_profile_settings`. Dispatch wraps each non-skip command in `time.monotonic_ns` timing; if the per-DB level matches, an entry is inserted into `<db>.system.profile` (auto-created capped 10 MB). Recursion guard skips ops against `system.profile` itself + handshake / cursor-continuation / profile-itself commands. Entry shape mirrors mongod (`ts`, `op`, `ns`, `command`, `millis`, `ok`, `client`, optional `user`, `errMsg` / `errCode` on failure). Out of scope today: `planSummary` / `keysExamined` / `docsExamined` / `nreturned` (would need post-handler stats plumbing).
- ~~Tailable / awaitData cursors~~ — implemented for change streams (see "In scope" in `CLAUDE.md`) **and** for plain capped collections + `local.oplog.rs` (`commands._find_tailable` / `_find_tailable_oplog`, blocking `getMore` on the oplog condition variable). The producer re-applies the find filter (with `let` vars + collation) to follow-up inserts, advances its watermark by `id_key`, and raises `CappedPositionLost` (136) on rollover.

## 5. Known bugs and edge cases to watch

Subtler than the above; these may bite specific test suites.

- [ ] **macOS CI test job hangs to the 6-hour kill (root cause unconfirmed).** The `test (macos-latest, 3.10)` job hit GitHub's 6-hour job timeout repeatedly (≥3× across unrelated PRs, including a *tooling-only* one), while passing in ~4 min on Linux/Windows and locally on macOS. Mitigated, not fixed: `9e16e49` dropped macOS from the continuous (push/PR) matrix, so it now runs **only at release time** (`publish.yml` / scheduled sweep) — where the hang can still bite. The per-test `timeout` (pytest-timeout, 600s) does **not** cover the wedge because it only guards a test's own body, not collection / session-scoped fixtures / xdist worker *shutdown*. **Prime suspect:** a daemon/thread not reaped on macOS keeping the worker process alive after its tests "finish" — most likely the Rust server's `stop()` / accept-thread join or a parked change-stream tailable `getMore` (the same shape as the *Python* `SecantusDBServer.stop()` use-after-free fixed in 0.5.3b5, below; `test_rust_server_stress.py` / `test_rust_pitr_cross_server.py` exercise the Rust lifecycle). **Diagnostics added** (`ci-macos-hang-guard`): a `timeout-minutes: 30` cap on the test job + a `faulthandler` session watchdog in `tests/conftest.py` that dumps every thread's stack and exits at 25 min — so the next occurrence names the wedged thread/test instead of dying silent. Close once a stack identifies the culprit and the lifecycle leak is fixed.
- ~~**Intermittent pytest-xdist worker crash at ~97% of full suite (post-b18).**~~ Fixed (0.5.3b5). Root cause: `SecantusDBServer.stop()` joined only the accept thread, then closed WiredTiger while per-connection daemon threads could still be mid-WT-operation (e.g. a change-stream tailable `getMore` reading the oplog) — a use-after-free that surfaced as the native worker crash ("node down: Not properly terminated"). `stop()` now closes connection sockets, wakes parked tailable getMores (`Storage.signal_shutdown`), and waits for the active-connection count to drain to zero before `storage.close()`. Reproduced deterministically (a connection thread in a tight WT-read loop vs `storage.close()` raised `Cursor_reset ... is None`, the Python-surfaced form of the same use-after-close); a 200-iteration stress now runs clean. Regression guard: `tests/test_server_shutdown.py`.
- ~~**Rust server `WT_PANIC` under concurrent start/stop (cross-server PITR flake).**~~ Fixed (Rust 0.5.3-beta.48). Root cause was the Rust analogue of the Python shutdown bug above: `RunningServer::stop()` signalled the flag and joined only the accept loop — the detached per-connection threads (each holding an `Arc<Storage>`) weren't waited for, so the WiredTiger connection didn't close until one of them later exited, and that connection's final close-checkpoint then raced the caller removing / reopening the data dir (`WiredTigerHS.wt: stat: No such file` → "the checkpoint failed, the system must restart: WT_PANIC"). `stop()` now drains a live-connection counter (`Shared.active`, an independent `Arc<AtomicUsize>` so a thread releases its storage ref *before* decrementing) to zero — bounded by a 10s deadline — before returning, making teardown synchronous and the data dir quiescent. Reproduced deterministically with the new `bench/wt_stress.py` (`invoke rust-stress` — 24 of 64 concurrent cycles panicked before, 0 after). Regression guard: `tests/test_rust_server_stress.py`; the previously-deselected `tests/test_rust_pitr_cross_server.py` cross-server tests now pass under `-n auto`.
- ~~**`$type: "int"` / `"long"`**~~ fixed (b29). `_TYPE_PREDS` keys on `isinstance(v, bson.Int64)` rather than Python value range — pymongo's BSON decoder preserves the int32/int64 distinction by class (int32 → plain `int`, int64 → `Int64`), so a doc inserted as `Int64(5)` now matches `$type: "long"` (not `"int"`). `$convert: {to: "long"}` returns `Int64` so its output round-trips correctly through the type predicate.
- [ ] **`$lookup` simple-form-plus-pipeline** — when both `localField`/`foreignField` and `pipeline` are present, we pre-filter by the simple form and then run the pipeline. Real MongoDB does this too in modern versions, but the documentation isn't crystal clear on the order. If a test breaks here, this is the place to look.
- [ ] **Aggregation `$group` stable order** — group buckets are emitted in first-seen order, not sorted. Matches MongoDB for unsharded but might differ from sharded behavior (which we don't model).
- ~~**`apiStrict: true` enforcement Java pool-clear cascade**~~ resolved (0.5.2b3) by narrowing the gate instead of the broad-whitelist invert. A focused `_API_V1_REJECTED_BY_NAME = {"distinct"}` rejects only the canary command the spec's unified runners actively probe (mongo-java-driver `crud-api-version-1-strict.yml` `distinct appends declared API version`). Empirical Java-gauge run: +1 pass for the canary, **zero** new failures and zero pool-clear symptoms across the 900-test suite. The previous cascade theory (broad whitelist would invalidate the pool through SDAM) is correct for the broad path but doesn't trigger from a single command rejection — the broad invert also rejected `count` (used internally by `estimatedDocumentCount`) and other handshake-adjacent admin commands, which is the actual mechanism for the 6 cascade failures, not pool-clear semantics. The narrow gate sidesteps that entirely.
- [ ] **Java gauge: 5 remaining failures are all driver-internal / out-of-scope (triaged 2026-06-30, Python server, HEAD d8e75ff).** A broad multi-gauge survey left java the only gauge with fresh failures (5; node/ruby's single fails are the known text-index + single-node-`w:2` artifacts; rust/kotlin/dotnet/cxx are 100%). All 5 were triaged and proven **not** to be server divergences — driving the same operations via pymongo against an on-disk daemon produced exactly the spec-expected wire replies, and the pymongo gauge passes the *identical* upstream command-monitoring / versioned-api spec files (which assert the same event counts → the server induces no extra round-trips). Recorded in `validation_summary/expected_failures.py` (`JAVA` list):
  - `ClientMetadataTest … metadata append does not create new connections …` — client-side `appendMetadata` crosses no wire; driver connection/handshake logic. Not server-fixable.
  - `VersionedApiTest … find and getMore append API version` — asserts the *driver* decorates outbound find/getMore with `apiVersion:"1"`; SecantusDB already accepts the serverApi fields. pymongo passes the identical `crud-api-version-1` spec.
  - `CommandMonitoringTest … A successful deleteMany` — server reply is spec-correct (`delete` → `{ok:1, n:2}`); pymongo passes the identical spec. Java-driver event accounting vs standalone topology.
  - `CommandMonitoringTest … A successful find with a getMore` — server emits exactly the spec find→getMore wire sequence (firstBatch 3 + Int64 id, then nextBatch 2 + id:0, no extra round-trip); pymongo passes the identical spec.
  - `ConnectionPoolLoggingTest … Create a client, run a command, and close the client` — asserts the Java driver's CMAP connection-pool *log messages*; the server emits no log lines over the wire. Out of scope.
- [ ] **Go gauge flake: `TestIndexView/drop_one` + `drop_all` server-selection timeouts** — **mitigated at the runner (2026-06-26), not server-fixable.** Root cause (not a server bug): under `validate-all`'s multi-gauge CPU / socket-buffer contention the daemon briefly misses a heartbeat and the Go driver's 30s server-selection deadline lapses mid-test (`context deadline exceeded`, topology `Type: Unknown`). Mitigation: `go_validation/runner.py` points the gauge `MONGODB_URI` at `serverSelectionTimeoutMS=60000`, so the transient blip is ridden out; a genuinely unreachable daemon still fails inside the 30m package timeout. **Unverified in its actual failure condition** (it only fires under the full `validate-all` fan-out, which wasn't re-run); the bump is a low-risk, well-motivated mitigation, not a confirmed fix. (History: surfaced 2026-05-14; the per-collection-lock-deadlock hypothesis was ruled out 2026-06-15 by a clean 12-thread DDL+CRUD stress — the cause was always cross-gauge resource exhaustion, not a SecantusDB fault.)
- [ ] **Go gauge flake: `TestChangeStream_ReplicaSet/try_next/one_getMore_sent`** — **NOT server-fixable; confirmed go-harness / load-timing artifact (2026-06-26, with new hard evidence).** Fails intermittently (~1 in 3 full gauge runs) with `TryNext returned true on iteration 1` (elapsed ~0.3s instead of 1.0s): the first `getMore` on a freshly-opened, supposed-empty collection-scoped stream returns an event. **Two runner-side fix attempts both FAILED** (proving it isn't what their hypotheses assumed): (a) isolating change-stream tests from the rest of the suite still flaked 1/3; (b) running each change-stream top-level function in its own serial `go test` process *still* flaked 1/2. **The server is provably correct under load** (the decisive 2026-06-26 evidence — all on-disk, matching the gauge): a collection-scoped stream on an untouched collection saw **0** events across 400 polls while 8 threads did 2074 concurrent writes to other collections; and **0** events across 300 fresh stream-opens under heavy create/drop/insert churn (4110 ops). The flake also does **not** reproduce in any isolated loop (try_next alone 15/15; whole `TestChangeStream_ReplicaSet` group, on-disk, `-parallel=1`, 12/12; faithful Standalone-then-ReplicaSet on-disk with the gauge skip list, fresh daemon per iter, 10/10). It only manifests under the *full-gauge* concurrent load. Net: the change-stream wire/scope behaviour is correct; the flake is a timing/scheduling property of the shared-daemon mongo-go-driver mtest harness that the runner cannot suppress without per-test isolation that even then doesn't hold, nor without editing the vendored tests (forbidden). **Accepted.** The §5 verdict below has the harness-mechanism analysis.

  **Repro session 2026-06-15 — verdict: test-harness artifact, not a SecantusDB scope-filter bug; accepted.** Two things were established. (1) **The change-stream scope filter does not leak.** A direct stress — a collection-scoped `watch()` open on `db.A` while a writer hammered `db.B` on the same shared `:memory:` daemon — polled 1650 times and saw **0** cross-collection events. The projection layer (`changestreams.project`) and `_ns_filter` correctly confine a collection-scoped stream to its own namespace. So the `TryNext returned true on iteration 1` is *not* SecantusDB surfacing a foreign write through a mis-scoped filter. (2) **The Go mtest harness shares one namespace across tests and truncates collection names to a colliding suffix.** `dbName` defaults to a single shared constant `TestDB` for every test (`mongotest.go:117-118`), and `collName = t.Name()` is then truncated to its *trailing* bytes to fit the 120-byte namespace cap (`sanitizeCollectionName`, `mongotest.go:591-602`: `coll = coll[len(coll)-remaining:]`). Two different long subtest names can therefore collide on the same `TestDB.<suffix>` collection. Combined with the parallel top-level `Test*` functions that call `t.Parallel()` (encryption-prose, `TestClient_BSONOptions`) writing during the change-stream's await window, a genuinely same-namespace write from a *concurrent test* can legitimately wake the stream early — which is correct server behaviour, not a leak. This reproduces only under the one-shared-daemon gauge (each test gets its own server in normal CI), and is invisible running `TestChangeStream_ReplicaSet` alone (30/30 pass). Not patchable on the SecantusDB side without breaking conformance; the honest fix is harness-side namespace isolation, which would mean editing the vendored submodule (forbidden — defeats the gauge). Left documented and accepted.
- [ ] **Ruby gauge: `Index::View#create_one with session` test client-side-stripped** — mongo-ruby-driver's `Mongo::Index::View#create_one when provided a session behaves like a failed operation using a session raises an error` test passes `view.create_one(spec, invalid: true)` and expects an `OperationFailure` to come back from the server. But the Ruby driver's `Options::Mapper.transform` filters the model hash against its `OPTIONS` whitelist (`lib/mongo/index/view.rb:61`) **before** the command is built, so `invalid: true` never reaches the wire. We added unknown-spec-option rejection on `createIndexes` (`commands.py:_INDEX_SPEC_KNOWN_OPTIONS` + the `Location40415` gate in `_create_indexes`) which DOES fire when the option arrives, so this is a working server-side guard — the test is just structurally broken against modern Ruby drivers. Real mongod has the same problem; the test would need the driver to keep `invalid: true` in the spec for the server-side rejection path to be reachable. Documented and accepted.
- [ ] **Ruby gauge: `applies the write concern passed in as an option` expected-fail under single-node topology** — mongo-ruby-driver's `Mongo::Collection#create ... when write concern passed in as an option` test (`spec/mongo/collection_ddl_spec.rb:211`) explicitly passes `w: 2` to `collection.create` and expects success — it assumes the canonical multi-node replica-set test cluster the Ruby driver's CI runs against. SecantusDB advertises as a single-node `secantus` replica set, so `w: 2` produces a `writeConcernError` (code 100, `CannotSatisfyWriteConcern`) — added in `commands.py:_unsatisfiable_wc_error` + dispatch wire-up. This is the correct mongod emulation; the test is structurally incompatible with our topology. **Net trade-off was +7 Ruby gauge passes**: seven `applies the write concern` tests that pass `INVALID_WRITE_CONCERN = {w: 4000}` and expect `OperationFailure` now pass because of the wce, this one test now fails. If the test cluster ever grows past 1 advertised member, this test will start passing organically.
- [ ] **PHP gauges landed (2026-06-15) — conformance gaps surfaced, not yet fixed.** Two new gauges: `php_ext_validation` (mongo-php-driver `.phpt`, the low-level C extension that wraps libmongoc — strictest wire-protocol gauge alongside Go) at **99.9%** (670/671 ran, 41 skipped — climbing: dup-key `errmsg` fix 0.5.3b9, cursor open-count 0.5.3b11, insertion-order `find` 0.5.3b13) and `php_lib_validation` (mongo-php-library PHPUnit, the high-level `mongodb/mongodb` package) at **98.8%** (3051/3091 ran, 39 skipped — climbing: `explain` 0.5.3b8, `collMod` TTL 0.5.3b10, `count` hint + 2dsphere 0.5.3b12, insertion-order `find` 0.5.3b13). Submodules pinned to the installed extension version (driver `2.3.1`, library `2.3.0`); the ext gauge runs against the already-installed extension via `run-tests.php` (no rebuild). Real divergences to chase (none block the gauge feature):
  - **`updateOne` / `deleteOne` (multi=false, no sort) pick the `_id`-order-first match, not the insertion-first one** — the sibling of the now-fixed `find()` insertion-order item (0.5.3b13). **Deprioritised / likely won't-do** after a scoping attempt (2026-06-16): only differs from mongod for an unindexed, multi=false update/delete over **non-monotonic `_id`s** — an extreme corner with no test/gauge consumer. A correct fix is *not* a one-line scan swap: (1) the common no-collation path goes through `_candidates_iter`, which collscans in `_id` order (`storage.py` ~`return list(self._scan_docs(...))`), so it'd need changing too (and `_candidates_iter` is shared by other callers); (2) the `list()` materialisation in the candidate loop is **load-bearing for WiredTiger cursor safety** (the loop writes to the doc table mid-iteration, which invalidates a still-walking scan cursor on the same session — see the comment at `update_matching`), so a lazy/short-circuit version must collect-matches-then-write, not stream-and-write. Net: a write-path refactor (concurrency-sensitive) for near-zero observable benefit. Leave as documented divergence unless a real consumer appears.
  - ~~**capped-collection tailable cursors** — php-ext `cursor-tailable_error-001`~~ **FIXED (0.5.4b17)** — the test opens a `tailable` query on a capped collection, iterates with awaitData polling, and expects a "collection dropped" error when the coll is dropped mid-iteration. Capped tailables ship (`e187fb7` + the 0.5.4b16 filter fix); dropping the collection now **tombstones** open tailable cursors (`CursorRegistry.kill_namespace` sets a `dropped` flag instead of removing them) so the next `getMore` returns `QueryPlanKilled` (175) "collection dropped: <ns>" — the message the php-ext test asserts. Non-tailable cursors are still removed (→ `CursorNotFound` 43, per mongo-c-driver's `error_document/getmore`). Regression: `tests/test_crud.py::test_tailable_drop_returns_collection_dropped`. (The sibling `cursor-destruct-001` — killCursors-on-destruct via the live `serverStatus.metrics.cursor.open.total` count — shipped in 0.5.3b11.)
  - Transaction-gated cases skip/fail under single-node topology (expected, same class as the Ruby `w:2` note above).
- [ ] **mongo-c-driver (`libmongoc`) gauge landed (2026-06-19) — conformance gaps surfaced, not yet fixed.** New `c_validation` gauge builds the vendored driver's `test-libmongoc` from source (CMake) and runs a curated set of wire-protocol suites against an embedded daemon over `MONGOC_TEST_URI`. Driver pinned to `1.30.8`. Current (after the 0.5.4b9 fixes below, gauge-verified): **712 pass / 21 fail / 69 skip**; was **707 / 26 / 69** at landing — +5 from the five fixes below, zero regressions. The failure set is byte-identical across repeated full runs (deterministic, not flaky). 8 of the 26 are documented in `validation_summary/expected_failures.py` (`C` list — the RS-primary `hello` advertisement makes libmongoc's standalone/secondary server-type and `lastWriteDate`-absent assertions fail; IPv6 needs a v6 listener). The remaining ~18 are real divergences to chase (none block the gauge):
  - **`maxMessageSizeBytes` split boundary** (`/BulkOperation/OP_MSG/max_msg_size`, "2 == 1") — a bulk write the C driver expects to split into 2 OP_MSGs goes out as 1; SecantusDB's advertised max-message-size / write-batch limits don't force the split. (Driver-side split decision; not addressed.)
  - ~~**`bypassDocumentValidation` on aggregate `$out`**~~ **FIXED (0.5.4b9)** — `$out` / `$merge` now enforce the destination collection's `validator` (when `validationAction: "error"`) unless the command set `bypassDocumentValidation`, raising `DocumentValidationFailure` (121). See `aggregate._enforce_target_validator` (`bypass_validation` threaded through `PipelineContext`).
  - ~~**getMore error-document shape**~~ **FIXED (0.5.4b9)** — dropping (or renaming) a collection now kills its open cursors (`CursorRegistry.kill_namespace`, called from `_drop` / `_rename_collection`), so a later `getMore` fails with `CursorNotFound` (43) instead of serving stale snapshot rows.
  - ~~**Decimal128 `batchSize`**~~ **FIXED (0.5.4b9)** — `find` / `aggregate` coerce a numeric `batchSize` of any BSON type via `_coerce_command_int` (Decimal128 → underlying Decimal → int).
  - ~~**Capped-collection tailable timeout** (`/Collection/tailable/timeout/single`)~~ **FIXED (0.5.4b16)** — not a capped-tailable gap (capped tailables already shipped, `e187fb7`): the tailable producer was returning follow-up inserts *unfiltered*, so a cursor watching `{a:1}` surfaced the unrelated `{}` doc and `mongoc_cursor_next` returned a doc instead of nothing. `commands._find_tailable` now re-applies the find filter to scanned rows. Regression: `tests/test_crud.py::test_tailable_await_filter_applies_to_follow_up_inserts`.
  - ~~**Namespace-length enforcement**~~ **FIXED (0.5.4b9)** — `dispatch` rejects any command whose database component exceeds 63 bytes with `InvalidNamespace` (73).
  - ~~**`collMod` error response** (`/collection-management/modifyCollection-errorResponse`)~~ **FIXED (0.5.4b9)** — `collMod {index: {prepareUnique: true}}` arms an index (new dup writes → 11000 via `prepareUnique` honoured in `storage._unique_conflict`) and `collMod {index: {unique: true}}` over existing duplicates is refused with `CannotConvertIndexToUnique` (359) + a `violations: [{ids:[...]}]` array (`storage.find_index_duplicates` / `set_index_options`).
  - ~~**`writeConcernError` reporting** (`/Client/command_w_write_concern`, `/Database/create_with_write_concern`, `/command_monitoring/unified/writeConcernError`)~~ **FIXED (0.5.4b13)** — per-test triage showed these were the `w: 99` case, resolved by the `w > 50` parse-error fix above. (The grouping was imprecise: the remaining two failures in the original list are unrelated and split out below.)
  - ~~**`/Collection/index_w_write_concern`**~~ **FIXED (0.5.4b14)** — not a write-concern issue at all: after the `w > 50` fix this test fails on its *invalid-index* assertion — it creates `{abc: "hallo thar"}` and expects the server to reject it. `storage.create_index` now rejects an index-key string that isn't a recognised plugin (`2d` / `2dsphere` accepted; `text` / `hashed` out-of-scope) with `CannotCreateIndex` (67) "Unknown index plugin '<value>'". Regression: `tests/test_driver_gaps.py::test_unknown_index_plugin_rejected`.
  - **`/command_monitoring/unified/writeConcernError`** — **flaky, not deterministic** (0.5.4b15 triage): a fresh full-run repro of the curated C include set has it **passing** (only `/Client/ipv6/single` + `/Collection/tailable/timeout` fail there now), so the earlier gauge report that listed it was a flaky red, not a reproducible state leak. The failpoint registry (`secantus.failpoints`) *is* per-server with no `appName` scoping, so a leaked `failCommand` from a prior test is a plausible mechanism, but it couldn't be reproduced in any isolable subset. Left as a watch item; if it recurs, add per-`appName` failCommand scoping (the mongod-faithful isolation) + audit failpoint teardown. **A real, separate bug found during this triage was fixed in 0.5.4b15**: a malformed `$and`/`$or`/`$nor` (non-array / empty / non-doc element) crashed the query engine into a generic `InternalError` instead of `BadValue` — see `query._match_clause` + the changelog.
  - ~~**State-ordering-dependent drop/rename/create** (`/Collection/drop`, `/Collection/rename`, `/Collection/index`, `/Database/drop`)~~ **FIXED (0.5.4b13)** — not state-ordering at all (misdiagnosis): each test ends with a DDL op carrying `writeConcern: {w: 99}` and asserts `assert_wc_oob_error` — for a server >= 4.3.3 (we advertise 7.0) that's `FailedToParse` (9) "w has to be a non-negative number and not greater than 50", because mongod caps numeric `w` at 50. SecantusDB was returning a `CannotSatisfyWriteConcern` (100) writeConcernError on a success instead. `commands._validate_write_concern` now rejects `w` outside `[0, 50]` with code 9 before the command runs (and `_drop_database` / `_rename_collection` now call it). Regression: `tests/test_crud.py::test_write_concern_w_above_50_is_parse_error`.
  - ~~**Atlas Search index management** (`/index-management/{list,drop,update,create}SearchIndex`)~~ **FIXED (0.5.4b18)** — `createSearchIndexes` / `updateSearchIndex` / `dropSearchIndex` commands and the `$listSearchIndexes` aggregation stage (+ `$search` / `$searchMeta` / `$vectorSearch`) are Atlas-only; a non-Atlas mongod fails them with a message naming Atlas. Now rejected with `CommandNotSupported` (115) + the shared `aggregate.SEARCH_INDEX_ATLAS_MSG` (the tests assert `errorContains: "Atlas"`). Regression: `tests/test_crud.py::test_atlas_search_index_commands_rejected` + `tests/test_aggregate.py::test_atlas_only_stage_rejected_with_atlas_message`.
  - **Change streams excluded** — see §3.2 (the C-driver fixture would need a fuller fake-replset `replSetGetStatus` reporting ≥1 member; the standalone error we ship makes those tests skip).
  - **Fresh gauge number (0.5.4b18):** 720 pass / 13 fail / 69 skip. The 13 remaining are all documented-expected (ipv6 ×2, `last_write_date_absent` ×2, `select_server` ×4 — all RS-primary `hello` artifacts in `expected_failures.py`) or known/deferred (`BulkOperation/max_msg_size` driver-side split; `command_monitoring/writeConcernError` flaky). No clean actionable server divergences remain in this gauge.
- [ ] **mongo-cxx-driver (`mongocxx`) gauge landed (2026-06-19) — conformance gaps surfaced, not yet fixed.** New `cxx_validation` gauge builds the vendored libmongoc (installed to a prefix) + the mongocxx `test_driver` Catch2 binary from source and runs it against an embedded daemon. Drivers pinned: mongocxx `r3.11.0` on libmongoc `1.30.8`. mongocxx's core tests hard-wire `mongodb://localhost:27017` (no `MONGOC_TEST_URI`-style override), so the gauge binds the daemon on **27017** (refuses to run if occupied). At landing: **880 pass / 9 fail / 9 skip (99.0%)** (Catch2 expands SECTIONs, so the testcase total > TEST_CASE count). After the 0.5.4b8 dup-index fix + the 0.5.4b9 fixes below + the 0.5.4b11 resume-token fix + the 0.5.4b12 `$currentOp` clientMetadata fix, the cxx gauge's real failures are **closed** (the prior `client metadata handshake` ×2 are fixed below). The remaining divergences to chase (none block the gauge):
  - ~~**Duplicate-index conflict not rejected** (3 tests)~~ **FIXED (0.5.4b8)** — `createIndexes` now rejects same-name-different-key with `IndexKeySpecsConflict` (86) and same-name-different-options with `IndexOptionsConflict` (85), and returns `note: "all indexes already exist"` on an identical re-create so drivers see the no-op (`storage.create_index` + `commands._create_indexes`).
  - ~~**`$out` not last in pipeline not rejected**~~ **FIXED (0.5.4b9)** — `apply_pipeline` rejects a non-terminal `$out` / `$merge` with `Location40601` (40601) before executing any stage.
  - ~~**Change-stream invalid-pipeline error timing**~~ **FIXED (0.5.4b9)** — `validate_stage_names` now also validates `$match` filter syntax (unknown query operators → `QueryError`) up-front, so a change stream opened with `{$match: {$foo: -1}}` errors at aggregate (`.begin()`) time rather than at the first `getMore`.
  - ~~**Change-stream resume-token continuity** (`Spec Prose Tests/1. ChangeStream must continuously track the last seen resumeToken`)~~ **FIXED (0.5.4b11)** — `postBatchResumeToken` now reflects the resume token of the last event *actually returned* in each batch (not the producer's prefetch tail), so per-batch tokens advance under any `batchSize`; and an empty `getMore` only re-mints the token when the oplog tail genuinely moved, so an exhausted quiet stream reports the same token as its last event. `commands._change_stream_cursor_doc` (PBRT = `batch[-1]["_id"]`) + the producer's guarded empty-batch advance; regression `tests/test_change_streams.py::test_resume_token_tracks_per_event_with_batch_size_one`. (Distinct from the Rust-server change-stream resume fixes in 0.5.3-beta.53/54: the open now polls once so already-available events ride the **firstBatch** — pymongo's `_has_next()` only inspects the client buffer, so a resumed stream with an empty firstBatch read as "no changes" — and `resumeAfter` on an **invalidate** token is rejected with `InvalidResumeToken` (260) via a `from_invalidate` marker in the token, while `startAfter` is allowed. Closes pymongo `test_start_after` ×2 + `test_resumetoken_uniterated_nonempty_batch_*` ×2.)
  - ~~**Client-metadata handshake** (`integration tests for client metadata handshake feature/with client`, `/with pool`)~~ **FIXED (0.5.4b12)** — the test connects with `?appName=xyz` and scans `db.aggregate([{$currentOp: {}}])` for an op whose `appName` matches, then reads its `clientMetadata.{application,driver,os}`. The `$currentOp` *aggregation stage* (`aggregate._stage_current_op`) was a bare stub; it now surfaces the connection's `clientMetadata` + a top-level `appName` (threaded via `PipelineContext.client_metadata` from the connection registry), like the `currentOp` command already did. Regression: `tests/test_hello_client_metadata.py::test_aggregation_currentop_surfaces_appname_and_metadata`.
  - Out-of-scope tags excluded in `cxx_validation/include_paths.py` (CSFLE, Atlas, search indexes, transactions, sessions, SDAM monitoring, and `[uri_options]` which needs the `URI_OPTIONS_TESTS_PATH` spec-data dir).
- [ ] **mongo-csharp-driver (C# / .NET) gauge landed (2026-06-19) — one conformance gap surfaced.** New `dotnet_validation` gauge runs the vendored driver's xUnit `MongoDB.Driver.Tests` via `dotnet test` against an embedded daemon over `MONGODB_URI`. Driver pinned `v3.9.0`; scoped to the **CRUD specification suite** (`MongoDB.Driver.Tests.Specifications.crud`) via `--filter` — `MongoDB.Driver.Tests` as a whole is enormous and dominated by non-server unit tests (LINQ/serialization) plus external-service suites (CSFLE/KMS, auth, Atlas Search, load balancing) and multi-node features (transactions, sessions, SDAM, retryable). At landing: **201 pass / 1 fail / 26 skip (99.5%)** (the 26 are `[RequireServer]`/CSFLE-gated skips). After the 0.5.4b9 validation-detail fix below, gauge-verified at **202 pass / 0 fail / 26 skip**. The one (now-fixed) divergence:
  - ~~**Document-validation error detail** (`CrudProseTests.WriteError_details_should_expose_writeErrors_errInfo`)~~ **FIXED (0.5.4b9)** — a failed query-expression validator now synthesises mongod's per-operator `errInfo.details` (`operatorName` / `specifiedAs` / `reason` / `consideredValue` / `consideredType`) via `commands._validation_failure_details` + `query.bson_type_name`, used by both the insert path and `_validate_doc_against_collection`. ($jsonSchema validators still report a minimal `{operatorName: "$jsonSchema"}` — their schema-rules detail is unsynthesised.)
  - The CRUD-only scope is deliberate and expandable — broaden the `--filter` in `dotnet_validation/include_paths.py` to add more spec families (e.g. `read_write_concern`, `change-streams`) as they're validated. Build note: the driver's `MongoDB.Driver.Encryption` project verifies a downloaded libmongocrypt with **gpg** at build time, so `gpg` (and network for the libmongocrypt download) are build prerequisites even though CSFLE itself is out of scope.

> **Rust-server follow-up — RESOLVED (Rust 0.5.3-beta.73).** The 0.5.4b8/b9
> Python-server fixes that the Rust server didn't yet mirror have now all been
> ported into `crates/secantus-{core,commands,storage,storage-adapter}`, each
> with a real-WiredTiger integration test in `crates/secantus-storage-adapter/
> tests/`: `$out`/`$merge`-not-last (`Location40601`), change-stream `$match`
> validation at open (unknown operator → `BadValue` at `.begin()` not first
> getMore), document-validation `errInfo.details` (`consideredValue` /
> `consideredType` via a new `secantus_core::query::bson_type_name`),
> index-conflict codes `IndexKeySpecsConflict` (86) / `IndexOptionsConflict`
> (85) + the `note: "all indexes already exist"` no-op, and (earlier) Decimal128
> `batchSize`, over-long db-name rejection, `$out`/`$merge` target-validator
> enforcement, `collMod prepareUnique`→`unique` 359 violations, and cursor-kill
> on drop/rename. Driver gauges pointed at the Rust server no longer regress on
> these.

## 6. Admin UI review punch list

End-to-end review of the secantus-admin web UI on `main` (May 2026, before the `admin-ui` branch lands its next slice). Severity tiers: P0 broken/silently-wrong, P1 inconsistency or significant usability gap, P2 polish. File refs are absolute under `src/secantus/admin/` unless noted.

### P0 — broken / silently wrong

(None at present.)

### P1 — significant inconsistency / usability

- [ ] **`/backup/dump` and `/backup/restore` long-task UX** — calls `backup_lib.run_mongodump` / `run_mongorestore` synchronously. Spinner + disabled button covers the visible UX gap for normal-sized dumps; the ideal version is a real background-task wrapper with poll status so the user can navigate away during multi-minute dumps of large collections. Not load-bearing — defer until someone actually hits a multi-minute dump.

### P2 — polish

- [ ] **Admin UI polish bundle** — small fixes that don't deserve individual entries; address opportunistically when touching nearby code. (Currently no entries — the bundle was cleared in `admin-ui-rest`, May 2026. Drop new ones here as they show up.)

## 7. Python → Rust rewrite (in progress)

### 7.1 Rust server performance and security review (2026-06-16) — CLOSED

Items identified during a security/performance audit of the Rust server. Fixed in
0.5.3-beta.19: SCRAM timing oracle (`subtle::ConstantTimeEq`), mutex poisoning
panics (graceful error returns), double BSON decode in wire layer (length-only
check), response buffer pre-allocation, compound multikey cap (10k).

The remaining eight items (two security, six performance) were resolved in
0.5.3-beta.22:

- **SASLprep for non-ASCII passwords** — `secantus-auth` now applies RFC 4013
  SASLprep (via the `stringprep` crate) in `derive_credentials`, matching the
  Python server's `auth.saslprep` and what a compliant driver sends. `createUser`
  / `updateUser` surface a prohibited-character / bidi failure as `BadValue`.
- **Concurrent message allocation budget** — `secantus-server` carries a global
  `AllocBudget` (512 MB cap); each connection reserves `body_len` before
  allocating its body buffer and releases it once the message is answered, so a
  flood of concurrent large messages can't exhaust the heap.
- **Global storage lock during oplog scans** — the cross-thread readers
  (`read_oplog` / `read_preimage` / `oplog_floor_seq` / `oplog_tail_seq` /
  `find_seq_for_ts`) no longer take the global `Mutex` (their fresh WT session's
  MVCC snapshot is consistent on its own); `prune_oplog` scans lock-free and takes
  the lock only for the delete phase.
- **Document cloning in aggregation stages** — `$addFields`/`$set`/`$unset` mutate
  the owned doc in place (no clone), `$unwind` clones array elements one at a time
  instead of the whole array, and `$facet` reuses the input for its last
  sub-pipeline.
- **Oplog clones every written document** — insert / batch-insert / replacement
  update / upsert move the document into the oplog `o` field instead of cloning it.
- **Nested lock for timestamp minting** — `current_cluster_time` mints and
  snapshots the recovery meta under a single oplog-mutex hold. (The seq counter is
  deliberately NOT a bare `AtomicI64`: `wait_for_oplog` pairs `next_seq` with
  `oplog_cv`, whose lost-wakeup guarantee requires reading the counter under the
  same mutex as the wait.)
- **Query path resolution clones** — `query::resolve_path` (and the `query` /
  `geo` field-operator helpers it feeds) now return/accept borrowed `&Bson` rather
  than cloning every value at every path component.
- **Encode/decode round-trips in find→cursor→client** — `find` (with projection)
  and `aggregate` send the `firstBatch` straight to the wire as `Bson` and encode
  only the cursor *remainder* (`split_docs_into_cursor`), dropping the
  encode-then-decode of the docs the client receives immediately.

---

> ⚠️ **Direction changed — authoritative plan is now `tasks/rust-server-plan.md`.**
> The end-state is **two completely separate servers** (a pure-Python server and a
> self-contained Rust server with a thin embedded Python lifecycle handle), **not**
> the in-process `secantus.engine` selectable model the items below assume. The
> *porting* work recorded here (the pure-Rust crates) is still valid and is the
> Rust server's foundation; the `SECANTUS_ENGINE` selection / Python `Storage`
> adapter / `EngineFallback` items are **retired**. Next work is the Rust server
> itself (`rust-server-plan.md` §4, R1–R8), not more in-process selection surface.

Tracking the incremental rewrite (plan: `tasks/rust-server-plan.md` — north star;
`tasks/rust-rewrite-plan.md`; Phase 0 spike results: `tasks/rust-rewrite-spike-
findings.md`; Phase 3+4 scoping: `tasks/rust-rewrite-phase3-scoping.md`). The Rust
side is a Cargo workspace under `crates/`: the pure-Rust engine lib crate
`crates/secantus-core` plus the PyO3 bindings crate `crates/secantus-core-py`,
which builds the abi3 extension `_secantus_core` via maturin (`invoke rust-build`
/ `rust-test` / `rust-parity`).

**Both implementations are permanent (not a replacement).** The pure-Python
engines power the **Python server**; the Rust engines power the separate **Rust
server**. The in-process `secantus.engine` selection was first made inert
(0.5.3b3) and is now **fully removed** (0.5.3b14): the `secantus.engine` module,
the `--engine` CLI flag, the `SecantusDBServer(engine=...)` parameter, and the
`SECANTUS_ENGINE=rust` CI full-suite step are all gone. The
`tests/test_rust_*_parity.py` oracle still imports `_secantus_core` directly to
pin the Rust engines against the pure-Python ones.

- [x] **Rust numeric type preservation** — DONE (0.5.3-beta.22). `secantus-core`
  `numeric::int_promoted_to_bson` + `is_int64` encode `$inc` / `$mul` (update
  engine) and `$sum` (group accumulator — `Num::Int` now carries a `wide` flag)
  with mongod's promotion (int64 if any operand is int64 or a 32-bit result
  overflows), matching `secantus.numerics`. The `_norm_int_width` normaliser is
  dropped from `tests/test_rust_update_parity.py` (it now compares BSON subtypes
  directly). The expression-language `$add`/`$subtract`/`$multiply` deliberately
  still narrow to match the pure-Python `expressions` fold (which doesn't promote
  either) — making *both* engines promote there is a Python-server behaviour
  change, out of scope for this parity fix.

**Rust server build-out (`tasks/rust-server-plan.md` §4).** Done: R1
(`secantus-wire`), R2a (dispatch framework + handshake family), R2b (`insert` /
`delete` / `count` + the `Storage` trait seam), R3a (`CursorRegistry` +
non-tailable `getMore` / `killCursors`), `find` (read path: skip/limit/projection
/ cursor split → the full `find → getMore → killCursors` path works in dispatch),
`update` (document-form, multi/upsert, pipeline-shape validation), R4a
(`secantus-server`: accept loop + connection handling, generic over the command
`Storage` trait — runs over real TCP, two WT-free roundtrip integration tests),
**R4b + R6 (merged in PR #31, CI-green across Linux/macOS/Windows): the WT adapter
and the `_secantus_server` embedded handle — pymongo → Rust → WiredTiger works.**
`aggregate` (storage-independent pipeline via `secantus_core::apply_pipeline`;
`count_documents` + direct pipelines pass through pymongo).
**R7 (the standalone `secantusdb` binary): `crates/secantusdb`** — args module in
`secantus-server` (WT-free, 11 unit tests) + a WT-linked bin (open Storage →
adapter → bind → print address → SIGINT/SIGTERM → clean stop), smoked by
`tests/test_rust_binary_smoke.py` / `invoke rust-binary-test` and the
`storage-engine` CI job (Linux/macOS).
**Distribution:** the binary ships two ways. (1) Prebuilt static-WiredTiger
archives on a GitHub Release via `.github/workflows/release-binaries.yml`
(tag `secantusdb-v<crate-version>`; x86_64-linux-gnu + aarch64-apple-darwin;
the file inside each tarball is named `secantusdb-rs`). (2) Bundled INTO the
`secantus` wheel as the `secantusdb-rs` command (non-Windows): `CMakeLists.txt`
installs it into `SKBUILD_SCRIPTS_DIR` under the `SECANTUS_BUILD_STORAGE_ENGINE`
flag, so a flag-on wheel puts `secantusdb-rs` on PATH (distinct from the
pure-Python `secantus.cli:main` `secantusdb` console script). The
`storage-engine` CI job asserts the bundled `secantusdb-rs` runs.
**Shipping wheels now flag-ON (0.5.4b1):** `wheels.yml` + `publish.yml` build
with `SECANTUS_BUILD_STORAGE_ENGINE=ON`, so `pip install secantus` bundles the
`_secantus_storage` / `_secantus_server` extensions **and** `secantusdb-rs` on
**Linux (manylinux_2_28 + musllinux_1_2, x86_64 + aarch64), macOS arm64, and
Windows AMD64** — every shipped wheel except Intel macOS (no wheel target).
Toolchain in `[tool.cibuildwheel].before-build`: Linux swig+clang-libs+rustup
with libclang symlinked to `/opt/libclang`; macOS swig (Xcode's libclang —
Homebrew's breaks the WT bindings); Windows choco swig+llvm with `libclang.dll`
copied to a space-free `C:/libclang`. `RUSTFLAGS=-Ctarget-feature=-crt-static`
for musl cdylibs. Build perf: `SECANTUS_CARGO_TARGET` shares one cargo target dir
across all wheels in a job (and rides the ccache mount/cache on Linux/macOS) so
the Rust deps compile once, not per-wheel — cut wall-clock ~72min → ~35min.
Windows paths are forward-slashed so CMake doesn't eat the `\c`. Verified: built
manylinux + Windows wheels contain `secantusdb-rs`(`.exe`) under
`*.data/scripts/` plus the two extensions. **Remaining:**
- [ ] **macOS x86_64 / Intel** stays pure-Python (no wheel target — runner-pool
  scarcity), so Intel-Mac pip users don't get the Rust bits.
- [ ] **Release fragility:** every wheel build now does cargo crates.io
  downloads in-container; a transient network failure (seen once on macOS) can
  fail a `publish.yml` release. The shared `SECANTUS_CARGO_TARGET` registry cache
  reduces re-downloads within a job; cargo vendoring / a registry mirror would
  remove the risk entirely.
**Deferred / not yet ported:**
- [ ] **R7 tail** — a Windows standalone binary (`secantus-wt`'s `build.rs`
  probes `libwiredtiger.a/.so`; the MSVC wheel build produces neither name, so
  the bin builds only where the .a exists — wheel-bundled `_secantus_server`
  covers Windows); the Python CLI's TOML config layer + tuning flags
  (`--log-level` / `--cache-size` / `--session-max` / `--sync-on-commit` /
  oplog retention / noop heartbeat) pending matching `Storage::open` knobs.
- [ ] **R8 tail** — only the pymongo gauge runs against the Rust server
  (`invoke validate --server rust` / the `pymongo-rust-server` entry in
  `validate.yml`). The Go/Node/Java/Ruby/Rust-driver gauges still gauge the
  Python server only: their runners spawn `python -m secantus` as the daemon,
  and pointing them at the Rust server needs a `secantusdb`-binary launch path
  in each gauge job — deferred until the Rust server's command surface is wide
  enough for those suites to be informative.
- [x] **Timeseries `_id` non-uniqueness in the Rust storage layer** — DONE
  (0.5.3-beta.22). `crates/secantus-storage` now mirrors the Python `Storage`:
  the command layer persists the `timeseries` option, `Storage::is_timeseries`
  detects it, and `timeseries_doc_suffix` (nanosecond timestamp + 16-bit counter)
  is appended to the doc-table key on insert / upsert so duplicate `_id`s coexist.
  `write_index_entries` / `delete_index_entries` gained an `id_key_override` so
  secondary-index entries point at the suffixed row (insert/update/delete/prune
  pass the actual key), and the `_id` point-lookup fast path (in both
  `try_index_id_keys` and the `explain` planner) is gated off for timeseries so a
  `{_id: x}` query scans and content-filters rather than reconstructing the bare
  key. Non-timeseries `_id` uniqueness is unchanged.
- [ ] **Gauge E11000 cluster — remaining tails (2026-06-12).** The
  order-dependent drop-then-reinsert E11000s are FIXED (stale WT read
  snapshot in the mutating scanners; see changelog Unreleased / the
  snapshot-refresh fix in `drop_collection` et al. — gauge went 93.5% →
  94.5%, E11000 failures 7 → 1). Arithmetic type errors are also FIXED
  (`$add`/`$subtract`/`$multiply`/`$divide`/`$mod` now raise mongod's
  errors on non-numeric operands, divide/mod-by-zero, and date misuse;
  Rust engine defers those cases — parity corpus extended first).
  Pipeline-update replace-vs-update misclassification FIXED, and
  timeseries `_id` uniqueness FIXED (suffixed doc keys; the one surviving
  E11000 is gone) — the 2026-06-12 E11000 triage is fully closed. The
  remaining ShowExpandedEvents / disambiguatedPaths introspection failures
  are separate unimplemented features. (`clusteredIndex` introspection is
  DONE in 0.5.3-beta.49 — `create` validates+stores it, `listCollections`
  surfaces `options.clusteredIndex` and omits `idIndex`, `listIndexes`
  reports the single `clustered: true` entry; mirrors commands.py. Closes the
  two `TestCollectionManagementClusteredIndexes` pymongo gauge tests.)
  Timeseries
  update restriction (mongod 7.0: an update may modify only the metaField)
  is now ENFORCED in the Rust server's `update` handler — non-meta /
  replacement / pipeline updates on a timeseries collection are rejected.
- [ ] **Go gauge: CI runs ~1/5 of the local set** — CI weekly artifacts have
  always reported ~450 tests (e.g. 401/453 on 2026-06-08, 447/900 on
  2026-06-12) while local `invoke validate-go` runs ~4700 (the numbers the
  pre-2026-06-12 committed report carried). Suspects: the 30-minute
  `go test -timeout`, cold caches on runners (GitHub cache service 400s), or
  package discovery under `./internal/integration/...` differing on CI.
  Diagnose before trusting week-over-week go comparisons.
- [~] **`aggregate` storage-backed stages** — DONE: `$lookup` (simple +
  `let`/`pipeline` forms), `$sample`, `$collStats`, `$indexStats`, `$out`,
  `$merge` (deep-merge default + replace/keepExisting/delete/fail modes),
  `$geoNear` (brute-force COLLSCAN via `secantus_core::geo::point_distance` —
  near/distanceField/key/query/min+maxDistance/distanceMultiplier/includeLocs/
  spherical; GeoJSON near ⇒ spherical, legacy `[x,y]` ⇒ planar). A
  `run_segmented` executor in `secantus-commands::aggregate` interleaves the
  storage-free core engine with these command-layer stages; `$lookup` `let`
  expressions are evaluated; `collation` threads through `$match`/`$sort`.
  **Source stages DONE:** `$currentOp` / `$listLocalSessions` / `$listSessions`
  emit one synthetic "op" row (port of `aggregate._stage_current_op`, with the
  `command` field echoing the request + `$db`/`cursor` defaulted), handled in
  `run_segmented` via `is_source_stage` / `apply_source_stage`. This is what
  makes a database-level `aggregate: 1` pipeline (no source collection) work —
  the `test_database.py` `$listLocalSessions` shape and the unified db-aggregate
  / versioned-API db-aggregate tests. **`$graphLookup` DONE** (0.5.3-beta.22+):
  command-layer BFS over the foreign collection (`startWith` / `connectFromField`
  / `connectToField`, `maxDepth` default 100, `depthField` as `NumberLong`,
  `restrictSearchWithMatch`), `_id`-dedup, value-match array-aware. **`$lookup` /
  `$graphLookup` nested inside `$facet` DONE**: `$facet` now runs each sub-pipeline
  through `run_segmented` (the command layer) instead of the storage-free core, so
  storage-backed stages work inside a facet. **`$geoNear` `key`-inference DONE**:
  when `key` is omitted it's inferred from the collection's lone `2d`/`2dsphere`
  index (ambiguous when there's more than one → error). **`$merge` pipeline-form
  `whenMatched` DONE** (inline pipeline applied to each matched doc with `$$new`
  bound to the incoming doc) plus **`on`-field unique-index validation** (a
  non-`_id` `on` requires a matching unique index on the target, else code
  51183). The command-layer storage-backed aggregate surface is now complete.
- [x] **`distinct` + DDL/introspection** — `distinct`, `create`, `drop`,
  `listCollections`, `listIndexes`, `createIndexes`, `dropIndexes` (the `Storage`
  trait gained the list/DDL methods; the R4b adapter forwards them).
- [x] **`findAndModify`** — composed at the command layer (find limit-1 + sort →
  update/remove → re-find for the new image → projection). **Caveat:** not atomic
  across the find+modify calls (a find-and-modify storage primitive would close
  that); `arrayFilters` / `let` / `collation` / `validator` deferred.
- [x] **db-admin commands** — `dropDatabase`, `renameCollection`, `collStats`,
  `dbStats`, `serverStatus` (trait gained drop_database / rename_collection /
  collection_is_capped / collection_data_size / index_sizes; adapter forwards).
  `serverStatus` minimal; `collStats`/`dbStats` use dataSize for storageSize.
- [x] **sessions + diagnostics** — `startSession` / `endSessions` /
  `refreshSessions` / `kill*Sessions` / `commit`+`abortTransaction` /
  `getParameter` / `getCmdLineOpts` / `connectionStatus` / `whatsmyuri` /
  `hostInfo` / `getLog` (storage-light; session/txn are no-ops). Removes
  CommandNotFound on driver connect/teardown.
- [~] **R5 — auth + TLS** (SCRAM-SHA-1/256, MONGODB-X509, rustls). **R5a DONE**:
  the SCRAM-SHA-256 mechanism (`crates/secantus-auth`, pure Rust, 6 tests, full
  client↔server round-trip). **R5b-1 DONE:** wired into the command layer —
  `saslStart`/`saslContinue` (`secantus-commands::auth`) over a per-connection
  `ConnectionAuth` (threaded one-per-socket through the server), plus
  `createUser`/`dropUser`/`usersInfo` over four new `Storage` trait methods
  (`add_user`/`get_user`/`drop_user`/`list_users`, adapter-forwarded to
  `secantus-storage`). Stored record is mongod-shape so both servers share the
  `secantus_users` table. 6 command unit tests + a pymongo TCP auth round-trip.
  **R5b-2 DONE:** dispatch-level `--auth` gating + RBAC. `secantus-commands::rbac`
  ports the built-in role catalogue + `check_privilege`; `dispatch`'s `authorize`
  rejects unauthenticated non-handshake commands (`Unauthorized` 13) and checks
  the principal's effective roles against a per-command `(action, scope)` table;
  `createUser` validates roles (`RoleNotFound` 31); `saslContinue` loads role
  bindings into `ConnectionAuth::effective_roles`. 11 unit tests. **R5b-3 DONE:**
  custom user-defined roles — `secantus-commands::roles` (`createRole`/`updateRole`/
  `dropRole`/`dropAllRolesFromDatabase`/`rolesInfo`) over four new role-storage
  trait methods (`add_role`/`get_role`/`drop_role`/`list_roles`, adapter-forwarded);
  `rbac::check_privilege_resolved` expands them via a `get_role`-backed resolver
  (privilege match + inheritance walk, cycle detection); `createUser` accepts
  storage-resident custom roles. 6 unit tests + a pymongo WT round-trip. **R5b-4
  DONE:** auth/RBAC completion — the role `grant`/`revoke` quartet
  (`grantPrivilegesToRole`/`revokePrivilegesFromRole`/`grantRolesToRole`/
  `revokeRolesFromRole`), `updateUser` (password rotation / role replacement +
  live `effective_roles` refresh), `dropAllUsersFromDatabase`, and `hello`'s
  `saslSupportedMechs`. 8 unit tests + a pymongo WT round-trip. **R5c-1 DONE:**
  TLS/mTLS transport in the accept loop (rustls, ring backend) — `ServerConfig.tls`
  (cert/key/ca/require_client_cert), handshake under the shutdown-poll timeout,
  client-cert subject DN (x509-parser, RFC 4514) → `CommandContext::peer_cert_dn`,
  `serve` generic over `TcpStream`|rustls `StreamOwned`, `RustServer` TLS params.
  Rust integration test (rcgen self-signed → rustls client → hello) + openssl-guarded
  pymongo TLS smoke. **R5c-2 DONE:** the MONGODB-X509 mechanism — createUser
  provisions X509-capable users (no password), saslStart / legacy authenticate
  read `peer_cert_dn`, match optional payload username, look up by DN on
  `$external`/admin, require an X509 credential, and auth without a password;
  hello/getParameter advertise MONGODB-X509. 4 unit tests. **This closes R5 (auth)
  bar SCRAM-SHA-1** (legacy MD5 prepass — deferred, low priority). Deferred:
  non-ASCII SASLprep.
- [x] **R4b — WiredTiger storage adapter** (`crates/secantus-storage-adapter`,
  `StorageAdapter`): CI-green (rust-storage builds it against vendored WT;
  `Send + Sync` confirmed). Bytes at the seam, `Hint` from `RawHint`, `map_err`.
- [x] **R6 — embedded Python handle** (`crates/secantus-server-py`, the
  `_secantus_server` extension / `RustServer`): CI-green — bundled into the wheel
  by CMake and smoke-tested via pymongo across Linux/macOS/Windows. `RustServer`
  auto-creates the storage dir. **Follow-ups:** a Python `secantus`-package
  wrapper for `SecantusDBServer`-style ergonomics; an `invoke rust-server-py` task.
- [ ] **R4 tail — TLS / mTLS** (`rustls`) + `peer_cert_dn` threading for X509.
- [~] **`update` options** — DONE: pipeline-form `u` (`[...]`) via
  `update_matching_pipeline` (diff-style oplog → change streams see
  `operationType: "update"`); **positional operators (`$` / `$[]` / `$[ident]`)
  + `arrayFilters`** — `secantus-core::update::apply_update_with` resolves
  positional paths (`find_positional_matches` for `$` from the query filter,
  `index_array_filters` + the query matcher for `$[ident]`); `secantus-storage::
  update_matching` always computes positional matches and takes `array_filters`,
  threaded through the command trait's `update_matching_array_filters` (default
  forwards) + adapter + handler (parses per-statement `arrayFilters`). Parity
  suite extended (`apply_update_with` binding + 11 arrayFilters cases).
  **`let` DONE** (update + delete): `resolve_let_vars` (handler) seeds `$$NOW`
  and evaluates each `let` value, threaded as query vars through
  `update_matching_array_filters` / `update_matching_pipeline` /
  `delete_matching_with_let` → the storage query matcher. **Still deferred:**
  `validator`.
- [x] **Collation server-wide (COLLSCAN-correct).** A command `collation` is
  parsed (`util::collation_of` → `secantus_core::collation::parse`) and threaded
  through `find` / `count` / `distinct` / `aggregate` (`$match` + `$sort` + lifted
  fetch) / `update` / `delete`. `secantus-storage::find_matching_with` /
  `count_matching` / `update_matching` / `delete_matching` take a collation and
  **force a COLLSCAN** when one is active (the byte-sortable indexes are
  collation-naive) + a collation-folded in-memory sort (`sort_key` →
  `encode_value_directed` with collation); the query matcher already honoured
  collation. Trait seam: additive `find_collated` / `count_collated` (default →
  uncollated) + collation params on the update/delete option-methods (defaults
  ignore → fakes unaffected); WT adapter routes. **Conservative:** any collation
  forces COLLSCAN (per-index-collation IXSCAN is a later optimisation — explain
  reports COLLSCAN under collation). Non-ASCII / `numericOrdering` collation →
  `BadValue` (the core engine defers; no Python fallback on the Rust server).
  **Deferred:** collection-*default* collation (needs `get_collection_options`);
  `$elemMatch` sub-query collation (matcher passes `None`); per-index-collation
  IXSCAN.
- [x] **`let` on reads (find / aggregate / findAndModify / distinct path).**
  `find_matching_with` gained a `vars` arg → threaded into the query matcher;
  the trait's `find_collated` gained a `let_vars` param (default ignores → fakes
  unaffected), routed by the WT adapter. Handlers resolve `let` via the shared
  `util::resolve_let_vars` (seeds `$$NOW`, evaluates each value) — this also
  fixed `aggregate`, which had passed the *raw* (un-evaluated) `let` doc as vars.
  findAndModify threads `let`+`collation` into its match (upsert-no-match `let`
  still deferred). **+7 gauge (`test_crud_unified` 275→282).**
- [ ] **`find` edges** — empty-collection/empty-result filter validation DONE:
  when nothing matched, the filter is re-run once against an empty document so an
  invalid / unsupported filter surfaces `BadValue` (consistent with the
  non-empty storage-scan path) instead of an empty cursor. Still deferred:
  `tailable: true` capped-collection poll. (Tracked in `find.rs` module docs.)
- [x] **R2c — `update` command.** Document-, replacement-, and pipeline-form `u`
  all apply; positional operators + `arrayFilters` + `let` + `collation` done;
  sort-rejection (9) + pipeline-stage validation (9 / 168) pre-checks done.
  `validator` still deferred (see "update options" above).
- [ ] **`find` command** — lands with R3 (cursor registry) + `secantus-core`
  projection; first-batch + `getMore`/`killCursors`.
- [x] **`collMod` + collection options + `validator` (insert).** `collMod` is now
  a registered command (`secantus-commands::admin::coll_mod`): merges recognised
  options (`validator` / `validationLevel` / `validationAction` /
  `changeStreamPreAndPostImages` / `capped`+`size`+`max`) into the collection
  (`NamespaceNotFound` 26 if missing); `create` persists the same set. Trait seam:
  `get_collection_options` / `set_collection_options` (defaults no-op/empty →
  fakes unaffected; WT adapter forwards to the existing storage methods). The
  `insert` handler enforces `validator` (code 121) unless
  `bypassDocumentValidation` / `validationAction: warn|off`. **+8 gauge
  (`test_change_stream` +7 from collMod enabling `changeStreamPreAndPostImages`,
  `test_collection` +1).** **Deferred:** `validator` on update/replace (needs the
  post-apply doc in storage); `collMod` TTL-index `index:{expireAfterSeconds}`
  modify; capped-size enforcement.
- [x] **`writeConcernError` for unsatisfiable `w > 1`.** `dispatch` attaches a
  `writeConcernError` (code 100, `CannotSatisfyWriteConcern`) when a request
  carries `writeConcern: {w: int > 1}` and the command succeeded — the single-node
  `secantus` RS can't satisfy it, but the write still happens (mirrors mongod +
  the Python server's `_unsatisfiable_wc_error`). One central place covers every
  write command. **+7 gauge (`test_collection`).**
- [x] **`explain` command.** `secantus-commands::admin::explain` ports
  `commands._explain`: parses the wrapped `find` / `aggregate` / `count` inner
  command, lifts a leading `$match` for aggregate, rejects a journaled /
  `w:"majority"` writeConcern (72 `InvalidOptions`), validates `verbosity`
  (`queryPlanner` / `executionStats` / `allPlansExecution`, else 2 `BadValue`),
  shapes `queryPlanner.winningPlan` (`FETCH` wrapping an `IXSCAN` inputStage with
  `indexName` / `keyPattern` / `direction`, or a bare `COLLSCAN`) via the trait's
  new `explain_plan` method (default → COLLSCAN; WT adapter converts the storage
  `ExplainPlan`), plus an `executionStats` block (runs `find_collated` to count
  `nReturned`) above `queryPlanner` verbosity, and the aggregate
  `stages: [{$cursor: {queryPlanner, …}}, …]` wrapper drivers look for. Collation /
  collectionless explain forces COLLSCAN.
- [x] **Query `$regex` / `$options` + bare BSON regex (Rust query engine).**
  `secantus-core::query` matches regex with the `regex` crate instead of deferring
  to Python: `field_matches` intercepts `$regex` (reading its sibling `$options`)
  and a bare `Bson::RegularExpression`; `op_regex` does `re.search`-style
  unanchored `is_match` over string values + string elements of arrays; flags
  `i`/`m`/`s`/`x` map to `RegexBuilder` (other flag chars ignored, mirroring
  Python's `_re_flags`). Patterns the linear `regex` crate can't compile
  (backreferences, lookaround) now fall back to the backtracking **`fancy-regex`**
  engine (flags ride an inline `(?ims x)` prefix) — closing the
  `test_list_collection_names` gauge gap (pymongo's `^(?!system\.)` negative
  lookahead). Only patterns neither engine compiles, or over the 1000-char cap,
  → `Fallback` (defer). Parity suite extended: curated regex cases (incl.
  lookahead/lookbehind/backref) + a 4000-iteration `test_regex_fuzz_parity`
  (safe-subset patterns/options/subjects; Rust ≡ Python `re`). **Known divergence
  (accepted):** the `regex` crate's `$` matches only end-of-haystack, not before a
  trailing `\n` like Python/PCRE — so `{x:{$regex:"foo$"}}` against `"foo\n"`
  matches on the Python server but not the Rust server. Rare; documented in
  `query.rs` module docs. Fuzz subjects are newline-free to avoid spurious parity
  failures from this gap.
- [ ] **CRUD cross-cutting still deferred in the Rust handlers:** `writeConcern`
  *value validation* (malformed `w`/`wtimeout`); `validator` on update/replace;
  `_reject_oplog_rs_write`; view-collection `count` (needs the aggregation
  engine). All tracked in `crud.rs`'s module docs.

- [x] **Engine selection** — `secantus.engine` is the single source of truth
  (`available()` / `selected()` / `set_engine()` / `enabled(component)`); all six
  shims consult it; `SecantusDBServer(engine=)` + `--engine` set it. Unit-tested
  by `tests/test_engine.py` (WT-independent).
- [x] **CI validates the Rust core** — `.github/workflows/test.yml` has a `rust`
  job (Linux, py3.12) that builds `_secantus_core`, runs `cargo fmt`/`clippy
  -D warnings`/`test`, the engine-parity suites, **and the full pytest suite
  under `SECANTUS_ENGINE=rust`** (the real differential check through pymongo /
  WiredTiger). Before this, CI built neither the extension nor selected the Rust
  engine, so the parity suites `importorskip`'d and the rewrite was effectively
  un-validated by CI. Note: the workflow triggers only on push/PR to `main`, so
  this job first runs when the rewrite branch opens a PR / merges — watch its
  first run (the YAML couldn't be exercised in the WT-less dev sandbox).
- [x] **Phase 0 spikes** — BSON fidelity, WiredTiger FFI, sortkey golden
  vectors all green (`rust/`, `rust/run_spikes.sh`).
- [x] **GIL release + benchmark** — every `#[pyfunction]` now wraps its pure-Rust
  compute in `Python::allow_threads` (BSON decode stays GIL-held since it borrows
  the Python buffers; the work runs GIL-free). `benchmarks/engine_bench.py` +
  `benchmarks/RESULTS.md` quantify it. **Findings:** (1) the byte seam does *not*
  eat the win — even paying `bson.encode`/`decode` per call, the Rust path is
  ~2× faster on the leaf ops and ~8× on the pipeline single-threaded; (2) GIL
  release parallelises only *coarse* calls — the pipeline scales ~1.5× on 2
  threads, but cheap per-op leaf calls regress under concurrency (per-call
  GIL release/re-acquire + GIL-held encode/decode dominate the tiny compute and
  ping-pong the GIL). **Implication:** the next real throughput lever for CRUD is
  **coarsening the seam** — batch the per-doc hot loops into one Rust call
  (`query_matches_batch`, `apply_update_batch`) so one GIL release covers many
  docs, the way `apply_pipeline` already does — not more operator coverage. Track
  with the benchmark; validate under a real concurrent server load (needs WT).
- [x] **Batched seam — prototyped and proven across all three CRUD engines.**
  `query_matches_batch` / `apply_update_batch` / `apply_projection_batch` (Rust)
  + `query.matches_batch` / `update.apply_update_batch` /
  `projection.apply_projection_batch` (shims) each do a whole list in one seam
  crossing (one GIL release for all N), whole-batch fallback to per-doc Python if
  any doc defers. Benchmark: each is faster single-threaded (1.06–1.23×) *and*
  fixes the multi-thread regression — per-doc anti-scales (~0.10–0.15× at 4
  threads) while the batch path *scales* (1.11–1.44×), **~10–12× more docs/s
  under 4-thread concurrency**. Parity: `test_batch_matches_parity` /
  `test_batch_apply_parity` / `test_batch_projection_parity` (batch results equal
  per-doc; the whole batch defers iff any doc would). Remaining (needs WT to land
  + measure): wire storage's scan / multi-update / projection paths to the
  `*_batch` shims; then the bytes-in/bytes-out variant (§4 step 2) that skips the
  per-doc Python decode entirely.
- [x] **Phase 1, leaf engine #1: `sortkey`** — ported to Rust behind the fat
  byte seam; pure-Python `secantus.sortkey` delegates when
  `SECANTUS_RUST_SORTKEY=1`. Parity pinned by `tests/test_rust_sortkey_parity.py`
  (curated + 2000-case fuzz, byte-identical).
- [x] **Packaging decision: ship the Rust core as a separate optional package**
  (not bundled into the `secantus` wheel). `crates/secantus-core` is a real
  `secantus-core` package (proper metadata, version **lockstep** with SecantusDB);
  `pip install "secantus[rust]"` pulls it via the new `rust` extra
  (`secantus-core==<this version>`); `.github/workflows/rust-wheels.yml` builds
  the abi3 wheels (maturin-action) across the macOS-arm64 / linux-x86_64 /
  linux-aarch64 / windows matrix + an sdist, and publishes on a release tag via
  trusted publishing. Keeping it separate (vs. merging the two native build
  systems into one wheel) is the bridge to the longer-term goal: the Rust side as
  a **first-class standalone Rust package** — a publishable crate and eventually a
  standalone `secantusdb` server binary.
- [ ] **One-time: configure a PyPI Trusted Publisher for `secantus-core`.**
  On PyPI, add a Trusted Publisher for the (new) `secantus-core` project pointing
  at `.github/workflows/rust-wheels.yml` (environment `pypi`), mirroring the
  `secantus` setup. Until this exists the tag-gated `publish` job fails auth; the
  `build`/`sdist` jobs run on every push/PR and are already CI-validated, so this
  is the only thing blocking the first published `secantus-core` wheel.
- [ ] **Release-process: bump & publish `secantus-core` in lockstep.** The
  `secantus[rust]` extra pins `secantus-core` to the exact SecantusDB version, so
  every release must (a) bump BOTH versions — root `pyproject.toml` and
  `crates/secantus-core/pyproject.toml` — and the `rust` extra pin in the root
  pyproject, and (b) publish both wheels. Wire this into the `/secantusdb-release`
  skill so the two can never drift (the `secantus[rust]` install breaks if the
  matching `secantus-core` version isn't on PyPI).
- [x] **Lib/bindings split (step 1 toward a standalone Rust package).** The Rust
  side is now a Cargo workspace (`crates/Cargo.toml`): `crates/secantus-core` is
  the **pure-Rust engine lib crate** (no PyO3 — engines over `bson`, public API =
  the 8 engines, internal helpers crate-private) and `crates/secantus-core-py` is
  the **thin PyO3 bindings crate** (byte seam + `#[pyfunction]` glue) that builds
  the `_secantus_core` extension / `secantus-core` wheel via maturin. All 62
  engine unit tests, clippy (`-D warnings`), and the maturin wheel build pass; the
  wheel is byte-for-byte the same `secantus_core-<ver>` distribution.
- [x] **Phase 4 sub-phase 0 — WiredTiger FFI foundation (`crates/secantus-wt`).**
  Safe Rust bindings over the vendored WiredTiger C lib (bindgen + `build.rs`):
  `Connection`/`Session`/`Cursor`, the key formats SecantusDB uses
  (`SS`/`SSu`/`SSS`/`SSSu`/`q`/`S`/`u`), `WT_NOTFOUND`/`WT_DUPLICATE_KEY`/
  `WT_ROLLBACK` translation, transactions. Verified against real WiredTiger
  (insert / byte-order scan / point search / NOTFOUND / update / remove / numeric
  `q` ordering / commit+rollback / on-disk reopen); `cargo fmt` + `clippy -D
  warnings` clean (`invoke rust-wt-test`). Excluded from the `crates` workspace so
  the green `secantus-core` CI is untouched. Scoping/status:
  `tasks/rust-rewrite-phase4-scoping.md`.
- [x] **Phase 4 sub-phase 1 — CRUD core (`crates/secantus-storage`).** A `Storage`
  over `secantus-wt` + `secantus-core`'s `sortkey`: `secantus_collections` +
  `secantus_documents` tables, `insert_one` (auto-`ObjectId`, duplicate-`_id`
  rejection), `find_by_id`, `scan_collection` (natural order), `replace_by_id`,
  `delete_by_id`, collection registry, the coarse serialize-everything lock;
  `id_key = sortkey.encode_value(_id)`. 7 integration tests vs real WiredTiger
  (cross-type natural order, db/coll isolation, reopen persistence); `cargo fmt` +
  `clippy -D warnings` clean (`invoke rust-storage-test`). Standalone crate
  (excluded from the `crates` workspace). Also fixed a latent use-after-free in
  `secantus-wt` (the `Cursor` now owns its `S`/`u` key/value buffers).
- [x] **Phase 4 — PyO3 exposure of the Rust storage (`crates/secantus-storage-py`).**
  A `RustStorage` PyO3 class over the BSON byte seam (`_id` wrapped as
  `{"v": id}`), exposing the CRUD core. Crucially this proves the
  **WiredTiger-linking extension builds (maturin → abi3 wheel) and imports**, and
  drives the Rust storage end-to-end from Python (`tests/test_rust_storage_smoke.py`,
  2 tests; `invoke rust-storage-py`) — the core risk of the wheel-matrix gate,
  de-risked on Linux. `cargo fmt` + `clippy -D warnings` clean. Standalone crate
  (excluded from the `crates` workspace).
- [x] **Phase 4 sub-phase 5a — write path in `secantus-storage`.** `update_matching`
  (operator + replacement, `multi`, `upsert`-with-seed, unique enforcement,
  index-entry / multikey upkeep, and the `$v:2` diff vs full-doc oplog split — closes
  the sub-phase-3b deferral), `delete_matching` (filter-routed, `limit`, op `"d"` +
  pre-images), `count_matching`, the shared materialised `candidate_docs` router, and
  public `UpdateOutcome`. 14 WT-backed tests in `tests/write.rs`; `clippy -D warnings`
  + `fmt` clean. **Deferred to the future engine-selection adapter (route to Python):**
  `array_filters`, positional update operators (`$`/`$[]`), `let`/`collation`,
  document `validator`, and geo-index validation on update — the Rust signatures
  don't accept these, so such ops stay on pure-Python `Storage`. (Capped-collection
  eviction now lands natively in `Storage::insert` via `enforce_capped_bounds` —
  oldest non-fresh docs evicted to stay within `size`/`max`, with `op:"d"` oplog +
  pre-images; mirrors Python's `_enforce_capped_bounds_locked`.)
- [x] **Phase 4 sub-phase 5b — collection/database lifecycle in `secantus-storage`.**
  `create_collection` / `drop_collection` / `drop_database` / `rename_collection`
  (move-by-rekey, `drop_target`, source/target guards) / `list_databases`, with the
  `op:"c"` command oplog entries (`create` w/ `idIndex`, `drop`, `dropDatabase`,
  `renameCollection`). Shared `purge_collection_tables` / `colls_of` /
  `collect_idx_rows` / `collect_entry_rows`. 10 WT-backed tests in
  `tests/lifecycle.rs`; clippy + fmt clean.
- [x] **Phase 4 sub-phase 5c — collection stats/introspection in `secantus-storage`.**
  `get_collection_options` (synthetic `local.oplog.rs` shape; `uuid` stays Binary),
  `collection_is_capped`, `collection_data_size`, `index_sizes` (`_id_` + per-index
  packed bytes), `scan_docs_after_id_key`. 7 WT-backed tests in `tests/stats.rs`;
  clippy + fmt clean.
- [x] **Phase 4 sub-phase 5d — full PyO3 surface (`crates/secantus-storage-py`).**
  `RustStorage` now exposes the whole `Storage` interface over the BSON byte seam
  (query/write/count, indexes+TTL, lifecycle, options+stats, oplog+cluster time,
  config setters) plus the exported `EngineFallback` exception (the
  `QueryUnsupported` "defer to Python" signal). Smoke-tested end-to-end against the
  built wheel (`tests/test_rust_storage_smoke.py`, 5 tests; `invoke rust-storage-py`).
  **Deferred to the 5e adapter:** rich E11000 `keyPattern`/`keyValue` propagation
  (DuplicateKey → `KeyError` w/ index name for now) and `BadHint` code mapping.
- [x] **Phase 4 sub-phase 5e-gap-a — users/roles/profiling in `secantus-storage`.**
  `add_user`/`get_user`/`drop_user`/`list_users` + role equivalents (opaque BSON
  record blobs over the `secantus_users`/`secantus_roles` `SS` tables, paginated
  `db`-filtered list), `get_profile`/`set_profile` (validated) over the
  `secantus_profile_settings` `S` table, and `ensure_profile_collection`. PyO3
  bindings + 6 WT-backed tests (`tests/auth.rs`) + smoke coverage. First of the
  5e gap-closure slices (port the 17 server-needed `Storage` methods missing from
  the Rust binding, then the engine adapter).
- [x] **Phase 4 sub-phase 5e-gap-b — batch insert + prune-all in `secantus-storage`.**
  `insert(db, coll, docs, ordered) -> (inserted, errors)` (auto-`_id`, unique +
  dup-`_id` write-errors, ordered/unordered, batched `op:"i"` oplog) and
  `prune_ttl_all_collections`. PyO3 bindings + 7 WT-backed tests
  (`tests/batch_insert.rs`) + smoke. **Deferred (no Rust capped support):**
  capped-collection eviction inside `insert` + geo-index validation on insert —
  capped collections under `SECANTUS_ENGINE=rust` won't enforce bounds, and a bad
  geometry won't be rejected (just indexed as no geo entry). Close when capped /
  geo-validation land in the Rust storage.
- [x] **Phase 4 sub-phase 5e-gap-c — change-stream tailable-wait in `secantus-storage`.**
  `wait_for_oplog(after_seq, timeout_ms)` (Condvar paired with the oplog mutex;
  one bounded wait, no lost-wakeup) + `notify_oplog_waiters()` (for killCursors) +
  `emit_oplog` notify. PyO3 `wait_for_oplog` releases the GIL while blocking. The
  Rust equivalent of `storage._oplog_cv`; `oplog_tail_seq_nolock` subsumed. 4
  cross-thread WT-backed tests (`tests/condvar.rs`) + threaded smoke. `commands.py`
  refactor off the raw `_oplog_cv` onto this method pair lands with the adapter.
- [ ] **Phase 4 sub-phase 5e — remaining gaps + adapter.** Remaining gaps:
  `checkpoint`/`close`/`create_archive` (admin/`fsync`/backup — none block the core
  conformance suites; `close` handled adapter-side). Then the `secantus.engine`
  storage-selection + Python `Storage` adapter over `RustStorage` (BSON seam,
  `EngineFallback` → Python-operators-over-Rust-docs, E11000/`BadHint` translation,
  `commands.py` getMore refactored onto `wait_for_oplog`/`notify_oplog_waiters`),
  then `test_storage.py`/`test_crud.py` + pymongo gauge under `SECANTUS_ENGINE=rust`.
- [ ] **Phase 4 — storage keystone (continued).** Remaining: (a) wire
  `secantus.engine` storage selection so `SecantusDBServer` can use the Rust
  `Storage` under `SECANTUS_ENGINE=rust` — **gated on porting the rest of the
  `Storage` surface** (the server needs `find_matching`/indexes/oplog/etc., not
  just CRUD), i.e. sub-phases 2-4 (indexes, geo, oplog/change-streams) plus the
  remaining higher-level methods (users/roles/profile/`checkpoint`/
  `create_archive`); then the conformance gate
  (`test_storage.py` / `test_crud.py` under `SECANTUS_ENGINE=rust`).
  (b) **The wheel-matrix gate — bundled behind an off-by-default build flag.**
  Decided (after rejecting a separate companion wheel — see scoping doc): the Rust
  `_secantus_storage` extension is built INTO the `secantus` wheel by the main
  wheel's CMake, against the WiredTiger that wheel already builds, gated behind the
  `SECANTUS_BUILD_STORAGE_ENGINE` CMake option (default **OFF** — wheel unchanged,
  no Rust/clang needed when off). The `storage-engine` job in `test.yml` is a 3-OS
  matrix (Linux / macOS / Windows) validating the flag-on build + import + smoke;
  the WT system-link flags are cfg-gated per target OS in `secantus-wt/build.rs`
  (Linux `pthread`+`rt`+`dl`; macOS `pthread`+`dl`; Windows none). Remaining: only
  once engine-selection makes the Rust storage engine selectable, flip the flag on
  in the shipping `wheels.yml` / cibuildwheel matrix (and add the abi3 storage
  extension to the wheel's repair/tag handling there).
- [ ] **Toward a standalone Rust package (continued).** With the lib/bindings
  split done, the remaining steps to "ultimately a Rust package": (a) settle the
  `secantus-core` lib's public API and flip `publish = false` → publish to
  crates.io; (b) add a `secantusdb` **binary crate** (a thin `main` over the
  engines + storage) — gated on the storage keystone (Phase 4 above), since a
  standalone server also needs storage in Rust, not just the operator engines.
- [ ] **Make Rust the *recommended* default (Python stays available).**
  Currently every component defaults to Python; `SECANTUS_ENGINE=rust` opts in.
  With the optional package now shipping (above), recommending Rust by default
  for installs that have the extension still wants: a decision on the byte seam's
  per-call `bson.encode({"v": value})` overhead vs passing values without
  re-encoding (see the batching work in `benchmarks/`). The Python engine remains
  a permanent, selectable mode regardless.
- [x] **Collation (query matcher).** `crates/secantus-core/src/collation.rs`
  implements the ASCII-safe, Unicode-version-independent cases (case-insensitive
  ASCII = ASCII-lowercase; accent-strip is a no-op on ASCII; strength-3 identity
  handles any string); threaded through the Rust query matcher's string `$eq`/
  `$ne`/`$gt`/`$gte`/`$lt`/`$lte`/`$in`/`$nin`. `query.matches` now delegates to
  Rust *with* a collation, which defers to Python for non-ASCII-under-transform
  and `numericOrdering`. Parity: `tests/test_rust_query_parity.py` collation
  cases + 4000-case fuzz.
- [x] **Collation-aware sortkey in Rust.** `sortkey.encode_value` /
  `encode_value_directed` thread a collation through to the Rust encoder
  (`collation::normalize_index_bytes`, where `numericOrdering` is the raw-bytes
  identity — matching Python's `_encode_string` skipping normalisation when
  `supports_index_encoding` is false). ASCII normalisation is reproduced;
  non-ASCII transforms defer to Python. Parity:
  `tests/test_rust_sortkey_parity.py` collation cases.
- [x] **Phase 1, leaf engine #2: `query.matches`** — common operators ported
  to Rust (`crates/secantus-core/src/query.rs` + `numeric.rs`) behind the byte
  seam; `secantus.query.matches` delegates when `SECANTUS_RUST_QUERY=1` (now
  including collation — see the collation item above). The Rust matcher returns
  `None` (→ pure-Python fallback) for anything not reproduced faithfully:
  `$jsonSchema`, geo,
  **any regex**, `$all`, structural/compound equality (array/doc operands),
  bool-as-int comparison, exotic BSON types. Parity pinned by
  `tests/test_rust_query_parity.py` (curated + 6000-case fuzz).
- [ ] **Widen the Rust query matcher (Rust server)** — the matcher backs the
  Rust server directly; there is no Python fallback in that path, so an unported
  construct surfaces as `BadValue`. `$all` is handled (element equality via
  `expressions::py_eq`; regex elements still defer). Remaining gaps to widen
  where faithful: bool-as-int `$gt`/`$lt` comparison and structural array/doc
  equality. (The retired in-process "flip `query.matches` default to Rust" item
  is gone — the two-server model has no per-call engine selection; `_secantus_core`
  is only the parity-test vehicle now.)
- [x] **Phase 1, leaf engine #3: `update.apply_update`** — the common
  deterministic operators ported to Rust (`crates/secantus-core/src/update.rs`,
  with the `secantus.paths` dotted-path helpers): replacement-style, `$set`,
  `$setOnInsert`, `$unset`, `$inc`, `$mul`, `$push`, `$pop`, `$rename`, plus
  `_id` immutability. `secantus.update.apply_update` delegates when
  `SECANTUS_RUST_UPDATE=1`. Returns `None` (→ pure-Python fallback) for pipeline
  (array) updates, positional ops / array filters, `$currentDate`,
  `$min`/`$max`/`$pull`/`$addToSet`/`$bit`, Decimal128/non-numeric arithmetic,
  and every error condition (so the exact `UpdateError`/`PathError` is raised by
  Python). Parity pinned by `tests/test_rust_update_parity.py` (curated +
  6000-case fuzz).
- [ ] **Widen the Rust update operators (Rust server)** to cover current defers
  where faithful: `$min`/`$max` (Python `<` cross-type / raise semantics),
  `$pull`/`$addToSet` (Python `==` membership incl. bool-as-int and structural),
  `$bit`. In the Rust server a defer surfaces as `BadValue`, so these are real
  capability gaps (not silent fallbacks). Field-order on `$set` of an existing
  key is **verified correct** (0.5.3-beta.22+): `bson::Document::insert` preserves
  an existing field's position and appends new keys — matching mongod. (The
  retired "flip `update` default to Rust" framing is dropped — no in-process
  default exists in the two-server model.) **`$currentDate` is done** (0.5.3-beta.47):
  the core engine still defers it (non-deterministic), but the Rust *storage* layer
  resolves it to a concrete clock value before `apply_update_with` (date / timestamp,
  one clock read per op), via `resolve_current_date` — mirrors `update.py`. Closes
  the pymongo gauge `test_update_and_replace`.
- [x] **Phase 1, leaf engine #4: `expressions.evaluate`** — a high-value core of
  the aggregation expression language ported to Rust
  (`crates/secantus-core/src/expressions.rs`): field paths / `$$var` / `$$ROOT` /
  `$literal`; comparison (`$eq`/`$ne`/`$gt`/`$gte`/`$lt`/`$lte`); logic
  (`$and`/`$or`/`$not`); control flow (`$cond`/`$ifNull`/`$switch`); arithmetic
  (`$add`/`$subtract`/`$multiply`/`$divide`/`$mod`); and common array ops
  (`$size`/`$arrayElemAt`/`$first`/`$last`/`$concatArrays`/`$reverseArray`/`$in`).
  `secantus.expressions.evaluate` delegates when `SECANTUS_RUST_EXPR=1`. Because
  expressions are recursive, the evaluator returns `None` (whole-call fallback)
  if ANY operator/value in the tree isn't ported. Parity pinned by
  `tests/test_rust_expressions_parity.py` (curated + 8000-case nested fuzz).
  **This also unlocked `$expr` in the Rust query matcher** (it now calls the Rust
  evaluator directly, Rust->Rust) — `query_matches` gained a `vars` arg threaded
  through `$expr`.
- [x] **Date arithmetic** — `$dateAdd`/`$dateSubtract`/`$dateDiff`/`$dateTrunc`
  ported via dependency-free civil-date math (`days_from_civil` inverse +
  `days_in_month`), UTC, bounded to Python's datetime range (out-of-range
  defers). The 8000-case date fuzz caught three real Python-semantics quirks:
  `$dateDiff second` truncates toward zero (`int(total_seconds())`) while
  hour/minute floor (`// n`); `$dateTrunc` truncates the *field* (keeping
  higher fields) not the total-since-midnight; and the sub-day `$dateDiff`
  units route through Python's lossy `timedelta.total_seconds()`
  (`total_microseconds / 10**6`, an int/int correctly-rounded divide) — Rust
  reproduces the same single-rounding float path, guarded to `|total_us| <=
  2**53` (where the `as f64` conversion is exact) and deferring extreme dates
  to Python so the double-rounding can't diverge.
- [ ] **Remaining expression operators are principled defers** (cannot be
  reproduced without a fidelity risk; all run pure-Python): regex
  (`$regexMatch`/`$regexFind`/`$regexFindAll` — Python `re`);
  `$dateToString`/`$dateFromString` (`strftime`/`strptime` + timezones);
  `$convert`/`$toDecimal` and float-`str()` / string→number / Decimal128
  conversion edges; `$round`/`$pow`/`$trunc` (rounding mode) and the
  transcendentals `$exp`/`$ln`/`$log`/`$log10` (last-ULP vs Python's libm);
  `$sortArray` (Python `sorted()` ordering/stability, raises on mixed types);
  non-ASCII `$toLower`/`$toUpper` and default-whitespace `$trim`. Each is a
  deliberate fallback, not a gap. (`$rand` is the lone exception that is
  *evaluated* in Rust rather than deferred — see the "now also handled" item —
  because deferring would error the standalone server, which has no Python.)
- [x] **Widen the Rust expression evaluator** — now also handled:
  `$slice`/`$indexOfArray`; ASCII `$concat`/`$toLower`/`$toUpper`/`$strLenCP`/
  `$split`/`$substrCP`; `$mergeObjects`/`$objectToArray`; the scope-introducing
  `$let`/`$map`/`$filter`/`$reduce`; and UTC date component extractors
  (`$year`/`$month`/`$dayOfMonth`/`$hour`/`$minute`/`$second`/`$dayOfWeek` +
  `$dateToParts`, via a dependency-free civil-date algorithm); a safe subset of
  conversions (`$toInt`/`$toDouble`/`$toBool`/`$toString` for numbers/bools/
  strings); exactly-deterministic math (`$abs`/`$floor`/`$ceil`/`$sqrt`); and
  `$range`/`$strLenBytes`/`$arrayToObject`; and the date-arithmetic ops
  `$dateAdd`/`$dateSubtract`/`$dateDiff`/`$dateTrunc` (UTC, civil-date math —
  see the "Date arithmetic" item); and `$rand` (uniform double in [0, 1) via the
  `rand` crate — evaluated directly, not byte-pinned to Python, since the
  standalone server has no Python to defer to). Remaining whole-call fallbacks to widen
  where faithful: `$dateToString`/`$dateFromString` (timezones, `strftime`/
  `strptime`); `$round`/`$pow`/`$trunc` (rounding mode) and transcendental math
  (`$exp`/`$ln`/`$log`/`$log10` — last-ULP divergence risk vs Python's libm);
  the conversion edges deferred above (Decimal128, string→number parsing, float
  `str()`, `$convert`/`$toDecimal`); `$sortArray` (uses Python `sorted()`
  ordering/stability + raises on mixed types); and non-ASCII case /
  default-whitespace `$trim` (defer for Unicode-fidelity safety). Regex ops
  (`$regexMatch`/…) need Python `re`. Done recently: string index/byte ops,
  `$getField`/`$setField`, `$zip`.
- [x] **Phase 1, leaf engine #5: `projection.apply_projection`** — inclusion /
  exclusion / `$slice` / `$elemMatch` projection shapes ported to Rust
  (`crates/secantus-core/src/projection.rs`). `secantus.projection` delegates
  when `SECANTUS_RUST_PROJECTION=1`; returns `None` for mixed inclusion/exclusion
  (Python raises), nested-document specs, unusual `$slice` arg types, and
  `$elemMatch` sub-filters the matcher defers. Parity:
  `tests/test_rust_projection_parity.py` (curated + 6000-case fuzz).
- [x] **Phase 1, leaf engine #6: `diff.compute_update_description`** — the
  change-stream `$v: 2` update diff ported to Rust
  (`crates/secantus-core/src/diff.rs`), reusing the expression engine's Python-`==`
  semantics. `secantus.diff` delegates when `SECANTUS_RUST_DIFF=1`; defers on
  Decimal128 / exotic values. Parity: `tests/test_rust_diff_parity.py` (curated +
  6000-case fuzz).
- [ ] **Shared path write helpers** (`set_path`/`unset_path`) now live in
  `paths.rs` alongside the read helpers; `update.rs` keeps a thin `Fallback`-
  mapping wrapper. (Note for the eventual default-flip: same `bson::Document`
  field-order question as the update engine applies to projection/diff outputs.)
- [x] **All six leaf engines are ported** (`collation` landed — see the
  collation items above). Remaining Phase-1 work is *widening* the
  already-ported engines (see the per-engine "widen" items above).
- [x] **Phase 2, aggregation pipeline — first slice.** `apply_pipeline` ported
  to Rust (`crates/secantus-core/src/aggregate.rs`) behind a list-of-docs byte
  seam (`{"d": [...]}` / `{"p": [...]}`), reusing the ported leaf engines
  (`query::matches`, `expressions::evaluate`, the `paths` helpers) so a pure
  pipeline runs end to end in Rust without re-entering Python per stage / per
  doc. Stages handled: `$match`, `$limit`, `$skip`, `$count`, `$project`
  (inclusion / exclusion / computed, mirroring `_project_one`'s mapping-only
  `_path_present`), `$addFields`/`$set`, `$unset`, `$replaceRoot`/`$replaceWith`.
  `secantus.aggregate.apply_pipeline` delegates when `SECANTUS_RUST_AGGREGATE=1`.
  **Graceful whole-pipeline fallback:** any unported stage or any deferred inner
  expression makes `apply_pipeline` return `None` and the pure-Python pipeline
  runs. Parity: `tests/test_rust_aggregate_parity.py` (curated + 4000-case fuzz).
- [x] **Widen the pipeline: `$sort` + `$unwind`.** `$sort` ported via a faithful
  cross-type BSON comparator (`crates/secantus-core/src/order.rs`, mirroring
  `_bson_lt` / `_bson_type_rank` — type ranks, doc/array recursion, the unified
  numeric type incl. NaN-is-equal). **Subtlety that drives the fidelity gate:**
  `sort_docs` wraps keys in `_SortKey` whose `__eq__` is Python `==` (so
  `False == 0`, `True == 1`, `1 == 1.0`) but whose `__lt__` is rank-based
  `_bson_lt`; a tuple sort advances to the next field only when `==` is True, so
  the comparator is *non-transitive* whenever `bool`/`NaN` mix with numbers (or
  the `==`-False-but-`<`-both-False types — Binary-with-subtype / Timestamp /
  Regex / Min/MaxKey appear). `order::is_sortable` therefore only green-lights
  the types where Python `==` agrees with `cmp == Equal` (null / non-NaN numbers
  / string / datetime / ObjectId / docs+arrays thereof) and defers the rest
  (bool, NaN, Decimal128, Binary, Timestamp, Regex, Min/MaxKey, exotic) to
  Python's Timsort. Single + multi-field, both directions, stable. `$unwind`
  ported (string + doc spec, `includeArrayIndex`, `preserveNullAndEmptyArrays`,
  missing/null/non-array/empty edges). **Refactor:** the pure comparator moved
  out of `storage.py` into a new I/O-free `secantus.ordering` module
  (`sort_docs`/`_bson_lt`/`_bson_type_rank`/`_SortKey`/`_to_decimal`; `storage`
  re-exports them) so `sort_docs` is importable without the WiredTiger
  extension — matching the pure-operator-engine layering. Parity:
  `tests/test_rust_aggregate_parity.py` (mixed-type sort corpus + fuzz with
  arrays / mixed-type fields).
- [x] **Widen the pipeline: `$group` + `$sortByCount`.** Ported in
  `crates/secantus-core/src/group.rs`. **Group-key bucketing** matches Python's
  dict semantics — each `_id` value is canonicalised into a hashable `GKey`
  (numbers + bool normalised through `numeric::NumVal`, so `1 == 1.0 == True`
  collapse into one bucket; docs/arrays recurse via `_hashable`'s key-sorted
  tuple) preserving first-seen `_id` and insertion order; key types we can't
  canonicalise faithfully (Decimal128, NaN, Binary/Timestamp/Regex/Min/MaxKey,
  exotic) defer. **Accumulators** reproduce Python's exact semantics:
  `$sum`/`$count`/`$avg` (running int-vs-float via a `Num` enum; `$avg` is
  always a double and the field stays *absent* when no non-null value is seen —
  matching the pure code that never creates the bucket key; non-numeric operands
  `TypeError` → defer); `$min`/`$max` (native `<`/`>` via `expressions::py_order`
  — cross-type pairs that Python would raise on → defer, not guess; null is a
  no-op that never "unsets"); `$first`/`$last`/`$push`; `$addToSet` (membership
  via Python `==` / `expressions::py_eq`). `$sortByCount` = group + stable count-
  descending sort. Validated across the in-repo curated + 5-seed fuzz **and** 8
  extra local seeds (~1,650–1,730 group/sortByCount pipelines handled per 4000,
  zero mismatches). `numeric::NumVal` gained `Eq + Hash`; `expressions::py_order`
  /`py_eq` are now `pub`.
- [x] **Widen the pipeline: `$bucket` + `$facet`.** `$bucket`
  (`group::bucket_stage`) places each doc into the half-open boundary range
  `boundaries[i] <= value < boundaries[i+1]` using `expressions::py_order`
  (Python's native `<=`/`<`, so cross-type / Decimal128 / array-doc boundaries
  defer rather than guess; NaN / TypeError fall through to `default`), then runs
  the `output` accumulators per bucket (reusing the `$group` accumulator
  machinery via `accumulate_into`). Reproduces the pure quirks: empty buckets
  emit only `{_id}` (accumulator fields are never created), an explicit `null`
  default counts as absent, missing/empty `output` falls back to
  `{count: {$sum: 1}}`, and pathological Python-equal bucket keys defer (the dict
  would collapse them). `$facet` (`aggregate::facet_stage`) runs each named
  sub-pipeline over a clone of the input via the recursive `apply_pipeline` and
  collects the results — any sub-pipeline that defers defers the whole stage.
  Validated across curated cases + the 5-seed in-repo fuzz **and** 8 extra local
  seeds (~500–580 `$bucket` and ~370–410 `$facet` pipelines handled per 5000,
  zero mismatches).
- [x] **Widen the pipeline: `$densify` (numeric path).** `densify::densify_stage`
  ports the numeric densify — partition the docs (Python dict semantics via the
  shared `group::GKey`), sort each partition by the numeric field, and fill every
  multiple of `step` strictly between the bounds (`"full"`/`"partition"` = the
  partition's observed min/max; explicit `[lo, hi]`). The cursor arithmetic
  mirrors Python exactly (a `Num{Int,Float}` enum so `int + int` stays int and
  widens to f64 once a float enters; `_densify_canon` collapses an
  integer-valued float filler back to an int), and the `existing_values`
  membership reproduces the set's `1 == 1.0 == True` collision via
  `numeric::NumVal`. The no-input-docs-with-explicit-bounds case and the
  "originals at/beyond `hi`" tail are reproduced. **Defers** to Python: any
  `range.unit` (date densify — fixed-duration `timedelta` *and* variable-length
  `relativedelta` month/quarter/year), non-numeric field values / bounds /
  partition keys (Python's `sorted` / comparisons would raise), and explicit
  bounds that would emit > 1M fillers (Python raises). `numeric::from_int` /
  `from_f64` and `group::gkey`/`GKey` are now exposed. Validated across curated
  cases + a dedicated 4000-case densify fuzz in-repo **and** 8 extra local seeds
  (5000 densify pipelines each, all handled, zero mismatches).
- [ ] **Pipeline: only the storage-backed stages remain.** Every pipeline stage
  that doesn't touch `Storage` is now ported. Still deferring to Python:
  `$lookup`/`$graphLookup`/`$geoNear`/`$out`/`$merge` (read/write collections via
  `ctx.storage`), non-deterministic `$sample`, and date-unit `$densify`. These
  wait for Phase 3+ (storage / wire / dispatch into Rust) per
  tasks/rust-rewrite-plan.md. Also still open: `$sort` defers on bool / NaN sort
  keys (the non-transitive `_SortKey` cases above) — reproducing Python's exact
  Timsort comparison sequence would widen it, but the risk/reward is poor.

### Two latent `sortkey` bugs fixed while porting (now Python == Rust == mongod)

Found by the parity test; both changed the **on-disk index-key bytes** for the
affected values (immaterial for ephemeral test data, but note it):

- **Date keys** used `int(total_seconds() * 1000)`, a float path that rounded
  sub-second values off by up to 1ms vs the integer millis BSON actually stores
  (and mongod sorts by). Now integer-exact.
- **Regex keys** did `bytes(r.flags)`, which — because a BSON-round-tripped
  `Regex.flags` is an *int* — produced N NUL bytes instead of the option string
  (e.g. flags=10 → ten NULs instead of `"im"`). Now reconstructs the option
  chars in pymongo's on-wire order (`ilmsux`).

### 7.2 Command/server tests moved onto real WiredTiger (2026-06-24) — CLOSED

The Rust command/server crates were unit-tested against hand-rolled in-memory
storage doubles, so the command×storage and wire×storage paths were only ever
exercised on real WiredTiger end-to-end via the driver gauges — a fake could
pass while real WT diverged. Both doubles are now scrapped (commits `a11328a`,
`e13d526`):

- **`FakeStorage` (six `secantus-commands` modules) → gone.** All 82
  storage-backed command tests (find / crud / findandmodify / admin / aggregate
  / distinct) were re-homed as real-WT integration tests in the WT-linked
  `secantus-storage-adapter` crate (`tests/command_*_wt.rs` + a shared
  `tests/common/mod.rs` `with_wt()` helper), each driving the real `dispatch`
  path over `WtStorage` via `StorageAdapter`. Fake-specific setup/verification
  was redone in real-WT terms (validators via real `create`; `$out`/`$merge`
  checked by reading the target back; the collMod unique-conversion over real
  duplicate docs instead of injected dup-groups).
- **`MemStorage` (`secantus-server` roundtrip tests) → gone.** The five
  wire/TCP roundtrip tests moved to `secantus-storage-adapter/tests/
  server_roundtrip_wt.rs`, binding the real server over real `WtStorage`
  (`secantus-server` added as a *dev-dependency* of the adapter crate only).
- **Why the adapter crate is the home:** `secantus-commands` and
  `secantus-server` are deliberately WiredTiger-free (clean-workspace members,
  so the fast `rust` CI job + manylinux wheels build with no libclang/WT). They
  *can't* link WT, so the tests had to relocate to a WT-linked, gate-covered
  crate (`rust-adapter-test` runs its `cargo test`). Those two crates keep only
  their pure-unit tests (lib/cursors/util) and no storage doubles; `clippy -D
  warnings` confirms the removal left no dead code.
- **Teardown race found + fixed while porting:** the awaitable-hello server test
  signalled completion after `stop()` but before the server finished dropping,
  letting the temp dir be removed out from under WiredTiger's final
  close-checkpoint (`WT_PANIC: WiredTigerHS.wt No such file`). Fixed by fully
  dropping the server before signalling; other tests rely on
  `RunningServer::Drop` → `stop()` draining connection threads first. 5/5 repeat
  runs clean.
- **Remaining storage doubles (intentional, not scrapped):** `ClockStorage`
  (`secantus-commands` lib test — injects a deterministic `Timestamp(555,9)`
  real WT can't reproduce) and `NoStorage` (`secantus-server` `tls.rs` — a
  hello-only TLS handshake never touches storage). Neither is a storage
  stand-in.

### 7.3 Natural-order (insertion) index ported to Rust storage (2026-06-25) — CLOSED

The Rust storage previously had no insertion-order index — its doc table is keyed
by `id_key = encode_value(_id)`, so an unsorted `find()` walked `_id`-sort order,
not insertion order. This only diverged from mongod / the Python server for
**mixed `_id` types inserted out of `_id` order** (monotonic ids hide it), which
php-lib `BulkWriteFunctionalTest::testInserts` exposed. Ported Python's design to
`crates/secantus-storage`:

- Two WT tables (`secantus_natural` `SSq→u` = `(db,coll,seq)→id_key` and the
  reverse `secantus_natural_seq` `SSu→q`) + a persisted monotonic `next_nat_seq`
  counter (in the oplog-meta blob; recovered on reopen, or by scanning the max
  nat seq when absent — so it survives oplog-disabled / legacy DBs).
- Maintenance on **every** doc mutation: insert / `insert_one` / upsert write a
  nat entry; `delete_by_id` / `delete_matching` / `prune_ttl` / capped eviction /
  `purge_collection_tables` (drop + dropDatabase + rename's dropTarget) remove it.
  rename drops the source's entries; the destination is re-keyed directly (no nat
  entries) so it falls back to `id_key` order (a minor, documented degradation).
- The collscan find paths + the `$natural` hint walk `scan_blobs_natural`
  (seq-ascending → fetch by `id_key`, with a legacy `id_key`-order fallback when a
  collection has no nat entries); the `_id_` hint stays `id_key` order.
- New `SSq` key / `q` value cursor accessors in `secantus-wt`.
- Tests: `crates/secantus-storage/tests/natural_order.rs` (insertion order for
  mixed `_id`s, reopen recovery, delete+reinsert no-doubling, drop+recreate reset,
  `$natural` hint) + an end-to-end adapter test.

Capped-collection eviction now also walks insertion order: `enforce_capped_bounds`
uses `scan_docs_natural` (beta.92), so FIFO holds for non-monotonic custom `_id`s.
This matches mongod; note the **Python server still evicts in `id_key` order**
(`_enforce_capped_bounds_locked` uses `_scan_docs`, not `_scan_docs_natural`) — see
the §4 capped note. The Rust server is now strictly more FIFO-correct than Python
here until Python's eviction is moved onto its natural-order index too.

### 7.4 Verified rust-only gauge tail (2026-06-25)

Authoritative `invoke validate-all-servers --jobs 4` run (every gauge on **both**
Python and Rust, same day, JDK 17 for java) — so each "rust-only" item below is a
failure the Rust server has that the **Python server does not**, not a stale-baseline
artifact. **Clean (0 rust-only): c, cxx, dotnet, kotlin, node, mongo-rust-driver.**
All four actionable themes are now fixed and merged: the geo `$center` / `$near` /
`$nearSphere` query operators (#66), php-ext write-reply shapes + cursor metrics +
Code `_id` (#67), ruby index-option validation/echo (#69), and
`splitLargeChangeStreamEvents` (#64). What remains is **only** the out-of-scope
session tests + the go harness race below.

- **Out of scope (session plumbing, do not chase):** ruby "behaves like a failed
  operation using a session raises an error" (×3, `Collection#create` / `#indexes` /
  `Index::View#create_one`) and php-lib `WatchFunctionalTest::testSessionFreed` (×1,
  `resumeCallable` unset on invalidate via reflection). Same class as the deferred
  change-stream session items — driver-internal session lifecycle, not a wire divergence.
- **go `TestChangeStream_ReplicaSet/try_next/one getMore sent` (3 entries, 1 test) — NOT a server bug; §5 harness race (verdict 2026-06-26).**
  The Rust change-stream behaviour was verified **correct** by two direct probes, so the
  earlier "confirmed real Rust bug / scope-leak" escalation was wrong and is retracted:
  (1) a pymongo repro against the standalone `secantusdb` binary (`/tmp/cs_repro.py`) —
  collection-scoped watch on an empty collection returns `try_next()==None` (start
  position correct), writes to a sibling collection / other DB do **not** surface (no
  scope leak), and an own-collection write **does** surface (control); (2) the exact go
  test run **in isolation** (`go test -run TestChangeStream_ReplicaSet/try_next
  -count=10` against the rust binary) passes **10/10**. The full-suite failure is the
  documented §5 artifact: other `t.Parallel()` top-level tests write the shared `TestDB`
  namespace (with collection-name-truncation collisions) during the `try_next` await
  window, and that genuinely-same-namespace write *correctly* wakes the stream — the test
  only assumes an empty stream because the shared-daemon gauge doesn't give it the
  per-test namespace isolation real `mongod`s would. Rust appears more susceptible than
  Python in the full suite (getMore/await response timing makes it catch the concurrent
  write more often — Python passed the same-day run, Rust failed), but that's scheduling
  sensitivity to a harness race, not a wire/correctness divergence. **Runner-side fixes do
  NOT work (2026-06-26):** two attempts — isolating change-stream tests from the rest of
  the suite, then running each change-stream top-level function in its own serial `go test`
  process — both still flaked (1/3 and 1/2). And the server is provably correct under
  on-disk load (collection-scoped stream on an untouched collection: 0 events / 400 polls
  while other collections took 2074 concurrent writes; 0 events / 300 fresh stream-opens
  under create/drop/insert churn). So the flake is a timing property of the shared-daemon
  mtest harness under full-gauge load, not suppressible at the runner and not a server bug.
  **Accepted**, same as the Python-server verdict in §5. See the top-section entry for the
  full 2026-06-26 evidence.
  - [ ] *Minor, separate:* the `secantusdb` binary doesn't accept `--noop-heartbeat-seconds`
    (stripped by `gauge_common._PYTHON_ONLY_FLAGS` for the Rust server), so the go gauge
    runs Rust without periodic noop heartbeats. Not the cause here (the `resume_token_
    updated_on_empty_batch` test that needs it is in the skip list), but worth adding the
    flag to the binary for gauge parity.

### 7.5 Remaining Rust-server feature gaps (defer audit, 2026-06-26)

From the `Fallback`-site audit (#2): constructs the Python server supports but the
Rust core defers (so they error on the Rust server). None are *correctness* bugs —
the actionable gauge tail is closed; these are feature completeness. **Shipped since
the audit:** `$exp`/`$ln`/`$log`/`$pow`/`$round`/`$trunc` (#74), `$sortArray` (#76),
`$unionWith` (#77), `$fill` (#123), `$toDecimal` + `$convert` (#132),
`$setWindowFields` (rank funcs + the **full time-series operator set** `$shift` /
`$expMovingAvg` / `$locf` / `$linearFill` / `$derivative` / `$integral` + `$group`
accumulators over document-based **and value-based `range`** windows — both
servers, single ascending numeric sortBy; only date-`unit` x-axes defer), the
regex family `$regexMatch` /
`$regexFind` / `$regexFindAll` (shared `regexutil`; all three served by both the
linear and backtracking `fancy-regex` engines — lookaround / backreference finds
compute, matching Python `re`),
`$jsonSchema` (`bsonType` / `type` / `enum` / numeric bounds / string length +
`pattern` / array + object counts + `items` / `required` / `properties` /
`additionalProperties` / `patternProperties` / `dependencies` + the `allOf` /
`anyOf` / `oneOf` / `not` combinators — both servers; only exotic keywords still
ignored, unreproducible shapes defer), `$dateFromString`
(canonical ISO — `YYYY-MM-DD` / `YYYY-MM-DDTHH:MM:SS`, optional trailing `Z` or
fixed `±HH:MM` offset → UTC instant — plus `null`→`onNull`; the parity harness now
compares the *bson-normalised* stored form so tz-aware results compare cleanly.
`format` / separate `timezone` field / fractional seconds / non-canonical strings
defer), and `$dateToString` (strftime-style formatting: `%Y`/`%m`/`%d`/`%H`/`%M`/
`%S`/`%L`/`%j`/`%w`/`%u`/`%%` + literals; a `timezone`, any other directive
(`%z`/`%Z`/`%G`/`%V`/`%U`/locale names), or a non-4-digit year defer to Python
`strftime`). **With `$dateToString`, the pure expression-operator surface is
complete on both servers** (only date *formatting/parsing* edges below remain).
**Remaining:**

- [ ] **Expression operators:** `$dateFromString` `format` (strptime) and the
  separate `timezone` field on both `$dateFromString` / `$dateToString` (named
  IANA zones need a tz database — `chrono-tz` or similar); `$dateToString` locale
  / `%z`/`%Z`/ISO-week directives (Python `strftime` handles those). Fractional
  seconds stay deferred (BSON is millisecond-only).
- [ ] **Aggregate stages:** `$setWindowFields` date-`unit` x-axes — the time
  `unit` scaling on `$derivative` / `$integral` and range windows over a date
  sortBy — absent from *both* servers, so each needs a Python oracle first, then
  the Rust port. (The full time-series operator set — `$shift` / `$expMovingAvg` /
  `$locf` / `$linearFill` / `$derivative` / `$integral` — plus document +
  value-`range` windows and rank/accumulators, all ship on both for numeric sorts.)
- [ ] **Query operator:** `$jsonSchema` exotic keywords absent from both servers
  (`uniqueItems` / `$ref`-style refs / `title`/`description` metadata / ...) —
  would need porting on **both** servers. (`bsonType`/`type`/`enum`/bounds/length/
  `pattern`/counts/`items`/`required`/`properties`/`additionalProperties`/
  `patternProperties`/`dependencies`/`allOf`/`anyOf`/`oneOf`/`not` all ship on both.)

## SQL / PostgreSQL interface — P0 spike limitations

The embedded SQL engine (`src/secantus/sql/`, `run_sql`) shipped as the P0 spike of
`tasks/sql-postgres-plan.md`. Known gaps, to close in later phases:

- [ ] **Wire server: simple + extended protocol, trust + SCRAM auth, optional TLS.**
  `pgserver.py` speaks v3 startup, simple `Query` (P1), extended `Parse`/`Bind`/
  `Describe`/`Execute`/`Close`/`Sync` (P3), `SCRAM-SHA-256` auth + TLS (P4). Binary
  parameter *input* (`pgextended._BINARY`) **and binary result *output*** are both
  decoded/encoded for the common type-OID set
  (`int2`/`int4`/`int8`/`float4`/`float8`/`bool`/`bytea`/`text`/`varchar`/`date`/
  `timestamp`/`timestamptz`/`numeric`/`jsonb`): the extended protocol honours Bind's
  per-column result-format codes (`pgextended._OUT_BINARY` / `_result_value`, and the
  portal-Describe `RowDescription` reports the chosen formats). The simple-query path is
  always text. Still missing: channel binding (`SCRAM-SHA-256-PLUS`), mTLS client-cert
  auth, user management via
  SQL (`CREATE ROLE` — users are constructor config, not stored in the catalog / shared
  with the Mongo user store), the `Copy` subprotocol, and cursor `DECLARE`. Real-driver
  gauges run in CI via **pg8000** (pure-Python, text params) **and psycopg 3**
  (`tests/test_pgserver_psycopg.py` — libpq via the `psycopg[binary]` wheel, the
  strictest wire exercise: binary params + server-side prepared statements + the psycopg
  SQLAlchemy dialect), each with a SQLAlchemy Core reflection smoke; `psql`/JDBC as live
  gauges still need a libpq CLI / a JVM (absent in the dev env). SQLAlchemy **reflection**
  works end to end (see below).
- [ ] **Reflected tables are now read-write.** A collection with
  no `CREATE TABLE` reflects (sampled schema-on-read) for `SELECT` — incl. `->`/`->>`/`#>`
  jsonb navigation **and GROUP BY / aggregates / JOIN** (the pipeline planner reflects
  via `planner._lookup_table_def(..., storage)`) — **and for INSERT / UPDATE / DELETE**: the
  write gate (`engine._require_table(..., storage)`) falls back to `reflect.reflect`, and the
  INSERT/UPDATE planners are permissive for reflected tables (an un-sampled field is a valid
  write target of the `any` type; the reflected `_id` PK is NOT NULL and immutable, so an
  INSERT must supply it and `SET _id = …` is rejected `0A000`). A write to a *non-existent*
  collection still `42P01` (no implicit collection creation — `CREATE TABLE` first). Remaining
  gaps: in a JOIN, an
  *unqualified* reference to an *un-sampled* field of a reflected table can't be routed (the
  resolver matches on sampled columns — qualify it as `alias.field`, or it must appear in the
  50-doc sample); type inference samples 50 docs and picks the first non-null type per field (no
  widening across conflicting types — first-seen wins).
- [ ] **jsonb operator surface landed (one gap + one parser quirk).** Containment/existence
  operators in WHERE compile to Mongo filters: `@>` (object → dotted-path equalities, array →
  `$all`, scalar → equality), `?` / `?|` / `?&` (key-or-element existence via `$exists` + array-aware
  equality). `jsonb_build_object` / `jsonb_build_array` / `jsonb_array_length` / `jsonb_typeof`
  (scalar) and `jsonb_array_elements` / `jsonb_object_keys` (set-returning) are evaluated per row;
  `->`/`->>`/`#>`/`#>>` navigation now also works inside the per-row scalar evaluator (not just the
  find projection / WHERE). **Gaps:** `<@` (contained-by) is `0A000` — "field is a subset of a
  constant" isn't expressible as a pushed-down filter (rewrite as `<const> @> field`); `jsonb_each`
  (key+value record SRF) and the `jsonb_*_text` family aren't modeled. **Parser quirk:** sqlglot reads
  a bare `f(a->'k')` arrow as a lambda, so a navigated *function argument* must be parenthesised
  (`f((a->'k'))`) or use `#>` (`f(a #> '{k}')`) — bare navigation in WHERE / projection is unaffected.
- [ ] **Aggregate/JOIN path has gaps (P5 + later slices landed the core).** `GROUP BY` +
  `HAVING` + `COUNT`/`SUM`/`AVG`/`MIN`/`MAX`, **multi-table** `INNER`/`LEFT JOIN` (equality
  `ON`, each join relating to the base or an already-joined table), **`SELECT DISTINCT`**
  (single-table and over a join), **and JOIN *combined* with GROUP BY / aggregates / HAVING /
  `array_agg`** compile to an aggregation pipeline. The join builds `$lookup`/`$unwind`, then
  `_plan_join_group_select` appends a `$group` whose keys + accumulators resolve through the
  join resolver (`a.region`, `SUM(b.amt)` → post-unwind paths); WHERE applies pre-group, HAVING
  post-group. **Computed scalar expressions in the SELECT list / ORDER BY** — arithmetic
  (`+`/`-`/`*`/`/`/`%`, PG int-division / mod semantics), `||`, and the common functions
  (`upper`/`lower`/`length`/`trim`/`substring`/`concat`/`abs`/`round`/`ceil`/`floor`/`power`/
  `coalesce`/`nullif`/`greatest`/`least`) now evaluate per row via the evaluated-select path
  (single-table and over a join). **Two-table `RIGHT` / `FULL OUTER JOIN`** now land (b53):
  `$lookup` is left-driven, so `A RIGHT JOIN B` is planned as `B LEFT JOIN A` (drive from B,
  preserve unmatched B) and `A FULL JOIN B` is the LEFT join from A `$unionWith` the B rows with
  no A match (an anti-join sub-pipeline that nests B under its alias so A's columns read NULL);
  `_build_outer_join_pipeline` / `_full_join_anti_branch` in the planner, `amap` kept in FROM order
  so `SELECT *` preserves Postgres column order. Works with WHERE / GROUP BY / aggregates / scalar
  exprs. **`CROSS JOIN` + comma-joins land** (b57): a join with no `ON` (`CROSS JOIN` or the implicit
  `FROM a, b` form) compiles to the cartesian product — an empty `$lookup` pipeline returns every
  foreign doc, then `$unwind` (no preserve) pairs each with the outer row; an outer join without `ON`
  is a `42601`. Non-equi (`a.x < b.y`) and `OR` join conditions already rode the `$lookup` `let`/
  `pipeline` form via `_OnTranslator` (the backlog was stale). Still `0A000`:
  `RIGHT`/`FULL` in a 3+ table chain, subqueries, **computed GROUP BY *keys*** and
  **scalar expressions *over* an aggregate** (e.g. `SUM(x) * 2`), and **function calls inside a
  WHERE comparison** (the `$expr` path lowers columns + arithmetic, not function nodes —
  `WHERE upper(name) = 'X'` is `0A000`). SUM/MIN/MAX
  result typing is approximate (uses the column's tag; AVG → float8; arithmetic → numeric).
  **`DISTINCT` aggregates landed** (b48): `COUNT`/`SUM`/`AVG(DISTINCT col)` compile to a
  `$addToSet` accumulator plus a post-`$group` `$addFields` that reduces the set
  (`_distinct_reduction`: `$size`+`$filter` for count, `$reduce` for sum, `$reduce`/`$size`+`$cond`
  for avg; NULLs filtered to match SQL). `MIN`/`MAX(DISTINCT)` run the ordinary accumulator (a set's
  extremum equals the raw extremum). Wired in both the single-table (`_plan_group_select`) and
  join (`_plan_join_group_select`) paths via the shared `_register_distinct_agg`; `_aggregate_of` /
  `_join_aggregate_of` now return `(func, col, distinct)`. **Still `0A000`:** a `DISTINCT` aggregate
  inside `HAVING` (the SELECT-list reduction stage isn't shared with the HAVING `$match`).
- [ ] **WHERE: column-to-column + arithmetic + non-correlated subqueries landed.** `column OP
  literal` keeps the indexable `{field: {op: val}}` fast path. A comparison where neither side is
  a constant — `qty > shipped`, `price < cost * 1.5` — lowers to a Mongo `{$expr: {$op: [...]}}`
  (`planner._to_agg_expr`), with `+`/`-`/`*`/`/` arithmetic over columns and literals nesting
  inside. **Non-correlated subqueries**: `x IN (SELECT col FROM t [WHERE ...])` → `$in`,
  `NOT IN` → `$nin` (via the `NOT`→`$nor` wrapper), and a scalar `x = (SELECT max(col) ...)`
  (any comparison op) → the evaluated value. The inner SELECT runs through the engine
  (`engine.run_inner_select` ← `planner.SubqueryCtx`), so it may itself aggregate / filter; it
  must select exactly one column. Plumbed via the single-table `plan_select` path (a subquery in
  a GROUP BY/JOIN query's WHERE isn't wired yet → `0A000`). `$expr` / subquery filters can't use
  a storage index (→ COLLSCAN). **EXISTS / correlated subqueries landed** (b45): a WHERE with
  `EXISTS`/`NOT EXISTS` or a subquery that references the outer row can't push down, so
  `planner.where_needs_per_row` routes it to `executor.execute_correlated_select`, which scans the
  outer table and evaluates the whole WHERE per row via `secantus.sql.scalar` — the inner query
  reads inner-table rows with outer-row references falling through (`scalar._inner_row_scopes` /
  `_eval_exists` / `_eval_in`, aggregate inner projections reduced by `_SUBQUERY_AGG_REDUCERS`).
  **Correlated WHERE in the pipeline paths landed** (b70): a JOIN or single-table GROUP BY whose
  WHERE is correlated / `EXISTS` no longer errors. `planner.where_needs_per_row` now short-circuits
  the pushdown `$match` in `_build_join_pipeline` and `_plan_group_select`; the WHERE is carried as
  `EvaluatedSelectPlan.where` (a JOIN — evaluated per joined row *after* the pipeline, outer scope via
  the join resolver) or `PipelineSelectPlan.residual_where` + `residual_resolve` (a GROUP BY —
  evaluated per base doc *before* the `$group`, so only survivors group). The inner query is still a
  simple `SELECT … FROM one_table [WHERE …]`; the per-row scan is `O(outer × inner)` (no index use).
  **Correlated WHERE + JOIN + GROUP BY landed** (b76, also a correctness fix — b70 silently *dropped*
  the WHERE for this shape): `PipelineSelectPlan.residual_split` records how many leading pipeline
  stages (the join prefix) run before the Python filter, so `_plan_join_group_select` runs the
  `$lookup`/`$unwind`, filters the joined rows by the correlated WHERE (outer scope via the join
  resolver), then runs the `$group` over the survivors (`executor._pipeline_input_docs` applies the
  split). Still `0A000`: a correlated WHERE combined with JOIN, GROUP BY, **and** a window function all
  in one SELECT (`_plan_join_group_window_select` raises rather than silently dropping the WHERE),
  function calls inside a comparison (`qty = abs(shipped)`), and `<@`-style structural predicates.
- [ ] **`RETURNING` landed** (b46). `INSERT` / `UPDATE` / `DELETE … RETURNING <proj>` projects the
  affected rows back as a result set (`planner._returning_columns` reuses the SELECT projection
  vocabulary `_out_columns`: `*`, columns, aliases, jsonb nav). `execute_insert` pins an `_id` on
  each doc before insert so the in-hand list is the authoritative inserted set; `execute_update`
  captures matched `_id`s and re-reads the **post-image**; `execute_delete` snapshots the victims
  before deleting. The wire layer already emits RowDescription+DataRows whenever `res.columns` is
  non-empty, so no pgserver change. **Computed expressions in `RETURNING` landed** (b67): each
  returning item is now `(name, Column, expr)`; `expr` is None for a plain column / `*` / jsonb (read
  straight from the doc), else the raw node evaluated per returned row by `executor._returning_result`
  against a scope over that row (arithmetic, `||`, function calls, `CASE`, …). Works for INSERT /
  UPDATE (post-image) / DELETE and `INSERT … ON CONFLICT`. A subquery inside `RETURNING` isn't
  supported (the eval ctx has no catalog/session).
- [ ] **Set operations landed** (b47). `UNION` / `INTERSECT` / `EXCEPT` (+ `ALL` variants, chained)
  in `engine._run_set_operation`: each arm runs through the full SELECT path, rows are combined
  with multiset semantics (`_combine_setop_rows` / `_multiset_filter` — DISTINCT collapses to set
  semantics, `ALL` keeps min-count for INTERSECT / left-minus-right for EXCEPT), output columns
  come from the first arm, and a trailing `ORDER BY` (output-column name or ordinal) + `LIMIT`/
  `OFFSET` apply to the combined result. `describe_statement` resolves the result shape from the
  leftmost arm so the extended protocol's Describe works. Arity mismatch → `42601`. **Limits:** no
  cross-arm type reconciliation (columns/types taken verbatim from the first arm); `ORDER BY` after
  a set op only accepts an output column name or ordinal, not an arbitrary expression.
- [ ] **Non-recursive CTEs landed** (b49). `WITH name AS (...) [, ...] <query>` in
  `engine._run_with`: each CTE is materialized to rows (run through `_run_query`) and registered as
  an ephemeral collection on a `CatalogBackend`, with a `_CTECatalog` overlay mapping CTE names to
  TableDefs built from each inner query's result shape; the `WITH` is stripped (`node.pop()`) and the
  main query runs against that backend + overlay, so CTE names resolve like tables in every path
  (single-table, pipeline/join, set-op). CTEs materialize in order (later may reference earlier);
  names are statement-scoped (overlay, no catalog mutation). `describe_statement` returns NoData for a
  `WITH` query (resolving columns would require executing the CTEs), so the extended protocol relies
  on Execute's RowDescription. **`WITH RECURSIVE` landed** (b56): `engine._run_recursive_cte` evaluates
  a CTE whose body is a `UNION [ALL]` of an anchor + a recursive term (detected by
  `_is_recursive_cte` — the right arm references the CTE name) via semi-naive iteration — run the anchor,
  then repeatedly run the recursive term against just the prior step's new rows (re-registered under the
  CTE name) until empty. `UNION` dedups vs all rows seen (cyclic graphs terminate); `UNION ALL` keeps
  every row, guarded by `_MAX_RECURSION_ROWS` (1M → `54001`). Optional column aliases (`name(a,b)`,
  `_cte_column_aliases`) rename the output and now apply to non-recursive CTEs too (`_register_cte`).
  A bare integer/bool literal in a SELECT list now types from its value (`_infer_scalar_tag` →
  `_infer_value_tag`) so `SELECT 0 AS lvl` rides the wire as an int, not text. **`WITH` on writes
  landed** (b68): `_run_with` accepts an `INSERT`/`UPDATE`/`DELETE` body — the CTEs materialize the
  same way, then the write is dispatched via `_run_statement` against the `CatalogBackend` (whose writes
  forward to real storage) + `_CTECatalog` overlay, with the CTE-aware `SubqueryCtx` published on
  `planner._pipeline_subctx` so an `UPDATE`/`DELETE` WHERE subquery over a CTE resolves. So
  `WITH cte AS (…) INSERT INTO t SELECT … FROM cte` and `… UPDATE/DELETE … WHERE id IN (SELECT … FROM
  cte)` work. **Still `0A000`:** `WITH RECURSIVE` on a write body (only on `SELECT` / set-op).
- [ ] **`INSERT … SELECT` landed** (b50). `INSERT INTO t [(cols)] SELECT …` routes through
  `engine._run_insert`: the source query (a SELECT / set operation; may join / aggregate / CTE) runs
  via `_run_query`, and its result rows map positionally onto the target columns through the shared
  `planner._insert_doc` (same coercion / NOT NULL / PK→`_id` path as VALUES, factored out alongside
  `insert_target_columns` / `plan_insert_rows`). Column-count mismatch (target vs query) → `42601`;
  `RETURNING` works (the source is materialized first, so a self-insert reads a stable snapshot).
  **Still `0A000`:** `WITH` before an `INSERT` (the source SELECT's own `WITH` is fine), `MERGE`.
- [ ] **Window functions landed** (b51). `func(...) OVER (PARTITION BY … ORDER BY …)` routes through
  the evaluated-select path (a window expr already trips `_stmt_needs_evaluation`). `secantus.sql.window`
  computes each window over the fetched rows — partition (repr-keyed groups), order within partition
  (stable multi-key sort), then apply the function — and stores the value on each doc under a synthetic
  `__win_<k>` field; `scalar.evaluate` resolves an `exp.Window` node to that value via the scope (so a
  window can nest inside a larger expression). Supported: `ROW_NUMBER`/`RANK`/`DENSE_RANK`,
  `SUM`/`COUNT`/`AVG`/`MIN`/`MAX` (whole-partition, or running under the default RANGE frame — peers
  tied on the order key share the cumulative value), `LAG`/`LEAD` (offset + default). `_infer_scalar_tag`
  types them (rank/count→int8, avg→float8, sum/min/max/lag/lead→arg tag). **Frames + value/rank funcs
  landed** (b55): `NTILE`, `FIRST_VALUE`/`LAST_VALUE`/`NTH_VALUE`, and explicit frames — `window._frames`
  builds each row's inclusive `[lo, hi]` index range; ROWS frames take any `UNBOUNDED`/`CURRENT ROW`/
  `n PRECEDING`/`n FOLLOWING` bound, RANGE frames take `UNBOUNDED`/`CURRENT ROW` (peer-group) bounds.
  Aggregate windows and the value functions reduce/select over the frame; rank-like funcs ignore it.
  **Window + `GROUP BY` in one SELECT landed** (b69): `planner._plan_group_window_select` runs a two-phase
  plan — a `$group` computes the grouping columns + every group aggregate (collected anywhere in the SELECT
  list / ORDER BY via `_group_agg_nodes`, which excludes a window's own aggregate operand), then the
  evaluated executor computes the windows over the grouped rows. Each group aggregate is replaced in the
  AST by a reference to its computed field (so `RANK() OVER (ORDER BY SUM(sal))`, `SUM(SUM(sal)) OVER ()`,
  and `PARTITION BY <group col>` all resolve), `HAVING` prunes groups before the window, and `ORDER BY
  <window alias>` is resolved in this planner (a bare alias term is substituted with its output expression).
  **Window + `GROUP BY` + `JOIN` landed** (b73): `_plan_join_group_window_select` is the join analogue —
  it builds the `$lookup`/`$unwind`/`$match`/`$group`/`$project` pipeline (group keys + aggregates
  resolved through the join resolver), then the same window phase runs over the grouped rows. The shared
  tail (`_finish_group_window`) is factored out of both planners. **Still `0A000`/unsupported:** numeric
  `RANGE` offset (needs interval arithmetic on the order key), and the general `ORDER BY <alias>` gap in
  the plain evaluated (non-group-window) path.
- [ ] **`INSERT … ON CONFLICT` landed** (b52). `INSERT … ON CONFLICT (cols) DO NOTHING | DO UPDATE SET …
  [WHERE …]` via `planner._plan_on_conflict` (an `OnConflict` on `InsertPlan`) + `executor.
  _execute_insert_on_conflict`: each proposed row probes the conflict target with `find_matching`; a
  clean row inserts, a conflicting row is skipped (`DO NOTHING`) or updated in place (`DO UPDATE`). SET
  expressions evaluate per-row through `scalar.evaluate` with a scope binding `EXCLUDED.<col>` to the
  proposed row and bare/target-qualified columns to the existing row (so `n = t.n + EXCLUDED.n` works);
  an optional `WHERE` gates the update. A bare `ON CONFLICT DO NOTHING` (no target) inserts and swallows
  any `11000` duplicate. Command tag counts rows inserted *or* updated (skipped don't count); `RETURNING`
  projects the inserted + updated rows. **Still unsupported:** `ON CONFLICT ON CONSTRAINT <name>` (→
  `0A000`; no named-constraint registry), `DO UPDATE` with no conflict target (→ `42601`).
- [ ] **`MERGE` landed** (b74). `MERGE INTO target [alias] USING source [alias] ON <cond> WHEN [NOT]
  MATCHED [AND <cond>] THEN UPDATE SET … | DELETE | INSERT [(cols)] VALUES (…) | DO NOTHING` via
  `engine._run_merge`. Per source row it scans the target snapshot (loaded once at MERGE start) for rows
  the `ON` condition matches, then applies the first `WHEN` of the right kind whose optional `AND`
  condition holds; `_merge_source` materializes a table / reflected-collection / `(SELECT …) alias` source
  into name-keyed rows; conditions + `UPDATE` RHS + `INSERT` VALUES evaluate through `scalar.evaluate` with
  a scope that resolves target vs source columns by alias (`_merge_pick_when` / `_merge_apply_matched` /
  `_merge_apply_not_matched`). Command tag `MERGE n` counts inserts + updates + deletes; each target row is
  affected at most once (`done` id-set), and MERGE targets are captured for savepoint snapshots
  (`_write_target_collection` recognises `exp.Merge`). **`RETURNING` + `WHEN NOT MATCHED BY SOURCE`
  landed** (b77): `_merge_pick_when` now keys on both the `matched` and `source` (BY SOURCE) flags; after
  the source loop a BY-SOURCE pass applies `UPDATE`/`DELETE`/`DO NOTHING` to target rows no source row
  matched (`source_matched` id-set). The apply helpers return `(count, image)` — an updated row's
  post-image, an inserted row, or a deleted row's pre-image — and `RETURNING` projects them via
  `planner._returning_columns` + `executor._returning_result` (plain + computed target-column items).
  **Limitations:** no multi-target-match cardinality error (Postgres raises 21000 when a source row matches
  >1 target; SecantusDB applies to all matched, once each), an unqualified column ambiguous between target
  and source resolves to the target, and `RETURNING` doesn't support `merge_action()` or source-column
  references.
- [ ] **Small cleanups landed** (b58). (1) A FROM-less `SELECT` now evaluates constant *expressions*
  (arithmetic, `||`, function calls, `CASE` …) via `scalar.evaluate` against an empty scope
  (`_const_scope`), not just bare literals + info functions; (2) a FROM-less `SELECT … WHERE <const>`
  is honoured — a false predicate yields zero rows (`ConstantSelectPlan.emit`), so a recursive-CTE
  anchor like `SELECT 1 WHERE 1=0` works; a column reference with no FROM → `42703`. (3) The jsonb `<@`
  (contained-by) operator lands in its pushable `const <@ field` form (== `field @> const`,
  `_jsonb_contains_filter`); `field <@ const` (subset-of-a-constant) stays `0A000`.
- [ ] **WHERE subqueries in the pipeline paths landed** (b59). The single-table pushdown always threaded
  a `SubqueryCtx`, but the pipeline planners (JOIN / GROUP BY / evaluated / DISTINCT) called
  `_where_filter` from many places without one, so a WHERE scalar/`IN` subquery there was `0A000`.
  `plan_pipeline_select` now publishes the context via a planning-scoped `contextvars.ContextVar`
  (`_pipeline_subctx`, reset in a finally) that `_where_filter` / the join `$match` pick up — one
  set-point, no signature churn. So `WHERE x OP (SELECT …)` / `x IN (SELECT …)` work in JOIN / GROUP BY /
  scalar-expr queries and in a recursive-CTE term's WHERE (session is None on that path — data
  subqueries don't need it). Correlated subqueries in a pipeline are still `0A000` (single-table only).
- [ ] **`ORDER BY` NULL placement landed** (b54). Postgres orders NULL as the largest value (ASC →
  NULLs last, DESC → NULLs first) with `NULLS FIRST`/`NULLS LAST` overriding; Mongo sort treats
  NULL/missing as the *smallest*, so the SQL layer no longer delegates NULL placement to storage.
  `planner._nulls_first` reads sqlglot's per-term flag (already PG-defaulted); the single-table,
  correlated, evaluated, and set-op paths sort in Python via the shared `executor._pg_sort`
  (`cmp_to_key`, NULL placement independent of direction) — which also pulls OFFSET/LIMIT off the
  storage fetch for an ordered single-table SELECT — and the join / GROUP BY pipeline paths get a
  companion null-rank `$addFields`/`$cond` field sorted ahead of each ORDER BY key
  (`planner._emit_pipeline_sort`), then `$unset`. **Note:** index-accelerated ORDER BY+LIMIT pushdown
  no longer applies to a single-table ordered SELECT (correctness over the storage-side sort
  optimisation — the SQL layer is a dev/test surface).
- [ ] **No transactions, no parameters, no prepared statements.** `BEGIN`/`COMMIT`,
  `$1` placeholders, and the extended query protocol come with the wire phases (P3/P5).
- [ ] **Composite primary keys rejected** (single-column PK ↔ `_id` only). Updating the
  PK column is rejected (mongod can't change `_id`).
- [ ] **`numeric`/`json`/`bytea` partial.** `numeric` round-trips via Decimal128; `json`
  passes dicts/lists through without a real `jsonb` operator surface; `bytea` is hex-string
  in / `bytes` out. Full `jsonb` navigation (`->`/`->>`/`#>`) is P6.
- [ ] **Catalog surface: joins landed, column-level reflection still missing.**
  `information_schema.tables`/`.columns`/`.schemata` and `pg_catalog.pg_class`/
  `pg_namespace`/`pg_type`/`pg_database` are served as virtual tables, and JOINs / GROUP BY
  across them now execute (`virtual.CatalogBackend` + `planner._lookup_table_def`), so
  SQLAlchemy `get_table_names()`/`has_table()` and `psql`'s `\dt` work. WHERE now handles
  `CAST`/`::type`, `col = ANY(ARRAY[...])`, and the always-true `pg_table_is_visible`/
  `pg_type_is_visible` predicates. `pg_attribute`/`pg_attrdef`/`pg_description` virtual
  tables now exist (attrelid lines up with pg_class.oid), so column-level introspection
  *joins* resolve — e.g. `pg_attribute ⋈ pg_class ⋈ pg_namespace` returns a table's columns.
  The column-metadata query SQLAlchemy / `psql \d` emit now runs end to end: compound
  multi-condition join `ON`s (`… AND attnum > 0 AND NOT attisdropped`) compile to a `$lookup`
  sub-pipeline; scalar SELECT-list functions (`format_type`, `pg_get_expr`, `coalesce`), `CASE`,
  and correlated scalar subqueries are evaluated per row (`secantus.sql.scalar`); compound join
  `ON`s compile to a `$lookup` sub-pipeline; a `(SELECT … GROUP BY …) AS alias` derived table
  materializes into an ephemeral collection (`CatalogBackend.register_ephemeral`); `array_agg`
  (→ `$push`) and `pg_sequence`/`pg_collation`/`pg_constraint`/`pg_enum` (present-but-empty) +
  pg_type domain columns are modeled. **Full SQLAlchemy reflection now works end to end** —
  `get_columns` / `get_table_names` / `has_table` / `get_pk_constraint` / `get_indexes` /
  `get_foreign_keys` and whole-table `Table(autoload_with=...)`. PK/index reflection rides
  set-returning functions (`unnest` / `generate_subscripts` → row expansion in the evaluated
  executor), GROUP BY / `array_agg` over a derived-table FROM, populated `pg_index`/`pg_constraint`/
  `pg_am`/`pg_opclass`, and a fix so boolean expressions (`conrelid IS NOT NULL`) type as `bool`
  (else the wire text `'f'` reads truthy in `if row["has_constraint"]`). Column comments reflect as
  `None`. **Constraint / sequence `information_schema` views landed** (b75):
  `information_schema.table_constraints`, `key_column_usage`, and `constraint_column_usage` (built from
  `virtual._pk_constraints`) surface PRIMARY KEY (and now FOREIGN KEY) rows, so the standard PK
  reflection join (`table_constraints ⋈ key_column_usage`) that Alembic / SQLAlchemy's inspector emit
  resolves; `sequences` is present-but-empty (no sequences).
- [ ] **CHECK / UNIQUE constraints — declared, reflected, NOT enforced** (b91): column-level (`col int
  CHECK (col > 0)` / `col text UNIQUE`), table-level named (`CONSTRAINT c CHECK (...)` / `... UNIQUE (a,
  b)`), and table-level unnamed CHECK/UNIQUE are parsed by `planner._extract_constraints`, stored on
  `TableDef.check_constraints` (`catalog.CheckConstraint`) / `TableDef.unique_constraints`
  (`catalog.UniqueConstraint`), and persisted in the catalog doc. Unnamed constraints get Postgres'
  default names (`<table>_<col>_key`, `<table>_<col>_check`). Reflection: `pg_catalog.pg_constraint`
  gains `contype='u'`/`'c'` rows (`virtual._unique_constraints` / `_check_constraints`, oid bases
  45000/47000); each UNIQUE is backed by an implicit unique index (`_unique_constraint_index_relations`,
  idx oid base 46000) whose `indexrelid == conindid`, because SQLAlchemy's `get_unique_constraints`
  joins `pg_constraint.conindid = pg_index.indexrelid` and reads columns from `unnest(indkey)`.
  `information_schema.table_constraints` / `.check_constraints` (new) / `.key_column_usage` /
  `.constraint_column_usage` gain rows; `pg_get_constraintdef(oid)` renders `UNIQUE (…)` and `CHECK
  ((…))`. SQLAlchemy's `get_unique_constraints()` / `get_check_constraints()` resolve end to end. **Not
  enforced:** no CHECK-predicate validation, no UNIQUE-duplicate rejection on write — schema-shape
  record only. **Limitations:** no `ALTER TABLE … ADD CONSTRAINT CHECK/UNIQUE` yet (still
  `feature_not_supported`; only ADD FOREIGN KEY is wired); CHECK columns aren't listed in
  `constraint_column_usage` (the predicate isn't parsed for referenced columns).
- [ ] **`CREATE VIEW` / `DROP VIEW` landed** (b87): a view is a stored `SELECT` persisted as its query
  text in a per-db `__sql_views__` collection (`catalog.put_view` / `get_view` / `drop_view` /
  `list_views`). `CREATE [OR REPLACE] VIEW` and `DROP VIEW [IF EXISTS]` dispatch on `exp.Create` /
  `exp.Drop` kind `'VIEW'` (`executor.execute_create_view` / `execute_drop_view`). Querying a view
  works by inline expansion: `engine._expand_views` rewrites every `FROM` / `JOIN` reference to a
  declared view into a `(<view def>) AS name` subquery before dispatch, so single-table reads,
  aggregates, joins against real tables, and nested views (a view over a view, expanded recursively
  with a depth guard) all ride the existing derived-table (`_resolve_source`) machinery.
  `select_needs_pipeline` now routes a from-subquery to the pipeline path so a bare `SELECT * FROM
  view` materializes. CTE names defined in the same statement shadow views. Reflection: `pg_class`
  gains `relkind='v'` rows (`virtual._view_oids`, base 50000), `pg_get_viewdef(oid)`
  (`scalar._call_func` → `virtual.viewdef_for_oid`), `information_schema.views`, and a `table_type='VIEW'`
  row in `information_schema.tables` — SQLAlchemy's `get_view_names()` / `get_view_definition()` reflect
  end to end. **Limitations:** views are read-only (no INSERT/UPDATE through a view) and not
  materialized (each query re-reads the base tables); no `CASCADE`/`RESTRICT` on `DROP`; no column-list
  aliasing (`CREATE VIEW v (a, b) AS …`); `WITH CHECK OPTION` not modeled.
- [ ] **`COMMENT ON TABLE` / `COLUMN` landed** (b86): the comment is stored on `TableDef.comment` /
  `Column.comment` (persisted in the catalog doc) by `executor.execute_comment` (dispatched on
  `exp.Comment`), surfaced through `virtual._pg_description` (table comment → `objsubid 0`, column
  comment → the column's attnum, `classoid` = pg_class 1259). SQLAlchemy's `get_table_comment()` and
  the `comment` field of `get_columns()` reflect them. `COMMENT ON … IS NULL` removes the comment
  (sqlglot can't parse a NULL comment expression, so `planner.parse` rewrites a whole `COMMENT ON … IS
  NULL` statement — anchored so a query's `WHERE x IS NULL` is untouched — to an `UNCOMMENT_SENTINEL`
  the executor reads as removal). `get_table_comment`'s join needs `'pg_catalog.pg_class'::regclass`, so
  `_coerce_cast` now maps a `regclass` cast of a catalog relation name to its OID (`_REGCLASS_OIDS`).
- [ ] **Foreign keys — declared, reflected, NOT enforced** (b81): column-level `col type REFERENCES
  t(c)` and table-level `FOREIGN KEY (c) REFERENCES t(c)` (incl. `ON DELETE` / `ON UPDATE` actions and
  the columnless `REFERENCES t` → target-PK form) are parsed by `planner._extract_foreign_keys`, stored
  on `TableDef.foreign_keys` (`catalog.ForeignKey`), and persisted in the catalog doc. Reflection:
  `information_schema.referential_constraints` (one row per FK), FK rows added to `table_constraints` /
  `key_column_usage` / `constraint_column_usage`, `pg_catalog.pg_constraint` gains `contype='f'` rows
  (`conrelid`/`confrelid`/`conkey`/`confkey`), and `pg_get_constraintdef(oid)` renders the `FOREIGN KEY
  (…) REFERENCES …` string (`virtual._foreign_keys` / `constraint_def_for_oid`, called from
  `scalar._call_func`). SQLAlchemy's `Inspector.get_foreign_keys()` + full `MetaData.reflect()` resolve
  the relationship end to end. **Not enforced:** no referential-integrity check on insert/update/delete
  — this is a schema-shape record only. Adding a FK after the fact via `ALTER TABLE … ADD [CONSTRAINT
  name] FOREIGN KEY` landed in b85 (see below). **Limitations:** FK actions never fire;
  `MATCH`/`DEFERRABLE` render as the defaults.
- [ ] **`ALTER TABLE … ADD [CONSTRAINT name] FOREIGN KEY` landed** (b85): parsed as `exp.AddConstraint`
  (a bare `ForeignKey` or a named `Constraint` wrapping one) in `executor._apply_alter_action`, which
  appends a `catalog.ForeignKey` (via `planner._make_fk`, now taking an optional constraint name) to
  the table and persists it through `Catalog.replace`. Reflects exactly like a CREATE TABLE FK
  (`information_schema.referential_constraints`, `pg_constraint` contype='f', SQLAlchemy
  `get_foreign_keys()`). Non-FK `ADD CONSTRAINT` (CHECK / UNIQUE) → `feature_not_supported`. Still not
  enforced.
- [ ] **`DISTINCT ON` + `LATERAL` joins landed** (b82). **`DISTINCT ON (exprs)`** keeps the first row
  per distinct value of `exprs` in ORDER BY order (single-table + join) — routed through the evaluated
  path (`planner._distinct_on`, `EvaluatedSelectPlan.distinct_on`, dedup in `executor._evaluated_value_rows`);
  before this it was silently mistreated as plain full-row DISTINCT. **`LATERAL`** (comma / `CROSS JOIN
  LATERAL` / `JOIN LATERAL … ON true` / `LEFT JOIN LATERAL … ON true`) lowers a single-table correlated
  subquery to a `$lookup` (`let` outer bindings + sub-`pipeline` with a `$match {$expr}` correlation via
  the existing `_OnTranslator`, optional `$sort`/`$limit` for top-N, `$project`) + `$unwind`
  (`planner._lateral_stage`, dispatched in `_build_join_pipeline`'s join loop). **Limitations:** the
  `LATERAL` subquery must be single-table (no join / GROUP BY / aggregate / scalar-fn subquery →
  `feature_not_supported`); `JOIN LATERAL` with a non-`TRUE` ON is rejected (correlate in the subquery
  WHERE); `DISTINCT ON` doesn't enforce Postgres' "ORDER BY must start with the DISTINCT ON exprs" rule
  (lenient — keeps whatever the sort order gives).
- [ ] **`GROUP BY ROLLUP` / `CUBE` / `GROUPING SETS` landed** (b83, single-table). Enumerated grouping
  sets (`planner._grouping_sets`: ROLLUP → prefixes, CUBE → all subsets, explicit GROUPING SETS as
  written, a leading plain `GROUP BY a, …` as a prefix in every set) are each compiled to a
  `$group`+`$project` branch (`_grouping_set_branch`; group columns absent from a set project as
  `{$literal: None}` so every branch shares one output shape) and combined with `$unionWith`
  (`_plan_grouping_sets_select`, routed in `_plan_pipeline_select`). **Limitations:** single-table only
  (GROUPING SETS over a JOIN → `feature_not_supported`); no `HAVING`, correlated WHERE, or DISTINCT
  aggregate with GROUPING SETS; no window over GROUPING SETS; the `GROUPING()` helper function isn't
  modeled.
- [ ] **`ALTER TABLE` landed** (b80): `ADD COLUMN [IF NOT EXISTS]`, `DROP COLUMN [IF EXISTS]`
  (`$unset`s the field on every doc), `RENAME COLUMN` (`$rename`s a non-PK field; a PK rename keeps
  the `_id` field and only changes the SQL name), `RENAME TO` (renames the table *and* moves the
  backing collection via `Storage.rename_collection`, so the old name stops resolving — otherwise the
  leftover collection reflects as a phantom table), and `ALTER COLUMN … SET/DROP NOT NULL`.
  `ALTER TABLE IF EXISTS` on a missing table is a no-op; dropping the PK column is rejected
  (`0A000`). `execute_alter_table` / `_apply_alter_action` in `executor.py`; catalog rewrite via
  `Catalog.replace`. `ALTER COLUMN … TYPE t` (retype in catalog) and `SET`/`DROP DEFAULT` landed in
  b84 (see below). **Limitation:** **multiple actions in one statement** are unsupported — sqlglot
  parses a comma-separated action list (`ADD …, DROP …`) as an opaque `Command`, so each action must
  be its own statement.
- [ ] **Literal column DEFAULTs + `ALTER COLUMN TYPE` / `SET`/`DROP DEFAULT` landed** (b84). `Column`
  gained `has_default` / `default`; a literal DEFAULT (number / string / bool / NULL) from `CREATE
  TABLE` (`planner._column_default`) or `ALTER COLUMN SET DEFAULT` is filled in for an omitted column
  in `_insert_doc`. `ALTER COLUMN … TYPE t` updates the catalog `type_tag` (new inserts/reads use it;
  already-stored BSON values are **not** rewritten). Fixed a latent bug: `DROP DEFAULT` and `DROP NOT
  NULL` both parse with `drop=True`, and the old AlterColumn handler conflated them, wrongly setting
  the column NOT NULL on `DROP DEFAULT`. **Limitations:** only *literal* defaults are stored — an
  expression/function default (`now()`) is accepted but never applied (reads NULL when omitted); a
  `TYPE` change doesn't recast existing rows.
- [ ] **`SET` is accept-and-record.** GUCs persist on the session and reportable ones
  echo a `ParameterStatus`, but nothing acts on them (e.g. `search_path` doesn't affect
  name resolution). (`BEGIN`/`COMMIT`/`ROLLBACK` are now real transactions — see below.)
- [ ] **Transactions: single-connection atomicity; SAVEPOINT is a no-op.**
  `BEGIN`/`COMMIT`/`ROLLBACK` open/commit/abort a real `Storage` user-transaction
  (statements in the block run on its WT session; ROLLBACK undoes them; an error poisons
  the block with `25P02` until it ends). `SET TRANSACTION ISOLATION LEVEL` / `READ ONLY` /
  `READ WRITE`, `SET SESSION CHARACTERISTICS`, and `BEGIN ISOLATION LEVEL …` are
  **accepted as no-ops** (single-node — isolation/read-only don't change behaviour).
  **Real nested savepoints landed** (b71): `SAVEPOINT name` / `ROLLBACK TO SAVEPOINT name` /
  `RELEASE SAVEPOINT name` do actual partial rollback. Each open savepoint (`session.savepoints`,
  a stack of `_Savepoint`) lazily captures a touched collection's deep-copied pre-image the first
  time it's written after the savepoint (`engine._capture_savepoint_snapshots`, run before every
  DML inside the txn — the snapshot pins to the savepoint's establishment state since nothing wrote
  in between). `ROLLBACK TO` restores each collection to the oldest captured snapshot among the
  target savepoint and the nested ones (`delete_matching({})` + re-`insert`), drops the nested
  savepoints, un-poisons the block, and keeps the savepoint open; `RELEASE` merges a savepoint's
  snapshots down into its parent (oldest-per-collection wins) so the parent can still undo them.
  `parse()` rewrites `RELEASE SAVEPOINT x` → `RELEASE x` (sqlglot parses only the latter). Outside a
  txn block → `25P01`; unknown savepoint → `3B001`. **Limitations:** it's a collection-granularity
  snapshot (fine for the ephemeral test data SecantusDB targets, `O(rows-in-touched-collection)` per
  first-write-per-savepoint, not a WT-native savepoint); **DDL inside a savepoint is not undone**
  (`CREATE`/`DROP`/`CREATE INDEX` — only DML restores); and recovery after a storage-engine
  `WT_ROLLBACK`-class error (vs an ordinary constraint violation) may leave the WT txn unusable for
  the restore writes. `DEALLOCATE`/`DISCARD` remain no-ops. **Server-side cursors landed** (b72):
  `DECLARE name [WITH HOLD] CURSOR FOR <query>` materializes the query at declaration
  (`engine._declare_cursor`, stored as a `session._Cursor`); `FETCH` / `MOVE` walk a scroll position
  (`_cursor_slice` — forward / backward / absolute / relative, so cursors are fully scrollable) and
  `CLOSE name` / `CLOSE ALL` drop them. `FETCH` accepts `NEXT` / bare-count / `ALL` / `PRIOR` /
  `FIRST` / `LAST` / `FORWARD [n|ALL]` / `BACKWARD [n|ALL]` / `ABSOLUTE n` / `RELATIVE n`; `MOVE`
  positions without a result set. `WITHOUT HOLD` cursors close at COMMIT/ROLLBACK, `WITH HOLD`
  survive. `MOVE` is hand-built in `planner.parse` (sqlglot can't tokenize it); FETCH/DECLARE come
  through as `exp.Command`, CLOSE as a bare `Alias`. Unknown/closed cursor → `34000`. **Limitations:**
  the cursor is a materialized snapshot at DECLARE (later same-txn writes aren't visible through it),
  and it isn't wired into the extended protocol's Portal machinery (it's a SQL-level cursor, like
  psycopg's named server-side cursors). DDL is transactional via BEGIN/COMMIT/ROLLBACK. Cross-connection
  isolation is the WT engine's job (the test double only models atomicity).
- [ ] **CREATE/DROP INDEX landed; ALTER not.** `CREATE [UNIQUE] INDEX [name] ON t (col [DESC], …)`
  maps to `Storage.create_index` (PK column → `_id`; auto-generated `field_dir` name when
  unnamed; duplicate → `42P07`); `DROP INDEX [IF EXISTS] name` finds the owning collection by
  scanning the catalog and calls `drop_index` (`42704` when absent). Indexes now reflect back
  through `get_indexes` / `Table(autoload_with=…).indexes` (the populated `pg_index` virtual
  table — `indkey`/`indclass`/`indoption` are typed as `int2vector`/`oidvector` so a libpq
  client renders them as the space-separated text its catalog reflection parses). Still
  missing: `ALTER TABLE` and partial/expression index options via SQL.
- [ ] **Dev-env import shim:** `tests/conftest.py` stubs `wiredtiger` only when the
  extension is absent so the pure SQL/operator tests import without a WT build. Inert in
  CI. Revisit if `secantus/__init__` is made lazy (would let `secantus.sql` import without
  dragging in the WT-backed server).

---

When you fix one of these, delete the line. When you discover a new one, add it under the right section with enough context to come back to it cold.
