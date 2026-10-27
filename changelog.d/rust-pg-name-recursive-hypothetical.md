### Rust PG server: `name` keeps 63 bytes, WITH RECURSIVE is analysed first, hypothetical-set aggregates rank rows of several values

Three divergences from PostgreSQL 15.19 closed together. Fixes RP-304
(`tests/backlog/test_rp131_names.py::test_a_cast_to_name_keeps_namedatalen_bytes`),
RP-295
(`tests/backlog/test_rp_top10_found.py::test_a_recursive_query_postgres_refuses`)
and RP-283
(`tests/backlog/test_rp_top10_found.py::test_a_hypothetical_set_aggregate_over_two_columns`,
`::test_a_hypothetical_set_aggregate_ranks_a_row_of_several_values`).

#### Fixed

- A cast to `name` keeps at most NAMEDATALEN - 1 (63) bytes, cut back to a
  character boundary as `namein` cuts: `repeat('a', 70)::name` is 63 bytes
  and `repeat('é', 40)::name` 62. It kept every byte.
- A plain SELECT with a WITH RECURSIVE runs PostgreSQL's parse analysis (the
  transcription CREATE VIEW already used) before it runs. A recursive term
  that changes a column's type is 42804 `UNION types integer and text cannot
  be matched`, and one holding an aggregate is 42P19 `aggregate functions
  are not allowed in a recursive query's recursive term`. Both used to be
  answered, and without a LIMIT they ran for ever.
- `rank`, `dense_rank`, `percent_rank` and `cume_dist` take one direct
  argument per ordering column (`dense_rank(2, 'x') WITHIN GROUP (ORDER BY
  a, b)`), and compare each row with the hypothetical one column by column,
  each in its own direction and NULLS order. Over more than one column they
  used to be 0A000.

#### Found

- RP-308: with the wrong number of direct arguments, the error names the
  function by `unknown`s and gives the generic hint, where PostgreSQL gives
  the argument types and a hint with both counts.
