### The Rust server's `explain` plan nodes were missing most of mongod's fields

mongod reports a fixed set of keys on each plan node, and a client reads them to
answer real questions. The Rust server's `IXSCAN` emitted four of the nine:

| key | what it answers | Rust before |
| --- | --- | --- |
| `isUnique` | can a reader assume one document per key? | absent |
| `isSparse` | does the index omit documents? | absent |
| `isPartial` | does a filter restrict what it holds? | only when true |
| `multiKeyPaths` | which fields made it multikey? | absent |
| `indexVersion` | the index format | absent |

Three more shapes were wrong rather than missing:

- **`FETCH` echoed the whole filter.** mongod carries only the RESIDUAL
  predicate there and omits the key entirely when the index bounds covered the
  query — which is exactly how a reader tells a fully-index-served query from
  one that re-checks every document. Echoing the whole filter erased that
  signal.
- **`COLLSCAN` had no `direction`** and emitted an empty `filter` where mongod
  omits the key.
- **`isCached` was never set.** It is a whole-plan property, so it belongs on
  the outermost node only, as its first key.

The Rust server's IXSCAN node is now identical to the Python server's, and the
probe's `winningPlan` count drops from 25 of 25 to 22 of 25.

Still open, and filed with the shapes that show it: the `SORT` / `SKIP` /
`LIMIT` / `PROJECTION_SIMPLE`|`PROJECTION_DEFAULT` stage tree
(`secantus.explain.build_stage_tree`), which the Rust server does not build at
all — so `{filter: …, limit: 3}` reports a bare `COLLSCAN` where mongod reports
`LIMIT` above one. That is what the remaining 22 are, and the blocking-`SORT`
half of it needs a `sorted_by_index` flag that `ExplainPlan` does not currently
carry.

#### Fixed

- `secantus-commands`: `explain`'s `IXSCAN` node carries mongod's nine keys in
  mongod's order; `FETCH` carries only the residual filter; `COLLSCAN` reports
  `direction` and omits an empty `filter`; `isCached` leads the outermost node.
