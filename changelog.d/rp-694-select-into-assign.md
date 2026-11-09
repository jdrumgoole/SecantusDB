### Rust PG server: `SELECT ... INTO` a PL/pgSQL variable assigns by the assignment rule

A query's row moved into PL/pgSQL variables with no coercion at all.
`declare x varchar(3); begin select 'abcdef' into x;` kept the overlong
value, where PostgreSQL 15.19 raises 22001 `value too long for type
character varying(3)`; `select 'no' into x` for a variable of a domain with
`check (value <> 'no')` stored `no`, where PostgreSQL raises the domain's
23514. An `int` variable took `5000000000` and `'abc'`, a `numeric(3,1)`
one took `12345`, and a `bit(3)` one five bits, each where PostgreSQL
raises 22003, 22P02, 22003 or 22026. The same held for a `FETCH ... INTO`,
an `INSERT / UPDATE / DELETE ... RETURNING ... INTO` and a list of
variables (`select 1, 'abcdef' into x, y`).

PostgreSQL fills the variables with `exec_move_row`, which assigns each
column through `exec_assign_value`, the coercion a `:=` makes: none when the
column is of the variable's own type and the declaration has no modifier,
otherwise a cast to the declared type with the length fitted by the
assignment rule and a domain's constraints checked. The interpreter now does
the same for each variable an `INTO` fills, through the coercion RP-687 gave
`:=`. A column of the variable's own domain is moved as it is, as
PostgreSQL does; the new `select_output_domains` names the domain of each
column a SELECT returns, which the row description does not carry.

#### Fixed

- `secantus-pg` (`plpgsql_fn.rs`, `plpgsql_fn/open_cursor.rs`, `lib.rs`):
  `assign_row` coerces each variable it fills (`coerce_moved`), told each
  column's domain by the new `Host::output_domains`.
- `secantus-pgplan` (`domain_scope.rs`): `select_output_domains`, the domain
  each output column of a SELECT is of, parameters included.
- Fixes RP-694 (tests/backlog/test_rp_found_rp687.py::test_select_into_variable_assigns_by_the_assignment_rule,
  tests/backlog/test_rp_found_rp687.py::test_a_moved_row_assigns_each_variable_by_the_assignment_rule).

#### Filed

- RP-712: an assignment between types with only an explicit cast (`int`
  from `bool`) uses it, where PostgreSQL converts through text and raises
  22P02.
- RP-713: a `FOR` over a query, or an `EXECUTE ... INTO`, refuses scalar
  variable targets with 0A000.
- RP-714: a variable of a NOT NULL domain with no default starts NULL,
  where PostgreSQL raises 23502 at the block's entry.
