# Phase 4 — storage keystone: scoping & status

Phase 4 moves SecantusDB's WiredTiger-backed storage layer into Rust. It is the
keystone: it unblocks running the whole read hot path (scan → filter → project)
in Rust under one GIL release, and it is the prerequisite for a standalone Rust
`secantusdb` binary. Strategy decisions live in `tasks/rust-rewrite-plan.md` §4–5
and `tasks/rust-rewrite-phase3-scoping.md` §3; this doc tracks the concrete
build-out.

## Decisions (locked)

- **Engine: WiredTiger FFI (option A).** Keep the same engine MongoDB uses — the
  product value is "on-disk semantics line up with `mongod`". The FFI is via
  `bindgen` + `build.rs` linking the vendored `libwiredtiger`. (Option B, a
  Rust-native KV, remains the escape hatch only if the wheel-matrix link proves
  prohibitive.)
- **Dual-engine at `Storage` granularity.** You cannot run half-Python /
  half-Rust storage on one DB in one process, so there are two whole `Storage`
  implementations behind one interface, selected process-wide by
  `secantus.engine`. (Coarser than the per-operator engine selection, but it
  preserves the "both engines permanent" invariant.)
- **Concurrency: 1:1 port first.** Global `RLock` → a coarse reentrant mutex,
  thread-local sessions → `thread_local!`, the change-stream condition → a
  `Condvar`. Revisit only if a perf gauge demands it. (Note: the WiredTiger
  C-level write-concurrency ceiling documented in `tasks/wt-bindings-plan.md`
  still applies — Phase 4's win is the read/query hot path and the standalone
  binary, not multi-writer scaling.)
- **On-disk format: reproduce encodings faithfully, no cross-rewrite migration.**
  Test data is ephemeral. Keep `sortkey` (already ported + golden-pinned) and the
  geo encodings byte-faithful so the index/geo/sort suites pass unchanged.

## Sub-phases (each gated on its WT-backed suite)

0. **FFI foundation — `crates/secantus-wt`. ✅ DONE (this slice).**
   bindgen + `build.rs` (WiredTiger resolved via `SECANTUS_WT_INCLUDE` /
   `SECANTUS_WT_LIB` or probed build dirs); safe `Connection` / `Session` /
   `Cursor`; the key formats SecantusDB uses (`SS` / `SSu` / `SSS` / `SSSu` /
   `q` / `S` / `u`); `WT_NOTFOUND` / `WT_DUPLICATE_KEY` / `WT_ROLLBACK`
   translation; transactions. Verified against real WiredTiger: insert / natural
   (byte-)order scan / point search / NOTFOUND / update / remove / numeric `q`
   ordering / commit+rollback / on-disk reopen persistence. `cargo fmt` +
   `clippy -D warnings` clean.
1. **CRUD core — `crates/secantus-storage`. ✅ DONE at the Rust level (this
   slice).** A `Storage` over `secantus-wt` + `secantus-core`'s `sortkey`: the
   `secantus_collections` + `secantus_documents` tables, `insert_one`
   (auto-`ObjectId`, duplicate-`_id` rejection), `find_by_id`, `scan_collection`
   (natural order), `replace_by_id`, `delete_by_id`, collection registry, the
   coarse serialize-everything lock. `id_key = sortkey.encode_value(_id)`.
   Verified against real WiredTiger (7 integration tests): cross-type natural
   order (double/int/long → string → ObjectId), db/coll isolation, reopen
   persistence; `cargo fmt` + `clippy -D warnings` clean (`invoke
   rust-storage-test`). **Still pending for the conformance gate:** the PyO3
   exposure + `secantus.engine` storage selection, then `test_storage.py` /
   `test_crud.py` under `SECANTUS_ENGINE=rust`.
   - Found + fixed a real latent use-after-free in `secantus-wt`: WiredTiger
     references the caller's memory for `S`/`u` columns until the operation, so
     the `Cursor` now **owns** its key/value buffers (`*_hold` fields) instead of
     leaving callers to keep temporaries alive.
2. **Indexes** — `secantus_indexes` + `secantus_index_entries` + the planner
   (`find_matching` / `explain_plan` / all pickers; single / compound / multikey /
   partial / TTL). Gate: `test_indexes.py`. **Sliced** (2a → 2f):
   - **2a ✅ DONE (this slice)** — the registry + `create_index` / `list_indexes`
     / `drop_index` / `drop_all_indexes`, and index-entry maintenance on
     insert / replace / delete. Byte-faithful entry packing (`escape_kb` /
     `pack_entry` / `unpack_entry`), the `index_key_variants` builder (scalar /
     descending-inverted / multikey per-element + whole-array / compound
     cartesian product), and create-time multikey-flag detection. Geo / text /
     hashed rejected (`CreateIndexUnsupported`); re-create-with-conflicting-opts
     rejected. Pinned by byte-exact unit tests (`crates/secantus-storage/src/
     lib.rs` `#[cfg(test)]`) + WiredTiger-backed integration tests
     (`crates/secantus-storage/tests/indexes.rs`). Added `secantus-wt`
     `get_key_sss` / `get_key_sssu` getters and a `secantus_core::{get_path,
     has_path}` re-export. **Deferred from 2a:** lazy multikey-flag marking on
     insert/update (2d); sparse / partial entry-gating, unique enforcement, TTL,
     collation (2e).
   - **2b ✅ DONE** — `find_matching` + `explain_plan`: single-field equality /
     `$eq` / `$in` / range (`$gt`/`$gte`/`$lt`/`$lte`, with DESC operator-flip)
     routing through the entries table, the `_id` primary-key point-lookup fast
     path (bare / `$eq` / `$in`), and COLLSCAN fallback — index candidates are
     re-checked with `secantus_core::query::matches` (which can over-include for
     multikey). `explain_plan` returns `ExplainPlan::{CollScan, IxScan{...}}`.
     New WT-backed tests in `tests/query.rs`. **Scoped to single-field indexes**
     (compound leading-field use is 2c, so the executor and `explain` stay
     consistent); a `matches()` "defer to Python" construct (e.g. whole-array
     literal equality) surfaces as `StorageError::QueryUnsupported` for the
     server's engine-selection layer to route to Python.
   - **2c ✅ DONE** — compound-index routing: bare-equality prefix (full-cover
     exact scan + strict-leading-prefix scan, filter-field-order-independent,
     shortest-covering-index preference), prefix + trailing-operator
     (`$eq`/`$in`/range on the next column, range pinned to the equality
     prefix), and mixed-direction compound indexes (per-field
     `encode_value_directed`, DESC operator-flip). Restored the `prefix` param on
     `range_scan_index` and added `range_scan_index_leading` (leading-field range
     with escaped-separator boundary detection); `find_leading_field_index` now
     returns the compound fallback so a single-field filter can ride a compound
     index's leading field. Pickers (`pick_compound_eq_index` /
     `pick_compound_range_index` / `partition_compound_range_filter`) shared by
     execution and `explain`. New WT-backed tests in `tests/compound.rs`.
   - **2d ✅ DONE** — lazy multikey marking: `maybe_mark_multikey` rewrites an
     index's registry options with `multikey: true` when an inserted/replaced doc
     has an array on an indexed field (sticky — never cleared); wired into
     `insert_one` / `replace_by_id`. (The sort-acceleration *exclusion* of
     multikey indexes is part of 2f, where sort lands.) New tests in
     `tests/indexes.rs` (lazy-mark-on-insert, mark-on-replace, sticky).
   - **2e ✅ DONE (collation deferred)** — three sub-commits:
     - **2e-1** — write-path correctness: an `IndexDesc { name, key_spec, sparse,
       unique, partial }` refactor of the CRUD entry-maintenance paths, the
       canonical `index_key` (one byte-key per doc, `None` under sparse when a
       field is missing), `sparse` support in `index_key_variants`, **unique
       enforcement** (`unique_conflict` prefix-probe on insert / replace /
       create-over-existing-data → `StorageError::DuplicateKey(Box<UniqueConflict>)`
       with the mongod-shaped `keyPattern`/`keyValue`), and **partial-index entry
       gating** (entries written / uniqueness scoped only to docs matching
       `partialFilterExpression`).
     - **2e-2** — read-path partial: `query_implies_partial` gates partial indexes
       in `find_leading_field_index` / `pick_compound_eq_index` (which also strips
       the partial-filter keys from the effective filter fields) /
       `pick_compound_range_index`.
     - **2e-3** — TTL: `prune_ttl(db, coll, now)` deletes docs whose TTL-indexed
       `DateTime` is older than `now - expireAfterSeconds` (injected clock, no
       background sweeper; oplog emission is sub-phase 3).
     - **Collation: deferred.** The Rust `sortkey` / `query` engines defer
       collation to Python (return the `Fallback` signal), so the canonical-key
       encoding and `matches()` can't honour a collation at this layer — the
       picker-level collation gates have nothing to gate against yet. Revisit when
       the leaf engines grow collation support.
     - New WT-backed tests: `tests/unique.rs` (9), `tests/partial.rs` (2),
       `tests/ttl.rs` (3).
   - **2f ✅ DONE** — sort acceleration + `hint`. `find_matching_with(filter,
     sort, hint)` / `explain_plan_with(...)` (the 3-arg forms are convenience
     wrappers): a single-field sort on the filter field, or an empty-filter sort
     matching a single-field / compound index, is served by walking the index
     forward / backward (skipping the post-sort); otherwise a COLLSCAN + a
     byte-sortable-key post-sort (`sort_key` via the same encoder, so order is
     consistent with the accelerated path). `hint` (`Hint::Name` /
     `Hint::KeySpec`) resolves to `$natural` / `_id_` / a named index
     (`resolve_hint` / `candidates_from_hint`); an unresolvable hint is
     `StorageError::BadHint` in `find` and COLLSCAN in `explain`.
     `compound_index_for_sort` is strict-shape and **excludes multikey indexes**
     (the 2d flag's payoff). `explain` now sets `direction` (`forward` /
     `backward`) via `make_ixscan_plan`. Ported from `storage`'s
     `_single_sort_spec` / `_multi_sort_spec` / `_compound_index_for_sort` /
     `_walk_index_in_order` / `_resolve_hint` / `_candidates_from_hint` /
     `_make_ixscan_plan` and `find_matching`'s sort/hint branches. New WT-backed
     tests in `tests/sort.rs` (10).

   **Sub-phase 2 complete (collation deferred).** 72 storage tests; the
   secantus-storage crate now covers the index registry, entry maintenance,
   single-field + compound + `_id` lookup routing, unique / sparse / partial /
   TTL, and sort + hint — all byte-faithful to `storage.py` and `clippy
   -D warnings` clean.

   **CI:** a `rust-storage` job in `.github/workflows/test.yml` builds the
   vendored WiredTiger via `uv sync` (`build/*/wt-build`, static
   `libwiredtiger.a`), points `SECANTUS_WT_INCLUDE`/`SECANTUS_WT_LIB` at it, and
   runs `cargo fmt --check` / `clippy -D warnings` / `cargo test` for the crate
   on every push-to-main / PR (Linux; cross-platform WT linking stays covered by
   the `storage-engine` wheel job). Next: sub-phase 3 (oplog / change streams).
3. **Geo** — `2dsphere` (s2) + `2d` (geohash) index acceleration, golden vectors.
   Gate: `test_geo_index.py`. The recon found geo is **not** storage-only: the
   Rust query engine had no geo operators, so a storage geo index is moot until
   `find_matching`'s post-filter `matches()` can evaluate geo predicates. Sliced
   geo-1 → geo-4:
   - **geo-1 ✅ DONE** — geo *query operators* in `secantus-core`. New
     `secantus_core::geo`: doc/query geometry coercion (GeoJSON / legacy `[x,y]` /
     `{x,y}`/`{lng,lat}`), planar containment via the `geo` crate's DE-9IM
     `Relate` (same OGC lineage as Shapely), haversine for `$centerSphere`;
     `$geoWithin` + `$geoIntersects` wired into `query.rs` `op_matches`. `$center`
     (Shapely 64-gon buffer — can't reproduce exactly) defers to Python via
     `Fallback`. Added the `geo = "0.28"` dep.
   - **geo-1b ✅ DONE** — `$near` / `$nearSphere` field-operator *matching* (within
     `[$minDistance, $maxDistance]`; sort-by-distance stays in the command layer):
     GeoJSON `{$geometry: Point, $maxDistance, $minDistance}` (metres) + legacy
     `[x,y]`/`[x,y,max]` (planar, or radians→metres for `$nearSphere`); the legacy
     *sibling* `$maxDistance` form falls back automatically ($maxDistance is a
     separate unknown op in the condition doc). Geo cases added to
     `test_rust_query_parity.py` — validated locally (built the extension; 105
     curated cases pass, incl. geo) and re-run by CI's `rust` job under the real
     extension.
   - **geo-2 (secantus-storage):** `2d` geohash index — write bit-interleaved
     buckets at the index's `bits` precision; route `$geoWithin` `$box`/`$center`
     to a single `(lo,hi)` bbox range scan. No external crate.
   - **geo-3 (secantus-storage):** `2dsphere` S2 cell coverings (needs `s2`
     crate — verify first). Each geometry writes covering cells + ancestors;
     queries do exact cell point-lookups + Shapely/haversine verify.
   - **geo-4:** `$geoNear` aggregation stage.
   - When geo-2/3 land, relax `create_index`'s current `CreateIndexUnsupported`
     rejection of `2dsphere`/`2d` (text/hashed stay rejected), and flag geo
     indexes `multikey: true` so the regular pickers skip them.
4. **Oplog + change-stream storage** — oplog / pre-images / meta tables, cluster
   time, retention, noop heartbeats; then re-home `$lookup` / `$geoNear` pipeline
   acceleration here. Gate: `test_change_streams.py`. **Sliced** (3a → 3e):
   - **3a ✅ DONE** — oplog foundation: `OplogState` (next_seq + last_ts) under a
     dedicated mutex, strictly-monotonic `mint_ts` / `mint_seq_and_ts`,
     `current_cluster_time`, `emit_oplog` (stamps `ts` + `wall`), op `"i"`
     emission wired into `insert_one`, `read_oplog(start, limit)` /
     `oplog_floor_seq` / `oplog_tail_seq` cross-thread reads (each call opens a
     fresh session — no sticky snapshot), and seq recovery on open
     (`load_oplog_meta`: meta row, else fallback scan). `enable_oplog` (default
     true) + `set_enable_oplog`. New tests in `tests/oplog.rs`. **Deferred:**
     the collection-UUID `ui` field (3c).
   - **3b ✅ DONE** — `replace_by_id` emits op `"u"` with `o` = the full new doc
     (the replacement shape — mongod's `$v:2` diff is only for operator-updates,
     which the storage layer doesn't expose; that path waits for a future
     `update_*` method using `secantus-core`'s `diff`), and `delete_by_id` emits
     op `"d"` with `o`=`o2`=`{_id}`. New tests in `tests/oplog.rs`
     (replace→`u`+full-doc, delete→`d`, insert/replace/delete → `[i,u,d]`).
   - **3c ✅ DONE** — collection UUID (`ui`) + pre-images. Collection-options
     read/write (`coll_options` / `write_coll_options`), `collection_uuid` (16
     bytes minted from two `ObjectId`s — no `uuid` crate dep — and persisted into
     the options on first use), the `ui` Binary subtype-4 field on every
     insert/replace/delete entry, `set_collection_options` (e.g.
     `{changeStreamPreAndPostImages: {enabled: true}}`), pre-image writes on
     replace/delete when enabled (`emit_oplog` now threads a parallel
     `pre_images` vec), and `read_preimage`. New tests in `tests/oplog.rs`
     (stable per-collection ui, pre-images when enabled, none when disabled).
   - **3d ✅ DONE (condvar deferred)** — retention `prune_oplog(now)` (drops rows
     past `oplog_retention_seconds`, then the oldest over `oplog_max_entries`,
     plus paired pre-images; injected clock, explicit-only like `prune_ttl`),
     `emit_noop_heartbeat` (op `"n"`), `find_seq_for_ts` (for
     `startAtOperationTime`), and `set_oplog_retention_seconds` /
     `set_oplog_max_entries`. New tests in `tests/oplog.rs` (retention prune,
     keep-recent, entry-cap, pre-image co-deletion, heartbeat shape,
     find-seq-for-ts). **Deferred:** the change-stream condvar / tailable-wait
     primitive — it only matters once a tailable cursor exists (server layer) and
     needs `Storage: Sync` + multithreaded tests; lands with that consumer.
   - **3e ✅ DONE** — change-stream event projection (`crates/secantus-storage/src/
     changestreams.rs`, a faithful port of `changestreams.py`): resume tokens
     (`{_data: hex}` over `{s,t,n,k}`, `make`/`parse`), `Scope`
     (cluster/db/coll) filtering, `project()` mapping insert/update/replace/
     delete/drop/dropDatabase/rename/createIndexes/dropIndexes (+
     `showExpandedEvents` gating) with `clusterTime`/`wallTime`/`ns`/
     `documentKey`, `updateDescription` (diff vs full-doc → update vs replace),
     `fullDocument` (incl. `updateLookup` re-fetch via `find_matching`),
     `fullDocumentBeforeChange` (via `read_preimage`; `required` → code-280
     `ChangeStreamFatal`), `invalidate_event`, and `stamp_split_event` (16 MB
     fragment envelope). New `tests/changestreams.rs` (11). The noop heartbeat
     projects to nothing (advances position only).
   - Then (follow-on): re-home `$lookup` / `$geoNear` pipeline acceleration onto
     the Rust storage, and the tailable-cursor / server cutover.

   **Sub-phase 3 complete.** The oplog / change-stream *storage* layer is done —
   emission (i/u/d), `ui` + pre-images, cluster time, retention, heartbeats,
   recovery, and event projection — 91 storage tests, `clippy -D warnings` +
   `fmt` clean. Deferred within the phase: the operator-update diff path (needs a
   future `update_*` storage method) and the tailable-wait condvar (server layer).

## PyO3 exposure (done) — `crates/secantus-storage-py`

The Rust storage is exposed to Python as the `_secantus_storage` extension
(`RustStorage` over the BSON byte seam, `_id` wrapped as `{"v": id}`). This
proves the **WiredTiger-linking extension builds (maturin → abi3 wheel) and
imports**, and drives the CRUD core end-to-end from Python
(`tests/test_rust_storage_smoke.py`; `invoke rust-storage-py`). That de-risks the
core of the gate below — what remains is the cross-platform *packaging*, not
whether a WT-linking Python extension can work at all.

Note: this is **not** yet wired into `secantus.engine`'s storage selection.
Swapping the Rust `Storage` into `SecantusDBServer` needs the *whole* `Storage`
surface (`find_matching`/indexes/oplog/…), so the server cutover is gated on
sub-phases 2-4, not just the CRUD core.

## The wheel-matrix gate — decision + status

**Decision (chosen): bundle the extension into the `secantus` wheel behind an
off-by-default build flag.** The WiredTiger-linking Rust `_secantus_storage`
extension is built by the main wheel's existing CMake against the SAME vendored
WiredTiger that wheel already builds — gated behind the
`SECANTUS_BUILD_STORAGE_ENGINE` CMake option, which defaults **OFF**. With the
flag OFF (the shipping default) the wheel is byte-for-byte unchanged and needs no
Rust/clang toolchain; the pure-Python storage path remains the default until
engine-selection makes the Rust storage engine selectable.

**Why this over a separate companion wheel.** A separate `secantus-storage` wheel
(building its own static WiredTiger via a `maturin-action` workflow) was
prototyped first and rejected after CI evidence: it re-derives the entire
cross-platform WiredTiger build *outside* cibuildwheel (toolchain install,
Python-dev for WT's CMake, the WT patches, per-platform link flags) — every CI
failure was something the main wheel's cibuildwheel + CMake build already solves.
Bundling reuses that proven WT build, so the flag-on path inherits the main
wheel's cross-platform machinery instead of reimplementing it. The cost — a
Rust/clang build dep *when the flag is on* — is acceptable precisely because the
flag is off by default and only flipped on deliberately.

**How it works (root `CMakeLists.txt`).** The `wiredtiger_ext` ExternalProject
builds WiredTiger static (`libwiredtiger.a` + generated headers under
`build/{wheel_tag}/wt-build`). When `SECANTUS_BUILD_STORAGE_ENGINE=ON`, a custom
command runs `cargo build --release` on `crates/secantus-storage-py` with
`SECANTUS_WT_INCLUDE` / `SECANTUS_WT_LIB` pointed at that WT output (bindgen finds
libclang from the environment), renames the cdylib to the platform Python-
extension filename, and `install(... DESTINATION .)`s it at the wheel root so
`import _secantus_storage` resolves.

**CI.** The `storage-engine` job in `.github/workflows/test.yml` is a 3-OS matrix
(Linux / macOS / Windows): each builds the `secantus` wheel with
`SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON`, asserts
`_secantus_storage` is bundled (hard failure if missing), and runs
`tests/test_rust_storage_smoke.py` against the installed wheel. The storage
crate's WiredTiger system-link flags are cfg-gated per target OS in
`crates/secantus-wt/build.rs` (Linux `pthread`+`rt`+`dl`; macOS `pthread`+`dl`,
no `librt`; Windows none beyond the MSVC defaults), mirroring what WT's own CMake
detects.

`secantus-wt` / `secantus-storage` stay excluded from the `crates/Cargo.toml`
workspace so the green `secantus-core` / `secantus-core-py` build and the `rust` /
`rust-wheels` CI stay untouched. `crates/secantus-storage-py/pyproject.toml` is
retained for local dev only (`invoke rust-storage-py` via maturin) — not
published to PyPI.
