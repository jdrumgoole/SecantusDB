### Rust PG server: a `select *` FROM subquery or CTE is type-checked

A FROM subquery or CTE written `select *` had no column types for the
operator check, so a comparison over its columns that PostgreSQL refuses
was lowered anyway and returned no rows. `select * from (select * from e) t
where a = true` (an integer `a`) is now 42883 `operator does not exist:
integer = boolean`, as on PostgreSQL 15.19, and so are the same comparison
over `with t as (select * from e)` and an INSERT reading that CTE.

#### Fixed

- A `*` or `t.*` in a derived table's select list expands to its FROM
  items' columns in order, as `ExpandColumnRefStar` does: a USING or
  NATURAL join's merged columns once and first, renamed by the derived
  table's alias list, and for a CTE by its own column list (`with t(x, y)
  as ...`) and then its FROM alias's. A name the derived table lists twice
  is ambiguous (42702) and is no longer typed as either column.
- Fixes RP-431 (tests/backlog/test_rp_analysis_order.py::test_star_derived_table_is_typed);
  `test_star_derived_table_expands_as_postgres_does` beside it pins the
  expansion's shapes against PostgreSQL 15.19's answers.

#### Found

- RP-434: a LATERAL subquery's columns, a view's, a VALUES column of
  untyped literals and a USING join's merged column of two types are still
  untyped, so a comparison PostgreSQL refuses over them returns no rows.
