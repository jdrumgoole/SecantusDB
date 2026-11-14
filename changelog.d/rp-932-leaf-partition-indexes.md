### A leaf partition's inherited indexes are built when it is created

A leaf partition created under a partitioned table now builds its copies of every ancestor index over its heap, as PostgreSQL's `DefineIndex` does, so `pg_class` shows each at 1 page and 0 tuples. Before, the copies of the user's own indexes had no page model at all and read as never built (0 pages, -1 tuples); only the primary key's copy was right.

#### Fixed

- The Rust PG server looked up a partition's inherited index definition in the partition's own index list; it now reads it from the ancestor that defines it.

Fixes RP-932 (tests/backlog/test_rp_found_rp921.py::test_a_leaf_partitions_inherited_indexes_are_built)
