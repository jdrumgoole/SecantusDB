### RM-391's regression test times each write against its own find

`tests/backlog/test_rm_found_2026_11_07d.py::test_a_write_tests_each_document_once` gives each write a `maxTimeMS` of 1.5 times what a `find` of the same slow document takes. A filter tested once fits in that budget, and one tested twice does not. The budget came from a single `find`, timed before all five writes, and on a loaded CI runner the later writes overran it while still testing their filter once (snapshot #2608, whose rerun passed). Each write now gets a budget from the slower of two `find`s run just before it.

#### Fixed

- `test_a_write_tests_each_document_once` no longer fails when CI's load changes between its `find` and its writes.
