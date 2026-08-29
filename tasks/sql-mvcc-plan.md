# MVCC above WiredTiger — scoping READ COMMITTED for the SQL server

> **Written 2026-08-29.** A scoping document, not a work order. It exists so the
> decision to start (or not start) is made on measured facts rather than on a
> one-line option summary — the previous summary omitted the largest problem,
> and a choice was made on it.
>
> **Nothing here is committed to. Read "Kill criteria" before Phase 1.**

## 1. The divergence, measured

`tests/test_sql_isolation_level.py`, against a live PostgreSQL 14:

    autocommit write-write        pg blocks -> 111   us blocks -> 111   MATCH
    explicit txn READ COMMITTED   pg blocks -> 111   us 40001  -> 101   DIVERGE
    explicit txn REPEATABLE READ  pg 40001  -> 101   us 40001  -> 101   MATCH
    explicit txn SERIALIZABLE     pg aborts one      us BOTH COMMIT     DIVERGE

Only the READ COMMITTED row is in scope here. SERIALIZABLE was settled
separately: accept, report, and document that it is snapshot isolation and
permits write skew (`docs/sql.md`, "Isolation levels").

**What PostgreSQL does that we do not:** under READ COMMITTED it takes a fresh
snapshot *per statement*, and on a write-write race it **blocks** on a row lock
rather than failing — then re-reads the committed row and re-applies the update
(EvalPlanQual). Both writes land: `100 + 1 + 10 = 111`.

**Why we cannot:** an explicit transaction is one WiredTiger transaction, so it
has one snapshot, pinned at its first statement. That is PostgreSQL's REPEATABLE
READ, faithfully.

## 2. Why the two obvious designs fail

Both were considered and neither is viable as stated. This section is the reason
this document exists.

### 2a. "Refresh the snapshot each statement"

WiredTiger exposes exactly that call and forbids it in exactly our case
(`vendor/wiredtiger/src/include/wiredtiger.in`):

> `reset_snapshot` … releases the snapshot of the database and gets a new one.
> This makes newer commits visible. … **It is an error to call this method when
> using an isolation level other than snapshot isolation, or if the current
> transaction has already written any data.**

A transaction that has written cannot take a fresh snapshot. Any design resting
on this is dead on arrival.

### 2b. "A WiredTiger transaction per statement, plus our own undo"

**The blocker is visibility, not durability.** A statement that commits to WT is
immediately visible to *every other session*, so an in-flight transaction's
uncommitted work becomes readable by everyone — **dirty reads**, which
PostgreSQL permits at no isolation level and which is a worse violation than the
`40001` being fixed.

Preventing it moves visibility from the storage engine into the server. That is
the MVCC project, and it is what the rest of this document scopes.

### 2c. "A pessimistic row-lock manager" — half-correct, and the dangerous half is silent

Take the row lock before writing so the conflict never happens. This keeps one
WT transaction per user transaction: no dirty reads, no undo log, no recovery
changes. Much smaller. But by §2a:

* **first write in a transaction** — nothing written yet, so `reset_snapshot` is
  permitted: block → reset → re-read → write. Matches PostgreSQL exactly.
* **any later write** — `reset_snapshot` refused, the stale snapshot yields
  `110` instead of `111`. **Silently wrong data**, worse than today's honest
  error.

Viable only as a partial mode that *raises `40001` on the second write* rather
than answering wrongly. See Phase 0.

## 3. Scope boundaries

Three that materially shrink the work, all verified:

* **SQL/PG only.** The Rust server has no PostgreSQL interface — the only
  `crates/` hit for "postgres" is a test asserting it is not a valid engine
  name. This is Python-side work exclusively.
* **The Mongo side must NOT change.** mongod's multi-document transactions *are*
  snapshot isolation, so the document interface is already correct. Any change
  made at the shared `Storage` layer would break Mongo-side conformance to fix a
  SQL-side one. **The seam must sit above `Storage`, not inside it.**
* **Autocommit is already correct** and must stay so — each statement is its own
  transaction, the second writer blocks and re-reads. It is the common path, and
  a regression here would be far more damaging than the gap being closed.

## 4. The seam

The SQL layer reaches storage through a narrow surface. Counted across
`src/secantus/sql/*.py`:

    find_matching     93 call sites
    delete_matching   54
    insert            39
    update_matching   17
    ---------------------------------
    ~95% of all storage traffic in four methods

A `VersionedStorage` facade implementing the same interface, installed for SQL
sessions only, can enforce visibility without editing 96 call sites and without
touching the Mongo path. **This is the single most important structural fact in
this document** — it is what makes the project bounded rather than open-ended.

The remaining ~16 methods (DDL, index, rename) are not row-visibility concerns
and pass through.

## 5. What MVCC actually requires

Not negotiable; each is load-bearing:

1. **Row versioning.** Every row carries the id of the transaction that wrote
   it, and a pointer to the version it superseded.
2. **A visibility rule on every read.** Is this version's writer committed, and
   did it commit before my statement's snapshot? READ COMMITTED takes a new
   statement snapshot; REPEATABLE READ keeps the transaction's.
3. **A transaction registry.** In-progress / committed / aborted, with commit
   ordering. **Nothing reusable exists** — checked:
   `tests/test_transaction_registry.py` covers the MONGO side's session
   transactions (`NoSuchTransaction` 251 / `TransactionCommitted` 256), which is
   a different concern. This is built from scratch.
4. **A durable undo log.** Statement-level commits mean WiredTiger will not roll
   anything back for us. Without durable undo, a crash mid-transaction leaves
   partially committed data permanently. **This is where atomicity stops being
   WiredTiger's problem and becomes ours.**
5. **Crash recovery.** Replay/abort in-flight transactions at open.
6. **Garbage collection.** Reclaim versions no live reader can see, or the store
   grows without bound.
7. **Row-level blocking.** Visibility alone does not give PostgreSQL's blocking
   write behaviour; the lock manager from §2c is still needed, now safe because
   a fresh statement snapshot is available.

Note that the existing savepoint machinery is **not** a starting point for (4):
`_Savepoint` deep-copies a whole collection on first write after establishment
(`sql/session.py`). That is O(collection) in memory and coarse — it would likely
be *replaced* by the undo log, not extended.

## 6. Phases

Each phase ends in something committable and independently valuable. No phase
begins before the previous one's exit criterion is met.

**Phase 0 — decide whether the partial mode is enough (days, not weeks).**
Build §2c's lock manager with the second-write case explicitly raising `40001`.
Measure how many real-world shapes it fixes: does the conflicting write tend to
be the transaction's *first*? Instrument the psycopg / SQLAlchemy / pgjdbc
gauges. **If it covers the shapes that matter, stop here** — the remaining
phases may never be worth their risk. This phase is cheap and it is the honest
test of whether the project is needed at all.

**Phase 1 — the facade, no behaviour change.** Introduce `VersionedStorage` as a
pass-through installed for SQL sessions. Exit: the full suite and all SQL gauges
are byte-identical to today. This is pure risk-reduction: it proves the seam
holds before anything depends on it.

**Phase 2 — versioning and visibility, single-connection.** Row versions,
transaction registry, visibility filter. Still one WT transaction per user
transaction (no statement commits yet), so no dirty-read exposure. Exit: reads
through the facade return exactly what they do today.

**Phase 3 — statement-level commits + undo log. The dangerous phase.** Only now
do statements commit independently. Requires (4), (5) and (6) together — undo
durability and recovery cannot lag, because the window between "statement
commits" and "undo is durable" is a data-loss window. Exit: **crash-injection
tests first**, then behaviour. Kill the phase rather than ship it with recovery
untested.

**Phase 4 — row-level blocking, and READ COMMITTED becomes real.** Exit: the
divergence table in §1 shows MATCH on all four rows against the live PostgreSQL,
and the two `test_known_divergence_*` tests are rewritten as conformance tests.

## 7. Verification

* The PG oracle is the arbiter throughout; hand-derived expectations are not
  evidence. `tests/test_sql_isolation_level.py` already has the harness.
* **Crash-injection before Phase 3 ships**, not after. Kill the process at each
  point between statement-commit and undo-durable, reopen, assert atomicity.
* The three SQL gauges (psycopg, sqllogictest, SQLAlchemy) must not regress at
  any phase boundary; they exercise transaction shapes no unit test will invent.
* Concurrency stress under `SECANTUS_FORCE_DURABLE=1`, since this project moves
  durability guarantees into our code.

## 8. Kill criteria

State these now, while it is cheap to walk away:

* **Phase 0 shows the partial mode covers the real shapes** → stop; ship that,
  keep `40001` for the rest.
* **Phase 1 cannot be made behaviour-identical** → the seam is not where this
  document claims; re-scope before continuing.
* **Phase 3 crash tests cannot be made to pass reliably** → stop and revert to
  Phase 2. A server that loses committed data under crash is far worse than one
  that returns a retriable error, and this repo's standing rule is that a
  storage error is never to be stepped over.
* **Any phase regresses autocommit or the Mongo side** → stop. Both are correct
  today.

## 9. Honest estimate

Phase 0 is days. Phases 1–2 are a substantial slice each. Phases 3–4 are the
real project: writing and proving a transaction manager, with crash recovery,
above a storage engine that already has one.

**The status quo is defensible.** Today's behaviour returns
`40001 serialization_failure` — the retriable signal every driver and ORM
already handles — and autocommit, the common path, matches PostgreSQL exactly.
The gap is real but narrow, and it is documented rather than hidden. That is a
reasonable place to stay indefinitely.

Recommendation: **do Phase 0, then re-decide with its measurement in hand.**
