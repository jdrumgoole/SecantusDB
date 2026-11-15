### EXPLAIN resolves names before it reads its options

PostgreSQL analyses the statement under EXPLAIN before it reads a single
option, so `explain (bogus) select * from nosuch` reports the missing
relation, not the unknown option. The Rust PostgreSQL server now does the
same for a missing relation, a missing function and a bad literal, while an
error that only planning raises, such as a constant division by zero, still
stays behind the option's.

#### Fixed

- EXPLAIN with a bad option reported 42601 ahead of a 42P01, 42883 or 22P02
  its statement raises in parse analysis.
  Fixes RP-428 (tests/backlog/test_rp_analysis_order.py::test_explain_resolves_names_before_options)
