### explain sizes a top-k `$sort` after `$documents` and in a `$facet` by what it keeps

A `$sort` that folds in the `$limit` behind it keeps only that many
documents, and mongod 8.2.11's `totalDataSizeSortedBytesEstimate` counts
only those. The per-stage runs of a `$documents` pipeline and of a `$facet`
sub-pipeline now take the `$limit` too, as a collection pipeline's already
did.

#### Fixed

- `totalDataSizeSortedBytesEstimate` of `[{$documents: [3 docs]}, {$sort}, {$limit: 2}]` is 490, not 735, and 0 for a `$limit` of 1, in a `$facet` as well.

Fixes RM-689 (tests/backlog/test_rm_found_2026_11_14_rm538.py::test_a_top_k_sort_reports_the_documents_it_keeps)
