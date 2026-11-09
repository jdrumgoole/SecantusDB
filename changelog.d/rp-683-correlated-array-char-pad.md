### Rust PG server: a correlated subquery over a `char(n)` column keeps its blanks

A correlated `ARRAY(...)` over a `char(n)` column returned each element without
its trailing blanks. `select k, array(select t.f from ... t(k, f) where t.f =
s.f) from ... s(k, f)` over `char(3)` values answered `{a}` where PostgreSQL
15.19 answers `{"a  "}`. A correlated scalar subquery over an inner `char(n)`
column had the same fault: `octet_length` of it was 1, not 3.

This server stores a `char(n)` value without its blanks and pads it back to its
width where it enters an expression. The uncorrelated sublinks already did that,
through a cast to the column's `character(n)` or `character(n)[]` at its
modifier. The correlated call answered the inner rows as they were stored. It
now pads each value, and each element of an array at any depth, to the width
carried in the call's modifier. Comparisons, grouping and `DISTINCT` over the
padded values still ignore the blanks, as `bpchar` comparison does.

#### Fixed

- Fixes RP-683 (tests/backlog/test_rp_found_rp679.py::test_correlated_array_of_char_n_keeps_the_elements_blanks),
  with the correlated scalar case pinned by
  tests/backlog/test_rp_found_rp679.py::test_correlated_char_n_values_enter_an_expression_padded
  and the helpers by the `secantus-pgplan` unit test
  `correlated::tests::a_correlated_char_n_result_is_padded_to_its_width`.
