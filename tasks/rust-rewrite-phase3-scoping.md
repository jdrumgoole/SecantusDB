# Phase 3+4 scoping — cursors / change streams, then the storage keystone

Written after Phases 1–2 landed (all six leaf engines + the entire
storage-*independent* aggregation pipeline are ported, parity-pinned, and on
`claude/python-rust-rewrite-plan-wjZou`). This note refines the roadmap in
`tasks/rust-rewrite-plan.md` §6 with what we learned and with the concrete next
slices. Numbering follows the plan: **Phase 3 = cursors + change-stream
projection**, **Phase 4 = storage (the keystone)**.

## 0. Two constraints that shape everything below

- **The byte-seam + graceful-fallback + parity-fuzz method does NOT transfer to
  storage.** Phases 1–2 worked because the operator engines are *pure functions*
  (docs in, docs out): a per-call `Fallback` to Python is always safe. Storage is
  *stateful* — WT transactions/cursors, the global `RLock`, monotonic oplog `seq`
  minting, thread-local sessions. You cannot "fall back to Python mid-transaction."
  So storage is ported **all-or-nothing per table-family with a hard cutover**,
  not the per-operator gradual widening that worked for the engines.

- **None of the Phase-3/4 modules can be conformance-tested in the current
  sandbox.** `test_change_streams.py`, `test_storage.py`, `test_indexes.py`,
  `test_geo_index.py` all import `wiredtiger`, which is not installable here
  (the Rust parity suites only run because they load the *pure* modules by path
  under a stub `secantus`). Phase-3b (projection) can be pinned here only by a
  **mock-storage** parity test; Phase 4 (storage) needs a WT-capable machine / CI.
  This is a real execution boundary, independent of design.

## 1. Value check (why the order is what it is)

The multi-core win comes from running the **GIL-bound hot path** in Rust under
`Python::allow_threads`. The hot path is CRUD + queries + the pipeline (Phase
1–2, done) and **storage** (WT access, Phase 4). The Phase-3 modules — cursor
bookkeeping and change-stream event projection — fire per-getMore / per-oplog-
event, **off** the hot path. So Phase 3 is low-value, low-risk *sequencing*
ahead of the keystone; the real remaining win is Phase 4.

## 2. Phase 3 — what actually ports

| Unit | Nature | Seam | Verdict |
|---|---|---|---|
| `changestreams.make_resume_token` / `parse_resume_token` | pure `bson` round-trip + hex | trivial byte seam | **3a — port now, runnable here** |
| `changestreams.project` event-shaping core | pure given the oplog entry + scope + modes | byte seam (oplog entry in, event out) | **3b — port; mock-storage parity only here** |
| `project`'s `fullDocument` (updateLookup) + `fullDocumentBeforeChange` | needs `storage.find_matching` / `read_preimage` | storage-coupled | **inject** the looked-up doc / pre-image as inputs; keep the lookup in Python until Phase 4 |
| `cursors.CursorRegistry` | stateful, lock-held, holds Python `producer` closures | not a pure seam | **defer to Phase 5** (becomes a Rust struct owned by the Rust server) |

### Slice 3a — resume-token codec (recommended immediate slice)
- `cs_make_resume_token(bytes) -> bytes` / `cs_parse_resume_token(bytes) -> bytes`
  behind the byte seam; `make_resume_token`/`parse_resume_token` become shims.
- Parity-pinned with a fuzz over `(seq, ts, ns, document_key)` tuples vs the pure
  functions. No WT needed → fully runnable in this sandbox.
- Honest caveat: the perf win is ~nil (it's a 4-field encode off the hot path).
  Its value is **validating the change-stream seam** and keeping the cadence.

### Slice 3b — change-stream projection core
- New seam `cs_project(seq, oplog_entry, scope, full_document_mode,
  before_mode, show_expanded) -> {event, invalidates, needs_update_lookup,
  needs_preimage}`. Rust does *all* the shaping (op→type, `update` vs `replace`,
  `updateDescription`, DDL drop/dropDatabase/rename, resume tokens, scope match,
  noop skip). It does **not** read storage: when `fullDocument`/`before-change`
  needs a doc, it sets `needs_update_lookup`/`needs_preimage`; the Python caller
  performs the storage read and attaches (the existing `_attach_full_document*`
  stay in Python until Phase 4).
- Parity: a mock `Storage` (returning canned docs / pre-images) drives both the
  pure `project` and the Rust path; assert event-equality across a corpus of
  oplog entries (i/u/d/replace/c-drop/c-dropDatabase/c-rename/noop) × scopes ×
  modes. Runnable here. **Real conformance (`test_change_streams.py`) must run in
  CI / on a WT machine before merge.**

## 3. Phase 4 — storage keystone (refined strategy)

The plan's §4.1/§4.2/§5 already cover the big decisions; the refinements:

- **Engine: option A (WT FFI via `bindgen` + `build.rs`).** The project vendors
  WiredTiger precisely for the "same engine MongoDB uses" / "on-disk semantics
  line up with `mongod`" product value (see CLAUDE.md). Option B (Rust-native KV:
  `redb`/`rocksdb`/`heed`) abandons that and is a visible product change needing
  explicit sign-off (§10). **Recommend A; keep B as the escape hatch** only if
  the FFI/wheel cost proves prohibitive. Phase-0 spike 2 already proved
  open/session/cursor/insert/scan from Rust against the vendored WT.

- **On-disk format: bump the version, drop backward-compat, reproduce encodings
  faithfully anyway.** Ephemeral test data — nobody carries a SecantusDB file
  across the rewrite. But keep `sortkey` (already ported + golden-pinned) and the
  geo encodings byte-faithful so `test_indexes.py` / `test_geo_index.py` /
  `test_sort_with_collation.py` pass unchanged.

- **Dual-engine at `Storage` granularity, not per-operation.** You can't run
  half-Python/half-Rust storage on one DB in one process. So there are two whole
  `Storage` implementations behind one interface, selected process-wide by the
  existing `secantus.engine` machinery at construction. This preserves the
  "both engines permanent" invariant — just at a coarser grain than the engines.

- **Concurrency: 1:1 port first.** Thread-per-connection; global `RLock` →
  `parking_lot::ReentrantMutex` (or a plain coarse `Mutex` keeping today's
  serialize-everything discipline); thread-local WT sessions → `thread_local!`;
  the change-stream `Condition` (separate from the storage lock) → `Condvar`.
  Revisit async only if a perf gauge demands it.

- **Sub-phase by table family, gate each on its suite:**
  1. `collections` + `documents` tables — the CRUD core (insert / find-by-`_id` /
     update / delete / natural-order scan), `:memory:` + on-disk open/reopen.
     **The first vertical** — smallest end-to-end proof of the FFI + `bson` +
     `sortkey` + lock stack. Gate: `test_storage.py` roundtrip + `test_crud.py`.
  2. `indexes` + `index_entries` + the planner (`find_matching` / `explain_plan`
     / all pickers; single/compound/multikey/partial/TTL). Gate: `test_indexes.py`.
  3. geo (`geo`/`s2` crates, golden vectors). Gate: `test_geo_index.py`.
  4. oplog + pre-images + meta + cluster time + retention + noop heartbeats; then
     re-home `$lookup`/`$geoNear` pipeline acceleration here. Gate:
     `test_change_streams.py`.

- **Go/no-go gate for the whole phase:** the WT FFI must build clean across the
  wheel matrix (cp310–313 × manylinux/musllinux/macos-arm64/windows). If it
  doesn't, that's the trigger to reconsider option B.

## 4. Recommended next action

Phase 3a (resume-token codec) is the only Phase-3/4 unit that is both *runnable*
and *low-risk* in this sandbox; everything past it (3b mock-only, all of Phase 4)
needs WiredTiger + CI to validate honestly. The decision of where to point next
(small runnable slice now vs. designing/prototyping the storage keystone that
can't be tested here vs. consolidating) is the user's — see the chat.
