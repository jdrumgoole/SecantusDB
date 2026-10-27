### Rust PostgreSQL server: the information_schema views run three to four times faster, and the built-in types match PostgreSQL's catalog

`information_schema.routines`, `parameters`, `data_type_privileges` and
`element_types` list every one of PostgreSQL's 3,244 built-in functions, and
on the Rust PostgreSQL server they took seconds to minutes. Five costs that
grew with the size of a view are gone. A domain's CHECK constraint is now
parsed once rather than for every value cast to it. A row expression copies
only the columns and statement parameters it reads, so it no longer copies an
uncorrelated subquery's rows on every evaluation. A LATERAL item's memo is
keyed by the left row alone. And a FROM subquery is no longer planned again
just to learn its column names. On an optimised build `routines` went from
3.1 s to 1.05 s and `element_types` from 62 s to 15.8 s, with the same rows
as before. They are still far from PostgreSQL's tens of milliseconds;
RP-131 says what is left.

Checking those views against PostgreSQL 15.19 row by row turned up two
catalog bugs, both fixed. 56 built-in `pg_type` rows had wrong element types,
collations or NULLs where `initdb` records values, and `format_type` printed
the one-byte `"char"` as `character`.

#### Changed

- `secantus-pgplan` (`domains.rs`): each domain CHECK's prepared expression
  tree is cached by base type and text (RP-131).
- `secantus-pgplan` (`row_column_expr`): a row expression keeps only the
  fields it references, renumbered (`prune_row_fields`), and NULL in place of
  each statement parameter it never reads (`params_read_by`). Its argument
  list is reused between rows (`RowScratch`). `plan_cache`'s substitution
  matches such a NULL with any stand-in (RP-131).
- `secantus-pgplan` (`whole_row_rewrite`): a FROM subquery whose output
  names its text gives is not planned when the statement never names its
  alias as a row (RP-131).
- `secantus-pg` (`lateral_rows`): a LATERAL item binds only the statement
  parameters its SQL mentions, and memoises by the left row's values
  (RP-131).

#### Fixed

- `pg_type`: a built-in type's `typtype`, `typdelim`, `typelem`,
  `typnotnull`, `typbasetype`, `typtypmod` and `typcollation` are now
  PostgreSQL 15.19's (`pg_type_identity.tsv`), so `information_schema` calls
  `int2vector` and `oidvector` `ARRAY`. Fixes RP-300
  (`tests/backlog/test_rp_catalog_identity.py::test_builtin_types_carry_postgres_identity_columns`).
- `format_type`: oid 18 is `"char"`, and a modifier on a type with no
  `typmodout` is printed in parentheses (`text(7)`). Fixes RP-301
  (`tests/backlog/test_rp_catalog_identity.py::test_format_type_of_the_one_byte_char`).
