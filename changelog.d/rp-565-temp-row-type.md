### Rust PG server: a bare type name is looked up through the search path

A routine over a temporary table's row type, `create function f(tt)`, was
stored naming no type at all: `pg_proc.proargtypes` read `-`, `drop table
tt` succeeded without a word, and the routine was left behind. The same
happened to a table's row type in any schema the search path names other
than `public`, where `(row(1)::t).a` and `'t'::regtype` were also refused
as an unknown type. PostgreSQL finds a bare type name the way
`TypenameGetTypid` does, in the session's temporary schema first and then
in each schema on the search path. This server now looks it up the same
way, so such a routine records its type, depends on the table, and keeps
the table from being dropped without CASCADE.

#### Fixed

- A bare type name in a cast, a column definition, a routine's parameters
  and result, `'t'::regtype` and `to_regtype` is found in the session's
  temporary schema first (where the path names `pg_temp`, at that
  position), then in each schema on the search path. The `_t` array
  spelling and an explicit `pg_temp.t` work for such a type too.
- A cast inside a field selection, `(row(1)::t).a`, is resolved through the
  search path like any other cast.
- A user type prints bare only when its bare name finds it, as
  `TypeIsVisible` decides: a temporary table's row type prints `tt`, and a
  `public` type that a temporary one of the same name hides prints
  `public.t`.
- DROP TABLE's 2BP01 names a table, and a foreign key's table, bare when
  the search path finds it (`RelationIsVisible`), a temporary one included,
  rather than by its stored `pg_temp_N.t` or `s.t` key.
- Fixes RP-565
  (tests/backlog/test_rp_found_rp546.py::test_routine_over_a_temp_table_row_type)
  and RP-889
  (tests/backlog/test_rp_found_rp565.py::test_a_bare_type_name_is_found_through_the_search_path).
  Planner regression test:
  `crates/secantus-pgplan/src/tests.rs::a_bare_type_name_searches_the_temporary_schema_first`.
- Filed while working it, each with a gap test in
  `tests/backlog/test_rp_found_rp565.py`:
  - RP-890: a DROP's DETAIL names a foreign key by its schema-qualified
    catalog key (`s.child_b_fkey`) instead of its own name.
  - RP-891: a temporary row type does not hide a built-in type of the same
    name, though PostgreSQL searches the temporary schema ahead of
    `pg_catalog`.
