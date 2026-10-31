### Rust PG server: a key lookup no longer reads the whole table to price itself

After any CREATE or DROP, and inside a transaction block, pricing a one-row
primary-key SELECT or UPDATE decoded every row of the table (to model its
heap pages) and counted every row (to size its indexes): about 2 s over
200,000 rows on a debug build. The planner now takes the heap's page count
and each B-tree's from their page models, as PostgreSQL's
`RelationGetNumberOfBlocks` does, and counts rows only for what has no
model.

#### Fixed

- Fixes RP-297 (tests/backlog/test_rp018_savepoint_undo.py::test_a_key_lookup_after_ddl_does_not_read_the_table)
