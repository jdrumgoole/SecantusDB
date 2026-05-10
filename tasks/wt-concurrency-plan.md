# Plan: unlock real WiredTiger write concurrency

Branch: TBD (`wt-concurrency`) · Worktree: `../SecantusDB-wt-concurrency`

## Goal

A single SecantusDB process serving N concurrent writer connections must scale write throughput sub-linearly with N (target: 0.6× per writer at N=4 on a single collection, 0.85× per writer at N=4 across distinct collections). Today, two writers on a single collection deliver **0.35×** the throughput of one writer — adding writers makes things worse, not better. See `bench/chaos.py` two-writer experiment numbers below for the baseline this plan must beat.

## Non-goals

- Multi-process / multi-host clustering. SecantusDB stays single-node.
- Lockless data structures inside Python — we'll lean on WT's MVCC, not invent our own.
- Read-side latency optimisation. Reads currently pay no lock cost; this plan must keep that property.
- Changing wire-protocol behaviour. PyMongo / mongo-go / mongo-node / mongo-java tests must still pass.

## Baseline (2026-05-10, captured by Phase 0)

Hardware: M-series Mac, on-disk WT, batch=100, logging on. Run via `invoke concurrency`.

**Per-writer collections** (each writer to its own collection — best case for cache locality):

| Writers | Total succeeded | Wall time | docs/s | Scaling |
|---|---|---|---|---|
| 1 | 79,000 | 30.0s | 2,633 | 1.00× |
| 2 | 30,100 | 30.0s | 1,003 | **0.38×** |
| 4 | 29,200 | 30.0s | 973 | 0.37× |
| 8 | 14,000 | 30.0s | 467 | **0.18×** |

**Shared collection** (all writers contend on the same `_id` space + same index entries — maximal contention):

| Writers | Total succeeded | Wall time | docs/s | Scaling |
|---|---|---|---|---|
| 1 | 38,000 | 30.0s | 1,266 | 1.00× |
| 2 | 14,000 | 30.0s | 467 | 0.37× |
| 4 | 32,100 | 30.0s | 1,070 | 0.84× |

**Read of the data**: scaling collapses immediately at N=2 and stays collapsed. Per-collection N=8 runs at **18%** of single-writer aggregate throughput — adding 7 more writers actively destroys 82% of throughput. The shared-collection numbers are noisier (a 4-writer run beat a 2-writer run, suggesting variance bigger than the signal at this lock-contended floor) but the same story: serialisation, not concurrency.

The Phase 0 regression test (`tests/test_concurrency.py::test_two_writers_scale_above_single_writer`, marked `slow`) asserts **N=2 ≥ 0.7×** and is intentionally red on `main` until Phase 2 lands. On 5s runs it currently measures around 0.59×.

## Architecture today

`Storage._lock` is a single `threading.RLock` taken by ~57 sites in `storage.py`. It protects, in one undifferentiated mass:

1. **Data writes** — `insert/update/delete` doc-table + index-entries-table writes
2. **Oplog state** — `_next_seq` counter, `_last_ts_secs/_last_ts_ord` cluster-time mint, `_oplog_emit_count` prune-cadence counter, oplog-table writes
3. **DDL** — `create_collection`, `drop_collection`, `drop_database`, `create_index`, `drop_index`, `rename_collection`, `coll_mod`
4. **Metadata caches** — collection-options registry, per-collection index list, partial-filter expressions, multikey flags
5. **Auth/RBAC** — user + role tables (rare path; not a hot loop)
6. **Lifecycle** — `_all_sessions` registry, `_closed` flag

Per-thread WT sessions already exist (`threading.local()`). WT itself supports concurrent writers via MVCC — we're throwing that away by serialising every write through Python.

## End-state architecture

| Resource | Today | Target |
|---|---|---|
| Doc-table + index-entries writes | `_lock` (RLock) | No Python lock; per-thread WT session + explicit `begin_transaction`. WT's MVCC handles conflicts. `cursor.insert(overwrite=False)` raises `WT_DUPLICATE_KEY` for unique-key races. |
| Oplog seq + timestamp mint | `_lock` | Tiny `_oplog_seq_lock` (`threading.Lock`), held only while incrementing the counter and updating the persisted meta row. Microsecond-scale critical section. |
| Oplog-table writes | `_lock` | Inside the same WT transaction as the data write. Each thread writes its own seq range, no cross-thread coordination after seq assignment. |
| DDL (create/drop/index) | `_lock` | RW lock (`SchemaLock`): DDL takes exclusive, CRUD takes shared. Built on `threading.Condition` since stdlib has no rwlock. |
| Metadata caches | `_lock` | Read-only after DDL; rebuilt lazily on a cache-version bump. CRUD uses snapshot-on-entry semantics. |
| `_oplog_cv` | already separate | Unchanged. |
| `_all_sessions` / `_closed` | `_lock` | `_closed` becomes an `Event`; `_all_sessions` becomes a thread-safe set with its own lock (only modified at session-create / session-close, both rare). |

## Phases

Each phase is independently mergeable — full test suite must stay green between phases, and the chaos + concurrency benchmark numbers should monotonically improve (or hold).

### Phase 0 — Instrument + benchmark harness (1 day)

**No behaviour change.** Establish the regression detector before any architectural surgery.

- [ ] Add `bench/concurrency.py`: spawns N writer processes (configurable 1, 2, 4, 8) against one server, runs each for 30s, prints aggregate + per-writer throughput, computes scaling ratio.
- [ ] Add `invoke concurrency` task wrapping it.
- [ ] Add a CI-affordable variant in `tests/test_concurrency.py` (marker: `slow`) that runs N=2 for 5s and asserts a basic floor (e.g. aggregate >= 0.7 × single-writer baseline). This will *fail* on the current code — that's the point; it'll only pass after Phase 2 lands. Keep marked `slow` so the default pytest run is unaffected; CI runs it on a separate matrix.
- [ ] Run the benchmark on three hardware profiles (M-series Mac, x86_64 Linux CI runner, aarch64 Linux CI runner) so the scaling target isn't tuned to one box.

**Exit criterion:** baseline numbers committed in `tasks/wt-concurrency-plan.md`; the regression test exists and (intentionally) fails on `main`.

### Phase 1 — Audit and shrink the lock scope (2 days)

**Small, safe, internal.** Each method that takes `_lock` gets reviewed for what it's actually protecting; mutations are kept under the lock, pure WT cursor reads are moved out. No locking-model change yet.

- [ ] Catalogue every `with self._lock:` call site (~57 of them). For each, annotate: writes? reads? holds across BSON encode? holds across user-supplied predicate? Output: a table in `tasks/wt-concurrency-plan.md`.
- [ ] Move long-running operations (BSON decode, predicate evaluation in `find_matching`) **out** of the lock when they're read-only — let WT cursors do their own snapshot iteration without Python serialisation.
- [ ] Identify any place where the lock is held while calling a user-supplied callable (filter predicate, projection); audit for re-entrance via the RLock and document why it's safe (or fix it).
- [ ] Tests: parallel-safe suite must stay green. Run the new `bench/concurrency.py` — should already nudge the 2-writer ratio above 0.35×, perhaps to 0.5–0.6×.

**Exit criterion:** annotated lock-call-site table in this doc; concurrency benchmark improves; full test suite passes.

### Phase 2 — Decompose the lock (1 week)

**Architectural.** This is the hot phase. `_lock` is replaced by a small set of purpose-built primitives.

- [ ] Add `_oplog_seq_lock = threading.Lock()`. Acquired only inside a new method `Storage._mint_oplog_seq_range(n: int) -> tuple[int, Timestamp]` that atomically reserves `n` consecutive seq numbers and one timestamp range. Held for nanoseconds. All `_emit_oplog` callers go through this; they then write the oplog rows in their own thread's WT transaction without holding the seq lock.
- [ ] Add `SchemaLock` class (RW lock built on `threading.Condition`): `acquire_shared()` for CRUD, `acquire_exclusive()` for DDL. Used to ensure DDL doesn't reshape schema while writes are mid-flight.
- [ ] Replace metadata caches with a snapshot pattern: `Storage._schema_snapshot()` returns an immutable view of `(collections, indexes, partial_filters, multikey_names)` valid for the life of one CRUD call. Bumped on DDL.
- [ ] Replace `Storage._lock` usage at the data-path call sites (`insert`, `update_matching`, `delete_matching`, `find_matching`, `find_and_modify`) with: take `SchemaLock.acquire_shared()` → fetch a schema snapshot → `with _batch_transaction():` for the WT work. Done.
- [ ] Replace `Storage._lock` at DDL sites with `SchemaLock.acquire_exclusive()` and bump the schema-version counter to invalidate cached snapshots.
- [ ] `_closed` becomes a `threading.Event`. Data-path operations check `_closed.is_set()` at entry and raise `StorageClosedError`.
- [ ] `_all_sessions` gets its own short-lived `_sessions_lock`.

**Risks:**
- **Unique-index race**: pre-check + insert was previously atomic under `_lock`. Now it's not. Mitigation: drop the pre-check; rely on `cursor.insert(overwrite=False)` raising `WT_DUPLICATE_KEY`, catch and translate to `DuplicateKeyError`. Already exists as a safety net in `Storage.insert`; promote to the only path.
- **Multikey flag race**: two threads inserting the same array-shaped doc both want to mark the index multikey. Race is benign — both writes set the flag to `True`, idempotent.
- **Capped-collection trim race**: `_enforce_capped_bounds_locked` deletes overflow docs; needs to stay serial per collection. Use a per-collection lock minted on demand (string-keyed dict + a metadata lock to mint).

**Exit criterion:** 2-writer ratio ≥ 1.5× (i.e. real scaling), 4-writer ratio ≥ 2.5×. Full test suite passes. Chaos benchmark numbers hold or improve.

### Phase 3 — WT explicit transactions per write batch (already done)

The `Storage._batch_transaction()` context manager from a prior commit is already in place around `insert`/`update_matching`/`delete_matching`. Phase 2 keeps it; no new work here. Listing it for completeness in the plan.

### Phase 4 — Concurrency stress + DDL races (3 days)

- [ ] Add `tests/test_concurrency_stress.py`: random mix of N writers + M readers + occasional DDL ops, run for 30s, assert no deadlocks and final state is consistent (count(docs) matches sum(per-writer-success-counts)).
- [ ] Add a focused test for the unique-index race: 8 threads racing to insert the same `_id`, assert exactly one wins and the other 7 raise `DuplicateKeyError`.
- [ ] Add a focused test for capped-collection trim under concurrent writers.
- [ ] Add a focused test for `dropCollection` mid-write: writers should see `NamespaceNotFound` cleanly (not segfault, not corrupt), and the collection state on reopen should be empty.
- [ ] Run all four test files under `pytest -n 8` to verify they're parallel-safe.

**Exit criterion:** stress tests pass for ≥1000 iterations under TSAN-style fuzzing (use Python's `faulthandler` + random sleeps to surface ordering races).

### Phase 5 — Documentation + release (1 day)

- [ ] Update `CLAUDE.md` "Architecture" section: document the new locking model and the snapshot pattern.
- [ ] Update `tasks/backlog.md`: close the durability stopgap entry's "throughput cost will be substantial" caveat (concurrency now makes batch-100 ~10× the prior single-writer floor).
- [ ] Add a docs page (`docs/concurrency.md`) explaining how the storage layer scales and what the architectural limits still are (single-process, single-host).
- [ ] Bump version, run full release flow.

**Exit criterion:** released, documented, regression test in default suite (no longer marked slow), backlog updated.

## Decision log

- **Why RW lock for schema, not optimistic CC?** Because most workloads are heavily CRUD-skewed (read-mostly; DDL rare). RW lock optimises for the common case (shared read access during CRUD) while still preventing DDL from reshaping schema mid-write.
- **Why drop the unique-conflict pre-check?** Two reasons: (a) it can't be made race-free without a lock; (b) WT already detects duplicates atomically as part of `cursor.insert(overwrite=False)`. The pre-check existed to give nicer error messages; we can build the same message from the conflict info WT returns.
- **Why a per-collection lock for capped trim, not the schema lock?** The schema lock is for DDL (creating/dropping indexes). Capped-collection trim is a data-path operation that happens to need exclusivity per collection. Different concern; deserves a different lock.
- **Why not just buy WT's per-table locking and be done?** WT's table-level locking is fine-grained and we'd benefit from it — but only if Python doesn't already serialise everything before WT sees the operations. Phase 2 is the prerequisite for WT's locks to even matter.

## Open questions

1. Does WT's `transaction_sync=enabled=false` sync log records on commit-without-fsync, or does it require an explicit `log_flush` for durability? (Affects whether we can keep current durability story under concurrent writers.)
2. Is `_emit_oplog`'s prune call (`_prune_oplog_locked`) expensive enough to be moved to a background thread instead of running synchronously every 1000 emits? (Current sync path is fine at low write rates but could become a stall under high-concurrency writers.)
3. Do we need to add `noWriteThrottle` flags to specific high-cardinality WT tables (oplog?) to reduce throttling-under-cache-pressure?

## Schedule estimate

| Phase | Effort | Wall clock |
|---|---|---|
| 0 — instrument + baseline | 1 day | 1 day |
| 1 — shrink lock scope | 2 days | 2 days |
| 2 — decompose lock | 5 days | 1 week |
| 3 — already done | — | — |
| 4 — concurrency stress | 3 days | 3 days |
| 5 — docs + release | 1 day | 1 day |
| **Total** | **~12 days** | **~2.5 weeks** |

Realistic for a focused track. If contention surfaces unexpected hazards (e.g. multikey-flag races require a different design), Phase 2 budget could double.

## Success criteria

The plan is done when:

- 2 writers / 1 writer aggregate throughput ratio ≥ **1.5×** on a single collection
- 4 writers / 1 writer aggregate throughput ratio ≥ **2.5×** on a single collection
- 8 writers / 1 writer aggregate throughput ratio ≥ **3.5×** on distinct collections
- Full pytest suite passes (no flakes under `-n 8`)
- pymongo / mongo-go / mongo-node / mongo-java conformance gauges are unchanged or higher
- Chaos durability ratio (acked vs persisted) at concurrent batch=100 is ≥ 99% (matches current single-writer behaviour)
- No new fsync calls per write (durability cost stays at "log written, OS-flushed eventually")
