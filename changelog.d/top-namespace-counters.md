### `top` reports real per-namespace operation counters

`top` answered with mongod's shape but every `{time, count}` was a hard zero, so
`mongotop` rendered an idle server no matter how much load it was under. Both
servers now instrument per-namespace operation timing and report it.

The section mapping was probed against a real mongod rather than inferred, which
was worth doing: the obvious mapping is wrong in four places. `aggregate`,
`count`, `distinct` and `findAndModify` all land in `commands`, not in
`queries`/`update` — mongod's `queries` section is essentially just `find`.
Counts are per command rather than per document, so a 50-document `insert` bumps
the count by one. And a successful `drop` resets a namespace's counters instead
of carrying its history forward.

#### Added

- `top`'s `total` / `readLock` / `writeLock` and per-operation sections now carry
  real microsecond times and operation counts, per namespace, on both the Python
  and Rust servers. Verified against mongod 8.3.4 over a mixed workload: 8 of the
  9 sections match exactly.
- The Python server reuses the profiler's existing clock read rather than adding
  a second one to the dispatch path.

#### Fixed

- A successful `drop` now resets that namespace's counters, matching mongod.
- Commands that name no collection (`ping`, `hello`, `serverStatus`,
  `listCollections`) are no longer attributed to a namespace; `explain` is
  attributed to the namespace of the command it explains.
