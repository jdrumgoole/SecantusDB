### A subquery in UPDATE ... FROM RETURNING reads the joined row

A subquery in the RETURNING list of an `UPDATE ... FROM` that reads a FROM
item now gets the value of the row each updated row joined, as PostgreSQL
does. The Rust PostgreSQL server used to find the joining row again after
the write. When the join condition read a column the UPDATE assigns, the
new row might no longer join, so the statement was refused as not
supported. A subquery over the FROM items alone is now evaluated before the
write, so a table it reads is as it was when the statement started, which
is what PostgreSQL's statement snapshot shows it.

#### Fixed

- `secantus-pgplan`: in `UPDATE ... FROM ... RETURNING`, a subquery's outer
  reference to a FROM item reads a hidden joined-row value evaluated over
  the old row before the write (`split_joined_returning`), typed as the
  FROM item's column (`joined_returning_type`, read by
  `correlated::outer_ref_type`). A subquery that reads no column of the
  target is the joined row's value whole, evaluated before the write.
  `x.*` and `(x).a` over a FROM item inside a subquery are its columns, at
  any depth. `update t set b = u.v from u where t.b < 0 and u.k = 5
  returning (select u.v)` returns `u.v` instead of 0A000, and `(select
  u.*)` over a two-column `u` is 42601, as on PostgreSQL 15.19.
  Fixes RP-686 (tests/backlog/test_rp_found_rp664.py::test_a_returning_subquery_over_a_from_item_when_the_join_reads_an_assigned_column).
