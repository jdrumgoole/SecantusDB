### The isolation level the SQL server provides is now documented and tested

No behaviour changes here — this records what the engine actually does, because
it does not match what it reports.

Every explicit transaction runs on snapshot isolation, which is what PostgreSQL
calls REPEATABLE READ, while `BEGIN ISOLATION LEVEL` echoes back whichever level
was asked for. Measured against a real PostgreSQL, that lands differently for
each level:

- **Autocommit statements match PostgreSQL exactly.** Two clients updating the
  same row both land their write; the second waits for the first and re-reads.
  This is the common path and it was already correct.
- **READ COMMITTED inside an explicit transaction diverges.** PostgreSQL blocks
  the second writer and completes it; we report a serialization failure, so a
  client that does not retry loses its write.
- **SERIALIZABLE is over-claimed.** We accept it and report it, but provide
  snapshot isolation, which permits write skew — two transactions can each read
  what the other is about to change and both commit. PostgreSQL aborts one.

All four cases are now pinned by tests, with the two divergent ones named so
they read as known divergences rather than as conformance. Closing the READ
COMMITTED gap needs a fresh snapshot per statement, which the storage engine
does not offer within one transaction; what to do about SERIALIZABLE is a
decision about what the server should promise, and is recorded rather than
quietly chosen.
