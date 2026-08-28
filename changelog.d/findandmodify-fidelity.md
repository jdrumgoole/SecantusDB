### `findAndModify` argument validation and reply shape match mongod

Differential-probing 18 `findAndModify` shapes against a real mongod found six
divergences: a crash, two commands mongod rejects that we silently performed, and
a reply that described a delete as an update.

`findAndModify` with a non-document `update` — `{update: 5}` — reached the update
engine, which called `.keys()` on it and raised `AttributeError`. That surfaced as
a bare `internal server error` (code 1) rather than mongod's parse error.

`remove: true` combined with `new: true` or with `upsert: true` was accepted and
the document removed. mongod rejects both: a remove has no "after" document to
return, and upserting while removing is contradictory.

`lastErrorObject` for a remove carried `updatedExisting`, which describes an
update. mongod omits it, so a driver reading that field saw an update-shaped
reply for a delete.

#### Fixed

- A non-document, non-array `update` returns `9 FailedToParse`
  (`Update argument must be either an object or an array`) instead of crashing.
- `remove` + `new` and `remove` + `upsert` are rejected with `9 FailedToParse`,
  and the document is left in place.
- A remove's `lastErrorObject` is `{n: 1}` (or `{n: 0}` when nothing matched).
  Update and upsert replies keep `updatedExisting` and `upserted` as before.
- `Cannot specify both an update and remove=true` gains the article mongod uses.

Probed on mongod 6.0.16 — the version the live differential gate spawns — and
cross-checked on 8.3.4. Every behaviour above is identical on both; only the
error wording differs (8.3 quotes the field names), and SecantusDB advertises 7.0,
so 6.0's wording ships.
