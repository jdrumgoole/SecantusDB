### The wrong-type sweep reaches zero code divergences

The last of the Python server's wrong-typed-argument work. Across the 685-shape
corpus measured against mongod 8.2.11, **code divergences are now zero** (from
36 at the start of this slice), and message divergences are down to six.

#### Fixed

- **`create`'s capped options were four separate defects.** `size` or `max`
  present without `capped: true` was **silently accepted** where mongod answers
  72; our "the 'size' field is required" message carried a trailing clause
  mongod does not have; a negative `size` answered 72 where mongod answers
  `2 BSON field 'size' value must be >= 1` (a floor of one, not zero); and
  `max` has **no lower bound at all** — 0, -1 and 2.5 are all accepted — where
  we rejected anything at or below zero. Two tests pinned the old behaviour and
  were rewritten.
- **`dropIndexes` could not drop an index by key pattern.** Every non-string
  `index` was rejected as a type error, so `{index: {a: 1}}` never worked.
  mongod resolves the key pattern, and answers `27 can't find index with key:
  { z: 1 }` when nothing matches. Dropping the `_id` index is **72**, not the
  67 both arms returned.
- **An explicit `hint: null` was accepted** on all six commands that take a
  hint, where mongod answers `9 Hint must be a string or an object`. An absent
  hint remains fine — the two are now distinguished.
- **A null in a required field is "missing", not "wrong type"** —
  `getMore.collection` and `renameCollection.to` answer `40414`. An optional
  typed field is the other way round: `renameCollection.dropTarget: null` is a
  `14`.
- **An empty write batch was accepted** on `update` and `delete` (`updates: []`
  or `{}`), where mongod answers `16 Write batch sizes must be between 1 and
  100000. Got 0 operations.`
- **`bypassDocumentValidation` was unchecked** on `insert`, `update` and
  `findAndModify`, and `listIndexes.cursor.batchSize` accepted a boolean
  through a bare `int()`.
- **Message families corrected** to mongod's own text: `insert.writeConcern`
  (was "writeConcern must be a document"), `distinct` (the IDL struct name,
  `distinctCommandRequest.query`), `aggregate.pipeline` ("A pipeline must be an
  array of objects"), `renameCollection.to`, `dropIndexes.index` (whose
  expected-type list differs for an array), and `$facet`, which rendered its
  spec with Python's `repr`.

#### Known gap

The message for a `hint` naming an index that does not exist is mongod's
multi-line **planner dump**, which renders its parsed match-expression tree
(`{a: 1}` becomes `Tree: a $eq 1`). Reproducing it in general means porting
mongod's `MatchExpression::debugString`; the error code already matches, and a
partial renderer would be wrong on anything nested. Recorded rather than
approximated.
