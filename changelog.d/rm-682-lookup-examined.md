### A `$lookup` over a collection reports what it examined

At `executionStats` the Rust MongoDB server now gives a `$lookup` whose
pipeline reads a collection its `totalDocsExamined`, `totalKeysExamined`,
`collectionScans` and `indexesUsed`, summed over the pipeline's runs as
mongod 8.2.11 counts them: once for an uncorrelated pipeline, whose read
mongod caches, and at each outer document for a correlated one.

#### Fixed

- A `$lookup` with a pipeline over a collection listed none of the four
  figures in `explain`.

Fixes RM-682 (tests/backlog/test_rm_found_2026_11_14_rm539.py::test_a_lookup_over_a_collection_reports_what_it_examined)
