### Rust PG server: a `LIKE` copy keeps each column's collation

`CREATE TABLE ... (LIKE t)` copies each column's collation along with its
type (`transformTableLikeClause` copies `attcollation`), whatever options
the LIKE names. The Rust PostgreSQL server gave each copied column its
type's default collation, so a copy of a `text COLLATE "C"` column sorted
and compared under a different collation than the column it came from.

#### Fixed

- A column `LIKE` copies keeps the column's collation: `attcollation` and
  `information_schema.columns.collation_name` name it, and the copy sorts
  and compares under it, as PostgreSQL 15.19 answers. A column with no
  collation of its own stays uncollated.
- Fixes RP-844 (tests/backlog/test_rp_found_rp839.py::test_a_like_copy_keeps_a_columns_collation).
  Crate test: `secantus-pgplan` `a_like_copy_keeps_a_columns_collation`.
