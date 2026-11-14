### A subquery in DELETE ... USING RETURNING reads the joined row

A subquery in the RETURNING list of a `DELETE ... USING` that reads a USING
item now gets the value of the row each deleted row joined, as PostgreSQL
does. The Rust PostgreSQL server used to find the joining row again after
the delete. When the USING item was a subquery over the table being
deleted from, the rows it needed were gone by then, and the subquery
answered NULL.

#### Fixed

- `secantus-pgplan`: a `DELETE ... USING` RETURNING subquery's references to
  a USING item read the joined row from before the delete, by the same
  mechanism as UPDATE's FROM items (RP-686). `delete from t using (select
  a, b from t) x where x.a = t.a returning (select x.b)` returns each
  deleted row's `b` instead of NULL.
  Fixes RP-938 (tests/backlog/test_rp_found_rp664.py::test_a_delete_returning_subquery_reads_a_using_item_as_it_was_before_the_delete).
