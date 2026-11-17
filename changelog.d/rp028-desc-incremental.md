### A descending ORDER BY of several columns over a join reads an index backward

An ORDER BY under a LIMIT over a join whose first column is descending, with
more columns after it, is now planned as PostgreSQL plans it. With a key
after the first, PostgreSQL's merge joins sort ascending, so the only path
already in the descending order is an index's backward scan, or a nested loop
over one, under an Incremental Sort; the join order search now models that
and returns the rows the LIMIT keeps, ties in the same order as PostgreSQL.

#### Fixed

- Rust PG server: `t0 JOIN t1 ON t0.b = t1.a ORDER BY t1.a DESC, t0.c LIMIT 5`
  with an index on `t1.a` leaves ties in PostgreSQL's order (an Incremental
  Sort over a Nested Loop reading the index backward), where it sorted the
  whole join and left them in another.

Fixes RP-028, in part (tests/backlog/test_rust_postgres_gaps.py::test_a_descending_first_key_with_another_reads_an_index_backward)
