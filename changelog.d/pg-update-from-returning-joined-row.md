### UPDATE ... FROM RETURNING reads the joined row as it was before the write

An `UPDATE ... FROM` whose RETURNING list names a column of a FROM item now
answers as PostgreSQL does: the FROM item's values come from the row the
target row joined, as it was before the statement wrote anything, beside
the target's new values. The Rust PostgreSQL server used to find the
joining row again after the write. When the join condition read a column
the UPDATE assigns, it refused the statement with `0A000`, because the new
row might no longer join. When a FROM item was a subquery over the target,
it returned values the statement had just written.

#### Fixed

- `secantus-pgplan` / `secantus-pg`: each FROM-item reference in an
  `UPDATE ... FROM` RETURNING item (outside a subquery) is evaluated over
  the old row with the SET values, before any write (`Update::joined`),
  and RETURNING reads it from there. `update t set b = u.v from u where
  t.b < 0 and u.k = 5 returning t.a, u.v` returns the joined `v` instead
  of `0A000`, and `update t set b = x.b + 100 from (select a, b from t) x
  where x.a = t.a returning x.b` returns the old `b`. A self-join's target
  name (`update t ... from t s`) now reads the target's new row. A subquery in
  RETURNING that reads a FROM item is still refused when the join
  condition reads an assigned column (RP-686, trimmed to that case).
  Fixes RP-686 (tests/backlog/test_rp_found_rp664.py::test_returning_a_from_item_when_the_join_reads_an_assigned_column)
  and RP-937 (tests/backlog/test_rp_found_rp664.py::test_update_returning_reads_a_from_item_as_it_was_before_the_write).
