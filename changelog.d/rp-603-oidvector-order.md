### Rust PG server: an `oidvector` orders by length first, as `btoidvectorcmp` does

`'1 2'::oidvector < '3'::oidvector` was true and `'3'::oidvector < '1
2'::oidvector` false. PostgreSQL 15.19 answers the other way round.
`oidvector`'s ordering operators (`oidvectorlt` .. `oidvectorge`) and its
B-tree opclass `oidvector_ops` go through `btoidvectorcmp`, which puts the
shorter vector first and then compares oid by oid, each unsigned. The server
stores an `oidvector` as an int array and compared it as one, element by
element. The wrong order reached every path that sorts or compares one:
`ORDER BY` a column, with or without an index on it, a window's `ORDER BY`,
`WHERE v < '...'`, BETWEEN, `< ANY (ARRAY[...])`, a parameter, `greatest` /
`least`, and a filter on `pg_proc.proargtypes`.

#### Fixed

- An `oidvector` operand of `<`, `<=`, `>` or `>=` now compares by an order
  key whose string order is `btoidvectorcmp`'s: the length, then each oid,
  in fixed-width hex. The operand is a column of the type or a cast to it.
  The other side is cast to `oidvector` first, as the operator's input is.
  The same rewrite that orders `inet` and enums applies the key to
  `ORDER BY` and window sorts, to BETWEEN and NOT BETWEEN, to each element
  of an `ARRAY[...]` under ANY / ALL, and to `greatest` / `least`, which read
  the value back from the key.
- `max` and `min` are unchanged. On PostgreSQL they are `array_larger` /
  `array_smaller` over `anyarray`, so they compare element by element there
  too. `=` and `<>` give the same answer either way.

Fixes RP-603 (tests/backlog/test_rp_found_rp575.py::test_oidvector_orders_by_length_first,
tests/backlog/test_rp_found_rp575.py::test_oidvector_column_orders_by_length_first).
The crate regression test is `secantus-pgplan`'s
`tests::oidvector_key_orders_as_btoidvectorcmp`.

#### Filed

- RP-610: a network type's BETWEEN compares its text, so
  `'10.0.0.10'::inet between '10.0.0.2' and '10.0.0.20'` is false where
  PostgreSQL answers true.
- RP-611: an `oidvector[]` compared whole, or under ANY when it is not an
  `ARRAY[...]` written in place, still orders its elements as int arrays.
- RP-612: `btoidvectorcmp` and `oidvectorlt` .. `oidvectorne` are refused
  as not supported.
