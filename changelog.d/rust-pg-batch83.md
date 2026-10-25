### The Rust PostgreSQL server: two transactions creating one table name, and classes beside outer joins

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
- `secantus-pg`: an equivalence class beside outer joins goes through the
  join order search.
  - Only inner joins' equalities make the class; an outer join's clause is
    delayed (`distribute_qual_to_rels`).
  - Each inner ON the class stands in for is checked where its tables
    meet, below any outer join that could NULL-extend them (it was checked
    on every row at the end).
  - An outer join's key that is a class member reads a side merged on the
    class unsorted, and a LEFT join merged on its preserved member comes
    out in the class's order.

  18 of 18 shapes of four and five tables match 15.19 (none before), and
  nested LEFT joins 20 of 20 (18 before). Part of RP-028
  (`tests/backlog/test_rust_postgres_gaps.py::test_a_left_join_over_an_inner_class_is_searched`,
  `::test_a_left_join_on_a_class_member_is_in_the_class_order`). RP-028 stays
  open; a join on two equalities is its new gap test
  (`::test_a_join_on_two_equalities_is_planned`).
- `secantus-pg`: a CREATE TEMPORARY TABLE is never treated as colliding
  with another session's table of the same name. A temp table is in its
  session's own schema, and mapping its write conflicts to 23505 turned an
  unrelated conflict into a duplicate key. Found by a stress test.
- `secantus-pg`: sessions creating tables side by side on a fresh store no
  longer fail 40001. Each session's first CREATE recorded its table's size
  in `__sql_relstats__`, which nothing had registered yet. Every one of
  those transactions tried to register it, and WiredTiger let only one
  through. It is now registered with the other catalog collections, before
  any transaction's snapshot, as PostgreSQL's catalogs exist from `initdb`.
  Sixteen sessions creating, filling and updating their own temp tables
  forty times each now fail none of 640, against about one in four before.
  Fixes RP-269
  (`tests/backlog/test_rp_batch82.py::test_sessions_create_their_own_temp_tables_side_by_side`).
  It may also be what hung
  `test_rust_pgserver_slice.py::test_parallel_sessions_resolve_their_own_new_temp_table`
  in CI, which runs the same pattern; that is not proven.
