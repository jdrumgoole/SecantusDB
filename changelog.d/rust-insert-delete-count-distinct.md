### Rust MongoDB server: `insert`, `delete`, `count` and `distinct` checked against mongod

The Rust MongoDB server stored two documents whose `_id` mongod holds to be
one: after `{_id: {a: 1}}` it accepted `{_id: {a: 1.0}}`. It also lost a
document when a `$merge` with `whenMatched: "replace"` was refused by a
unique index, because it deleted the old document before inserting the new
one. Both are fixed. They were found by sending 905 `insert`, `delete`,
`count` and `distinct` commands to the server and to mongod 8.2.11 and
comparing each reply and the documents left behind; 162 replies differed
and 32 still do (`tasks/backlog.md` 7.078).

No stored key changes. A document `_id` takes a second claim, by value,
beside its row in the `_id` index. A document stored by an earlier version
holds no claim, so a duplicate of it by value is still accepted.

#### Fixed

- A document `_id` is unique by value: `{a: 1}`, `{a: 1.0}` and
  `{a: NumberLong(1)}` are one `_id`.
- `$merge` with `whenMatched: "replace"` replaces the document in place. A
  replacement that fails leaves the document as it was.
- A delete statement with no `limit` is refused. It removed every match.
- `delete` checks every statement before it runs any.
- A `limit: 1` delete removes the first document its `hint` reads.
- An `$in` over an index returns documents in the index's order, for `find`
  and for `delete`.
- `insert` refuses a `$`-prefixed field at any depth of an `_id` document,
  a document nested past 180 levels, and a batch holding an element that
  is not a document; it drops a null element.
- A duplicate-key message prints dates, documents, binary and timestamps
  as mongod does. They were printed as Rust debug text.
- A document that repeats both an `_id` and a unique index key is reported
  for the `_id`.
- `count` reads `limit` and `skip` as mongod does: a decimal `skip` is
  rounded, a `limit` that is not a whole number is refused.
- `insert`, `delete`, `count` and `distinct` refuse fields they do not
  take, and `count` and `distinct` refuse a `writeConcern`, as do 38 other
  commands measured.
- A `readConcern` that is not a document is refused.
- A write concern's `j` and `fsync` take any number, are refused together,
  and `wtimeout` is accepted whatever its type.
- An `apiVersion` other than "1" is refused in mongod's words.
- `$expr` with an unknown operator answers 168, and `$text` with no text
  index answers 27, on `delete`, `count` and `distinct`.
