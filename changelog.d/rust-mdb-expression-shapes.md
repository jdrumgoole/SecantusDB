### Rust MongoDB server: BinData, JavaScript, MinKey and document comparisons match mongod

A line-coverage run showed about 2,000 lines of the expression engine that
no test had ever executed. Probing them against mongod 8.2.11 with 3,360
hand-built cases found 22 divergences, now filed with gap tests. Four of
them were in the comparison code every query shares, and are fixed here:
BinData now orders by length, then subtype, then bytes; JavaScript with a
scope is a type of its own and compares its scope; MinKey and MaxKey equal
themselves; and two documents are equal only with their fields in the same
order.

The BinData and JavaScript order is also the order of index keys, so the
index-entry format moves from 4 to 5. A format-4 index is rebuilt when its
store opens; formats 1-3 are still refused.

PostgreSQL orders `bytea`, which the PG server stores as BinData, byte by
byte instead, so the order is now a property of the server: each thread the
PG server runs statements on uses PostgreSQL's order, every index records
the order it was built in and is keyed by it whichever server writes, and a
server reads only through the indexes in its own order.

#### Fixed

- `secantus-core` `order.rs` / `sortkey.rs` / `query.rs`: BinData compares
  by length, subtype, then bytes, in `$sort`, indexes, `find` ranges,
  `$group` `$max` / `$min` and expressions. Fixes RM-232
  (tests/backlog/test_rm_found_2026_10_21b.py::test_bindata_orders_by_length_then_subtype).
- JavaScript-with-scope ranks after JavaScript and compares by code, then
  scope. Fixes RM-233
  (tests/backlog/test_rm_found_2026_10_21b.py::test_code_with_scope_is_its_own_type_and_scope_counts).
- `expressions.rs` `py_eq`: `{$eq: [MinKey, MinKey]}` is true. Fixes RM-234
  (tests/backlog/test_rm_found_2026_10_21b.py::test_minkey_and_maxkey_equal_themselves).
- `py_eq`: document equality respects field order. Fixes RM-235
  (tests/backlog/test_rm_found_2026_10_21b.py::test_document_equality_respects_field_order).

#### Changed

- `secantus-storage`: index-entry format 5 (`ENTRY_FORMAT`); a format-4
  index is rebuilt at open (`OLDEST_REBUILDABLE_ENTRY_FORMAT`). An index
  records PostgreSQL's BinData order as the internal `binDataOrder` option,
  which `listIndexes` and change events leave out. `_id` keys and `_id`
  claims keep format 4's encoding (`order::BinDataOrder::Format4`), so no
  stored `_id` moves.
- `secantus-core` `order::BinDataOrder`: a per-thread BinData order; the PG
  server (`secantus-pg` `server::mark_bytea_order`) sets PostgreSQL's on its
  runtime, connection and portal-reader threads, and opens its store in it.
  New test: `crates/secantus-pg/tests/bytea_order.rs`.

#### Added

- `tools/probes/expression_shapes.py`: 3,360 structured expression cases
  against mongod, in `$addFields` and, with `--match`, inside
  `$match: {$expr}`.
- Backlog section 7.088: RM-236 to RM-253, each with a gap test.
