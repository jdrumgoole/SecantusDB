### The Rust server had the same four index defects

The index-layer fixes that landed for the Python server — a sparse index
answering queries that match a *missing* field, a compound sparse index that
under-indexed, a partial index whose implication check compared across BSON type
brackets, and a query naming only a partial filter's own fields — were fixes to
`storage.py`. The Rust server has its own storage layer with its own port of
those helpers, and it still had all four. Three are silent data loss: the query
succeeds, the shape is right, and rows are simply missing.

Nothing would have caught them. The engine-parity suites that keep the two
engines honest pin `query`, `update`, `expressions`, `projection`, `sortkey`,
`diff` and `aggregate` — the pure operator engines. None of that is the storage
layer, so a divergence there is invisible to the one mechanism built to catch
divergence.

One of the four behaves differently on this side and worse. Where the Python
server raised `IndexError` out of the command handler for a query covering only
a partial filter's own fields — loud, and visible as an internal error — Rust
built an empty key prefix and scanned for the bare separator, which matches no
key. It returned nothing, quietly.

#### Added

- `tools/probes/index_result_sets.py`: the sweep, which compares the server
  against **itself** with and without the index rather than only against
  MongoDB. That isolates the index from the sort engine, and it is what makes
  the output diagnostic: `indexed=[1] no-index=[1,2] mongod=[1,2]` names the
  dropped row. Against the pre-fix Rust server it reports 12 of 15 curated and
  53 of 1692 randomised; after, zero on both servers.

#### Fixed

- `secantus-storage`: `sparse_covers` (at least one indexed field present),
  `sparse_index_usable` / `predicate_may_match_missing` (a sparse index is
  unusable for a query that could match a missing field — including any
  comparison against `null`, not just `$eq`), `type_bracket` on the partial
  implication check, and a whole-index scan where there is no key prefix to pin.
