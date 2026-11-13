### Rust MongoDB server: a rank over documents with no sort key is refused, as on mongod

A `$_internalSetWindowFields` rank in a pipeline compares the sort key each
document carries. Behind a `$group`, or the documents a `$unionWith` adds, a
document carries none at all. That is different from the empty key a read
with no `$sort` gives. mongod 8.2.11 refuses the rank there. This server tied
the documents instead, ranking each of them 1.

#### Fixed

- A `$rank`, `$denseRank` or `$documentNumber` in a pipeline's own
  `$_internalSetWindowFields` is refused with 9973200 "Attempt to get sort
  key which has not been populated" when it reaches a document with no sort
  key. That covers documents behind `$group`, `$bucket`, `$bucketAuto`,
  `$sortByCount`, `$count`, `$facet` and `$redact`, including behind a later
  `$sort`, which gives them no key. It also covers the documents a
  `$unionWith` adds, unless a `{$meta: "sortKey"}` reads them before the
  rank does. As on mongod, the refusal comes when the rank reaches such a
  document, so a `$limit` straight after it that is filled first still
  answers. The same applies in a `$lookup` sub-pipeline.
- `{$meta: "sortKey"}` behind those stages is missing, as on mongod. It used
  to be `[]`, or refused in a `$project`. A `$group` on it gives `_id: null`,
  and a `$push` of it adds nothing.
- The command layer (`agg_meta::rewrite_sort_metadata`) marks such documents
  as they reach a rank, and `$unionWith`'s sub-pipeline marks its own
  documents. `secantus-core`'s window stage refuses a marked document.
- Fixes RM-522 (tests/backlog/test_rm_found_2026_11_10_rm410.py::test_a_rank_with_no_sort_key_is_refused).
  Regression tests: `tests/mongod_live/test_no_sort_key.py` (30 shapes
  against mongod 8.2), plus unit tests in `agg_meta` and `windowfields`.
- Filed while working on it: RM-630 (`$unionWith` ends the outer documents'
  sort key), RM-631 (`$densify` sets no sort key) and RM-632 (`$graphLookup`
  and `$fill` lose it).
