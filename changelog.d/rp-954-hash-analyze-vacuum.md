### ANALYZE and VACUUM record a hash index's size

A hash index kept the pages and tuples its build recorded in `pg_class`, however
many rows came after. PostgreSQL's ANALYZE records every index's pages and the
rows it covers, and a VACUUM that bulk-deletes from a hash index records what
`hashvacuumcleanup` reports. Both now record the hash index's metapage model:
its pages, grown by the splits its inserts made, and the tuples it holds.

#### Fixed

- Rust PG server: ANALYZE, and a VACUUM with an index pass, record a hash
  index's `relpages` and `reltuples`.
  Fixes RP-954 (tests/backlog/test_rp_found_rp929.py::test_analyze_and_vacuum_record_a_hash_indexs_size)
