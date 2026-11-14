### DELETE ... USING RETURNING reads the joined row as it was before the delete

A `DELETE ... USING` whose RETURNING list names a column of a USING item now
returns the values of the row each deleted row joined, as PostgreSQL does.
The Rust PostgreSQL server used to find the joining row again after the
delete. When the USING item was a subquery over the table being deleted
from, the rows it needed were gone by then, and RETURNING answered NULL.

#### Fixed

- `secantus-pgplan` / `secantus-pg`: a USING-item reference in a `DELETE
  ... USING` RETURNING item (outside a subquery) is evaluated over each
  row before it is deleted, and before its BEFORE trigger runs
  (`Delete::joined`, sharing `plan_write_returning` with UPDATE). `delete
  from t using (select a, b from t) x where x.a = t.a returning x.b`
  returns each deleted row's `b` instead of NULL. A subquery in RETURNING
  that reads the USING item still finds it after the delete (RP-938,
  trimmed to that case).
  Fixes RP-938 (tests/backlog/test_rp_found_rp664.py::test_delete_returning_reads_a_using_item_as_it_was_before_the_delete).
