### Rust MongoDB server: change-stream post-images are the update's, and a retry waits for the attempt still running

Two silent wrong answers, each measured against mongod 8.2.11. A change
stream reading `fullDocument: "whenAvailable"` or `"required"` returned the
document as it was when the stream read it, so a later write or delete
changed or removed every earlier event's post-image. It now returns the
post-image mongod returns: the pre-image stored when the update ran, with
that update applied. And a retryable write retried while its first attempt
was still running (a driver's retry after a socket timeout) ran a second
time beside it, so the retry of an insert that had succeeded answered
E11000. It now waits for the first attempt and is answered from its result.

#### Fixed

- A change stream's `fullDocument: "whenAvailable"` / `"required"` is the
  stored pre-image with the update applied, as on mongod: a later update or
  delete leaves it as the update left it. With no pre-image it is null, or
  error 47 for `"required"`, and turning images off afterwards does not take
  away the ones already stored. Fixes RM-229
  (`tests/backlog/test_rm_found_2026_10_21.py::test_change_stream_serves_the_stored_post_image`,
  `::test_a_post_image_outlives_the_document`).
- A missing `"required"` image fails the whole command whose batch reaches
  it, as on mongod: the opening `aggregate` itself when its first batch does
  (under mongod's aggregate prefix), while a batch that fills first is
  returned and the error comes with the next `getMore`. The server returned
  the events before it and failed the `getMore` after; it also failed a
  `getMore` batch that the events exactly filled. Part of RM-229
  (`::test_a_missing_required_image_fails_the_batch_that_reaches_it`).
- A command that carries a `txnNumber` checks its session out for as long
  as it runs, as mongod does, and another one on the same session waits for
  it: a retry is then answered from the first attempt's result, and a newer
  or older transaction number is judged after it. Commands with only an
  `lsid`, and `killSessions`, do not wait. Fixes RM-230
  (`tests/backlog/test_rm_found_2026_10_21.py::test_a_retry_waits_for_the_attempt_still_running`,
  now a slow update that is still running when the retry arrives on both
  servers: the 30,000-document insert it used finished on mongod before the
  retry was sent, so the reference side measured a sequential retry).

#### Found

- RM-313: `killSessions` does not interrupt the command holding the session,
  which mongod does (11601, and the write does not apply)
  (`tests/backlog/test_rm_found_2026_10_25.py::test_kill_sessions_interrupts_the_command_holding_the_session`).
