### An explain of a contested read tries its plans over what they reach

The Rust MongoDB server's `explain` of a read whose plan two indexes
contest no longer reads the whole collection into memory first: its trial
reads only what the candidate plans reach, as mongod 8.2's does, so its
cost no longer grows with the collection.

#### Fixed

- `explain` of a contested `find` at `queryPlanner` costs the same over
  10,000 documents and 100,000, as on mongod.
  Fixes RM-610 (tests/backlog/test_rm_found_2026_11_13_contested_reads.py::test_a_contested_read_costs_what_it_returns)
