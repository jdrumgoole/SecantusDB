### Rust PG server: a PL/pgSQL assignment to a `varchar(n)` or `char(n)` variable refuses an overlong value

A PL/pgSQL variable silently kept a truncated value. `declare x
varchar(3); begin x := 'abcdef';` stored `abc`, where PostgreSQL 15.19
raises 22001 `value too long for type character varying(3)`. A `char(3)`
variable, a variable of a domain over `varchar(3)`, a DEFAULT and a bare
`char` (which is `char(1)`) did the same. A `bit(3)` variable took a value of
another length, where PostgreSQL raises 22026.

PostgreSQL assigns to a variable by the assignment rule (`exec_assign_value`
-> `exec_cast_value`), whose length coercion refuses an overlong value
unless the excess is blanks. The interpreter cast the expression explicitly
to the declared type, and an explicit cast truncates. A variable of (a
domain over) `char(n)`, `varchar(n)`, `bit(n)` or `varbit(n)` now reads the
expression as the bare base type, fits it to the modifier by the assignment
rule, and only then casts it to the declared type, which pads a `char(n)`
and checks a domain's constraints. A value already of the domain is kept as
it is, as before.

#### Fixed

- `secantus-pg` (`plpgsql_fn.rs`, `lib.rs`): `eval_typed`, which runs every
  `:=` to a variable and every DEFAULT, asks the new
  `Host::assignment_length` / `Host::fit_assigned` for such a variable.
- `secantus-pgplan`: `declared_type_and_typmod` reads a declared type text
  as the grammar does (`varchar(3)` is `varchar` at 7, `char` is `bpchar`
  at 5).
- Fixes RP-687 (tests/backlog/test_rp_found_rp670.py::test_variable_assignment_refuses_an_overlong_value,
  tests/backlog/test_rp_found_rp670.py::test_variable_assignment_stores_a_fitting_value).

#### Filed

- RP-694: `SELECT ... INTO` a scalar variable truncates an overlong value
  and skips its domain's CHECK.
- RP-695: an assignment to a `varchar(n)[]` variable truncates an overlong
  element.
- RP-696: a cast to a domain over a domain names the inner domain in its
  23514.
