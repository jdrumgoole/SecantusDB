### Rust PostgreSQL server: backlog batch 9

More of the Rust PostgreSQL server's backlog, measured against PostgreSQL
15.19.

#### Fixed

- **Each empty tsquery raises its own NOTICE**, as PostgreSQL does: `select
  ''::tsquery, ''::tsquery` sends two `text-search query doesn't contain
  lexemes` notices, and a prepared statement sends one on every execution.
  A statement's notices were deduplicated, because it is planned for its
  Describe as well as its Execute; the Describe's plan now drops its own.
  Fixes RP-044
  (tests/backlog/test_rust_postgres_gaps.py::test_each_empty_tsquery_raises_its_own_notice).
