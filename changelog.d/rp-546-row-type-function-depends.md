### Rust PG server: a function over a table's row type depends on the table

`create function f(r t) ...` over a table `t`, followed by `drop table t`,
dropped the table and left `f` in `pg_proc` over a type that no longer
existed. On PostgreSQL 15.19 `ProcedureCreate` records a `pg_depend` row
from the routine to every type its signature names, a table's row type and
its array among them. So the plain DROP TABLE is 2BP01 `cannot drop table t
because other objects depend on it`, with DETAIL `function f(t) depends on
type t`, and `drop table t cascade` drops the function with the table.

#### Fixed

- DROP TABLE refuses while a routine names the table's row type or its
  array type. That counts an argument, a `SETOF` or plain result, an OUT
  parameter and a `RETURNS TABLE` column, and a procedure as well as a
  function. CASCADE drops each routine and its ACL, along with the casts
  from or to the row type and the casts those routines implement.
- The DETAIL and the CASCADE notice name the routines as
  `getObjectDescription` does: `function name(input types)`, the types
  joined by a bare comma, and a name that needs quoting quoted
  (`function "Q f"(t)`). A routine or type in a schema off the search path
  is qualified (`function s.h(s.t)`, `depends on type s.t`). The routines
  come first, those over the array type before the rest, then the table's
  inheritance children, foreign keys and views.
- DROP VIEW and DROP MATERIALIZED VIEW do the same for a view's row type,
  each refused under its own name (`cannot drop materialized view mv
  because ...`). A table turned into a view by `CREATE RULE "_RETURN"` keeps
  its row type and the routines over it, as PostgreSQL's does.
- `DROP SCHEMA ... CASCADE` names a routine in the schema in that same
  form, so one also reached through a member table's row type is named
  once. It used to read `function s.h("s.t")`.

Fixes RP-546 (tests/backlog/test_rp_found_rp527.py::test_row_type_function_depends_on_its_table),
with the regression tests
`tests/backlog/test_rp_found_rp527.py::test_row_type_dependants_as_postgres_names_them`,
`::test_row_type_dependants_in_a_schema` and `::test_view_row_type_dependants`.
Files RP-560 (DROP TABLE's dependants are named by kind, not in creation
order), RP-561 (an operator over a table's row type does not depend on it),
RP-562 (DROP TYPE of a row type with a routine over it names the routine),
RP-563 (DROP TYPE names a routine with a blank after each comma), RP-564 (a
multi-table DROP's refusal names the first table), RP-565 (a routine over a
temporary table's row type names no type), RP-566 (a SQL routine over a type
that does not exist is created), RP-567 (a `regprocedure` prints its
name unquoted and a schema's type quoted whole) and RP-568 (a view dropped by
CASCADE leaves the routines over its row type).
