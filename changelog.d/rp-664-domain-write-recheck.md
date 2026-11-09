### Rust PG server: a write checks a domain only where it coerces a value into it

PostgreSQL checks a domain's constraints where a value is coerced into the
domain. A column an UPDATE does not assign is stored as it was, and a value
already of the column's domain is no coercion (`coerce_to_target_type`
returns it as it is), so neither is checked. The Rust PostgreSQL server
checked every domain column of every row it wrote. So once a CHECK was
added `NOT VALID`, a row stored before it could not be updated at all,
even in another column (`update t set b = b`), nor copied by `INSERT ...
SELECT` or kept by an upsert: each failed with 23514 where PostgreSQL
15.19 succeeds.

#### Fixed

- An UPDATE checks only the domain columns its SET list assigns a value not
  already of the column's domain.
- A value is checked where it is coerced, before a BEFORE ROW trigger sees
  the row, as PostgreSQL forms the row first: a bad value the trigger then
  replaces is refused, where it used to be written. A domain column the
  trigger changes is checked after it returns.
- A value is already of a column's domain when it is the column itself, a
  column of that domain through a FROM item, a FROM subquery or a WITH, a
  scalar or multiple-column subquery's column of it, a COALESCE, GREATEST,
  LEAST or CASE (with an ELSE) whose every input is of it, or a cast to it.
  This is `select_common_type`'s rule: a set operation's column keeps a
  domain only when every arm's column is of it. Anything computed, of
  another domain or of the base type is coerced and checked, as before.
- `INSERT ... SELECT` does not check a column whose source column is
  already of its domain. `ON CONFLICT DO UPDATE` checks only the columns
  it assigns, less those given `excluded.a` or the target's own value. An
  `ON UPDATE CASCADE` / `SET NULL` / `SET DEFAULT` action checks only the
  key columns it writes. Routing a row into a partition no longer
  re-checks the parent's domains.

Fixes RP-664 (tests/backlog/test_rp_found_rp655.py::test_write_checks_only_what_it_coerces_into_a_domain,
tests/backlog/test_rp_found_rp664.py::test_write_checks_a_domain_only_where_it_coerces
and ::test_triggered_write_checks_a_domain_where_it_coerces),
also pinned by `secantus-pgplan`'s
`domain_scope::tests::an_update_value_of_the_column_domain_is_no_coercion` and
`domain_scope::tests::an_insert_source_column_keeps_its_domain`.
Files RP-668 (an `UPDATE ... FROM` a subquery over the target whose WHERE
names the target is 42P01) and RP-670 (a PL/pgSQL assignment to a record's
field does not check the field's domain).
