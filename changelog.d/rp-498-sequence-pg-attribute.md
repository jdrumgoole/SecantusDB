### Rust PG server: `pg_attribute` lists a sequence's columns

A sequence had a `pg_class` row (relkind `S`), and `select * from s` answered
`last_value`, `log_cnt` and `is_called`. But `pg_attribute` had no row for it,
so a client that read a sequence's columns from the catalog found none.
PostgreSQL lists them, because a sequence is a relation with storage.

#### Fixed

- `pg_attribute` lists a sequence's three columns, as `sequence.c`'s
  `DefineSequence` makes them: `last_value` and `log_cnt` (`int8`) and
  `is_called` (`bool`), attnum 1-3, each NOT NULL. It also lists the six
  system columns (attnum -6 to -1). This covers a sequence in any schema
  and the sequence behind a `serial` or identity column. Every
  `pg_attribute` column of the nine rows matches PostgreSQL 15.19.

Fixes RP-498 (tests/backlog/test_rp_ctas_subquery.py::test_pg_attribute_lists_sequence_columns).
