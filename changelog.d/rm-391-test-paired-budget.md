### RM-391's regression test compares each side's fastest round

`tests/backlog/test_rm_found_2026_11_07d.py::test_a_write_tests_each_document_once` checks that a write tests its filter against a slow document once rather than twice. The filter is CPU work, so the test times three alternating rounds of a `find` and the write, and requires that the write's fastest round take under 1.5 times the `find`'s fastest. Load only slows a round, so each side's fastest round is its own cost. Three earlier forms failed a write on loaded CI runners while it tested its filter once: one budget from a single `find`, the slower of two `find`s beside each write, and the median of three rounds (snapshots #2608, #2622 and #2634, 2026-11-10). mongod measures 0.99 to 1.08, and a filter tested twice takes about 2. A failure now prints its rounds' times.

#### Fixed

- `test_a_write_tests_each_document_once` no longer fails on CI load.
