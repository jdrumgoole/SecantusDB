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
   partial / TTL). Gate: `test_indexes.py`.
3. **Geo** — `2dsphere` (s2) + `2d` (geohash) index acceleration, golden vectors.
   Gate: `test_geo_index.py`.
4. **Oplog + change-stream storage** — oplog / pre-images / meta tables, cluster
   time, retention, noop heartbeats; then re-home `$lookup` / `$geoNear` pipeline
   acceleration here. Gate: `test_change_streams.py`.

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

## Open gate for the whole phase: the wheel matrix

`secantus-wt` / `secantus-storage-py` build and test where WiredTiger is present
(dev machines, and CI jobs that build the vendored WT). The unsolved question —
and the phase's go/no-go — is **shipping it**: the WiredTiger-linking Rust
extension has to build clean across the wheel matrix (cp310–313 × manylinux /
musllinux / macOS-arm64 / Windows), the same matrix the pure `secantus-core`
wheel already covers. The maturin build today produces only a host-glibc wheel.
Options:

- Link the same vendored WiredTiger the main `secantus` wheel already builds
  (scikit-build-core CMake output) into the storage extension, reusing that
  toolchain rather than maturin's manylinux container.
- Or build the storage extension through the existing scikit-build path (which
  already vendors + builds WiredTiger) instead of maturin.

Until that's resolved, `secantus-wt` is deliberately excluded from the
`crates/Cargo.toml` workspace so the green `secantus-core` / `secantus-core-py`
build and the `rust` / `rust-wheels` CI stay untouched. CI coverage for
`secantus-wt` itself is a follow-up: it needs a job that builds the vendored
WiredTiger first, then `SECANTUS_WT_INCLUDE`/`_LIB` → `cargo test` in the crate.
