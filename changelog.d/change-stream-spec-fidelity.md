### A change stream could quietly ignore the options you asked it for

Phase 2 of `tasks/remaining-work-plan.md`, fifth and last surface: 13
change-stream shapes against a real replica-set mongod 6.0.16. **All 13
diverged.**

The dominant failure was arguments **accepted and ignored**, which is a worse
shape here than almost anywhere else in the server, because the caller believes
they asked for something. `parse_spec` guarded every field with `isinstance`
and silently skipped a wrong-typed value, so a client that asked for
`fullDocument: "updateLookup"` — or to resume from a token — got a stream that
did neither and reported success.

#### Fixed

- **A crash.** A resume token that is valid hex but not valid BSON
  (`{"_data": "aa"}` — two hex digits, one byte) raised `InvalidBSON` straight
  out of the handler and escaped as `internal server error` (code 1).
- **An unknown `fullDocument` or `fullDocumentBeforeChange` value** is rejected
  (`BadValue`) instead of falling back to the default. Misspelling
  `updateLookup` used to give you a stream without lookups and no indication.
- **An unknown `$changeStream` field** is rejected (`Location40415`).
- **A wrong-typed `resumeAfter` / `startAfter` / `startAtOperationTime`** is
  rejected (`TypeMismatch`). These were the most consequential of the ignored
  arguments: the stream started from the beginning rather than the requested
  position, and said it had succeeded.
- **`$changeStream` anywhere but the first stage** is rejected
  (`Location40602`). We built an ordinary aggregation and answered an exhausted
  cursor — a "stream" that never yields an event and never says why.
- **`$changeStream: 5`** answers mongod's `Location6188500` rather than a
  generic `BadValue`, and a non-hex resume token reports mongod's own wording
  instead of our prefix wrapping Python's `fromhex()` complaint.
- **Event field order.** `fullDocument` now sits immediately after
  `operationType`, where mongod puts it, rather than being appended at the end.
  The event *contents* already matched exactly; only the order differed, and
  the event's field order is the contract drivers read off the wire.

#### Known gap

The `$project`-drops-`_id` fatal error carries the right code (280) and message
body but not mongod's `Executor error during getMore :: caused by ::` prefix —
the same wrapper-prefix class already tracked for aggregation errors.

The differential cases for this surface are deliberately **not** added to
`tests/test_mongod_differential.py` in this PR: another session is editing that
file to generalise the mongod-version gating, and the coverage here lives in a
dedicated test file instead so the two do not collide.
