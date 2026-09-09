### Projections come back in MongoDB's field order, and refuse what MongoDB refuses

`apply_projection` sits on the read path of every `find` and had never been
compared against MongoDB. A new probe found three divergences on its first run,
all shared by both servers.

#### Fixed

- **Projected fields came back in the wrong order** when `$slice` or
  `$elemMatch` was involved. MongoDB emits `_id` first, then the *document's*
  own key order — not the projection spec's — with computed fields appended.
  Those two operators were applied after the plain inclusions, so their fields
  landed at the end. Field order is what a driver renders, and comparing
  documents for equality ignores it entirely, so nothing else could see this.
- **`{$elemMatch: {$gt: 2}}` failed with *"unknown top level operator: $gt"***
  whenever the array held documents. A criterion of only `$`-operators is an
  element-*value* predicate — each element tested as a value — and the code
  branched on the element's type instead of the criterion's shape. The value
  predicate also suppresses the implicit one-level array traversal, so a nested
  array is not descended:

  | array | criterion | result |
  | --- | --- | --- |
  | `[1, 2, 3]` | `{$gt: 2}` | `[3]` |
  | `[1, [3, 4], 5]` | `{$gt: 2}` | `[5]` |
  | `[[1, 2], [3, 4]]` | `{$gt: 2}` | field omitted |
  | `[[1, 2], [3]]` | `{$size: 2}` | `[[1, 2]]` |

- **A projection naming both a path and its ancestor was accepted** and returned
  a truncated document. MongoDB refuses the pair, with the code depending on the
  order: `{a: 1, "a.x": 1}` is `31249 Path collision at a.x remaining portion
  x`, and `{"a.x": 1, a: 1}` is `31250 Path collision at a`. Siblings, a shared
  string prefix that isn't a path component, and the same path twice remain
  legal.
- On the Rust server, a **named** projection error is no longer rewritten to a
  generic `BadValue` — only the positional cases whose exact code it cannot
  reproduce still fall back.

#### Added

- `tools/probes/projection_results.py` — 37 shapes, comparing field order as
  well as content. 0 divergent on both servers.

Measured against mongod 8.2.11.
