### An SBE sort's whole-document row keeps the fields read beside it

When a `$group` behind a slot-based sort reads both `$$ROOT` and named
fields, mongod's sort row holds the whole document and a slot for each
named field. The Rust MongoDB server now spills and counts the same row.

#### Fixed

- `[{$sort: {n: 1}}, {$group: {_id: "$k", f: {$first: "$$ROOT"}}}]` over a
  sort that spills reports mongod 8.2.11's `sortSpilledBytes` (956700 over
  900 records, where it reported 952200).

Fixes RM-615 (tests/backlog/test_rm_found_2026_11_13_rm506.py::test_a_whole_document_row_keeps_the_fields_read_beside_it)
