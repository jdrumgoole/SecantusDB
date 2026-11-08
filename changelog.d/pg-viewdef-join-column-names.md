### Rust PG server: `pg_get_viewdef` of a join names its columns as PostgreSQL does

`pg_get_viewdef` now prints a view over a join the way PostgreSQL 15's
`ruleutils.c` does. The deparser now has a port of PostgreSQL's
column-naming pass (`set_using_names`, `set_relation_column_names`,
`set_join_column_names`, `make_colname_unique`), and it prints a join's
column through the input column it stands for (`get_variable`). Before,
it gave up on any aliased JOIN and returned the statement as written
(` SELECT j.c FROM (fs JOIN fe USING (a) ) j;`). That was also what
`pg_dump` and `\d+` showed.

#### Fixed

- An aliased JOIN prints its alias, with its column list when a name had
  to be made unique (`(fs JOIN fe ON ((fs.a = fe.a))) j(a, c, a_1, b)`).
  A column read through a USING alias prints as its input's (`SELECT fs.a
  FROM (fs JOIN fe USING (a) AS u)`).
  Fixes RP-467 (tests/backlog/test_rp_subquery_scope.py::test_aliased_join_viewdef)
- A merged USING column of an unnamed join prints as the input column its
  join variable is. That is the left one for an INNER or LEFT join (or the
  right one when only it needs no coercion), and the right one for a RIGHT
  join. A FULL join's merged column, or one whose sides need a coercion,
  prints unqualified, and every other column of the statement with that
  name is renamed out of its way (`fx x(a_1, c, d)`). Before, a bare
  merged column made the deparser give up and return the text as written.
- `SELECT *` over a USING or NATURAL join takes each merged column once
  (`fs.a, fs.c, fe.b`, not `fs.a, fs.c, fe.a, fe.b`).
- A NATURAL join prints as the USING list it stands for (`JOIN fe USING
  (a)`), as PostgreSQL does, not as `NATURAL JOIN`.
- A USING name chosen from a join's alias list is pushed down to the
  join's inputs (`(fs fs(x, c) JOIN fe fe(x, b) USING (x)) j(x, y, z)`).
- A table's alias column list prints only when it renames a column, as
  `set_relation_column_names` decides (`fs f`, not `fs f(a, c)`).
- In the pretty form, an unnamed join on the right of another join is
  parenthesised.
  Regression tests: tests/backlog/test_rp_subquery_scope.py::test_join_viewdef_names_columns,
  and `ruleutils::tests::a_join_names_its_columns_as_ruleutils_does`.
- A table of another schema, one the search path finds outside `public`,
  or a temporary table prints its alias only when the statement wrote one
  (`rte->alias`), as `get_rte_alias` decides: `FROM b41a.t`, not `FROM
  b41a.t t`, while `FROM b41a.t t` as written still prints its `t`. Such a
  reference is stored under its `schema.name` key and carries its bare
  name as an alias qualification supplied; that alias is now marked as
  supplied, and a view or materialized view keeps, beside its definition,
  which aliases were written (`written_aliases`). A definition stored
  before this has no such record, and an alias there equal to the bare
  name is taken for a supplied one. A `public.t` written qualified no
  longer prints `t t`, and the session's own temporary tables print bare
  (`FROM tt`), not as `pg_temp_3.tt`.
  Regression test: tests/backlog/test_rp_subquery_scope.py::test_viewdef_qualified_relation_alias
  (it also holds the `b41_fixes` corpus's two lines that RP-467 broke).

#### Found

- RP-512: a view whose `*` covers a table's alias column list
  (`select * from fs f(x, y)`) is created, and every read of it is 42703,
  because CREATE VIEW expands the `*` to the table's own column names
  (tests/backlog/test_rp_found_rp467.py).
- RP-525: a materialized view is not a dependent of what it reads: a
  plain DROP of its table succeeds, and DROP ... CASCADE of the table or
  its schema leaves it behind
  (tests/backlog/test_rp_found_rp467.py::test_drop_reaches_a_dependent_materialized_view).
- RP-526: two schemas' unaliased tables of one name in a FROM are 42712,
  where PostgreSQL allows them, and an alias equal to another schema's
  unaliased table is accepted in a view
  (tests/backlog/test_rp_found_rp467.py::test_same_name_relations_in_from).
