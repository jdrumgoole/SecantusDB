### Rust MongoDB server: the kill-session commands select sessions as mongod does

`killAllSessionsByPattern` and `killAllSessions` act on the sessions they
name, and close those sessions' cursors as well as their transactions.

#### Fixed

- `killAllSessionsByPattern` matches a pattern's `lsid` and `uid`. Every
  named field must match, and a pattern naming neither matches every
  session. `users` and `roles` say whom to act as and select nothing, as on
  mongod 8.2.11. Every pattern used to abort every transaction.
- `killAllSessions` selects by user. An empty user is the unauthenticated
  owner.
- Both commands kill the matched sessions' cursors; a later getMore is 43
  CursorNotFound. Malformed `uid`, `lsid`, `users` and `roles` fields are
  refused with mongod's paths.

Fixes RM-217
(`tests/backlog/test_rm_found_2026_10_20b.py::test_kill_sessions_by_pattern_kills_only_the_match`,
`tests/backlog/test_rm_found_2026_10_20b.py::test_kill_all_sessions_closes_cursors`).
