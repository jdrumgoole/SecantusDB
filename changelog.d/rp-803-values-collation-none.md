### Rust PG server: a VALUES or set-operation column of conflicting collations has none

On PostgreSQL, a VALUES list or set operation whose rows carry two
different implicit collations gives its column no collation. Over
`(values ('a'::d1), ('B'::d2)) s(v)`, with `d1` and `d2` domains collated
differently, `collation for (v)` is NULL, and `order by v` and `max(v)` are
42P22 `could not determine which collation to use for string comparison`.
The Rust PostgreSQL server gave such a column the default collation and
sorted it bytewise. It now leaves the column with no collation, as
PostgreSQL does.

#### Fixed

- A VALUES or set-operation column whose rows' non-default implicit
  collations differ has no collation, as `select_common_collation` with
  `none_ok` leaves it. This counts the collations of the columns each leg
  passes through, not only casts to a collated domain. A subquery passes
  the missing collation on, and a leg with a collation of its own settles
  it in a set operation over the column.
- `ORDER BY` such a column, by name or by position, is 42P22 when the sort
  runs, a NULL row included.
- `max` and `min` over it are 42P22 once they compare two values, as
  `text_larger` and `text_smaller` are. One non-NULL value, or a window
  frame of one row, is still answered.
- `collation for` it is NULL.
- A comparison with a constant takes the constant's collation, as before.

Fixes RP-803 (tests/backlog/test_rp_found_rp796.py::test_a_column_of_conflicting_implicit_collations_has_none).

Files RP-820: such a column is still grouped, made distinct and compared
with itself bytewise, and a sort by it over no rows answers instead of
failing.
