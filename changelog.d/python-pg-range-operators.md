### Python PostgreSQL server: the range and multirange operators

The `range_ops` corpus went from 260 divergences of 771 to 0 against PostgreSQL 15.

#### Fixed

- Range ordering (`<`, `<=`, `>`, `>=`) follows PostgreSQL's range order: empty first, then lower bound, then upper bound. It raised `42883` for an unbounded or empty range.
- `<<`, `>>`, `&<` and `&>` work on ranges and multiranges. `&<` / `&>` answered `0A000`, and `<<` / `>>` fell through to the bit-shift path.
- Multirange `+`, `*` and `-` are implemented, and `pg_typeof` reports the multirange type.
- A non-contiguous range union or difference answers `22000`, where it raised an internal error.
- A multirange against a plain range answers PostgreSQL's `42883 operator does not exist: int4multirange + int4range`.
- `daterange @> date` no longer raises an internal error.
