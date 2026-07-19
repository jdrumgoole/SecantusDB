### `arrayFilters` nested-identifier extraction

`arrayFilters` identifiers are now extracted recursively through `$and` / `$or`
/ `$nor`, completing the arrayFilters validation. A filter like
`{$and: [{"x.a": {…}}, {"x.b": {…}}]}` correctly resolves the single identifier
`x` (so a `$[x]` update path applies to the matching elements), and mongod's
"exactly one identifier per filter" rule is now enforced: a filter carrying two
distinct identifiers — top-level or nested — is rejected, as is a bare `$expr`.
Both the Python and Rust servers behave identically, verified against real
mongod 7.0.12.

#### Fixed

- A single arrayFilter identifier nested inside `$and`/`$or`/`$nor` (e.g.
  `{$and: [{"x.score": {$lt: 50}}]}` for `$[x]`) is now extracted and applied,
  instead of failing with "arrayFilters has no entry for identifier x".
- An arrayFilter referencing two or more distinct identifiers now raises code 9
  ("Expected a single top-level field name, found 'x' and 'y'") — previously a
  second top-level identifier was silently ignored.
- An arrayFilter that is a bare `$expr` (no field identifier) now raises code 224
  ("$expr is not allowed in this context").
