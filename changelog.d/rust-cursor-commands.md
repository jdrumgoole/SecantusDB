### Rust MongoDB server: the cursor commands checked against mongod

We compared `find`'s cursor options, `getMore`, `killCursors`, tailable
cursors, `count` and `distinct` on the Rust MongoDB server with `mongod`
8.2.11: 85 scripts of commands. BEFORE differed. AFTER do now.

Five of the differences were wrong results. `find` with `singleBatch`
returned every matching document whatever `batchSize` said. A tailable cursor
applied its filter and projection to the first batch only, so every document
inserted later came back whole. `min` and `max` compared documents where
`mongod` compares index keys, and left out a document whose array had one
element inside the bounds, or which lacked the field. `distinct` kept one
value per BSON type, so 1, 1.0 and a Decimal128 1 were three values, and it
did not look through an array of documents. `count` ignored a negative
`limit`.

#### Fixed

- `find`: `singleBatch` returns one batch of `batchSize`; `batchSize: 0`
  opens a cursor even when nothing matches (so does an `aggregate`, over a
  missing collection too); a negative `batchSize`, an unknown field, a
  non-boolean cursor flag, `awaitData` without `tailable` and a malformed
  `$natural` hint are refused; an empty `hint` is no hint; `showRecordId`
  keeps the projection.
- `min` / `max` bound the hinted index's keys, and are refused with
  `mongod`'s codes when no index is hinted (291), when the index does not
  fit (51174) and when `min` is not below `max` (51175).
- `let`: a name must start with a lower-case letter, and a value may not
  read a field.
- Tailable cursors: the filter and projection apply to every batch; a
  tailable `find` on an empty capped collection or a missing one returns a
  closed cursor; a sort other than `{$natural: 1}` and `singleBatch` are
  refused; no `postBatchResumeToken` outside a change stream.
- `getMore`: the cursor id is an int64; a `getMore` for another namespace is
  13 and names both; `maxTimeMS` is refused on a cursor that is not
  `awaitData`; a negative `batchSize` and an unknown field are refused. None
  of these consumes the cursor.
- A cursor whose collection is dropped or renamed, or whose database is
  dropped, answers its next `getMore` 175 with `mongod`'s reason. It used to
  answer `CursorNotFound`, or go on returning documents after a
  `dropDatabase`.
- `killCursors`: ids are int64, a null is passed over, an unknown field is
  refused.
- `count`: a negative `limit` counts as its size; a negative `skip`, a
  fractional `limit` and an unknown field are refused.
- `distinct`: values are distinct by BSON equality and come back in BSON
  sort order; a dotted key reads through an array of documents; an unknown
  field and a key with a NUL are refused.
- `aggregate`, `listIndexes`, `listCollections`: an unknown field, an
  unknown `cursor` option and a negative `batchSize` are refused.
- A collection name that is not a string, or is empty, is 73 with
  `mongod`'s message on every command that takes one.
- The parser's errors 40414 and 40415 are named `IDLFailedToParse` and
  `IDLUnknownField` everywhere.
