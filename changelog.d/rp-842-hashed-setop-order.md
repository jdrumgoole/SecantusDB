### Rust PG server: a hashed INTERSECT or EXCEPT returns its groups in PostgreSQL's order

When PostgreSQL 15.19 plans an INTERSECT or EXCEPT as a HashSetOp, it
returns the groups in its hash table's order, with all the copies of a group
next to each other. The Rust PostgreSQL server returned them in the order
the first input gave them, and under ALL put each copy where its row had
been.

#### Fixed

- A hashed INTERSECT, INTERSECT ALL, EXCEPT or EXCEPT ALL now builds the
  same hash table as PostgreSQL's `setop_fill_hash_table`, using the
  simplehash model the HashAggregate order already uses. The table starts at
  the plan's group estimate, capped by hash memory. Only the first input's
  rows insert into it, so a repeated row can grow the table. The groups are
  read back in table order, each output as often as `set_output_count`
  says. A probe of 301 hashed set operations over VALUES lists (integer,
  text, numeric and two-column keys, with and without ALL) matched
  PostgreSQL 15.19 on every row. Fixes RP-842
  (tests/backlog/test_rp_found_rp692.py::test_a_hashed_set_operation_returns_its_groups_in_hash_order,
  tests/backlog/test_rp_found_rp692.py::test_a_hashed_set_operation_outputs_each_group_together_in_hash_order).
