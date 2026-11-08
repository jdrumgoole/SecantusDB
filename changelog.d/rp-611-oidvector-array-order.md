### Rust PG server: an `oidvector[]` orders its elements by `btoidvectorcmp`

`array['1 2'::oidvector] < array['3'::oidvector]` was true. PostgreSQL 15.19
answers false. `array_cmp` compares two arrays element by element with the
element type's B-tree support function, which for `oidvector` is
`btoidvectorcmp`: the shorter vector first. The server stores an
`oidvector[]` as nested int arrays and compared it as one. Every path that
compares, sorts or picks an extreme of one was wrong: `<` .. `>=`, BETWEEN,
`ORDER BY` a column or a window's `ORDER BY`, `greatest` / `least`, `max` /
`min`, `array_larger` / `array_smaller`, and `v < ANY (vs)` / `ALL` over an
`oidvector[]` that is not an `ARRAY[...]` written in place.

#### Fixed

- An `oidvector[]` now compares and sorts by an order key whose string order
  is `array_cmp`'s over `btoidvectorcmp`. Each element is keyed in storage
  order, as RP-603 keys an `oidvector`. A NULL element sorts after every
  other. The array whose elements run out first comes first, then the one
  with fewer dimensions. The array's text follows a `|`, which `greatest` /
  `least` / `max` / `min` / `array_larger` / `array_smaller` read the value
  back from. An operand counts as an `oidvector[]` when it is a column of
  the type, a cast to it, or an `ARRAY[...]` of `oidvector`s. The other side
  is cast to `oidvector[]` first.
- Under `ANY` / `ALL`, an `oidvector[]` that is not written in place is keyed
  element by element, so each element compares as the left `oidvector`
  does. An `ARRAY[...]` beside an unknown-typed left side is keyed the same way.
- A VALUES list or subquery column that casts to `oidvector` or `oidvector[]`,
  or builds an `ARRAY[...]` of `oidvector`s, is now typed as one. Its
  `ORDER BY` and its comparisons take the order keys too. RP-603 covered table
  columns only, so `select v from (values ('1 2'::oidvector), ('3')) s(v)
  order by v` still sorted as int arrays.

Fixes RP-611 (tests/backlog/test_rp_found_rp603.py::test_oidvector_array_orders_by_btoidvectorcmp).
The crate regression test is `secantus-pgplan`'s
`tests::oidvector_array_key_orders_as_array_cmp`.

#### Filed

- RP-651, filed and closed: a VALUES list's `inet` / `cidr` column ordered
  by its text in `ORDER BY` and in a window's `ORDER BY`. RP-610's typing
  of a derived network column fixed it first; its gap test is now a
  regression test (tests/backlog/test_rp_found_rp611.py::test_values_network_column_orders_by_network_cmp).
- RP-652: an outer `ORDER BY` naming a window function, by position or
  alias, is refused as `function row_number() is not supported yet` when the
  window orders by a range, a multirange or an `oidvector`.
