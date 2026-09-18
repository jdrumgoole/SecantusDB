### The Rust PostgreSQL server starts in-process now

Starting a SecantusDB server in a test has always been one or two lines with no
external processes to manage — except for the Rust PostgreSQL server, which
every test and the psycopg gauge had to spawn as a `secantusd-pg` subprocess,
wait for a readiness line from, and remember to terminate. That server's accept
loop lived in its `main.rs`, so nothing else could reach it. The loop has moved
into the library as `secantus_pgserver::bind`, and `_secantus_server` grows a
`PgServer` handle over it that mirrors the Mongo server's `RustServer`:
`PgServer(path)` binds a socket on an OS-assigned port, `.dsn` is handed
straight to psycopg, and `.stop()` shuts it down. Python is only the launcher —
no statement ever enters it.

The interesting half of that move is who owns the store. WiredTiger's
close-checkpoint runs when `Storage` is dropped, and without it every write
acknowledged since the previous checkpoint is gone — measured last month as a
`CREATE TABLE` plus `INSERT` that the client had been told succeeded and that
were not there afterwards. An `Arc<Storage>` parameter would have let a caller
hold a second reference, skip the checkpoint, and hear nothing about it. So
`bind` takes the `Storage` **by value** and the returned handle owns it for the
rest of its life: stopping the server drains the connections, tears down the
runtime, and drops the last reference, which is where the checkpoint happens.
"Stopping checkpoints the store" is a property of the type rather than a rule
someone has to remember, and if a wedged connection ever defeats the drain,
`stop` says so on stderr instead of returning as though the data were safe.

Shutdown also had to learn to tell the connections, not just wait for them. A
pooled PostgreSQL connection sits idle indefinitely — a real backend never hangs
up on one — so a stop that only drained blocked for its full timeout whenever a
caller left a connection open, which in an embedded test is most of the time:
10.8 seconds, measured. Every connection now selects its socket against a
shutdown signal and lets go when it fires, which brings the same stop to 0.11
seconds with the writes still landing on disk.

`secantusd-pg` is now a thin CLI wrapper around the same `bind` — same readiness
line, same `--database` handling, same clean SIGINT/SIGTERM shutdown — so there
is one serve path instead of two, and a durability or shutdown fix lands in both
the binary and the embedded handle by construction.

#### Added

- `secantus-pgserver`: `bind()` / `RunningPgServer` (`address()`, `dsn()`,
  idempotent `stop()`, `Drop`) — a synchronous, runtime-free entry point that
  owns its own tokio runtime and resolves the bound address before returning, so
  `127.0.0.1:0` works from a plain (non-async) caller.
- `_secantus_server.PgServer`: the embedded Python lifecycle handle —
  `PgServer(storage_path, port=0, host="127.0.0.1", databases=None)`, with
  `address` / `port` / `dsn` / `version` properties, `stop()`, and the
  context-manager protocol. Behind the default-on `pgserver` cargo feature;
  `_secantus_server.HAS_PGSERVER` reports whether a build carries it.

#### Changed

- Stopping the PostgreSQL server now signals its live connections instead of
  only waiting for them, so an open client connection no longer holds the
  shutdown for the full drain timeout (10.8s → 0.11s). The old binary aborted
  the accept task and dropped a still-shared `Arc<Storage>`, which skipped the
  close-checkpoint silently in exactly that case.
- `secantusd-pg` is a CLI wrapper over `secantus_pgserver::bind` rather than
  carrying its own accept loop. Observable behaviour — the
  `secantusd-pg listening on {bound} storage={home}` readiness line, the
  ephemeral-port form, `--database`, and clean SIGINT/SIGTERM shutdown — is
  unchanged.
