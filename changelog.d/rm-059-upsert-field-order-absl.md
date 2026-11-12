### Rust MongoDB server: an upsert seeds its query's fields in mongod's absl hash order

An upsert whose query names several equalities now stores those fields in
the order mongod 8.2 can give them, not sorted. mongod iterates an absl
`flat_hash_map` to build the document, and absl seeds that order per process
and per table, so the server now ports absl's hash and table instead of
fixing an order.

#### Fixed

- `{a: 1, b: 2, c: 3, d: 4}` upserted `a, b, c, d`, an order mongod 8.2.11
  never gives for four fields. mongod sets the equalities in the order its
  `EqualityMatches` (a `StringDataMap`, an `absl::flat_hash_map`) iterates
  them, having inserted them in the canonical query's order, sorted by
  path. An explicit `$and` is flattened and sorted with the rest, so
  `{$and: [{d: 1}, {c: 2}, {b: 3}, {a: 4}]}` gives the same orders
  (measured on three mongod 8.2.11 processes, 2026-11-12).
- New `secantus_core::absl_hash`, a port of abseil `20250512.1` (the fork
  mongod 8.2 vendors). It covers `absl::Hash<string_view>` and the hash
  seed taken from a static's address, which ASLR moves per process. It
  covers the per-table seed from a thread-local that starts at its own
  address and steps by `0xad53`. And it covers the swiss table's control
  bytes, probing, growth and single-group shuffle, at the group width of
  the Linux x86-64 exemplar (16). Four fields come out in one of the four
  orders absl can give, one complementary pair of them per process, as
  mongod's do.
- `order_upserted_doc` kept `_id` first and sorted the update's new fields,
  which is still right. It also sorted the query-seeded fields; it now
  keeps the seed's order.
- The port is checked against the real library: string hashes for every
  length class, and the iteration order of 42 tables of 1 to 600 keys,
  recorded by `tools/probes/absl_order/harness.cc` for x86-64 (SSE2, width
  16) and arm64 (NEON, width 8).

Fixes RM-059 (tests/backlog/test_rm_057_084.py::test_upsert_field_order_from_several_equalities;
crates/secantus-core/tests/absl_hash_order.rs::port_matches_absl_on_x86_64_sse2)
