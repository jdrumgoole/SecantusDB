### Rust MongoDB server: an insert's write conflict names the `_id` index, and a retryable insert does not count a null

A transaction's insert that conflicts on an `_id` another open transaction
inserted now answers 112 with mongod 8.2.11's cause, naming the index and the
table it conflicted in (`WiredTigerIdIndex::_insert: index: _id_; uri:
table:<ident>`), where the cause was empty. A retryable insert drops a null
from its `documents` before counting them against `stmtIds`, as mongod does,
instead of refusing the write (16).

#### Fixed

- Fixes RM-087 (tests/backlog/test_rm_085_112.py::test_an_insert_conflict_names_the_index_table)
- Fixes RM-087 (tests/backlog/test_rm_085_112.py::test_a_retryable_insert_does_not_count_a_null)

The storage change (`secantus-storage`'s write-conflict cause) is linked by the
PostgreSQL server too; it reads nothing new from it.
