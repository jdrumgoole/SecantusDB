### Wrong-typed command arguments are parse errors, not crashes

Twice in one session a caller-supplied scalar reached code that assumed a
document and crashed as `internal server error`: `pipeline: [42]` called `len()`
on an int, `update: 5` called `.keys()` on one. Sweeping that shape across every
document-valued command argument showed the pattern was systemic — **45 of 56
probed argument slots crashed**, where mongod returns a parse error.

A crash is worse than a wrong answer here: the client learns nothing about what
it got wrong, and a bare `internal server error` is indistinguishable from a
genuine server fault.

#### Fixed

Wrong-typed arguments now return mongod's parse errors, matching its per-command
message families rather than one blanket check:

- `find` — `filter` / `sort` / `projection` return
  `Expected field filterto be of type object` (mongod's own missing space,
  reproduced deliberately).
- `count.query`, `distinct.query`, `delete.deletes.q`, `update.updates.q`,
  `findAndModify.query` / `.sort` / `.fields` return
  `BSON field '<path>' is the wrong type '<type>', expected type 'object'`.
- `aggregate.pipeline` returns `'pipeline' option must be specified as an array`.
- `update`'s `u` accepts an object *or* an array: a scalar is `9 FailedToParse`
  (`Update argument must be either an object or an array`), while an array of
  non-documents gets the pipeline-element error (14), since an array `u` is a
  pipeline.

Verified 56/56 against mongod 6.0.16 — the version the live differential gate
spawns — and cross-checked identical on 8.3.4.
