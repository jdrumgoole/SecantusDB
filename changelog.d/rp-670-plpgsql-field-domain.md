### Rust PG server: a PL/pgSQL assignment to a trigger record's domain field checks the domain

A BEFORE trigger could store a row its own body had refused. `new.a := -5;
new.a := 7; return new;` on a column of a domain with `check (value > 0)`
wrote the row, where PostgreSQL 15.19 raises 23514 at the first assignment:
NEW's field was typed by the domain's base type, so the assignment cast to
the base and checked nothing.

PostgreSQL's `exec_assign_value` casts an assigned value from its own type
to the field's, and a trigger's NEW and OLD fields are of the columns'
declared types. The fields are now typed by the column's domain
(`pg_typeof(new.a)` is the domain, as on PostgreSQL). An assignment keeps a
value already of the domain (`old.a`, a COALESCE of it, a cast to the
domain) as it is, so a row stored before a `NOT VALID` CHECK still passes
through a trigger. Any other value is coerced to the base type at the
domain's modifier, so an overlong `varchar(3)` domain value is 22001 at the
assignment, and is checked against every domain of the chain under the name
of the one it is coerced to.

#### Fixed

- `secantus-pg` (`triggers.rs`, `plpgsql_fn.rs`, `lib.rs`): NEW / OLD
  fields carry the column's domain; an assignment to a record field asks
  the new `Host::coerce_to_domain`, which keeps a value already of the
  domain and otherwise fits it to the base type
  (`secantus_pgplan::fit_assigned_length`) and checks the domain chain.
- `secantus-pgplan`: `select_target_domain` answers `exprType`'s domain
  for a bound PL/pgSQL expression (a parameter is of its declared type);
  a cast of a parameter declared of a domain to that domain, or of a cast
  to a domain to the same domain, is no coercion and checks nothing,
  while the inner cast of `(-5)::d::d` still checks.
- Fixes RP-670 (tests/backlog/test_rp_found_rp664.py::test_plpgsql_assignment_to_a_record_field_checks_its_domain,
  tests/backlog/test_rp_found_rp664.py::test_assignment_to_a_domain_field_coerces_only_what_is_not_of_it,
  tests/backlog/test_rp_found_rp664.py::test_cast_to_a_domain_of_a_value_already_of_it).

#### Filed

- RP-687: a PL/pgSQL assignment to a `varchar(n)` / `char(n)` variable
  truncates an overlong value where PostgreSQL raises 22001.
- RP-688: a `SELECT ... INTO` record's fields are typed without their
  domain or modifier.
- RP-689: a variable of a table's row type has no assignable fields.
- RP-690: an embedded `stop()` with an idle connection open took 3.8 s
  once under load, against `stop_does_not_wait_on_an_idle_connection`'s
  3 s budget.
