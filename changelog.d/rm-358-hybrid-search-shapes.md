### `$rankFusion` and `$scoreFusion` name their query shape

The Rust MongoDB server now prints a `queryShapeHash` for an `aggregate`
that starts with `$rankFusion` or `$scoreFusion`. Before this change it
printed none, where mongod 8.2 prints one. The hash appears on the `Slow
query` line, in the profile entry and in `explain`, and it is the hash
mongod prints for the same command.

mongod hashes a hybrid search as the pipeline it rewrites it into, so this
server builds the same pipeline, transcribed from mongod 8.2.11's
`document_source_rank_fusion.cpp` and `document_source_score_fusion.cpp`.
Each input pipeline is taken in name order. The first runs inline and each
later one runs in a `$unionWith` on the same collection. Each input
pipeline's rank or score, weight and normalization are kept, then a
`$group` fuses the documents, the combined score is set, and the results
are sorted on it. `scoreDetails` adds the stages that build the details,
and an input of `$text` reports its own score in them. A hybrid search
nested in `$unionWith`, `$lookup` or `$facet` runs against that stage's
collection. A `$lookup` of one also carries mongod's internal hybrid-search
flag, which the hash covers but `$queryStats` does not print.

A `$geoNear` without `distanceField`, which mongod 8.x accepts, now has a
shape as well. `$jsonSchema` still prints no hash.

#### Fixed

- `secantus-commands` `query_shape.rs`: mongod 8.2.11's `queryShapeHash`
  for `$rankFusion` (weights, `scoreDetails`, several input pipelines) and
  `$scoreFusion` (each normalization, weights, a custom combination
  expression, `scoreDetails`). Covers both at the top of a pipeline and
  inside `$unionWith`, `$lookup` and `$facet`, plus a `$geoNear` with no
  `distanceField`. Partly fixes RM-358
  (`crates/secantus-commands/src/query_shape.rs::tests::hybrid_search_hashes_match_mongods`,
  `tests/mongod_live/test_query_shape_hash.py::test_a_hybrid_search`). The
  `$jsonSchema` part stays open
  (`tests/backlog/test_rm_355_364.py::test_a_query_of_every_kind_names_its_query_shape`).
- Filed RM-552 (an `explain` of `$geoNear` fails on a namespace of its own)
  and RM-553 (`$scoreFusion` takes `"average"` where mongod takes
  `"avg"`), found while measuring.
