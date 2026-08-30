### Nine test files share one database server instead of standing up dozens

Most test files start a fresh database server for every single test, which is the
safe default but costs about a quarter of a second each time. Where a file's tests
already write to separate collections, none of that isolation is actually being
used — the tests cannot see each other regardless.

Nine such files now start one server per file. Their combined runtime drops from
101 seconds to 14. The selection was made by checking that every collection name
in the file is unique and that nothing in it depends on a private server, rather
than by judgement, so files involving change streams, the oplog, reopening a
store, capped collections, TTL clocks or authentication are untouched.

#### Changed
- Nine test modules take a module-scoped server fixture.

#### Fixed
- `invoke rust-test`'s description claimed it tested the whole Rust workspace. It
  tests the *clean* workspace, which excludes the six WiredTiger-linked crates —
  the entire storage layer and the shipped binary — so it could report success
  having never compiled them.
