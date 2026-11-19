### Rust MongoDB server: a $sort after $addFields-like stages counts mongod's interrupt checks

An `aggregate` that closes with a `$sort` after stages passing one document
on for each they take now prints `numInterruptChecks` on its line and its
`getMore`s' lines, as mongod 8.2.11 does.

#### Fixed

- A `$sort` after `$addFields`, `$set`, `$unset`, `$project`, `$replaceRoot`
  or `$replaceWith` over a classic read takes a global IS for each buffer
  mongod's cursor stage fills, and counts a tick of the pipeline's interrupt
  checker for each `getNext` of the cursor stage and of each stage between
  it and the `$sort` (once per document and once for the end); its
  `getMore`s take no lock of their own. A first batch of none included.
  Part of RM-360 (tests/mongod_live/test_interrupt_checks.py::test_a_pipeline_line_counts_mongods_interrupt_checks).
