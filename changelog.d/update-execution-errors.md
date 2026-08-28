### Update errors that depend on the stored document match mongod's

Errors an update raises against a particular stored document — `$inc` on a
non-numeric field, an array operator on a non-array field — used our own codes
and wording for `$push` and `$addToSet`. Worst of the set: **`$pop` on a
non-array was a silent no-op**, reporting `n: 1` with no write error for an
update mongod refuses.

Found by differential-probing the update operator family against a real mongod.

One note on message shape: mongod 8.3 wraps these in `Plan executor error during
update :: caused by :: ` and 6.0 does not, while the codes and bodies are
identical either way. SecantusDB advertises 7.0, and the repo's live differential
gate runs whatever mongod is on PATH, so the bare body is what ships. The
classification is kept in the code as a single switch point.

#### Fixed

- `$push` on a non-array returns code 2 with mongod's message
  (`The field 'a' must be an array but is of type int in document {_id: 1}`),
  replacing our code 9 and our own wording. `$addToSet` likewise.
- `$pop` on a *present* non-array now errors with mongod's code 14 and message
  instead of silently succeeding. A missing field or an empty array remain
  no-ops, as on mongod.
- The Rust engine had the same `$pop` gap — a non-array fell through its
  `if let Some(Bson::Array(..))` — and now defers so the exact error is raised.
- `$inc` / `$mul` / `$pull` were already correct on codes and bodies and are
  unchanged.
