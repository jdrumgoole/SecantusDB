### `pg_get_indexdef` quotes every name, and a quoted column keys an expression index

The Rust PostgreSQL server now prints an index definition with each name
through `quote_identifier`, as PostgreSQL's ruleutils does, and a plain
column that needs quotes can sit beside an expression in an index key.

#### Fixed

- `create index i on t ("Big Y" desc) include ("W w")` printed `btree (Big
  Y DESC) INCLUDE (W w)` in `pg_get_indexdef` and `pg_indexes.indexdef`;
  it now prints `btree ("Big Y" DESC) INCLUDE ("W w")`, as PostgreSQL 15.19
  does. The index name, the table, a PRIMARY KEY's and a UNIQUE
  constraint's columns are quoted the same way (`CREATE UNIQUE INDEX "Q
  t_pkey" ON public."Q t" USING btree ("P k")`).
- `create index on t (lower("W w"), "Big Y")` failed with `column "big"
  does not exist`, because the plain key was kept as the unquoted text
  `Big Y`, and the failed index was left on the table, so every later
  INSERT failed the same way. The key is now kept quoted, and the index
  builds and serves.
- An index made by an earlier build keeps the key text it stored then.
- Fixes RP-1082 (`tests/backlog/test_rp_found_rp860.py::test_pg_get_indexdef_quotes_a_key_and_an_include_column`,
  `tests/backlog/test_rp_found_rp860.py::test_pg_get_indexdef_quotes_every_name`).
