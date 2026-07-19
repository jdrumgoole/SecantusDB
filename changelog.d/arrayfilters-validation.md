### `arrayFilters` validation

`arrayFilters` (the `$[<identifier>]` filter documents passed to an update) are
now validated the way real mongod validates them, instead of silently accepting
malformed input. A filter that isn't an object, is empty, carries an
identifier that isn't a lowercase-letter-led alphanumeric name, repeats an
identifier, or isn't actually referenced by any `$[<id>]` path in the update is
rejected with mongod's exact error code. Covered on both the Python and Rust
servers (the Rust core defers each invalid case), verified against real mongod
7.0.12.

#### Fixed

- A non-object array filter now raises code 14 ("BSON field
  'update.updates.arrayFilters.N' is the wrong type …, expected type 'object'").
- An empty array filter (`{}`) now raises code 9 ("Cannot use an expression
  without a top-level field name in arrayFilters").
- An identifier that isn't an alphanumeric string beginning with a lowercase
  letter (e.g. `1x`, `X`) now raises code 2 ("Error parsing array filter …").
- Two array filters with the same top-level identifier now raise code 9
  ("Found multiple array filters with the same top-level field name …").
- An array-filter identifier that no `$[<id>]` path in the update references now
  raises code 9 ("The array filter for identifier '<id>' was not used in the
  update …").

#### Notes

- An array filter whose only top-level keys are `$`-operators (e.g.
  `{$and: [{x: …}]}`) carries a *nested* identifier that SecantusDB doesn't
  extract yet; such a filter is left unvalidated rather than wrongly rejected
  (tracked in `tasks/backlog.md`).
