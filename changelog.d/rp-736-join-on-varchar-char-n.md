### Rust PG server: a JOIN ... ON a `varchar` column equal to a `char(n)` one finds the row whose `varchar` has trailing blanks

PostgreSQL resolves `varchar = bpchar` to `bpchareq` and hashes the join on
it with `hashbpchar`, so neither side's trailing blanks count. This server
hashed the join on the stored values, which kept the `varchar` side's
blanks, so a `varchar` `'ab '` never met the `char(3)` `'ab'`.

#### Fixed

- `select x.k, y.k from t x join t y on x.v = y.c`, `v` a `varchar` and `c`
  a `char(n)`, finds the row whose `varchar` has trailing blanks, as on
  PostgreSQL 15.19: inner, LEFT, RIGHT and FULL joins, with a second key,
  and with the cast written out (`x.v::bpchar = y.c`). A join's string hash
  now ignores trailing blanks; the ON condition still decides each pair, so
  `text = char(n)` (`texteq`) keeps telling `'ab '` from `'ab'`.

Fixes RP-736 (tests/backlog/test_rp_found_rp726.py::test_join_on_varchar_against_char_n)

Filed while working it: RP-738 (EXPLAIN prints that join's Hash Cond
through `text` casts, not `(x.v)::bpchar = y.c`). Renumbered RP-732 (a
simple CASE and NULLIF over a `char(n)`) to RP-737: two lanes had filed
RP-731 and RP-732 at once.
