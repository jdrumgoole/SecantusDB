### A partition's CHECK violation names the partition

A row written through a partitioned table that breaks one of its CHECK or
NOT NULL constraints is now reported against the leaf partition the row was
routed to, as PostgreSQL's `ExecConstraints` reports it: `new row for
relation "p974b" violates check constraint "p974_x_check"`. The constraints
are checked after routing, so a row that fits no partition gets the "no
partition" error first, as it does on PostgreSQL.

#### Fixed

- Rust PG server: a CHECK or NOT NULL violated by a row routed into a
  partition names the leaf partition, not the partitioned table.
  Fixes RP-975 (tests/backlog/test_rp_found_rp816.py::test_a_partitions_check_violation_names_the_leaf)
