### A retried write no longer applies twice

Every official MongoDB driver retries a failed write automatically, resending
it with the same session id and transaction number after a network blip or a
write-concern error. Real MongoDB remembers that it already ran the statement
and hands back the original answer. SecantusDB did not: it ran the write a
second time.

For an insert this was noisy — the retry collided with its own first attempt
and raised a duplicate-key error. For anything non-idempotent it was silent
and much worse. A retried `{$inc: {n: 1}}` incremented twice, a retried
`$push` appended twice, and in both cases the client was told exactly one
document had been modified. Nothing surfaced an error; the data was simply
wrong.

The Python server now keeps a record of each completed retryable write and
replays it when the same write arrives again. Only writes that fully took
effect are recorded — a failed one must genuinely re-run, or a momentary
error would become a permanent one.

#### Fixed

- Retryable writes are idempotent on the Python server: `insert`, `update`,
  `delete` and `findAndModify` carrying a session's transaction number are
  executed once, and a retry replays the original reply.

#### Known limitations

- The **Rust server still applies retried writes twice**; the same fix has yet
  to be ported. See `tasks/backlog.md` §5.
- Records are whole-command, not per-statement, so a partially-failed batch
  re-runs in full rather than retrying only its missing documents.
- Records expire after 30 minutes, matching MongoDB's own sweep.
