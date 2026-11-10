### Rust PG server: a comparison in an ORDER BY key compares by its type's order

A comparison inside a sort key, such as `ORDER BY x > '9.5.0.0'::inet`, now
compares the way the same comparison does in the select list: by
`network_cmp` for `inet` and `cidr`, by declaration order for an enum, and by
sort key under a locale collation. Before, the type-order rewrite keyed a sort
key only when the key itself had such a type, and never rewrote it as an
expression, so a boolean key over a network value compared the values' text
and `10.0.0.1` sorted with the rows below `9.5.0.0`.

#### Fixed

- A comparison over a network value, an enum or a collated string in an ORDER
  BY key, a window's ORDER BY or an aggregate's ORDER BY sorts as PostgreSQL
  15.19 does. That includes a positional key or an output name that names
  such a comparison in the select list.

Fixes RP-762 (tests/backlog/test_rp_found_rp754.py::test_an_order_by_comparison_over_a_network_value, tests/backlog/test_rp_found_rp754.py::test_an_order_by_comparison_over_an_enum_column)

#### Filed

- RP-766: an enum column of a VALUES list sorts by its labels' text
  (tests/backlog/test_rp_found_rp762.py).
