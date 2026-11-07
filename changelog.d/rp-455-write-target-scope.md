### Rust PG server: an UPDATE's or DELETE's target is in scope for its subqueries, and a hidden table name is 42P01

A subquery inside an UPDATE or a DELETE now sees the statement's target as
a relation of the level around it, as PostgreSQL's `setTargetTable` puts it
there: a subquery's `t.a` reads the target row, even when the subquery's
own FROM has `t` under an alias. A qualifier that names a table some level
holds without being able to see it is now PostgreSQL's `errorMissingRTE`,
found the way `searchRangeTableForRel` finds it: the first matching
range-table entry, innermost level first. An aliased target or FROM table
named by its own name is 42P01 `invalid reference to FROM-clause entry for
table "t"` with the hint `Perhaps you meant to reference the table alias
"x".`, and an entry an inner alias shadows gets the hint `There is an
entry for table "x", but it cannot be referenced from this part of the
query.`

#### Fixed

- `update fe x set a = (select fe.a from fs limit 1)` and `delete from fe x
  where exists (select fe.a from fs)` updated and deleted the row. Each is
  42P01, as are the same references in a WHERE and a RETURNING, and an
  aliased table of an UPDATE's FROM or a DELETE's USING named by its own
  name (`delete from fe using fs y where exists (select fs.a from fe x)`).
  That one deleted the row too.
- `update fe set a = (select fe.a from fe x limit 1)` was 42P01; it reads
  the target's `fe.a`, as in PostgreSQL. So does `delete from fe where
  exists (select fe.a from fe x)`.
- `select 1 from fe x where exists (select fe.a from fs)` answered a row,
  and so did the same under a nested EXISTS and in a join's ON. Each is
  42P01 with the alias hint.
- A CTE hidden by its alias answered to its name: `with c as (select 1 as
  a) select 1 from c x where exists (select c.a from fs)` answered its row,
  and `with c as (select 1 as a) select (select c.a from fs limit 1) from c
  x` was `missing FROM-clause entry for table "c"` at the WITH's own name.
  Each is 42P01 `invalid reference to FROM-clause entry for table "c"`
  with the hint `Perhaps you meant to reference the table alias "x".`, at
  the reference.
- `insert into fe select (select fe.a from fs limit 1), 'z'` said `missing
  FROM-clause entry`. It is the "cannot be referenced from this part of
  the query" form, since the INSERT's target is in the range table but
  not visible to its source.

Fixes RP-455 (tests/backlog/test_rp_subquery_scope.py::test_write_target_scope_in_a_subquery,
tests/backlog/test_rp_subquery_scope.py::test_enclosing_hidden_name_in_a_subquery)
Fixes RP-456 (tests/backlog/test_rp_subquery_scope.py::test_cte_alias_hides_its_name)
