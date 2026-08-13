### Change streams resume when the server says a getMore failed resumably

Drivers decide whether to resume a change stream by looking for the
`ResumableChangeStreamError` label on the error a `getMore` returns. mongod
attaches that label from inside its change-stream machinery, which is why
the drivers' own test suites reach for the `failGetMoreAfterCursorCheckout`
failpoint to provoke one. SecantusDB's Python server ignored that failpoint
entirely — the `getMore` simply succeeded, no error was raised, and no
resume ever happened. The Rust server already handled it, so the two
servers disagreed about whether a stream should recover.

The distinction between the two failpoints is deliberate and is now pinned
by tests: `failGetMoreAfterCursorCheckout` with a resumable code resumes the
stream, while plain `failCommand` with the *same* code does not, because it
short-circuits before the change-stream path and carries only the labels the
failpoint itself named. Stamping the label unconditionally would silently
swallow errors that callers expect to see.

#### Fixed

- The Python server honours `failGetMoreAfterCursorCheckout` and stamps
  `ResumableChangeStreamError` on the sixteen error codes mongod treats as
  resumable, matching the Rust server's table exactly. libmongoc's
  `change-streams-resume-errorLabels` now passes.
