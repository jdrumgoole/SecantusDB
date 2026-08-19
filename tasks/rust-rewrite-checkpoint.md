# Rust rewrite — checkpoint (branch `claude/python-rust-rewrite-plan-wjZou`)

> **HISTORICAL — superseded (audited 2026-08-20).** This plan is built on the
> *in-process selectable-engine* model (`SECANTUS_ENGINE=python|rust|auto`, the
> `secantus.engine` shims, the `EngineFallback` adapter). That model was retired
> in favour of **two separate servers**, and CLAUDE.md names
> `tasks/rust-server-plan.md` as the authoritative plan. Kept for the design
> reasoning and the measurements; do not take its next-steps as current work.

> ⚠️ **Integration model changed since this checkpoint — see `tasks/rust-server-
> plan.md` (authoritative).** This checkpoint describes the in-process
> selectable-engine model (`SECANTUS_ENGINE`, the per-component shims). That has
> been replaced by **two separate servers** (pure-Python + a self-contained Rust
> server with a thin embedded Python handle). The *engine/storage porting* work
> recorded here is still valid; the *selection/cutover* framing is superseded.

HEAD `87644fa`, 29 commits ahead of `main`, working tree clean, all pushed.
This is the state at which the WiredTiger-free work was taken as far as it
honestly goes. Companion docs: `tasks/rust-rewrite-plan.md` (strategy),
`tasks/rust-rewrite-spike-findings.md` (per-engine notes),
`tasks/rust-rewrite-phase3-scoping.md` (what's next), `benchmarks/RESULTS.md`
(perf), `tasks/backlog.md` §7 (running checklist).

## What's done (and locally verified)

- **Phase 0** spikes (BSON fidelity, WiredTiger FFI, sortkey golden vectors).
- **Phase 1** — all six leaf engines + collation ported behind the BSON byte
  seam with graceful per-call fallback: `sortkey`, `query.matches`,
  `update.apply_update`, `expressions.evaluate`, `projection.apply_projection`,
  `diff.compute_update_description`.
- **Phase 2** — the entire storage-*independent* aggregation pipeline:
  `$match`/`$project`/`$addFields`/`$set`/`$unset`/`$replaceRoot`/`$replaceWith`/
  `$sort`/`$unwind`/`$group`/`$sortByCount`/`$bucket`/`$facet`/`$densify`.
- **GIL release** — every `#[pyfunction]` runs its compute in
  `Python::allow_threads`.
- **Batched seam** for the whole CRUD trio: `query_matches_batch` /
  `apply_update_batch` / `apply_projection_batch` (+ shims), each ~10–12× more
  docs/s under 4-thread concurrency (`benchmarks/`).
- **Engine selection** (`secantus.engine`): process-wide, default `python`,
  per-component overrides; both implementations permanent.
- **Refactor**: pure sort comparator extracted from `storage.py` into
  `secantus.ordering` (I/O-free), re-exported for back-compat.
- **CI**: a `rust` job added to `.github/workflows/test.yml`.

Local verification (this dev sandbox, **no WiredTiger**): the full Rust
parity sweep is **515 cases green** (curated + multi-seed fuzz vs the pure
engines), plus `cargo test` (62), `cargo clippy -D warnings`, `cargo fmt`, and
`ruff`. The pure engines are parity-pinned byte-for-byte; the Rust side returns a
fall-back signal for anything it can't reproduce.

## What is NOT yet verified

- **The full WiredTiger-backed pytest suite has not run on this branch** —
  `test_crud.py` / `test_storage.py` / `test_indexes.py` / `test_change_streams.py`
  etc. need the `wiredtiger` extension, which isn't buildable in this sandbox.
  This matters because the branch *modified* load-bearing default-path code
  (`storage.py`'s ordering extraction; the `query`/`update`/`projection`/
  `aggregate`/`sortkey` shims).
- **CI has never run** — `test.yml` triggers only on push/PR to `main`, so
  neither the default matrix nor the new `rust` job has fired. The `rust` job
  YAML itself is unexercised.

## Pending (all WiredTiger-gated — needs CI / a WT machine)

- Wire storage's scan / multi-update / projection paths to the `*_batch` shims;
  then the bytes-in/bytes-out variant (skip per-doc Python decode).
- Phase 3 (cursors / change-stream projection), Phase 4 (the storage keystone /
  WT-FFI cutover), Phase 5 (wire/dispatch), Phase 6 (packaging).

## Merge readiness

**Recommendation: open a PR to `main` and let CI go green before merging — do
not merge blind.** Rationale:

- The design is additive and fallback-safe (default engine is `python`; shims
  fall back), so the *expected* risk to default behaviour is low.
- BUT the branch changed default-path code (`storage.py` / the shims) and **no
  WiredTiger-backed test has run against it on this branch**. The project's
  "CI is load-bearing" rule exists for exactly this. The PR is the gate: it runs
  the default matrix (Linux/macOS/Windows) *and* the new `rust` job — which
  builds the core and runs the **full suite under `SECANTUS_ENGINE=rust`**, the
  real differential check through pymongo/WiredTiger.
- Watch the `rust` job's first run closely — its YAML has never executed.

If the PR is green on both jobs, merging is safe and low-risk. If the `rust` job
surfaces a divergence the synthetic parity fuzzes missed (rust-through-pymongo-WT
vs python), that's a genuine bug to fix before merge — which is the whole point
of having run it.
