### A duplicate document `_id` under a collation reports its sort keys

When a collection has a default collation, mongod treats two `_id` values as
the same key if they compare equal under it. A second insert is then refused
with a duplicate-key error. If the `_id` is a document, mongod does not show
the strings in that error as the client wrote them. It shows each one as its
raw collation sort key, both in `keyValue` and in the message. For
`{_id: {a: "x"}}` under `en`, strength 2, that is `dup key: { _id: { a:
"W\x01\x05" } }`, with no `hexEncoded` field. The Rust MongoDB server now
does the same for strings at any depth in a document or array, including keys
whose bytes are not UTF-8. The bytes themselves are still ICU4X's rather than
ICU 57's until RM-091 links the ICU mongod uses.

#### Fixed

- Rust MongoDB server: a duplicate under a collated index whose key value is
  a document or array reports each string inside it as its raw collation
  sort key in `keyValue` and `errmsg`, as mongod 8.2.11 does, instead of the
  string as written. The change is in `secantus-storage`, which the Rust
  PostgreSQL server also links. That server does not build this error.
- RM-092's remaining clause was about the Python server not enforcing
  collated `_id` uniqueness. It is retired rather than worked, under the
  2026-10-16 decision that compatibility with the deprecated Python servers
  is no longer a constraint.

Fixes RM-092 (tests/backlog/test_rm_085_112.py::test_collated_duplicate_document_id_shows_sort_key;
crates/secantus-storage-adapter/tests/collated_id_wt.rs::a_document_id_duplicate_reports_its_strings_as_sort_keys)
