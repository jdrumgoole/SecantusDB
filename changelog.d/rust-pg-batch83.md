### The Rust PostgreSQL server: two transactions creating one table name

A second CREATE TABLE of a name another open transaction is creating now
behaves as in PostgreSQL 15.19. It waits for the other transaction. If that
one commits, the second fails with a duplicate key in
`pg_type_typname_nsp_index`; if it rolls back, the second goes on. Before,
the second failed at once with 40001 "could not serialize access due to
concurrent update".

#### Fixed

- `secantus-pg`: a CREATE TABLE that collides with another transaction's
  catalog row waits as a row write does, in a block and in autocommit. Once
  the other has committed it raises 23505 with PostgreSQL's message, detail,
  schema, table and constraint, rather than the 42P07 that a fresh look at
  the catalog gives. Found while chasing RP-268
  (`tests/backlog/test_rp_batch82.py`); RP-268 itself stays open, since the
  Windows failure it records creates two different names.
