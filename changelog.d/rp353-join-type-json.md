### Rust PG server: EXPLAIN's structured formats name a join's type and whether its inner side is unique

`EXPLAIN (FORMAT JSON)` (and YAML / XML) now gives every join node the
`Join Type` and `Inner Unique` PostgreSQL 15.19 prints, in its order, and
its `Node Type` is the bare `Hash Join` / `Nested Loop` / `Merge Join`
rather than the text format's `Hash Left Join`.

#### Fixed

- A join node's `Join Type` (`Inner`, `Left`, `Full`, `Right`, `Semi`,
  `Anti`) follows `Async Capable`, and `Inner Unique` follows the costs.
  `Inner Unique` is decided as `innerrel_is_unique` decides it for a base
  relation: true when one of the inner table's unique indexes -- immediate,
  without a WHERE, on plain columns -- has every key column fixed by the
  join's equalities or a constant. A DISTINCT or GROUP BY subquery on the
  inner side is not proven unique yet (RP-355).

Fixes RP-353 (tests/backlog/test_rp_explain_join_json.py::test_a_join_node_names_its_join_type)
