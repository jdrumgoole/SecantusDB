### Rust PG server: `min` / `max` over `varchar`, `name` and `"char"` answer `text`

PostgreSQL 15 has no `max(varchar)`. A call over a type with no overload of
its own resolves like any function call (`func_select_candidate`), and
`varchar`, `name` and `"char"` reach `max(text)`, so the answer is `text`.
This server described the input type instead. The result type now comes
from the overload that `pg_aggregate`'s 22 resolve to.

#### Fixed

- `max(v)` and `min(v)` over a `varchar(n)`, `varchar`, `name`, `"char"` or
  a domain over `varchar` are described `text` (25), not `character
  varying` (1043), `name` (19) or `"char"` (18). The same holds in a GROUP BY,
  a HAVING, a FILTER or DISTINCT aggregate, a window call, `pg_typeof`, a
  FROM-subquery or CTE column, and a view's column in `pg_attribute`.
- Fixes RP-681 (tests/backlog/test_rp_found_rp671.py::test_min_max_over_varchar_is_text,
  tests/backlog/test_rp_found_rp681.py::test_min_max_take_their_overloads_type,
  tests/backlog/test_rp_found_rp681.py::test_a_views_min_max_column_is_text).

#### Known gaps

- `max(regclass)` is `max(oid)` and `max(cidr)` is `max(inet)` on
  PostgreSQL, with each input coerced before it is compared; here they
  keep their input type (RP-706).
- `pg_typeof` of a `text` scalar subquery is 42P18 (RP-707).
