### A panic can no longer wedge a collection until restart

Every per-collection write lock, cursor-registry lock, and per-statement
transaction lock in the Rust server took its mutex with a plain
`.lock().unwrap()`. Rust poisons a mutex when a thread panics while holding
it, and a poisoned mutex stays poisoned — so a single stray panic inside a
critical section would make every subsequent write to that collection panic
too. The server itself survives (the dispatch loop catches the panic and
replies with `InternalError`), which is precisely what makes the damage
durable rather than fatal: the process keeps answering health checks and
serving every other collection while one collection is silently unwritable
until someone restarts it.

All 73 such call sites across `secantus-storage` and `secantus-commands` now
use the poison-tolerant `unwrap_or_else(|e| e.into_inner())` form that the
same crates already used for the logging, failpoint, transaction, and auth
locks. A panic now costs you the operation that panicked instead of the
collection.

No reachable panic trigger was found on these paths — the critical sections
are `Result`-propagated rather than panic-capable — so this is blast-radius
hardening, not a fix for a live failure.

#### Fixed

- Rust server: per-collection write locks, the cursor registry, and
  per-statement transaction locks are poison-tolerant, so a panic inside a
  critical section no longer leaves the collection, cursor, or transaction
  permanently unusable.
