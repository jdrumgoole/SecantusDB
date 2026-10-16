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
- **`SELECT DISTINCT` of plain columns over one table comes out in
  PostgreSQL's HashAggregate order**, as a GROUP BY does since batch 8:
  PostgreSQL plans the DISTINCT as a HashAggregate over the select list.
  Part of RP-028
  (tests/backlog/test_rust_postgres_gaps.py::test_distinct_rows_come_out_in_hash_aggregate_order).
