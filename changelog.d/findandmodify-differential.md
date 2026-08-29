### A second differential pass over `findAndModify` — 14 of 49 option combinations diverged from mongod

Phase 2 of `tasks/remaining-work-plan.md` asks for the differential harness to be
pointed at surfaces it has never covered. `findAndModify` was one of them: 49
option combinations run against a live mongod 6.0.16, comparing the **raw
command reply** rather than the driver wrapper, so `lastErrorObject`'s shape and
the field order of an upserted document were compared too.

Fourteen diverged. Two were silent wrong data, and both turned out to be shared
with the plain `update` command:

#### Fixed

- **An empty update document silently kept every field.** `update: {}` is a
  *replacement with an empty document* — mongod reduces the stored document to
  its `_id` and reports `nModified: 1`. Both write commands short-circuited on a
  falsy update and returned the document untouched, with `ok: 1` and no error,
  so every field the caller asked to drop stayed. An empty **pipeline** (`[]`)
  is the genuine no-op and remains one.
- **An upsert from a dotted query stored a literal dotted key.** `{"sub.k": 77}`
  upserted a document with a key that has a dot *in* it — one mongod cannot
  produce, most drivers refuse to send, and which then never matched the very
  query that created it. It now builds the nesting mongod builds, at any depth,
  merging with any dotted paths the update itself sets.
- **An update that would create a field under a non-document did nothing at
  all.** `$set: {"n.x": 1}` against `{n: 5}` reported success and wrote no
  change. mongod answers `PathNotViable` (28) with `Cannot create field 'x' in
  element {n: 5}`. Creation only: `$unset` down the same path is still a no-op,
  and an out-of-range array index still pads with nulls.
- **`findAndModify` reported every update failure as `14 TypeMismatch`.** The
  errors escaped to the generic dispatch handler, so an unknown modifier
  (mongod: 9), a changed `_id` (66), a path conflict (40) and a non-viable path
  (28) all arrived under one wrong code, and the drivers' canonical handling
  keyed on those codes never fired. `findAndModify` also — uniquely on 6.0.16 —
  wraps its *execution* errors in `Plan executor error during findAndModify ::
  caused by ::` while leaving parse errors bare; both halves now match.
- **`codeName` no longer contradicts `code`.** Any user-facing exception that
  named its own code was reported with that code and `codeName: "TypeMismatch"`
  — a pair mongod never sends. The name now follows the code.
- **`findAndModify.new` was never type-checked.** A string went through Python's
  truthiness, so `new: "no"` returned the *post*-image. It now takes the same
  bool-or-number rule as `upsert` and `remove` (numbers and `null` accepted,
  arrays / documents / strings rejected), and a zero-valued `Decimal128` is
  false rather than truthy.
- **An unknown top-level `findAndModify` field is rejected** with
  `Location40415`, as mongod does. A misspelled option — `field` for `fields`,
  `returnNew` for `new` — was silently dropped, and the caller got a
  correct-looking reply computed under options they had not asked for.
- **`findAndModify.hint` was accepted and ignored**, so hinting an index that
  does not exist got a silent collection scan and an `ok: 1` reply. It is now
  honoured, an unresolvable hint is `BadValue` (2), a non-string / non-object
  one is `FailedToParse` (9), and `$natural` — which mongod does not accept on
  this command, unlike `find` — is refused.
- **`arrayFilters` type errors named a field path that does not exist** on this
  command (`update.updates.arrayFilters.0`), with the wrong type. They now name
  `findAndModify.arrayFilters`, and an explicit `null` takes mongod's older
  `Location10065`.
- **An update path referencing an undeclared array filter** answered
  `9 FailedToParse` with hand-written wording; mongod answers `2 BadValue` with
  `No array filter found for identifier 'e' in path 'arr.$[e]'`, naming the
  path.
- **The "unknown modifier" message is mongod's**, and there is one of it.
  Unknown `$`-operators and documents mixing operators with replacement fields
  are the same complaint to mongod, which names the offending key —
  `Unknown modifier: z` for a bare field. We had two different sentences, and
  neither is one any real server emits.
- **Field order matches on the wire.** An upserted document leads with `_id`,
  then the query-seeded fields, then the update's, each group in field-name
  order; the `update` reply puts `upserted` / `writeErrors` before `nModified`.
  BSON keeps field order, and drivers do compare raw reply bytes.

The Rust side carries the same fixes: the core engine's empty-update
short-circuit is gone, its "unknown modifier" wording matches, and a non-viable
path now defers with a named `PathNotViable` validator wired to code 28 for the
standalone server, mirroring the `arith_type_error` template.

`tests/test_mongod_differential.py` grew 36 cases (57 → 93) so none of this can
drift back.
