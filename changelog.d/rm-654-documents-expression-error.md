### A failing `$documents` expression reports its own error

The Rust MongoDB server now answers a `$documents` argument or element whose
expression fails with that expression's error, as mongod 8.2.11 does.

#### Fixed

- `$documents: {$divide: [1, 0]}` and `$documents: [{x: {$divide: [1, 0]}}]`
  answer 2 "Failed to optimize pipeline :: caused by :: can't $divide by
  zero", and the same inside a `$lookup` reading a `let` variable under the
  executor prefix naming the outer collection. They answered 2 "$documents
  expression not supported".

Fixes RM-654 (tests/backlog/test_rm_found_2026_11_14_rm531.py::test_a_documents_expression_error_is_the_expressions)
