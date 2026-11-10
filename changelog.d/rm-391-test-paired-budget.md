### RM-391's regression test compares medians, not one sample

`tests/backlog/test_rm_found_2026_11_07d.py::test_a_write_tests_each_document_once` checks that a write tests its filter against a slow document once rather than twice. It used to give each write a `maxTimeMS` of 1.5 times one `find`'s time, and later the slower of two `find`s'. On loaded CI runners a write still overran that budget while testing its filter once (snapshots #2608 and #2622, 2026-11-10). The test now times three alternating rounds of a `find` and the write, and requires that the median write take under 1.5 times the median `find`. mongod measures 0.99 to 1.15, and a filter tested twice takes about 2.

#### Fixed

- `test_a_write_tests_each_document_once` no longer fails on one slow sample under CI load.
