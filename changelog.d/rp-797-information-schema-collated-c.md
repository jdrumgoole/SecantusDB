### information_schema's text columns are collated "C"

PostgreSQL declares the `character_data` and `yes_or_no` domains that type
every text column of the `information_schema` views `COLLATE "C"`. The Rust
PostgreSQL server now gives those columns the same collation, so `collation
for (data_type)` answers `"C"` and the columns compare and sort bytewise, as
on PostgreSQL.

#### Fixed

- Rust PG server: `information_schema` views' text columns carry collation
  "C" instead of the database's default.
  Fixes RP-797 (tests/backlog/test_rp_found_rp782.py::test_information_schema_character_data_columns_are_collated_c)
