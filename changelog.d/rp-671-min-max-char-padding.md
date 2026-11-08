### Rust PG server: `min` / `max` over a `char(n)` column keep the value's blanks

Over a `char(3)` column holding `'xy'`, `octet_length(max(c))` is now 3,
`concat(max(c), '|')` is `xy |` and `max(c) like 'xy %'` is true, as on
PostgreSQL 15.19, where `bpchar_larger` returns one of the stored, padded
values. The aggregate's answer had entered an expression without its
blanks, and so had a FROM-subquery's column over it and a scalar subquery
`(select max(c) from t)`. The answer stays trimmed, as this server keeps
every `char(n)` value, and is padded where it enters an expression: the
aggregate's slot carries the column's width, as a `char(n)` column does,
and so does its column in a subquery's output. Padding the grouped value
itself was tried and dropped, because a HAVING `IN` list, an `IN`
subquery, a join and a `UNION` over it then compared the blanks too.

#### Fixed

- `min` / `max` over a `char(n)` column keep the value's trailing blanks in
  an expression, a FROM-subquery and a scalar subquery.

Fixes RP-671 (tests/backlog/test_rp_found_rp656.py::test_min_max_over_char_n_keep_its_blanks,
tests/backlog/test_rp_found_rp671.py::test_min_max_over_char_n_still_compare_as_bpchar)

Files RP-679 (a `char(n)` value carrying its blanks compares and sorts with
them), RP-680 (a HAVING comparison of a `char(n)` `min` / `max` does not
compare as `bpchar`), RP-681 (`min` / `max` over `varchar` are typed
`varchar`, not `text`) and RP-682 (OVER on a plain function is 0A000, not
42809).
