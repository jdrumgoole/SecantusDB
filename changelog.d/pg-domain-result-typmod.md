### Rust PG server: a domain-typed result column carries its base type's modifier

A result column of a domain type now reaches the client with the type
modifier PostgreSQL sends for it, so a driver reports the same length,
precision and scale it would from a real server.

#### Fixed

- PostgreSQL's `SendRowDescriptionMessage` sends a domain as its base
  type and the modifier `getBaseTypeAndTypmod` gives it, which is the
  innermost domain's `typtypmod`. Here a domain column went out with -1
  wherever it was computed rather than read from a table:
  - `select 'ab'::d`, where `d` is a domain over `char(3)`: 7.
  - A cast to a domain over `numeric(5,2)`: 327686.
  - A cast to a domain over `timestamp(0)`: 0.
  - A domain over a domain: the inner domain's modifier.
  - A cast in a SELECT over a table, a UNION of two domain casts, and a
    VALUES list: the domain's modifier.
  - `information_schema`'s `time_stamp` columns: 2.

  This covers the simple and the extended protocol. `CREATE TABLE AS`
  over a table's rows reads the same description, so a column it makes
  from a domain cast now keeps the domain's width. It is still made of
  the base type rather than the domain (RP-661).
  Fixes RP-557 (tests/backlog/test_rp_found_rp551.py::test_domain_result_column_typmod)
- Filed while working it, each already wrong before this change:
  - RP-659: `min` / `max` over a `char(n)` column describe its width.
  - RP-660: a set operation of a domain column and its base type keeps
    the width.
  - RP-661: `CREATE TABLE AS` turns a domain column into its base type.
