### Rust PG server: LATERAL, view, VALUES and USING columns are type-checked

A comparison PostgreSQL refuses at parse analysis returned no rows when its
column came from a LATERAL subquery, a view, a VALUES list of untyped
literals or a USING join's merged column of two types: the operator check
could not type those columns, so the query was lowered anyway and matched
nothing. Each is now 42883, as on PostgreSQL 15.19: `select * from e cross
join lateral (select e.a + 1 as c) l where c = true` and `select * from v
where a = true` over a view of `e` are `integer = boolean`, `select * from
(values (1, 'x')) w where column2 = 1` is `text = integer`, and `select *
from e join n using (a) where a = true` with a bigint `n.a` is `bigint =
boolean`.

#### Fixed

- A LATERAL subquery's columns are typed with the FROM items before it in
  view, qualified or not, as `transformFromClauseItem` gives it them.
- A view is typed from its stored query, as the RTE_SUBQUERY PostgreSQL
  reads it as; it no longer counts as a relation the check could not find.
- A USING or NATURAL join's merged column has the sides' common type, and
  an unqualified name reads it rather than either side's (the outermost
  join's when they nest, and still ambiguous beside another FROM item with
  that name). The same type reaches a `*` over the join in a derived table.
- The common type of a VALUES column, CASE, COALESCE, GREATEST / LEAST and
  a merged column follows `select_common_type`: inputs that are all untyped
  literals or NULL resolve as `text` (`coalesce(null, null) = 1` is `text =
  integer`, and a CASE of string literals in WHERE is 42804), numbers meet
  along the implicit casts (`numeric` with `real` is `real`, no longer
  `double precision`), and among strings a leading `text` stays, `name`
  takes over and otherwise the first input's type stays.
- Fixes RP-434 (tests/backlog/test_rp_analysis_order.py::test_lateral_view_values_and_using_columns_are_typed);
  `test_common_type_is_select_common_types` beside it pins the common-type
  rules against PostgreSQL 15.19's answers, and the `optype` unit test
  `select_common_follows_the_implicit_casts` the pairwise meet.
