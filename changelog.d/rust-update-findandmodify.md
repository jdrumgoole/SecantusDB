### Rust MongoDB server: `update` and `findAndModify` checked against mongod

An update of a document on the Rust MongoDB server could rewrite a field it
did not name. A stored sub-document shaped like extended JSON, such as
`{$numberLong: "1"}` or `{$oid: "..."}`, came back from the server's BSON
decoder as the value it would stand for, so writing the document back
replaced the sub-document with a number or an ObjectId. Every decode now
keeps it as the document it is.

It was found by a new probe, `tools/probes/update_commands.py`, which sends
1,295 `update` and `findAndModify` commands to the server and to `mongod`
8.2.11 and compares each reply and the documents left behind. The server
differed on 213 of the first 1,096 and differs on 17.

#### Fixed

- A stored sub-document shaped like extended JSON is no longer rewritten
  when its document is updated or read through `aggregate`.
- An update adds new fields in path order across its operators, as `mongod`
  does, not in the order written.
- The positional operator `$` names the one array position `mongod` names
  when a query has more than one array predicate, applies to any array in
  the update, and is refused on an upsert.
- A pipeline update can no longer change the `_id`. An operator that writes
  the `_id` without changing it is accepted.
- `{$set: {$x: 1}}`, and the same in a pipeline or in `aggregate`, no
  longer stores a top-level `$x` field.
- `$bit` keeps a long a long. `$addToSet`, `$pull`, `$pullAll` and `$push`
  with `$sort` store what `mongod` stores for seven shapes they got wrong.
- A pipeline `$set` or `$addFields` of a dotted path follows the
  aggregation rules: through a scalar, through an array, and for a nested
  document.
- An upsert whose query names both `a` and `a.b`, or uses `$expr`, is
  refused.
- An update with no `upsert` no longer creates the collection it names.
- `Timestamp(0, 0)` at the top level of an inserted or replacement
  document is filled with the current time.
- A document may not nest deeper than 180 levels.
- `findAndModify` reads `upsert: 1` as true, refuses `arrayFilters` with
  `remove` or a pipeline, and checks a pipeline and a projection when
  nothing matches.
- `update` and `findAndModify` refuse unknown fields, and `update` checks
  the type of `updates`, of each statement and of `upsert`.
- Error messages for malformed operator arguments are `mongod`'s, where
  the server had answered that it does not support the construct.

#### Added

- The expression operator `$const`.
