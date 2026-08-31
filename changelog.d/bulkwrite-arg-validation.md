### `bulkWrite` reports mongod's errors for a malformed command

A differential sweep of 47 `bulkWrite` shapes against mongod 8.2.11 found five
divergences, all of them error *shape* — the command's behaviour on well-formed
input already matched.

#### Fixed

- **A missing `nsInfo` was reported as a wrong type** (`2`) rather than a
  missing required field (`40414`), and an explicit `null` was not recognised
  as missing at all.
- **A non-array `nsInfo` reported a batch-size problem.** mongod validates
  `nsInfo` *before* the operation count, so `{ops: [], nsInfo: 5}` is a type
  error and not "Got 0 operations" — our check order had it the other way
  round.
- **`nsInfo` entries were unvalidated**: a non-document entry, an unknown field
  inside one, and a non-string `ns` now answer mongod's codes. The entry error
  carries its index (`bulkWrite.nsInfo.0`) while the field errors do not
  (`bulkWrite.nsInfo.x`) — mongod's own inconsistency, reproduced.
- **An invalid namespace** (`"nodot"`, `""`, `"."`) answered "invalid nsInfo
  index" instead of `73 Invalid namespace specified for bulkWrite: '<db>'`.
- **A negative namespace index** answered our own wording instead of
  `2 BSON field 'insert' value must be >= 0` — which names the bare op kind,
  not the IDL path — and a wrong-typed index is now a `14` rather than a
  generic index error. A double index (`0.0`) is valid and still accepted.
- **An unknown op kind** now names the offending key
  (`BSON field 'bulkWrite.frobnicate' is an unknown field.`).
- **`filter` is required on `update` and `delete`.** It defaulted to `{}`,
  silently turning a malformed operation into a match-all — the only one of
  these that could change which documents a write touched.
