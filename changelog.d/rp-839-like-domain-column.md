### Rust PG server: a `LIKE` copy of a domain column is one of the domain

`CREATE TABLE ... (LIKE t)` copies each column's type as it is
(`transformTableLikeClause` copies `atttypid`), so on PostgreSQL a column
of a domain stays a column of that domain. The Rust PostgreSQL server
gave the copy the domain's base type instead. The domain's CHECK and
NOT NULL then went unenforced on the new table: a value the original
column refuses was accepted.

#### Fixed

- A column `LIKE` copies from a column of a domain is a column of the
  domain: the domain's CHECK and NOT NULL are checked on every write, an
  INSERT that leaves the column out takes the domain's DEFAULT, and
  `information_schema.columns.domain_name` and `format_type` name the
  domain, as PostgreSQL 15.19 answers.
- `INCLUDING DEFAULTS` no longer copies the domain's DEFAULT in as the
  new column's own. The domain's DEFAULT belongs to the type
  (`typdefault`), not to the column, so there is nothing to copy and
  `column_default` stays NULL. A DEFAULT the column set itself is still
  copied.
- Fixes RP-839 (tests/backlog/test_rp_found_rp825.py::test_a_like_copy_of_a_domain_column_is_one_of_the_domain).
  Crate test: `secantus-pgplan` `a_like_copy_of_a_domain_column_is_one_of_the_domain`.

#### Filed

- RP-844: a `LIKE` copy drops a column's collation.
- RP-845: a domain whose name needs quotes is printed without them in its
  NOT NULL and CHECK errors.
- RP-846: a `char(n)` column's DEFAULT prints its cast as `::character`,
  not `::bpchar`.
