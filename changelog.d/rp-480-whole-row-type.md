### Rust PG server: a whole row is of its relation's row type

`select x from fs as x(p, q)` sent its row as a `record` (oid 2249), which
psycopg reads as the tuple `('1', 'y')` where PostgreSQL sends `fs`'s row type
and psycopg reads `(1,y)`. A column-alias list is planned as a subquery over
the table, and that subquery's whole row was taken for an anonymous one's. It
is now the table's row type alone, over a join, and NULL on the side an outer
join NULL-extends. As in PostgreSQL its fields keep the table's own names:
`row_to_json(x)` prints `{"a":1,"c":"y"}`, not the aliases. A view's whole row
(`select v from v`, alone, over a join or under `v(p, q)`) had the right value
but was described as varchar (1043). It now goes out under the view's row-type
oid, `'v'::regtype`, with the composite's fields, as a table's does.

#### Fixed

- A table's whole row under a column-alias list is of the table's row type,
  and its field names are the table's own.
- A view's whole row, and a value cast to a view's row type
  (`row(1, 'y')::v`), is described by the view's row-type oid, not varchar.

Fixes RP-480 (tests/backlog/test_rp_subquery_scope.py::test_whole_row_under_column_aliases, tests/backlog/test_rp_subquery_scope.py::test_whole_row_of_a_view_is_its_row_type, tests/backlog/test_rp_subquery_scope.py::test_whole_row_under_column_aliases_keeps_field_names)
