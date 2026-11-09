### Rust PG server: `= ANY` / `ALL` of a `char(n)` array compares without the blanks

`select k from t where f = any('{a,xy}'::char(3)[])` over a `char(3)` column
now finds the `'a'` and `'xy'` rows, as on PostgreSQL 15.19. It found
nothing: the array's elements were read padded (`'a  '`) and compared, with
their blanks, against the trimmed stored value. The same happened in a
FROM-subquery's column over a `min` / `max` and in a HAVING. The operator of
`x op ANY / ALL (array)` is now resolved as PostgreSQL resolves it over `x`
and the element type: a `char(n)` side drops its trailing blanks, and a
`varchar` or untyped side does too when the other side is `char(n)`.
Against a `text[]` (or `ARRAY['a ']`, which is one) the `char(n)` side is
compared as `text` through `rtrim`, so the text elements keep theirs.

#### Fixed

- `= ANY`, `<> ALL` and the ordering operators over a `char(n)` operand and
  an array compare as `bpchar` (`bpchareq` / `bpcharcmp`, which ignore
  trailing blanks) in a WHERE, over a FROM-subquery's column and in a
  HAVING; a `text[]` array keeps its elements' blanks.

Fixes RP-684 (tests/backlog/test_rp_found_rp680.py::test_any_of_a_char_n_array_compares_as_bpchar)
