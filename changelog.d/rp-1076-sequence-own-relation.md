### A PostgreSQL sequence keeps its state in a relation of its own

`secantusd-pg` now stores each sequence as PostgreSQL does. Its catalog row
stays in `__sql_sequences__` (`pg_sequence`). Its state row (`last_value`,
`log_cnt`, `is_called`) lives in a relation of its own, `__sql_seq_<oid>__`,
the way PostgreSQL keeps that row in the sequence's file. Until now one
shared collection held every sequence's state. `pg_relation_filepath` now
names a different file for each sequence. A rename keeps the file, and a
DROP removes it. The store format moves to 13, so `secantusd-mdb` and
`secantusd-pg` both refuse a format-12 store by name. Dump such a store
with the build that wrote it.

#### Fixed

- `pg_relation_filepath` named the same file for every sequence. Fixes
  RP-1076 (tests/backlog/test_rp_storage_model.py::test_each_sequence_has_its_own_file;
  regression: tests/test_rust_pgserver_slice.py::test_a_sequence_keeps_its_state_in_its_own_file)
