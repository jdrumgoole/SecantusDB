### RM-391's regression test measures CPU time where it can

`tests/backlog/test_rm_found_2026_11_07d.py::test_a_write_tests_each_document_once` checks that a write tests its filter against a slow document once rather than twice. On 2026-11-10 it failed on CI runners in three wall-clock forms: a one-`find` budget, the slower of two `find`s, and the median of three rounds. A fourth, the fastest of three rounds, failed when mongod's own rounds of one operation ranged from 0.58 to 1.59 seconds. The test now compares each operation's `cpuNanos` from its `Slow query` line, which both servers print on Linux, so a loaded runner cannot move it. Where no CPU time is printed it falls back to the fastest round of wall time, which is reliable on an idle macOS machine. Time-based yields are off on both servers, because under load mongod yields mid-write and tests the document again when it resumes. mongod measures 1.00 to 1.08, and a filter tested twice costs about 2.

#### Fixed

- `test_a_write_tests_each_document_once` no longer fails on CI load.
