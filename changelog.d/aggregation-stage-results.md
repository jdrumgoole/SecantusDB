### A sort no longer fails on a NaN, and `true` no longer groups with `1`

`aggregation_stage_specs` and its tests cover the errors aggregation stages
*reject*. Nothing covered the documents they *emit* — which is where a silently
wrong answer lives: the pipeline succeeds, the shape looks right, and the rows
are wrong. A new probe found real divergences on its first run, as the same gap
did twice before for queries and updates.

#### Fixed

- **A plain `{$sort: {v: 1}}` failed outright on the Rust server** for any
  collection holding a `NaN`, a `MinKey` or a `MaxKey` — ordinary data, ordinary
  query, whole pipeline refused with *"aggregation pipeline uses a stage or
  operator not supported"*. It took `$group`, `$bucket` and `$topN` down with
  it, since those sort too. The sort gate had been kept deliberately narrow to
  protect the comparator from types with no ordering arm; every one of those
  arms now exists, so the gate was only firing a refusal.
- **`true` shared a `$group` bucket with `1`.** MongoDB keeps them apart — a
  bool is not a number to it — while merging `1` with `1.0`, and merging the
  signed zeros:

  | bucket | members |
  | --- | --- |
  | `0` | `0`, `0.0`, `-0.0` |
  | `false` | `false` |
  | `true` | `true` |
  | `1` | `1`, `1.0` |

  The Python server merged `true` into the numbers; the Rust server was worse,
  keying by **truthiness**, so six documents collapsed into two buckets.
- **A `Decimal128` group key sat in its own bucket** on the Python server, where
  MongoDB merges `Decimal128("1")` and `Decimal128("1.0")` with `1` and `1.0`.
  On the Rust server it failed the whole `$group` instead.

#### Added

- `tools/probes/aggregation_stage_results.py` — 48 stages, comparing emitted
  documents and **field order**, which content comparison silently drops.

Measured against mongod 8.2.11.
