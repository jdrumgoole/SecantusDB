### Tests: RM-391's once-per-document check no longer trips on mongod's own timing

RM-391's regression test compares how long a write of one slow document takes
against a `find` of it, on mongod and on the Rust MongoDB server. It took the
fastest write over the fastest `find`, so one fast `find` round could push
mongod's own ratio past the 1.5 budget under CI load. It failed that way three
times (RM-547), last on snapshot PR #2703.

#### Fixed

- The ratio is now the median, over five rounds, of each write against the
  `find` run just before it, so both see the same load. The 1.5 budget, which
  separates a filter tested once from one tested twice, is unchanged. Against
  mongod 8.2.11 the ratios were 1.02-1.07 on an idle machine and 1.02-1.12
  with ten CPU-bound processes running. Fixes RM-547
  (`tests/backlog/test_rm_found_2026_11_07d.py::test_a_write_tests_each_document_once`).
- The interrupt tests' harness (`tests/mongod_live/test_interrupts.py`) sizes
  its slow documents from the fastest of three timings, and fails with a
  message naming the command and its reply when the command answers before
  the kill is sent, instead of a bare `KeyError: 'code'` (seen once on PR #2703
  and not reproduced on an idle machine).
