### Malformed aggregation pipeline elements now fail like mongod

A `pipeline` array element that isn't a document — `pipeline: [42]` — crashed the
Python server. `_apply_stage` called `len()` on the raw element, so a scalar
raised `TypeError: object of type 'int' has no len()` and the client got a bare
`internal server error` (code 1) with no indication of what was wrong. Real
mongod answers `14 TypeMismatch` with a specific message, and libmongoc's
`/change_stream/accepts_array` asserts on it verbatim.

Chasing that surfaced two further divergences in the same area. Our arity error
for a stage that *is* a document but isn't a single `{operator: spec}` pair used
the generic code 14 and our own wording, where mongod uses a dedicated
`Location40323`. Worse, the leading-`$match` optimisation matched on
`"$match" in stage` alone: a malformed two-key stage such as
`{"$match": {...}, "$count": "n"}` had its filter hoisted into the initial fetch
and the stage dropped, so the `$count` was silently discarded and the aggregate
returned wrong results instead of an error.

All the malformed-pipeline responses are now byte-identical to mongod, verified
by differential probe against 6.0.16 and 8.3.4 (which agree with each other).

#### Fixed

- `aggregate` with a non-document pipeline element (`42`, `"str"`, `[...]`,
  `null`, a bool, a double) returns mongod's `14 TypeMismatch` /
  *"Each element of the 'pipeline' array must be an object"* instead of crashing
  with an unhandled `TypeError` behind a generic `internal server error`.
- A stage document with the wrong number of fields — an empty `{}` or a
  multi-key stage — returns mongod's `40323 Location40323` /
  *"A pipeline stage specification object must contain exactly one field."*,
  replacing our own code-14 wording.
- The leading-`$match` initial-filter lift now requires a single-key stage, so a
  malformed multi-key stage is rejected rather than partially applied and
  dropped.
- The Rust server had the same gaps on its plain-`aggregate` path, where both
  malformed shapes were skipped by `continue` during validation, and had no
  arity check on its change-stream path (an empty stage was misreported as
  `40324` *"Unrecognized pipeline stage name: ''"*). Both now match mongod and
  share the message constants with the change-stream path, which already had the
  element-type error right.
