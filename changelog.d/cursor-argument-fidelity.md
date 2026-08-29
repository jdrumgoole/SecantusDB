### Malformed cursor arguments crashed the server instead of being rejected

Phase 2 of `tasks/remaining-work-plan.md`, second surface: 51 cursor /
`getMore` / `killCursors` shapes run against a live mongod 6.0.16. **22
diverged, four of them crash-class** — a malformed argument reached a bare
`int()` and the `ValueError` / `TypeError` escaped as `internal server error`
(code 1), which is the shape that tells a client nothing and looks like a
server fault rather than their own bad request.

#### Fixed

- **Four crashes.** `getMore` with a string cursor id or a string `batchSize`,
  and `killCursors` with a non-array `cursors` or a wrong-typed element, all
  answered code 1. They now answer mongod's `TypeMismatch` (14) naming the
  field and both types.
- **`getMore` answered `CursorNotFound` (43) for parse errors** — for a cursor
  that existed. A missing `collection` (mongod: `40414`), a non-string one
  (`14`), an unknown top-level field (`40415`) and an int32 cursor id (`14`;
  mongod requires a long, the same int64 strictness the Go and C drivers
  enforce on the reply side) all reported a plausible-looking lie about the
  cursor instead of the parse error mongod reports before it looks a cursor up.
- **Negative `batchSize` / `limit` / `skip` were silently accepted** on `find`,
  `getMore` and `aggregate`'s cursor spec. A negative `batchSize` fell through
  `or DEFAULT` and quietly became the default; a negative `limit` returned the
  whole collection. mongod answers `Location51024`, and — unlike the type error
  on the same slot — names the field bare rather than by its IDL path.
- **`maxTimeMS` on a getMore for a non-awaitData cursor was accepted and
  ignored.** It is the awaitData wait budget, so mongod refuses it on a cursor
  that cannot wait, tailable-but-not-awaitData included (probed both ways).
  Accepting it hides a client bug: a caller who believes it has bounded a
  blocking read has bounded nothing.
- **`aggregate` ran without a `cursor` option**, which mongod requires except
  with `explain`. A client that forgot the option never learned it had.
  Unknown keys inside the cursor spec are now `40415` too.
- **`killCursors` with no `cursors` field returned a cheerful all-empty success
  reply**; mongod requires the field (`40414`). A `null` element is skipped,
  and a `null` `cursors` takes mongod's older `Location10065` — both probed
  rather than assumed.
- **`awaitData` without `tailable` was accepted** and ran an ordinary find, so
  a client that asked to block got a plain batch back with no indication its
  option had been dropped.

The accepted-type matrix is unchanged and was re-verified against mongod while
adding the range check: int, long, double, decimal and null are all taken, and
a fractional double truncates toward zero (`batchSize: 2.5` yields two
documents).

#### Changed

- `tests/test_crud.py::test_tailable_drop_closes_pymongo_cursor_cleanly` no
  longer calls `max_await_time_ms` on a plain `TAILABLE` cursor. It passed only
  because we accepted `maxTimeMS` there; the same code against a real mongod
  raises. pymongo sends the option despite documenting it as ignored for
  non-await cursors, because its guard is a bitmask test
  (`_query_flags & CursorType.TAILABLE_AWAIT` is `2 & 6 == 2`, truthy, for a
  plain `TAILABLE` cursor).

`tests/test_mongod_differential.py` grew 29 cases (93 → 122).
