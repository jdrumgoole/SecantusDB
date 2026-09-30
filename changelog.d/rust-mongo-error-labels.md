### The Rust MongoDB server labels injected errors the way mongod does

A driver decides whether to retry a write, resume a change stream or replay a
transaction from the `errorLabels` on an error, not from the code alone. Errors
injected with `failCommand` -- which is how every driver's retry and resume
tests work -- carried a label in only one of the places mongod puts one. Every
code from 1 to 520 was swept against mongod 8.2.11 and the Rust server now
answers each one the same way.

#### Fixed

- `ChangeStreamFatalError` (280) and `ChangeStreamHistoryLost` (286) carry
  `NonResumableChangeStreamError` on every command, a plain cursor's `getMore`
  included (the PHP driver's `bug1419-001.phpt`).
- The aggregate that opens a change stream carries
  `ResumableChangeStreamError` for mongod's 28 resumable codes; a retryable
  write carries `RetryableWriteError` for its 23 codes, which it never did;
  the transaction and commit label sets gain the codes they were missing; and
  four overload codes add `SystemOverloadedError`.
- `failCommand` refuses a code whose error needs extra information (duplicate
  key, stale config and 22 more) with mongod's 40671, and drops the connection
  on the four codes where mongod does.
- An injected error reports mongod's real `codeName` for 452 codes that used
  to come back as `Location<code>`.
- An oversized multi-document transaction now fails with
  `TransactionTooLargeForCache`'s real code, 388. It said 313, which on mongod
  is a different error.
- The C++ driver gauge started its server without test commands, so every
  failpoint test in it failed on the harness.

#### Added

- `tools/probes/error_labels.py`: code, code name and labels for injected
  errors across nine command contexts, compared against mongod.
