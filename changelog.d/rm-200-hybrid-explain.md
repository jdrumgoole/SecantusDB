### `explain` shows hybrid search as mongod desugars it

mongod 8.2 runs `$score`, `$rankFusion` and `$scoreFusion` as stages it already has: `$setMetadata`, `$replaceRoot`, a `$rank` window, a `$unionWith` for each further input pipeline, a `$group` per document, and a `$sort` by score. Its `explain` lists those stages. The Rust MongoDB server ran the same answers but listed each stage as it was written. It now lists them as mongod does, for every normalisation, with weights, score details or a custom combination expression. The cursor absorbs each input pipeline's leading `$match`, `$sort` and `$limit`, a `$limit` behind a `$score` included. A score read back prints as `{$meta: "score"}`. A `$rankFusion` can also be the first stage of a `$facet` sub-pipeline, where its further inputs read the aggregated collection, as on mongod.

#### Fixed

- `explain` of an aggregate lists `$score`, `$rankFusion` and `$scoreFusion` as mongod 8.2.11's desugaring, with each listed stage counting what mongod's does, and prints `{$meta: ...}` reads as written rather than as `{$meta: [{$const: ...}]}`.
- `$rankFusion` as the first stage of a `$facet` sub-pipeline runs instead of being refused with 10170100. A `$scoreFusion` there is refused with 40218, as mongod refuses it.
- Fixes RM-200 (tests/backlog/test_rm_added_in_82.py::test_explain_of_rank_fusion_is_its_desugaring, tests/backlog/test_rm_added_in_82.py::test_explain_lists_hybrid_stages_as_mongod_desugars_them, tests/backlog/test_rm_added_in_82.py::test_rank_fusion_runs_first_in_a_facet).

#### Added

- Backlog items RM-426 (a `$unionWith` sub-pipeline's own `$cursor` in `explain`), RM-427 (a `$sort` and the `$limit` after it as one listed stage) and RM-428 (`SORT_KEY_GENERATOR` under a `$facet` that keeps a sort key), each with a gap test in `tests/backlog/test_rm_found_2026_11_08_explain_listing.py`.
