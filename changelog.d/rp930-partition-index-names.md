### A partition's copy of an index is named by its columns

When a partition is attached to a partitioned table, each of the parent's
indexes is copied onto it. PostgreSQL names that copy afresh, from the
partition's name and the index's columns (`p_1_a_b_idx`), never from the
name the user gave the parent's index. The Rust PostgreSQL server took the
parent index's name and swapped the table prefix, so a user-named `p_ab`
appeared on the partition as `p_1_ab`; it now names the copy as
PostgreSQL's `ChooseIndexName` does.

#### Fixed

- Rust PG server: a partition's copy of a parent index is named
  `<partition>_<columns>_idx` (`_key` for a unique constraint, `_excl` for
  an exclusion constraint, `_pkey` for the primary key).
  Fixes RP-930 (tests/backlog/test_rp_found_rp921.py::test_a_partitions_copy_of_a_named_index_is_named_by_its_columns)
