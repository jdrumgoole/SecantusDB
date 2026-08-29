### Numeric and cursor command arguments no longer crash the server

Extending the wrong-type sweep past document-valued arguments found 24 more
slots that crashed with `internal server error` instead of returning a parse
error: `find`'s `limit` / `skip` / `batchSize`, `aggregate`'s `cursor` and
`cursor.batchSize`, `listIndexes.cursor`, `createIndexes.indexes`, and a `$match`
stage whose spec isn't a document.

**mongod's strictness here is per-slot, not per-class.** `find.limit: {}` is a
type error, while `delete.deletes.limit: {}` is *accepted* and means "no limit" —
so a blanket "validate every numeric argument" rule would have fixed one and
broken the other. Every slot was probed individually.

#### Fixed

- `find.limit` / `.skip` / `.batchSize` and `cursor.batchSize` return mongod's
  `BSON field '<path>' is the wrong type '<t>', expected types
  '[long, int, decimal, double']`. `find`'s are reported under mongod's internal
  IDL name — `FindCommandRequest.limit`, not `find.limit`. Booleans are rejected
  explicitly, since Python otherwise reads `true` as `1`.
- `aggregate`'s `cursor` uses its own wording, `cursor field must be missing or
  an object`; `listIndexes.cursor` and `createIndexes.indexes` use the BSON-field
  form with `'object'` and `'array'` respectively.
- A `$match` stage whose spec isn't a document returns `15959` /
  `the match filter must be an expression in an object`.
- `delete`'s `limit` is deliberately *not* type-checked: anything that isn't a
  numeric `1` means "no limit", matching mongod — including `true`, which mongod
  does not treat as `1`. We used to call `int()` on it and crash.

Verified against mongod 6.0.16 and 8.3.4: zero crashes on both, and the same
behaviour on both.
