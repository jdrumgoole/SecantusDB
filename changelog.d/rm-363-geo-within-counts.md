### A `$geoWithin` find counts what it examined

The Rust MongoDB server's slow-query line and profile entry for a `find`
that reads a `$geoWithin` or `$geoIntersects` region now carry
`keysExamined`, `docsExamined`, `nBatches` and `cursorExhausted`, as
mongod 8.2.11's do, instead of no counts at all.

#### Fixed

- A `$box` over ten points under a `2d` index counts 5 keys and 4
  documents examined, as mongod does.

Fixes RM-363 in part (tests/backlog/test_rm_355_364.py::test_a_geo_within_read_counts_what_it_examined)
