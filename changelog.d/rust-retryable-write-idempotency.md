### The Rust server stops applying retried writes twice

The Python server learned to recognise a retried write; the Rust server had
not, so the two disagreed about something as basic as whether a write
happened once or twice. A driver that retried after a network blip — which
every official driver does automatically — would silently double a
`$inc` against the Rust server while the Python server handled it correctly.

Both servers now keep the same record and apply the same rules, so a retry
replays the original reply rather than re-running the write.

#### Fixed

- Retryable writes are idempotent on the Rust server, matching the Python
  server: `insert`, `update`, `delete` and `findAndModify` carrying a
  session's transaction number execute once, and a retry replays the stored
  reply. Verified over the wire against a release build — a retried `$inc`
  leaves 1 where it previously left 2.
