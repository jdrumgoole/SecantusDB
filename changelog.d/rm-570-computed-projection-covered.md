### Rust MongoDB server: a computed projection over index keys reads no documents

A `find` projection that computes a value (`{k: {$add: ["$x", 1]}}`, or a
field-path string `"$x"`) is answered from the index keys when every field it
reads is in the index, as on mongod 8.2.11 (`PROJECTION_DEFAULT <- IXSCAN`,
and over an `$or` of scans no FETCH either). One that reads a field the
index lacks, or `$$ROOT`, still fetches.

#### Fixed

- Fixes RM-570 (tests/backlog/test_rm_found_rm471.py::test_a_computed_projection_reads_index_keys); regression test tests/backlog/test_rm_found_rm471.py::test_a_computed_projection_from_index_keys_returns_mongods_rows.
