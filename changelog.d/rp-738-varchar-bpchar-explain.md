### EXPLAIN prints a varchar against char(n) comparison as PostgreSQL resolves it

A `varchar` compared with a `char(n)` resolves to `bpchareq`, with the
`varchar` side relabelled to `bpchar`. The Rust PostgreSQL server's EXPLAIN
and deparse now print it that way, `((x.v)::bpchar = y.c)`, instead of
casting both sides to `text`.

#### Fixed

- `compare_coercion` resolves `varchar` against `bpchar` to `bpchareq`.
  Fixes RP-738 (tests/backlog/test_rp_found_rp726.py::test_explain_join_on_varchar_against_char_n)
