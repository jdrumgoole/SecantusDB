### A `$sort` after a `$group` counts its interrupt checks and locks as mongod's does

An `aggregate` whose `$group` mongod's slot-based engine runs, with a
`$sort` after it, now logs mongod 8.2.11's `numInterruptChecks`, global
locks and execution admissions on its line and on each `getMore`'s.

#### Fixed

- The Rust MongoDB server's line for such an aggregate printed no
  `numInterruptChecks` and one global IS. mongod reads the groups through
  its cursor stage, which takes the global lock for each buffer it loads
  (32 documents, then twice as many each time), and its pipeline checker
  ticks once per `getNext` of the `$sort` and of the cursor stage; its
  `getMore`s take no lock of their own and go on counting. This server now
  does the same. Fixes RM-360 in part
  (tests/mongod_live/test_interrupt_checks.py::test_a_pipeline_line_counts_mongods_interrupt_checks, the
  `aggregate sorting ... groups` shapes).
