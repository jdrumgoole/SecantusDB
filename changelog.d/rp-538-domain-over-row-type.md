### A domain may be over a composite, a table's row type included

`create domain dfs as fs` over a table `fs` is now created, as on
PostgreSQL 11 and later, and a column of it holds rows. A null test over
such a column reads the fields as it does over the composite itself:
`row(null, null)` IS NULL, `row(1, null)` is neither NULL nor NOT NULL.
The server refused the CREATE with 42704 `type "fs" does not exist`.

#### Fixed

- `CREATE DOMAIN` over a composite type; `type_is_rowtype` looks through
  a domain.
  Fixes RP-538 (tests/backlog/test_rp_found_rp534.py::test_domain_over_a_table_row_type)
