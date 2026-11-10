### A `$`-prefixed document in `$in` is refused wherever the filter holds it

mongod refuses a `$in` or `$nin` element that is a `$`-prefixed document,
such as `{_id: {$in: [{$x: 1}]}}`, with "cannot nest $ under $in" while it
parses the filter, before it reads a document. The Rust MongoDB server
refused it only when its matcher evaluated that clause, so an aggregate or
`find` that read one `_id` from the index, an `$or` branch it never reached,
an `$elemMatch` over a missing field, or an update, delete, count or
`distinct` that matched nothing answered instead. The filter's structure is
now checked as it parses, on every command that takes one.

#### Fixed

- `secantus-core` `query::nested_in_error`, called from
  `argtypes::unknown_operator_in_filter`: the structural check, through
  `$and` / `$or` / `$nor`, `$not` and both forms of `$elemMatch`. A DBRef
  (`{$ref, $id}`) stays a value, as on mongod.
- Fixes RM-420 (tests/backlog/test_rm_found_2026_11_08_rm330.py::test_an_id_match_refuses_a_dollar_object_in_in);
  pinned across commands by `tests/mongod_live/test_nested_in_parse.py`.
