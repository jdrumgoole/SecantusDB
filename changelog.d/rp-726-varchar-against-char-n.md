### Rust PG server: a `varchar` compared with a `char(n)` ignores trailing blanks, as `bpchareq` does

PostgreSQL resolves `varchar = bpchar` (and `<>`, `<`, `<=`, `>`, `>=`) to
the `bpchar` operator, the `varchar` side read as `bpchar` through its
implicit cast, so neither side's trailing blanks count. This server kept
the `varchar` side's blanks, so `'ab '::varchar = 'ab'::char(3)` was false.

#### Fixed

- `v = c` over a `varchar` `ab ` and a `char(3)` `ab` is true, both ways
  round and in a WHERE, an IN list, the ordering operators and a domain
  over `varchar`, as on PostgreSQL 15.19.
- A row comparison whose pair is a `varchar` and a `char(n)` -- two row
  constructors, or a row against a subquery's row -- compares that pair
  the same way.

Fixes RP-726 (tests/backlog/test_rp_found_rp697.py::test_varchar_against_bpchar_ignores_trailing_blanks, tests/backlog/test_rp_found_rp726.py::test_varchar_against_char_n_every_form)

Filed while working it: RP-731 (a JOIN ... ON a `varchar` column against a
`char(n)` one), RP-732 (a simple CASE and NULLIF compare without their
types' operator) and RP-735 (a cancel during a huge `IN` list's planning
still has a ~0.25 s tail, measured before and after this change alike).
