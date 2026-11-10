### Rust PG server: an enum column of a VALUES list sorts by declaration order

A VALUES list whose rows cast one value to an enum type gives the column
that enum type, as PostgreSQL's `select_common_type` resolves the other
rows' untyped literals to it. The Rust PostgreSQL server typed such a column
as text for its ORDER BY, comparisons, `min` / `max` and window ordering, so
`select x::text as v from (values ('a'::e), ('z'), ('m')) s(x) order by x`
came back in the labels' alphabetical order instead of the enum's
declaration order. The derived column now takes the enum type from the cast,
as a range, network or citext cast already did.

#### Fixed

- An enum column of a VALUES list (or a FROM subquery or CTE whose rows cast
  to the enum) sorts and compares by declaration order.

Fixes RP-766 (tests/backlog/test_rp_found_rp762.py::test_a_values_enum_column_sorts_by_declaration)
