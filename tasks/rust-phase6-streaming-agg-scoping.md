# Phase 6 scoping — streaming / raw aggregation (the `$group` gap)

Status: **scoping** (2026-07-19, pinned at `9f87edf3`). This is the design
scope for the next Rust-server performance lever after the raw-BSON serving
path (Phases 1–5) landed. Not yet implemented; no branch cut.

## Why this phase, and what it must beat

The post-raw-BSON three-way re-profile
(`tasks/rust-perf-findings.md` § "Post-raw-BSON three-way re-profile") put four
of six workloads at ~1.5× of mongod. Two outliers remain:

| Workload | Rust ×mongod | Character |
|---|:---:|---|
| **aggregate_group** | **3.1×** | `[{$group}]` full scan — a *materializing* stage |
| find_all | 2.4× | full scan reply — near the cursor/wire floor |

`aggregate_group` is the standout and the target of this phase. `find_all` is a
separate, smaller lever (cursor-batch efficiency) and is explicitly **deferred**
until after this phase changes the aggregate materialization costs it would be
tuned against.

**Target:** move `aggregate_group` from 3.1× toward the ~1.5× band the
scan/write workloads already occupy. The mongod delta here is SBE (slot-based
execution over typed columns, no per-doc BSON round-trip); we won't match that
fully in a single-node surrogate, but the bulk of our 3.1× is avoidable
materialization, not execution-engine sophistication.

## Current execution model (what materializes, precisely)

`secantus_core::aggregate::apply_pipeline` (`crates/secantus-core/src/aggregate.rs:38`)
threads an **owned `Vec<Document>`** stage-to-stage: `apply_stage` takes
`Vec<Document>` in and returns `Vec<Document>` out. The command layer
(`crates/secantus-commands/src/aggregate.rs`) fully decodes the storage blobs
into that `Vec<Document>` via `decode_docs` (line ~217) *before* `apply_pipeline`
is called — after Phase 5's `reduce_raw_prefix` has already trimmed any
pass-through `$skip`/`$limit`/`$match` prefix over raw bytes.

`$group` (`crates/secantus-core/src/group.rs:838` `run_group`):

- receives `docs: &[Document]` — **already fully materialized**;
- per doc: `eval(id_expr, d, vars)` computes the group key, then
  `apply_acc(acc, c.arg, d, vars)` evaluates each accumulator's arg expression;
- both only touch the fields the `_id` expression and the accumulator args
  name — typically 1–3 fields of a wide document.

So for `[{$group: {_id: "$k", n: {$sum: "$v"}}}]` over 10000 wide docs, the
dominant cost is `decode_docs` building 10000 owned `Document`s (each an
`IndexMap` with a heap alloc per field) when the group reads only `k` and `v`.
This is **Finding 1's materialization surviving into the heavy stage** — the
same waste the scan-match path (Phase 2) already eliminated for `find`, not yet
eliminated for `$group`.

## The two levers — and why raw accumulation comes first

The plan (`delightful-shimmying-turing.md` Phase 6) frames this as "streaming
iterator + SBE-lite slots." That is really *two* independent changes with very
different risk/payoff for the workload the profile flags:

### 6a — Raw accumulation (the direct answer to the 3.1×). **Do first.**

Feed `$group` (and `$sort`) the **raw fetched blobs** and decode only the
fields the `_id`/accumulator expressions reach — the Phase-2 `matches_raw`
pattern applied to the accumulation path.

- Add `group_stage_raw(spec, blobs: &[&RawDocument], vars) -> R<Vec<Document>>`
  (output stays owned `Document` — group *output* is small: one row per group,
  and it flows into whatever follows).
- The key + accumulator-arg evaluation needs an **`eval_raw(expr, raw, vars)`**
  that resolves a field path against a `RawDocument`, decoding only the reached
  leaf into an owned `Bson` — the expression-language analogue of
  `resolve_path_raw` (which already exists for the *query* language from
  Phase 2). Expressions that need the whole doc (`$$ROOT`, `$$CURRENT`,
  variadic object rebuilds) full-decode that doc or **defer to the owned
  engine** (the two-sided `Fallback` contract).
- Command-layer wiring: when the pipeline's first *materializing* stage is a
  `$group` whose `_id` + accumulator args are all simple field paths, skip
  `decode_docs` for that stage and hand the raw blobs to `group_stage_raw`.
  Any expression shape outside the raw-supported set → fall back to today's
  `decode_docs` + owned `group_stage` (no regression, just no speedup).

**Expected win:** proportional to (doc width)/(fields the group touches) — the
same shape as the 2.8× Phase 2 scan-match win, directly on the profiled
workload. Lowest risk of the two: reuses the proven raw-decode + parity
pattern, output semantics are byte-identical because only the *input decode* is
deferred.

**Also apply to `$sort`** (`aggregate.rs:288`): a sort by named fields needs
only those fields' typed values — a `sort_key_raw` over blobs, carrying the raw
blob alongside the key and decoding the full doc only for the survivors that
reach a later materializing stage (or the reply, where Phase 1 already splices
raw bytes). Sort is the second-most-common materializing stage; worth folding
into 6a.

### 6b — Streaming iterator model. **Larger, do only if 6a leaves a gap.**

Replace `Vec<Document>`-between-stages with a pull-based
`Iterator<Item = R<Document>>` per stage, so intermediate result sets aren't
fully buffered. This helps *multi-stage* pipelines with large intermediates
(e.g. `$unwind` fan-out feeding a `$group`), which the single-stage benchmark
does **not** exercise — so its payoff is unproven by the current profile.

- Rewrite behind the **same `apply_pipeline` boundary** — callers
  (`run_segmented`, `$facet` sub-pipelines, `$lookup` `let`/pipeline) keep
  their signature.
- Blocking stages (`$group`, `$sort`, `$bucket*`) are natural iterator sinks
  that consume their whole input before yielding — they cap the streaming
  benefit, which is exactly why 6a (making *those* stages cheap) is the higher
  lever first.
- Full parity surface: every stage's ordering, error timing (stages that raise
  must still raise at the same point), and `Fallback` behavior must be
  preserved against the owned model.

### 6c — Slot/SBE-lite compilation. **Explicitly out of scope for now.**

Compiling the common prefix to typed slots is the true mongod analogue but the
largest lift by far and speculative for a single-node surrogate. Not scoped
here; revisit only if 6a+6b measurement shows a residual gap worth it.

## Recommended sequence

1. **6a raw `$group` + `$sort`** on a feature branch off `origin/main`. Measure
   `compare-servers --server rust` `aggregate_group` before/after, pinned to a
   SHA. This is the contained, proven-pattern lever that directly attacks the
   3.1×.
2. **Re-profile.** If `aggregate_group` reaches the ~1.5× band, Phase 6 is done
   for now — 6b/6c stay deferred behind a measured decision. Re-measure
   `find_all` at that point (the deferred cursor-batch lever).
3. **6b streaming** only if a multi-stage aggregate workload (add one to
   `bench.compare_servers` — the current suite is single-stage `$group` only)
   shows a buffering-bound gap 6a didn't close.

## Gates (every step)

- **Parity vehicle:** extend `_secantus_core` (`crates/secantus-core-py/src/lib.rs`)
  with a `group_raw` / `eval_raw` entry point and add a byte-equality suite to
  `tests/test_rust_aggregate_parity.py` (or a new `test_rust_group_parity.py`)
  asserting `group_raw(blobs, spec) == group(decode(blobs), spec)` across the
  curated + fuzz corpora, **two-sided defer** (raw must defer wherever owned
  defers/raises, never return a concrete result Python wouldn't).
- `./inv rust-gate` green (clean workspace + WT crates + parity + ruff + full
  pytest).
- `./inv validate --server rust` non-regressing vs the committed gauge report
  (run in a sub-agent; rebuild the extension with `./inv rust-server-build`
  first).
- `./inv compare-servers --server rust` before/after on a pinned SHA, recorded
  in `tasks/rust-perf-findings.md`.

## Risk summary

| Step | Risk | Why |
|---|---|---|
| 6a raw group/sort | **medium** | touches the accumulation path, but only defers *input decode*; output semantics unchanged; reuses Phase-2 raw + parity pattern |
| 6b streaming | high | rewrites the execution model behind `apply_pipeline`; large parity surface (ordering, error timing) |
| 6c slots | very high | speculative; out of scope |

## Note on the benchmark's blind spot

`bench.compare_servers`'s `aggregate_group` is a **single-stage** `$group` over
a full scan — it measures exactly the input-materialization cost 6a targets, and
nothing of the inter-stage buffering 6b targets. Before committing to 6b, add a
multi-stage aggregate workload (e.g. `$match` → `$unwind` → `$group` → `$sort`)
so the streaming lever is measured against a workload that can actually show its
benefit, per the plan's "spend effort where the profile says the gap is" rule.
