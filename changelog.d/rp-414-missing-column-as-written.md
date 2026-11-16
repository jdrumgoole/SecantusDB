### A missing column is reported first, and as written

When a statement names more than one column that does not exist, the Rust
PostgreSQL server now reports the one PostgreSQL does: the first in parse
analysis's order (an UPDATE's WHERE before its SET list), by the name as
written -- an alias's qualifier kept, `excluded.bb` rather than an internal
name -- and positioned at its first token, so a schema-qualified
`public.e.bb` points at `public`. The closest-column HINT follows it.

#### Fixed

- A 42703 for a missing column names the first missing reference in
  PostgreSQL's analysis order, with its qualifier as written and its
  position at the reference's first token.
  Fixes RP-414 (tests/backlog/test_rp_column_errors.py::test_missing_column_reported_as_written)
