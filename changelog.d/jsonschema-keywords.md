### $jsonSchema grows mongod's full keyword surface — and rejects what mongod rejects

The `$jsonSchema` query operator now covers every keyword real mongod accepts,
on both servers, with semantics pinned by a live probe against mongod 7.0:
`multipleOf` (fmod semantics, fractional divisors included), tuple-form
`items` with `additionalItems` (false / schema / absent), and the `title` /
`description` metadata keywords (accepted and ignored, with mongod's string
type check). Exclusive bounds move to the draft-4 semantics mongod actually
implements — `exclusiveMinimum` / `exclusiveMaximum` are booleans that
sharpen `minimum` / `maximum` to a strict bound, and the draft-6 numeric form
is rejected at parse time (the previous numeric treatment was a silent
divergence).

Just as important is what gets rejected: schema keywords are now validated at
parse time, recursively through every sub-schema, with mongod's verbatim
codes and messages — an unknown keyword or a known-but-unsupported one
(`$ref`, `$schema`, `default`, `definitions`, `format`, `id`) is
`9 FailedToParse`, and a type violation (non-number `multipleOf`, non-boolean
exclusive bound, non-string metadata, non-object schema) is
`14 TypeMismatch` — before a single document is scanned, even on an empty
collection. Previously both servers silently ignored anything they didn't
recognise, so a typo'd keyword matched everything.

#### Added

- `$jsonSchema` keywords `multipleOf`, tuple-form `items` +
  `additionalItems`, and `title` / `description`, on both servers, with
  curated parity coverage.
- Parse-time recursive keyword validation on both servers
  (`query._check_json_schema_keywords` / `secantus_core::query::
  json_schema_keyword_error`), with mongod's verbatim errors.
- `QueryError` carries a `code` / `code_name` (default `2 BadValue`), so
  parse-time errors with documented distinct codes surface faithfully
  through find, update, and delete write-error paths.

#### Changed

- `$jsonSchema` exclusive bounds follow draft-4 (boolean) semantics, matching
  mongod; the draft-6 numeric form now errors instead of silently applying.

#### Fixed

- A `$jsonSchema` with a mistyped or unsupported keyword no longer silently
  matches every document — it errors exactly as mongod does.
