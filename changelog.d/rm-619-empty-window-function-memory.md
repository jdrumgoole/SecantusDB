### A window that has held no document counts no function memory

A `$setWindowFields` `$sum`, `$push` or `$avg` with no lower end whose
window has not yet reached a document now reports
`maxFunctionMemoryUsageBytes` 0, as mongod 8.2.11 does, instead of the
accumulator's size (and, for `$push`, a value it was never given).

#### Fixed

- Fixes RM-619 (tests/backlog/test_rm_found_2026_11_13_rm509.py::test_an_empty_window_counts_no_function)
