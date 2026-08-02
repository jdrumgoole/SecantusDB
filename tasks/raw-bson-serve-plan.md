# Plan: raw-BSON serve path (read-side)

The last large structural performance item from `tasks/rust-perf-findings.md`
(Findings 1+2): the scan/serve path materializes every stored blob into an
owned `bson::Document` to filter it, then re-encodes for the reply — ~65% of
scan-path CPU measured at stake. Only the *insert* half shipped (#608, +12%).
This plan is the read half. It targets exactly the benchmark rows still above
mongod: find full scan 2.3×, filtered scan 1.7×, multi-stage aggregate 1.7×,
indexed range 1.5×.

## Shape

The storage layer already returns raw blobs (`find_matching -> Vec<Vec<u8>>`).
The waste is above it: match/projection/reply decode-and-re-encode. End state:

1. **Filter on `bson::RawDocument`** — a `raw_matches(blob, filter) ->
   Option<bool>` alongside the owned-Document matcher. `None` = "can't decide
   on raw" → fall back to the existing materializing path for THAT document.
   This mirrors the `_secantus_core` engines' defer-to-Python signal: the raw
   fast path handles the overwhelmingly common operator subset (`$eq`-shape
   scalar compares, `$gt/$gte/$lt/$lte` on numbers/strings/dates, `$in` of
   scalars, `$exists`, dotted paths that don't traverse arrays, `$and` of the
   above); everything else defers. Cross-type BSON ordering must reuse the
   same comparison rails as the owned matcher (`numeric::classify` fast path).
2. **Serve verbatim blobs** — a no-projection find's reply batch appends the
   stored blob bytes directly into the OP_MSG reply buffer (the reply builder
   already takes raw bytes for the insert path). Projection present → keep
   materializing at first (projection-on-raw is a later slice).
3. **Aggregation leading edge** — a pipeline whose leading `$match` was lifted
   into the fetch gets the same raw filter; the pipeline body keeps owned
   `Document`s until a later slice.

## Slices (each independently mergeable, parity-gated)

- **S1 — raw filter for COLLSCAN `find`, no projection.** `raw_matches` +
  per-doc fallback + verbatim reply blobs. Gate: `find full scan` and
  `find filtered scan` rows in `compare-servers`; expect the 2.3× row to move
  materially. Correctness: the pymongo gauge + a new randomized
  raw-vs-owned matcher parity fuzz (same corpus discipline as
  `tests/test_rust_*_parity.py`).
- **S2 — cursor batches / getMore** serve stored blobs verbatim (today each
  batch re-encodes; Finding 2 measured the reply path re-materializing).
- **S3 — IXSCAN fetch path** — `_docs_by_recordids` equivalents hand back
  blobs; the residual-filter recheck uses `raw_matches` first.
- **S4 — projection-on-raw** for inclusion-only specs without array
  operators (the common `{field: 1}` shape can be built by splicing raw
  element ranges).
- **S5 — aggregation**: leading-`$match` raw filter; `$count`/`$limit`/`$skip`
  short-circuits on raw.

## Rules

- **Never let the two matchers drift**: every operator the raw matcher
  claims must be pinned by the parity fuzz against the owned matcher, and
  the owned matcher stays the semantic oracle. When in doubt, defer.
- **Per-document fallback, not per-query**: a query with one exotic operator
  still fast-paths the documents the raw matcher can decide.
- Measurement per slice: paired A/B (`compare-servers --reps 5`, interleaved,
  quiet box; the practical noise floor is ±2-3% — see Finding 18's note), and
  the pymongo + at least one strict wire gauge (C or PHP-ext) before merge.
- The three-way mongod probe (memory: `threeway-probe-mongod`) for any
  operator-semantics doubt.

## Risks

- `bson` crate `RawDocument` iteration cost — element walks are cheap but
  keyed lookups are linear; dotted-path descent needs a hand-rolled walker.
  Benchmark the walker before committing to S1's shape.
- Collation: any collation present → defer (owned path), full stop.
- Type-bracketing subtleties (arrays on the path, null-vs-missing): the fuzz
  corpus must cover these before S1 merges; they are exactly where a naive
  raw matcher silently diverges.

## Non-goals

- Update/delete paths (their read side is small; Finding 11 showed the write
  side is IO-bound, not encode-bound).
- Any new cache layers (Finding 18: measured zero).
