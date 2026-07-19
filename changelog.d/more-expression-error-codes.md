### More mongod error codes for `$zip` / `$arrayToObject` / `$replaceOne` / `$dateDiff`

A second batch of aggregation expressions that raised a generic `TypeMismatch`
(code 14) now return mongod 7.0.12's specific error code, so a `pymongo` client
sees the same `code`. This clears the named-operator error-code rows from the
divergence catalog's Tier 3. Python-server refinement (the Rust core already
defers each case, so the Rust server surfaced `BadValue` — unchanged).

#### Fixed

- `$zip` with a non-array `inputs` now raises `Location34461`, and with a
  non-array element inside `inputs` `Location34468`, instead of 14.
- `$arrayToObject` on a non-array input now raises `Location40386`, and
  `$objectToArray` on a non-document input `Location40390`, instead of 14.
- `$replaceOne` / `$replaceAll` with a non-string argument now raise mongod's
  per-argument code — `input` → 51746, `find` → 51745, `replacement` → 51744 —
  instead of a single generic 51745.
- `$dateDiff` with an unknown `unit` now raises code 9 ("unknown time unit
  value: …") instead of 14.
