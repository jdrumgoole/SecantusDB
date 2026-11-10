### Rust PG server: a literal in the select list of a query with a FROM item has its own type

PostgreSQL gives a literal its type from how it is written: `1.5` and `1e-3`
are `numeric`, and `B'101'` and `X'1F'` are `bit`. In a query with no FROM
item, the server already gave them those types. Over a FROM item (a table,
`generate_series`, a VALUES list), it worked out the type from the decoded
value instead, so these literals were described as `text`. A client then read
`select 1.5 from t` as the string `'1.5'` rather than a number. An
`INSERT ... SELECT 1.0, g FROM generate_series(1, 2) g` into a `numeric`
column was also refused with 42804 (`column "x" is of type numeric but
expression is of type text`). The type now comes from the literal itself,
the way it already did with no FROM item. Measured on PostgreSQL 15.19: 42
literal and FROM-item pairs now agree with it.

#### Fixed

- `select 1.5 from t`, `select 1e-3 from generate_series(1, 2) g` and the
  same over a VALUES list are `numeric` (oid 1700), and `select B'101' from
  t` is `bit` (oid 1560). All four were `text` before.
- `insert into t (x) select 1.5 from u` and `insert into t select 1.0, g
  from generate_series(1, 2) g` insert their rows into a `numeric` column,
  rather than failing with 42804.

Fixes RP-843 (tests/backlog/test_rp_found_rp692.py::test_a_numeric_constant_selected_from_a_from_item_is_inserted),
pinned beside the planner by
`crates/secantus-pgplan/src/tests.rs::a_select_list_literal_over_a_from_item_is_typed_by_its_node`.
