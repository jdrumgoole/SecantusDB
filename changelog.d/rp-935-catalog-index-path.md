### A catalog read directly picks its index from every clause

A statement that reads a system catalog directly now has its catalog scan
counted through the index PostgreSQL 15.19 would choose. Every restriction is
matched against every column of each index, not only the leading one, so
`attrelid = 1259 and attnum = 1` scans `pg_attribute_relid_attnum_index` on
both keys; and a range on the first column the equalities leave free bounds
the scan, so `relname >= 'pg_s' and relname < 'pg_t'` returns the 74 entries
it covers rather than all of `pg_class_relname_nsp_index`.

#### Fixed

- Rust PG server: a direct catalog read's index scan uses every matching
  clause and its inequalities' bounds in `pg_stat_sys_indexes` /
  `pg_statio_sys_*`.
  Fixes RP-935 (tests/backlog/test_rp_catalog_reads.py::test_a_catalog_scans_index_path)
