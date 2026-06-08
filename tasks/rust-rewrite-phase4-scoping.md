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

## The wheel-matrix gate — decision + status

**Decision (chosen): a separate companion wheel.** `secantus-storage` ships as
its own wheel that builds its own vendored WiredTiger (static) and links the Rust
extension against it — leaving the main `secantus` wheel completely untouched
(symmetry with the `secantus-core` companion). The alternative (bundling the
extension into the `secantus` wheel to reuse its CMake WT build) was rejected to
keep the load-bearing main wheel unchanged and the extension optional.

**Recipe — proven locally end-to-end:**
1. `cmake/build_wt_static.py <dir>` builds vendored WiredTiger **static**
   (`ENABLE_STATIC=ON / ENABLE_SHARED=OFF / ENABLE_PYTHON=OFF / ENABLE_CPPSUITE=OFF`
   — no SWIG needed), applying the same `patch_wt_strict` / `patch_wt_musl`
   patches the main build uses. Verified: builds `libwiredtiger.a` + headers.
2. `maturin build` the extension with `SECANTUS_WT_INCLUDE` / `SECANTUS_WT_LIB`
   pointing at that build (bindgen needs `LIBCLANG_PATH`). Verified: produces an
   abi3 wheel that imports and round-trips CRUD through WiredTiger.

**CI — `.github/workflows/storage-wheels.yml`** runs that recipe per platform via
maturin-action (`before-script-linux` builds WT in the manylinux_2_28 container,
then maturin links it). Started with the **Linux targets**; macOS / Windows /
musl are follow-up matrix entries (same recipe + platform toolchain setup) and
will be shaken out via CI — cross-platform native-link packaging can't be
validated locally. Tag-gated publish needs a **PyPI Trusted Publisher for the
`secantus-storage` project** (one-time, like `secantus-core`).

The crates stay excluded from the `crates/Cargo.toml` workspace so the green
`secantus-core` / `rust` / `rust-wheels` CI is untouched.
