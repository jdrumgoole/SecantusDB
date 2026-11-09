### Rust PG server: a padded `char(n)` value compares and sorts without its blanks

`select k from (values (1, 'a'::char(3)), (2, 'xy'::char(3))) s(k, f) where
f = 'xy'` now finds its row, as on PostgreSQL 15.19, where `bpchareq` and
`bpcharcmp` ignore trailing blanks (`bcTruelen`). It found nothing: a
FROM-subquery (a VALUES list, a cast to `char(n)`, a scalar subquery over a
`char(n)` column) stored its `char(n)` cells padded, so the outer query's
filter compared them with their blanks, and a correlated subquery passed
them on padded. The cells are now stored trimmed, as every `char(n)` value
is here, and padded again where they enter an expression and on output.
A computed ORDER BY key of type `char(n)` -- the query's own, a window's
and an aggregate's -- is compared without its blanks, so `order by
f::char(5)` puts `'a'` before `E'a\t'`. A correlated scalar subquery over a
`char(n)` column is described with the column's modifier (`exprTypmod` of
an EXPR sublink), which is also what lets a FROM-subquery over it store the
value trimmed.

#### Fixed

- A FROM-subquery's `char(n)` column, a VALUES list's included, compares
  without its trailing blanks in the outer query's WHERE, IN and joins.
- An ORDER BY over a `char(n)` expression, in a query, a window or an
  aggregate, sorts without the trailing blanks.
- A correlated scalar or `ARRAY` subquery is described with its column's
  type modifier.

Fixes RP-679 (tests/backlog/test_rp_found_rp671.py::test_padded_char_n_compares_without_its_blanks,
tests/backlog/test_rp_found_rp679.py::test_char_n_cells_and_sort_keys_compare_without_their_blanks)

Files RP-683 (a correlated `ARRAY(...)` over a `char(n)` column drops its
elements' blanks).
