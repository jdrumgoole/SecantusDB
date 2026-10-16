### Rust PostgreSQL server: jsonb sorts in PostgreSQL's order

`jsonb` is stored as its text, and the Rust PostgreSQL server compared that
text: `ORDER BY` a jsonb column sorted `"a"` before `[]` before `false`
before `null`, and `'[1]' < '[0,0]'` was false. PostgreSQL's jsonb has an
order of its own, now transcribed from `compareJsonbContainers`.

#### Fixed

- **`ORDER BY` over `jsonb`** -- a plain one, `DESC`, a window's and an
  aggregate's -- and the `<` / `<=` / `>` / `>=` operators order jsonb as
  PostgreSQL 15.19 does: null < string < number < boolean < array <
  object, arrays and objects by their element and pair counts before their
  contents, numbers as numerics, and an empty array below every scalar.
  Before, each compared the stored text, a silent wrong order
  (tools/probes/pg_corpora/jsonb_order.sql).

#### Added

- **`bttextcmp`, `bpcharcmp`, `jsonb_cmp`, `tsvector_cmp` and
  `tsquery_cmp`.** The first three compare as the database collation orders
  (`C` here) and `tsvector_cmp` / `tsquery_cmp` by the types' storage
  layouts, transcribed from PostgreSQL 15 (`silly_cmp_tsvector`,
  `CompareTSQ`); each answers only the sign, -1 / 0 / 1, which is all
  PostgreSQL promises (Joe, 2026-10-16). The signs match PostgreSQL 15.19
  on every one of 576 tsvector and 576 tsquery pairs. Closes RP-015
  (tests/backlog/test_rust_postgres_gaps.py::test_strcoll_compares_answer_the_sign,
  tools/probes/pg_corpora/sign_compare.sql).
