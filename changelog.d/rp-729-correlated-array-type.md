### Rust PG server: a correlated `ARRAY(...)` over an array column keeps the column's type

A correlated `ARRAY(...)` over an array column got one dimension too many in its
type. `select k, array(select t.f from ... t(k, f) where t.k >= s.k) from ...
s(k, f)` over an `integer[]` column was described as type 1043 and reached the
client as the text `{{1,NULL},{2,3}}`. `pg_typeof` of it named `integer[][]`.
PostgreSQL 15.19 describes it as `integer[]` (1007), and psycopg reads back a
nested list.

PostgreSQL types an ARRAY sublink with `get_promoted_array_type`. That function
gives a scalar column's array type. An array column keeps its own type, and the
result is one more dimension of it. The uncorrelated sublink already followed
this rule. The correlated one added `[]` to every column type, so it now applies
the same rule.

#### Fixed

- Fixes RP-729 (tests/backlog/test_rp_found_rp683.py::test_correlated_array_of_an_array_column_is_its_own_type).
  The rule is also pinned by the `secantus-pgplan` unit test
  `correlated::tests::an_array_sublink_over_an_array_column_is_its_own_type`.
- Filed RP-730: an ARRAY sublink accumulates element arrays of different
  shapes, or a NULL one, where PostgreSQL raises 2202E or 22004
  (tests/backlog/test_rp_found_rp729.py).
