# Engine benchmark results

Run `uv run --no-sync python benchmarks/engine_bench.py` (no WiredTiger needed).
Numbers below: 4-core box, CPython 3.12.3 (standard GIL build), release wheel.
They will vary by machine — re-run locally — but the *shape* is the finding.

## Single-threaded: pure-Python vs the Rust seam

The "Rust" column is the **full seam** the shim pays: `bson.encode` the args →
call `_secantus_core` → `bson.decode` the result. So this already includes the
per-call byte-seam cost.

| engine                              | py ops/s | rust ops/s | speedup |
|-------------------------------------|---------:|-----------:|--------:|
| `query.matches`                     |   81,600 |    167,800 |  2.06x  |
| `expressions.evaluate`              |  110,600 |    196,100 |  1.77x  |
| `update.apply_update`               |   70,300 |    139,600 |  1.99x  |
| `aggregate.apply_pipeline` (200 docs) |     409 |      3,280 |  8.03x  |

**Finding 1 — the byte seam does *not* eat the win.** Even paying
`bson.encode`/`decode` on every call, the Rust path is ~2× faster on the leaf
ops and ~8× on the pipeline. The pipeline wins biggest because its single call
amortises the seam over 200 docs of compute; the leaf ops pay the seam per call,
so their win is smaller but still solid.

## Multi-threaded: does releasing the GIL parallelise?

Each thread runs the Rust seam in a tight loop; total work is held constant and
split across threads. `Python::allow_threads` releases the GIL for the Rust
compute, but the seam's `bson.encode`/`decode` run in Python and hold it.

| engine                              | 1 thr | 2 thr | 4 thr |
|-------------------------------------|------:|------:|------:|
| `query.matches`                     | 1.00x | 0.76x | 0.20x |
| `expressions.evaluate`              | 1.00x | 0.65x | 0.18x |
| `update.apply_update`               | 1.00x | 0.62x | 0.21x |
| `aggregate.apply_pipeline` (200 docs) | 1.00x | 1.49x | 1.22x |

**Finding 2 — GIL release pays off only for *coarse* calls.** The pipeline
(large Rust compute per call) scales to ~1.5× on 2 threads — real parallelism.
But the cheap leaf ops *regress* under concurrency: the per-call GIL
release/re-acquire plus the GIL-held encode/decode dominate the tiny compute, so
multiple threads hammering the same cheap op just ping-pong the GIL and thrash.

This tight-loop micro-benchmark is a worst case — in a real server the threads
do varied work (wire parsing, storage I/O) that overlaps with another thread's
Rust compute, so the GIL release still buys connection-level concurrency. But the
lesson is clear and matches the plan's "move the loop outward":

> **The multi-core win for CRUD requires coarsening the seam** — batch the
> per-doc hot loops into a single Rust call (`query_matches_batch(docs, query)`,
> `apply_update_batch(...)`) so one GIL release covers many docs, the way
> `apply_pipeline` already does. Per-doc calls across the seam are GIL-bound by
> their Python encode/decode.

## Batched seam — the fix, prototyped across all three CRUD engines

A batched call does the whole list in **one** crossing (one `bson.encode` of the
doc array, one GIL release covering all N) instead of one seam crossing per doc.
Implemented for `query_matches_batch`, `apply_update_batch`,
`apply_projection_batch` (+ the matching shims). 200 docs per call, docs/s:

| engine                | 1 thr (batched speedup) | per-doc @4 thr | batched @4 thr |
|-----------------------|------------------------:|---------------:|---------------:|
| `query.matches`       |       264,000 (1.23×)   |  30,700 (0.15×)| 380,000 (1.44×)|
| `update.apply_update` |       228,000 (1.20×)   |  28,400 (0.15×)| 278,000 (1.22×)|
| `projection.apply`    |       280,000 (1.06×)   |  26,900 (0.10×)| 312,000 (1.11×)|

**Batching converts the per-doc anti-scaling into real parallelism, uniformly.**
Single-threaded it's already faster (one encode/decode + one GIL release instead
of N). Under 4 threads each engine does **~10–12× more docs/s batched than
per-doc** (e.g. query 380k vs 31k) — the coarse GIL release lets the threads'
Rust compute overlap, while the per-doc path thrashes the GIL. (The 4-thread
batched figure dips slightly below 2-thread because the GIL-held decode of the
200-doc array / encode of the result becomes the cap; bigger batches and passing
*raw doc bytes* / returning *filtered bytes* would push it further — see step 2
of the batching task in `tasks/rust-rewrite-phase3-scoping.md` §4.)

This is the prototype for the production direction: storage's scan / multi-update
/ projection paths should call the `*_batch` shims once over a candidate list,
not the per-doc function in a loop.

## Takeaways

- Keep `allow_threads` everywhere: single-threaded cost is negligible and the win
  is intact; it's correct for the server-concurrency goal and clearly helps the
  coarse pipeline path today.
- **Batching the leaf-engine seams is the real throughput lever, and it works**
  (see above) — not more operator coverage. All three CRUD-path engines now have
  batch primitives (`query_matches_batch` / `apply_update_batch` /
  `apply_projection_batch` + shims), each ~10–12× faster under 4-thread
  concurrency. The remaining work is to **wire storage** (scan / multi-update /
  projection paths) to call the `*_batch` shims, which needs WiredTiger to land +
  measure. Track regressions with this benchmark.
- Validate under a real concurrent-connection server load (needs WiredTiger / the
  full server) before drawing production conclusions — these are operator-core
  micro-numbers.
