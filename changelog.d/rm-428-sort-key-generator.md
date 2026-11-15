### Rust MongoDB server: explain plans the SORT_KEY_GENERATOR a later stage's sort key needs

An aggregate whose later stages read each document's sort key -- a
`$rank`, `$denseRank` or `$documentNumber` window, a `{$meta: "sortKey"}`,
a `$rankFusion`, in a `$facet` too -- now explains its cursor as mongod 8.2
does: `SORT_KEY_GENERATOR` over the scan when the query sorts nothing
itself, and without the dependency projection. Not behind a `$group`, nor
for a `$lookup`'s pipeline.

#### Fixed

- `explain` of `[{$facet: {f: [{$setWindowFields: {sortBy: ..., output: {r: {$rank: {}}}}}]}}]`
  listed a bare `COLLSCAN` under the `$cursor`.

Fixes RM-428 (tests/backlog/test_rm_found_2026_11_08_explain_listing.py::test_facet_that_reads_sort_keys_has_a_key_generator)
