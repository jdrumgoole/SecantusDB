### Wrong-typed `createIndexes` and `collMod` options

The second half of the Python server's wrong-type sweep, measured the same way
against mongod 8.2.11: 97 code divergences down to 37, with every
`createIndexes.*` and `collMod.*` case now matching mongod byte-for-byte.

#### Fixed

- **`collMod` silently accepted a wrong-typed `validator`,
  `changeStreamPreAndPostImages` or `viewOn`** — a `viewOn: 5` reached the
  catalog. All three now answer `14 TypeMismatch` under mongod's IDL path, and
  all three still accept an explicit `null`.
- **Wrong-typed per-index options on `createIndexes` were ignored** —
  `collation`, `partialFilterExpression`, `expireAfterSeconds`, `unique` and
  `sparse`. These do not use the plain `BSON field '<path>'` form: mongod
  echoes the offending index spec and appends the reason after
  `:: caused by ::`, in three distinct shapes (14 for the object slots, 67 for
  the TTL one, 14 with a different wording for the bool ones). Each is now
  reproduced verbatim — including the two unbalanced quotes that are mongod's
  own, and the fact that `unique` / `sparse` accept a double.
- **`wildcardProjection` reported one error where mongod reports three.** All
  three arms answered `67 CannotCreateIndex`; mongod answers `14` for a wrong
  type, `9` for an empty object (with its own wording, "can't be an empty
  object"), and `2` for a non-wildcard base index. The spec in those messages
  was also rendered with Python's `repr` (`{'a': 1}`) instead of mongod's
  shell syntax, and omitted the index name.

#### Added

- `secantus.bsontypes.render_bson`, which renders a value the way mongod
  echoes it inside an error message — inner spaces on non-empty documents and
  arrays but not empty ones, unescaped strings, `new Date(<ms>)`,
  `BinData(0, ABCD)`, `UUID("...")` for subtype 4, `/pattern/flags`,
  `Timestamp(t, i)`, `MinKey` / `MaxKey`. Every rule probed, not inferred.
