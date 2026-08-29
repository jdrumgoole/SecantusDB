### `delete` and `update` performed the write when the hint named no index

Phase 2 of `tasks/remaining-work-plan.md`, third surface: index and query
planning, probed against a live mongod 6.0.16. The headline is not a missing
error message — it is **an operation MongoDB declines to run being executed**.

#### Fixed

- **`delete` and `update` ignored their per-statement `hint` entirely.** mongod
  refuses the statement when a hint names no index — `n: 0` plus a writeError —
  rather than falling back to a collection scan. Both commands dropped the
  field and performed the write, so a caller who hinted a typo'd index name had
  their delete applied. Now a per-statement writeError (code 2), with the batch
  stopping or continuing per `ordered`, exactly like the other per-statement
  errors on those commands.
- **`explain` did not validate a hint** — and `explain` is what you run to
  *check* one. `find`, `count` and `aggregate` all rejected an unresolvable
  hint already; `explain` alone reported a `COLLSCAN`, which tells the caller
  their hint is fine and that it is being ignored, in the same breath.
- **`explain` fabricated plans for commands that do not exist.**
  `{explain: {nosuchcmd: "c"}}` answered a plausible `COLLSCAN` with `ok: 1`;
  so did `{explain: {}}` and a non-document `explain` argument, the latter
  inventing a namespace (`<db>.$cmd`) to report it against. They now answer
  mongod's `CommandNotFound` (59) and `TypeMismatch` (14). Confidently wrong is
  worse than absent here: a client explaining a mistyped command got an answer
  about a query that could never run.
- **`{$natural: -1}` did not reverse the scan.** Both directions resolved to
  the same `"$natural"` token, dropping the sign, so a caller asking for
  reverse insertion order silently got forward order.
- **`distinct` accepted any field and ignored it**, so a misspelled option was
  silently dropped. Unknown fields now answer `Location40415`.
- **`explain.verbosity` conflated two errors.** mongod separates a wrong *type*
  (`TypeMismatch`, 14) from an invalid *enum value* (`BadValue`, 2, with its
  own wording); we emitted one hand-written message for both.

#### Known divergences, recorded rather than changed

Both are deliberate and are described in `tasks/backlog.md`:

- `hint: "$natural"` **as a string** is accepted here and rejected by mongod,
  which takes only the document form. It is a documented SecantusDB
  convenience with existing tests, and pymongo's `.hint("$natural")` produces
  exactly that string.
- `distinct.hint` is accepted although 6.0.16 rejects it, because a later
  MongoDB release added the option — accepting is the safe direction for a
  field whose status changed between versions.

The `explain` **stage vocabulary** (`SORT` / `LIMIT` / `SKIP` /
`PROJECTION_SIMPLE` wrappers, `COLLSCAN`'s `direction`, `IDHACK`,
`COUNT_SCAN`, `DISTINCT_SCAN`, populated `rejectedPlans`, `explainVersion`) is
a separate and much larger piece of work, now scoped as one backlog item rather
than half-built — a partial stage tree would be more misleading than an
honestly flat one.
