### Rust PostgreSQL server: advisory-lock waits get deadlock detection

A session waiting on a `pg_advisory_lock` another session holds blocked until
`lock_timeout` — or forever, `lock_timeout = 0` being the default — even when
the wait had closed a cycle of sessions waiting on each other. The docs
promised detection (`docs/sql.md` claimed a cycle raises `40P01`), and the
deprecated Python server's hub has it (`src/secantus/sql/pgadvisory.py`); the
Rust server's advisory waits were the one lock kind with no cycle check.

Blocking advisory acquires now register in a process-wide wait-for graph
(`lock_manager`, the same edges-and-depth-first-search shape `row_waits` uses
for row waits) and check it once `deadlock_timeout` (1 s) of waiting has
passed, and each second after, so a cycle that forms later is caught too. As
with the other lock kinds, only real holder edges are ever registered — a
false-positive `40P01` would be a bug — and of two sessions closing a cycle
the one whose check runs first fails, as on PostgreSQL. The victim's
session-scoped holds survive its failed statement, matching PostgreSQL; the
surviving waiter's statement goes through once the victim lets go.

#### Fixed

- `crates/secantus-pg/src/advisory.rs`: a blocking `pg_advisory_lock*` /
  `pg_advisory_xact_lock*` wait is checked against the shared wait-for graph;
  a real cycle raises PostgreSQL's `40P01 deadlock detected`, where the wait
  had previously outlived `lock_timeout` (`lock_timeout` still fires first
  when it is shorter, and cancels still end the wait). Verified against
  PostgreSQL 15
  (`tests/test_rust_pgserver_slice.py::test_a_cycle_of_advisory_waits_is_a_deadlock`,
  `::test_advisory_lock_waits_for_a_session_hold`,
  `::test_an_advisory_lock_wait_ends_on_lock_timeout`).
- `docs/sql.md`: the row-locking clauses section and the `BEGIN ISOLATION
  LEVEL` paragraph described the deprecated Python server's no-op behaviour;
  both now state the Rust server's real semantics (real `FOR UPDATE` / `FOR
  SHARE` row locks with wait, `55P03` on `NOWAIT`, `SKIP LOCKED`, `40P01` on a
  cycle; honored isolation levels with the wait-and-re-evaluate READ COMMITTED
  path), and the isolation table's READ COMMITTED row now matches what the
  Rust server does. Advisory-lock and `pg_locks` wording checked against the
  implementation.

#### Added

- `crates/secantus-pg/src/lock_manager.rs`: the process-wide wait-for graph
  every lock wait registers its edges in — one depth-first search serves table
  locks, row waits, and advisory locks, so a cycle that runs through more than
  one lock kind is seen once each kind's wait migrates onto it.
