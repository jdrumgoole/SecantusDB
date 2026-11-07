### Rust PG server: a join's ON after a comma item is type-checked

PostgreSQL analyses a join's ON with only that join's two sides in view, so
in `select * from e, s join k on b = k.a` the bare `b` can only be `k.b`.
The operator check here judged the ON against the whole FROM list. There `b`
named both `e.b` and `k.b`, so it was treated as ambiguous and never typed,
and the `text = integer` comparison was planned. Over empty tables it
answered no rows, and once there were rows it failed at run time with a
`0A000`. PostgreSQL 15.19 refuses the statement with `42883 operator does
not exist: text = integer`, and so does this server now.

#### Fixed

- A join's ON is type-checked in a scope of its own join's sides plus the
  enclosing queries, as `transformJoinOnClause` scopes it. A FROM item
  beside the join is no longer in view. The same applies to a
  parenthesised, aliased join (`from e, (s join k on b = k.a) j`), which
  answered `0A000 an aliased JOIN is not supported yet` and now gives
  PostgreSQL's `42883`.

Fixes RP-416
(tests/backlog/test_rp_column_errors.py::test_join_on_after_comma_item_is_type_checked).

#### Found

RP-430 (an outer query's column named in a subquery's join ON is a 42P01 or
42703, where PostgreSQL runs the query), with its gap test.
