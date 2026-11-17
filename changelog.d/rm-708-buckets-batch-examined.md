### Rust MongoDB server: each batch of a clustered collection's `find` says what it examined

A `find` of a clustered collection (a time-series collection's buckets
among them) that leaves its cursor open now logs, on its own line and on
each `getMore`'s, what that batch examined, as mongod 8.2.11 does: as far
as the plan went to fill the batch, and the last batch on to the end of
the read (`docsExamined` 4, 4, 2 for ten buckets read four at a time).

#### Fixed

- Those lines said `nreturned` alone.

Fixes RM-708 (tests/backlog/test_rm_ts_ddl_lines.py::test_a_batched_find_of_buckets_says_what_each_batch_examined)
