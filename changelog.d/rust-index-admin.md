### Index management on the Rust MongoDB server follows mongod

`dropIndexes` given the key `{_id: 1}` dropped the `_id` index on the Rust
MongoDB server. `mongod` refuses, by name, by key or in a list, and so does
the Rust server now.

That came out of a probe of index management against `mongod` 8.2.11
(`tools/probes/index_admin.py`): the Rust server differed on 68 of 87
results and now differs on 9. Seven of the nine are `text` and `hashed`
indexes, which it does not build, and what follows from their absence; the
other two are below.

#### Fixed

- `dropIndexes` never drops the `_id` index. It accepts a list of names,
  checks every name before dropping any, reports a missing collection, and
  answers `non-_id indexes dropped for collection` for `"*"`.
- `createIndexes` checks every spec before it builds anything or creates the
  collection. Refused with `mongod`'s errors: a key value of 0 or a boolean,
  an empty or `$`-prefixed field name, more than 32 fields, an unknown
  option, an empty or `*` name, index version 3, `partialFilterExpression`
  with `sparse`, a negative, oversized or compound TTL, any option on the
  `_id` index, and an empty `indexes` list. All were accepted.
- `unique: 1` builds a unique index. A number was read as "not unique".
- The `createIndexes` reply has `mongod`'s fields: `commitQuorum`, the `_id`
  index counted for a collection the build creates, `index already exists`
  when some of the specs existed, and no `createdCollectionAutomatically`
  when all did.
- `collMod` with `index: {hidden: ...}` hides or shows the index and reports
  `hidden_old` / `hidden_new`. It answered `ok` and did nothing. Hiding the
  `_id` index is refused.
- `collMod` of a TTL reports both values as int64 and no old value for an
  index that had none, and names the missing index or option in its errors.
- `listIndexes` lists `_id_` first.
