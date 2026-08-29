### `$graphLookup` stopped following a chain at the first null link

Phase 2 of `tasks/remaining-work-plan.md`, fourth surface: 27 `$lookup` /
`$graphLookup` shapes against a live mongod 6.0.16. **20 diverged.**

The worst was a **short answer with no error**. `$graphLookup` treated a null
`connectFromField` as "no further links", so a four-document chain came back
with one document in it. Nothing failed; the traversal simply stopped early,
which is the hardest kind of wrong to notice.

#### Fixed

- **A null link no longer ends the traversal.** mongod follows it to documents
  whose `connectToField` is explicitly null; only a *missing* link stops the
  walk. Both halves were wrong in the same place, because the value was tested
  for `None` — which conflated missing with null.
- **A null link no longer reaches documents that lack the field.** Missing and
  null are different values here; comparing `get_path`'s `None` for both made
  every field-less document reachable from a null.
- **An empty-array `localField` matches null.** mongod unwinds the local array
  for matching and an empty one still joins against the null-valued foreign
  rows; we produced no lookup keys at all and matched nothing. **Both** join
  paths had it — the hash join and the index-driven one — and the index path
  carried a comment asserting mongod's `$in: []` semantics that the oracle
  contradicts. A `$lookup`'s `localField` is not an `$in`.
- **`as` is a path, not a key.** `as: "a.b"` now produces `{a: {b: [...]}}`
  instead of a literal key with a dot in it — the same bug, and the same fix,
  as the dotted-equality upsert seed fixed earlier in this campaign. Wherever a
  user-supplied path is used as a key, it has to go through `set_path`.
- **Two crashes.** `$lookup` with `let: 5` reached `.items()` and with
  `pipeline: 5` was iterated; both raised bare exceptions that escaped as
  `internal server error` (code 1).
- **Argument errors name the argument.** mongod answers `FailedToParse` (9)
  with a per-argument message; we answered `TypeMismatch` (14) with one of two
  generic sentences that named neither the field nor the problem. Which message
  applies to a half-specified field pair depends on whether a `pipeline` is
  present — probed both ways.
- **Unknown arguments are rejected** on both stages (`$lookup` → 9,
  `$graphLookup` → `Location40104`). They were accepted and ignored, so a
  misspelled `foreignFeild` silently became a join over the whole foreign
  collection.
- **A negative `maxDepth` is rejected** (`Location40101`). It was accepted and
  matched nothing, so every document got an empty array — which reads as "no
  connections" rather than "bad option".

#### Correction

`$graphLookup` was first reported here as "does not recurse at all". It does.
The original fixture happened to put a null link on the very first hop, so one
narrow bug looked like a missing feature; a chain with no nulls showed the
traversal, the `maxDepth` cut-off and an array `startWith` all working. The fix
is a guard, not an implementation.
