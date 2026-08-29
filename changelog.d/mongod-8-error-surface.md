### Error messages and codes now match a current MongoDB server

SecantusDB's job is to be indistinguishable from a real MongoDB server, and that
includes being wrong in the same places it is. Its error surface had been matched
against MongoDB 6.0 — closely enough to reproduce a genuine quirk where 6.0 put a
closing quote inside a bracket in one of its type-mismatch messages. MongoDB has
moved on since, and the differences had quietly accumulated: negative batch sizes
reported a different code, update failures were missing the wrapper newer servers
put around execution errors, an explicitly null argument was rejected where a
current server treats it as simply absent, and `$lookup` and `distinct` had both
switched to generated argument parsing with a different family of messages.

The surface is now matched against MongoDB 8.2, verified case by case against a
real server rather than transcribed. Applications that key on error codes will
see the current server's codes: a negative `limit`, `skip` or `batchSize` now
reports `BadValue` rather than the old internal location code, and malformed
`$lookup` arguments report the same missing-field and unknown-field codes a real
server sends. The differential test suite that compares the two servers
operation-by-operation passes in full, and the same changes have landed in the
Rust server so both stay in step.

#### Changed
- Negative cursor-sizing values report `2 BadValue` instead of `51024`.
- Type-mismatch messages carry the current server's per-field type lists.
- Execution-time `update` and `aggregate` failures are wrapped in the server's
  executor-error prefixes; parse-time failures are not.
- An explicit `null` for `findAndModify.arrayFilters` / `killCursors.cursors` is
  treated as an absent field.
- `$lookup` and `distinct` argument errors use the generated-parser messages and
  codes (`40414` / `40415` / `14`), with `IDLFailedToParse` / `IDLUnknownField`
  code names.
