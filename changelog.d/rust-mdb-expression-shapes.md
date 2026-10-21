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
index-entry format moves from 4 to 5. A store with indexes written by an
earlier build is refused at open, as earlier format changes were; rebuild
it with a dump and restore.

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

- `secantus-storage`: index-entry format 5 (`ENTRY_FORMAT`).

#### Added

- `tools/probes/expression_shapes.py`: 3,360 structured expression cases
  against mongod, in `$addFields` and, with `--match`, inside
  `$match: {$expr}`.
- Backlog section 7.088: RM-236 to RM-253, each with a gap test.
