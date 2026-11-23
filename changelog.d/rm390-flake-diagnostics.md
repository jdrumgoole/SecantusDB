### RM-390's regression tests report what decided a failure

#### Fixed

- The `maxTimeMS` write tests of RM-390 (`tests/backlog/test_rm_found_2026_11_10_rm390.py`,
  `test_rm_found_2026_10_27.py`) failed once under load without saying how far
  the write got. Their assertions now carry the plan, budget, documents
  written, elapsed time and load average. Not reproduced in 44 loops at load
  up to 54; the item's prose has the measurement.
