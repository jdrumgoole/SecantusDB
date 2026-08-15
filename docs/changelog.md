# Changelog

All notable changes to SecantusDB are documented here. This file is the
**system of record** for what shipped in each release — the per-release
blog posts on [secantusdb.com](https://secantusdb.com/categories/releases.html)
are generated from these entries via `tools/generate_blog_post.py`.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
with one extension: each release carries a one-to-three-paragraph **prose
lede** between the date line and the structured `#### Added` /
`#### Changed` / `#### Fixed` subsections. The prose lede is what the
blog generator lifts verbatim as the marketing-post body, so it should
read as a self-contained narrative — not as "v0.5.1bN ships X."

This project adheres roughly to [Semantic
Versioning](https://semver.org/spec/v2.0.0.html), but while we're in
beta the patch number `bN` rolls forward on every PyPI-visible push;
the API surface itself is shaped by Semantic Versioning intent.

## [Unreleased]

## [0.6.0b11] — 2026-08-15

### A silent lost update caught in the act, pipelined batches with real transaction semantics, and query cancellation

The headline of this release is a data-integrity fix that took three CI
platforms, a paired A/B sampler, and a deterministic race harness to pin
down. Statements a client pipelines before a single Sync now run in one
implicit transaction, exactly as PostgreSQL treats them — pgjdbc's
184-variant BatchFailureTest depends on a failed batch rolling back its
earlier inserts, and both batch classes now pass in full. Making that
correct exposed a genuine race: an implicit transaction's commit lands
outside the statement lock, and a concurrent autocommit computed update
(`SET n = n + 1`) whose read-compute-write window it straddled would
silently overwrite it while still reporting `UPDATE 1`. The fix runs the
whole computed update as one storage snapshot transaction, so a
mid-window commit surfaces as a write conflict and the statement retries
from a fresh read — proven by a paired fix-on/fix-off sampler catching
the loss in the wild on the exact runner conditions that first exposed it.

The PostgreSQL surface took two more long-standing steps. The wire
cancel sub-protocol is honoured — a CancelRequest verifies the
BackendKeyData secret and interrupts the target's running statement with
PG's `57014`, which is what JDBC's `setQueryTimeout` and pgx's
context cancellation lean on. And named prepared statements revalidate
their cached plan under DDL, raising `cached plan must not change result
type` with the routine field pgjdbc's transparent re-prepare matches on
— its 1056-variant AutoRollbackTest matrix now passes in full. Around
them: dollar-quoted strings and nested block comments parse everywhere,
`CREATE TABLE AS SELECT` works, `now()` is transaction-stable,
LISTEN/NOTIFY pushes to idle connections asynchronously, and per-session
temp-table namespacing matches PG's `pg_temp_<n>` scheme.

On the MongoDB side, the default `getMore` batch now fills mongod's 16MB
envelope instead of stopping at 101 documents — a large cursor drain
that took dozens of round trips now takes one — and retryable writes
are idempotent across reconnects on both servers, with `failGetMore`
carrying the resumable-error label change streams expect.

### Prepared statements revalidate their cached plan, like Postgres

A named server-prepared statement whose result shape changed under DDL
(a `SELECT *` after `ALTER TABLE ADD COLUMN`) now raises PostgreSQL's
`cached plan must not change result type` (0A000) instead of silently
re-planning — and it raises at planning time, before any side effect, so
a data-modifying CTE's INSERT does not run. The ErrorResponse carries
`ROUTINE=RevalidateCachedQuery`, which is the field (not the SQLSTATE)
pgjdbc's transparent re-prepare-and-retry matches on; without it every
recoverable case surfaced the raw error. Unnamed statements re-plan per
Bind and never raise, matching PG. pgjdbc's AutoRollbackTest — the
1056-variant autosave × DDL × transaction matrix — now passes in full.

#### Fixed
- Named prepared statements: result-shape changes under DDL raise 0A000
  with the RevalidateCachedQuery routine field, before side effects;
  first execution captures the plan identity; unnamed statements are
  exempt.

### Query cancellation: the wire CancelRequest is honoured

The PostgreSQL cancel sub-protocol — a client opens a fresh connection
and sends the (pid, secret) pair from BackendKeyData to cancel the query
running on its main connection — was parsed and silently dropped, and
`pg_sleep` ran as an uninterruptible `time.sleep`. Drivers lean on this
machinery for statement timeouts and context cancellation: pgx sends a
CancelRequest whenever a context is cancelled mid-query.

CancelRequest now fires the target session's cancel event (after
verifying the secret), and cancellation points observe it and raise PG's
`57014 canceling statement due to user request` while the connection
stays fully usable — cancel is not terminate. `pg_sleep` is such a point
in every context (FROM-less and per-row), `pg_cancel_backend` now
cancels instead of closing the target's connection (matching real PG;
`pg_terminate_backend` still closes), and a cancel that lands while the
session is idle is discarded, like real PG. In support of pgx's
liveness-poll shape, `pg_stat_activity` now reports an
extended-protocol statement's original text with `$1` placeholders
intact — the bound render inlined parameter values, which both leaked
them and made a `query like $1` poll match its own row.

#### Fixed

- `sql/pgserver.py` / `sql/session.py`: CancelRequest verifies the
  BackendKeyData secret and fires the target session's cancel event;
  stale cancels are discarded at the next statement's start.
- `sql/functions.py` / `sql/scalar.py`: `pg_sleep` waits interruptibly
  and raises 57014 on cancel, in FROM-less and per-row contexts alike
  (per-row numeric arguments arrive as Decimal128 and are now coerced);
  `pg_cancel_backend` cancels the target's running query without closing
  its connection.
- `sql/pgextended.py`: `pg_stat_activity.query` shows the prepared
  statement's original text (placeholders intact), not the
  parameter-inlined render.

### The legacy COPY ... BINARY keyword selects the binary format

`COPY t FROM STDIN BINARY` — the pre-9.0 bare-keyword spelling that pgx
still emits — parses as a value-less COPY parameter, which the option
reader did not recognise. The format stayed "text", so the client's
PGCOPY binary stream was fed to the text parser and rejected with
`22021 invalid byte sequence for encoding "utf-8"`. The same applied to
`COPY t TO STDOUT BINARY`.

The bare `BINARY` keyword now selects the binary format on both COPY
directions, riding the existing PGCOPY parse/encode machinery that the
`WITH (FORMAT binary)` spelling already used.

#### Fixed

- `sql/engine.py`: `_copy_options` recognises the value-less `BINARY`
  COPY parameter (legacy pre-9.0 syntax) as `FORMAT binary` for both
  COPY FROM and COPY TO.

### Stray COPY frames no longer poison the connection

Drivers that stream COPY data concurrently with the command — pgx's
`CopyFrom` pumps `CopyData` without waiting for the server's
`CopyInResponse` — kept sending frames after the COPY command itself had
already failed (a syntax error, a missing table). The wire server routed
those stray frames into the extended-protocol dispatch, answered
`08P01 unexpected message type 'd'`, and left the connection in a
discard-until-Sync state that a simple-protocol client can never clear:
one failed COPY wedged the connection for good.

Real PostgreSQL accepts and silently discards `CopyData`, `CopyDone`,
and `CopyFail` messages that arrive outside a COPY operation, exactly so
that this optimistic-streaming pattern stays safe. The wire server now
does the same, so a failed COPY reports its error and the connection
remains fully usable — including an immediately following valid COPY.

#### Fixed

- `sql/pgserver.py`: `CopyData` / `CopyDone` / `CopyFail` frames arriving
  outside a COPY operation are accepted and discarded, matching
  PostgreSQL's `PostgresMain` behaviour, instead of raising `08P01` and
  poisoning the extended-protocol state.

### Change streams resume when the server says a getMore failed resumably

Drivers decide whether to resume a change stream by looking for the
`ResumableChangeStreamError` label on the error a `getMore` returns. mongod
attaches that label from inside its change-stream machinery, which is why
the drivers' own test suites reach for the `failGetMoreAfterCursorCheckout`
failpoint to provoke one. SecantusDB's Python server ignored that failpoint
entirely — the `getMore` simply succeeded, no error was raised, and no
resume ever happened. The Rust server already handled it, so the two
servers disagreed about whether a stream should recover.

The distinction between the two failpoints is deliberate and is now pinned
by tests: `failGetMoreAfterCursorCheckout` with a resumable code resumes the
stream, while plain `failCommand` with the *same* code does not, because it
short-circuits before the change-stream path and carries only the labels the
failpoint itself named. Stamping the label unconditionally would silently
swallow errors that callers expect to see.

#### Fixed

- The Python server honours `failGetMoreAfterCursorCheckout` and stamps
  `ResumableChangeStreamError` on the sixteen error codes mongod treats as
  resumable, matching the Rust server's table exactly. libmongoc's
  `change-streams-resume-errorLabels` now passes.

### `generate_series` accepts an untyped parameter as a bound

`select generate_series(1, $1)` — with the parameter sent without a type OID,
as clients routinely do — was rejected outright. Nothing upstream had coerced
the value, because the wire never said what type it was, so the bound arrived
as text and the function refused it as a non-numeric range. Real PostgreSQL
infers the parameter's type from the argument position and reads it as an
integer.

The cost of this was out of all proportion to the gap. The pgx driver's test
suite calls that exact query in a helper that runs at the end of 66 of its
connection tests, to check the connection is still usable. Every one of those
tests failed at the final step, regardless of what it was actually testing —
which made a single missing coercion look like sixty-six unrelated bugs.

#### Fixed

- A numeric-looking text bound (or step) is parsed as a number, so
  `generate_series` works with untyped parameters. Bounds that are genuinely
  not numbers still raise, and `numeric` bounds are now accepted alongside
  int and float. The pgx `pgconn` package goes from 86 failures to 29.

### A full scan now drains in two round trips, like mongod

MongoDB's 101-document cursor default applies only to a query's first
batch — a `getMore` with no `batchSize` fills its reply with as many
documents as fit in 16MB. Both servers were reusing the 101-document
default on every `getMore`, so draining a collection cost one round
trip per 101 documents; a 10,000-document full scan paid ~100 round
trips where mongod pays two. That round-trip tax was the entirety of
the benchmark's remaining full-scan gap to mongod: with the corrected
default the Rust server's `find` full scan lands at parity with
mongod on the same box.

Both servers now fill an unspecified `getMore` batch up to mongod's
16MB budget (always at least one document, so a drain makes
progress), and an explicit `batchSize` is byte-capped the same way —
a batch stops before the document that would push the reply past
16MB, and the cursor stays open for the remainder. Tailable
change-stream cursors keep the small incremental default.

#### Fixed

- `getMore` without `batchSize` returned 101 documents per batch on
  both servers instead of filling the reply to mongod's 16MB budget —
  a full collection scan paid `count / 101` wire round trips instead
  of ~2. (`crates/secantus-commands/src/cursors.rs`,
  `src/secantus/commands.py::_get_more`)
- A `getMore` with an explicit `batchSize` could assemble a reply of
  unbounded size; batches are now capped at 16MB of documents with
  the cursor kept open for the remainder, matching mongod.

### INSERT rows can fill a column prefix, like Postgres

`INSERT INTO t VALUES (1, 2)` into a three-column table is legal
PostgreSQL — the row fills a prefix of the columns and the rest take
their defaults. The SQL server required an exact arity match, which broke
pgjdbc's rewritten batch inserts (`reWriteBatchedInserts=true` collapses a
repeated INSERT into one multi-VALUES statement without a column list)
and several JDBC tests that insert partial rows. Too many expressions is
still an error, and an explicit column list still requires an exact
match, both with PostgreSQL's wording. pgjdbc's
BatchedInsertReWriteEnabledTest (60) and TimeTest now pass in full.

#### Fixed
- A VALUES row shorter than the table's column list (no explicit column
  list) fills the leading columns; remaining columns take DEFAULT/NULL.

### Dollar quotes, nested comments, CTAS, stable now(), and the JDBC escape functions

A grab-bag of SQL-surface fixes driven by pgjdbc's StatementTest and
PreparedStatementTest. Dollar-quoted string literals now work in any
expression position — including digit-bearing tags (`$A0$`) and
tag-vs-content ambiguity (`$B$;$b$B$`) — and nested block comments
(`/* /* */ */`, which PostgreSQL nests) parse correctly. With
`standard_conforming_strings = off`, plain string literals honour
backslash escapes exactly like `E''` strings.

`now()` and `CURRENT_TIMESTAMP` are now transaction-stable: every call
in a statement (and across an explicit transaction block) returns the
same instant, as in PostgreSQL — so interval round-trips like
`extract(second from ((interval '3s' + now()) - now()))` are exact.
`CREATE [TEMP] TABLE … AS SELECT` ships with PG's `SELECT <n>` command
tag, and `TRUNCATE` resolves schema-qualified and session-temp table
names. The scalar-function surface behind JDBC's `{fn …}` escapes is
complete: the trig family (`acos` through `atan2`, hyperbolics, degree
variants), `replace`, numeric-aware `power` and `trunc(x, n)`, and
`to_char`'s word tokens (`Day` / `Dy` / `Month` / `Mon`).

#### Added
- Dollar-quoted string literals (`$$…$$`, `$tag$…$tag$`) in expressions.
- `CREATE [TEMP] TABLE … AS SELECT` (CTAS) with `IF NOT EXISTS`.
- Trig/hyperbolic scalar functions, `atan2`, `cot`, degree variants,
  `replace(text, from, to)`.
- `standard_conforming_strings = off` backslash-escape semantics.

#### Fixed
- Nested block comments mis-tokenized into stray operators.
- `now()` / `CURRENT_TIMESTAMP` drifted between calls in one statement.
- `power` / `trunc` raised TypeError on numeric-vs-double operand mixes.
- `TRUNCATE` dropped the schema qualifier, missing temp and
  schema-qualified tables.
- `to_char` rendered `'Day'` as `'5ay'` (the `D` token matched first).

### Plain json columns render compact, jsonb keeps its canonical spacing

PostgreSQL treats `json` and `jsonb` output differently: a `json` value's
text is preserved verbatim from input, while `jsonb` re-renders in its
canonical spaced form (`{"a": 1, "b": 2}`). SecantusDB rendered both from
the parsed stored value with jsonb's spacing, so a client that inserted
compact JSON into a `json` column — which is what every machine-serialized
payload looks like — got visibly different bytes back from `SELECT` and
`COPY TO`.

A plain `json` (oid 114) column now renders compact (`{"abc":"def"}`)
across the simple protocol, the extended protocol (text and binary
formats), and both COPY TO forms, reproducing typical input byte-for-byte;
`jsonb` keeps PG's canonical spacing. Full verbatim text preservation is
deliberately out of scope: the parsed-subdocument storage shape is what
lets json-path filters push down to indexed storage lookups, so a
hand-spaced `json` literal still re-renders normalised.

#### Fixed

- `sql/typemap.py` / `sql/pgserver.py` / `sql/pgextended.py` /
  `sql/engine.py`: plain `json` (oid 114) result columns render compact in
  DataRows (text + binary), `COPY table TO STDOUT`, and
  `COPY (SELECT …) TO STDOUT`; jsonb rendering is unchanged.

### The notification-push check stays off the hot path

The async LISTEN/NOTIFY push decides per message read whether a session
is a listener. That check took the server-wide notify-hub lock and
scanned the channel registry — on every message, on every connection, a
shared lock on the whole server's hottest path. It now reads a
per-session counter maintained by the hub at LISTEN / UNLISTEN time: a
plain attribute read, no lock, no scan.

#### Changed

- `sql/pgnotify.py` / `sql/session.py`: `is_listening` reads
  `Session.listen_count` (maintained under the hub lock by
  `listen` / `unlisten` / `unlisten_all`) instead of locking and
  scanning the channel registry per message read.

### 65535-parameter statements work; the 1 MB statement cap was too small

The parser guarded against oversized statements with a 1 MB length cap
(a parse-cost DoS guardrail). The premise — "1 MB is far above any real
query" — turned out to be false: a statement using the extended
protocol's full 65535 parameters (`values ($1::text), … ($65535::text)`,
the shape pgx's max-parameter tests exercise) is ~1.04 MB of SQL, and
real PostgreSQL accepts statements up to its 1 GB message limit. The cap
now stands at 16 MB — the same ceiling as the MongoDB document size —
which keeps parse cost bounded while accepting every legitimate shape.

`ParameterDescription` also now wraps its int16 parameter count for
65536-and-up parameters exactly like real PG does (`pq_sendint16`),
instead of crashing the encoder: preparing a 65536-parameter statement
succeeds server-side, with the client responsible for the
65535-parameter execution limit, matching PostgreSQL's behaviour.

#### Fixed

- `sql/planner.py`: `MAX_SQL_LENGTH` raised 1 MB → 16 MB.
- `sql/pgwire.py`: `parameter_description` wraps the int16 count for
  ≥65536 parameters instead of raising `struct.error`.

### Notifications reach idle connections without waiting for a query

LISTEN/NOTIFY delivery was piggybacked on the query cycle: a queued
notification was written to the listener's socket only when that
connection next issued a command. A client that just blocks reading the
socket — pgx's `WaitForNotification`, psycopg's `notifies()` — waited
forever, because real PostgreSQL pushes notifications to idle
connections asynchronously.

Listening sessions now wait for their next command in short slices and
flush queued notifications between them, from the connection's own
thread so socket writes stay serialized. Sessions with no LISTENs — the
overwhelming default — keep the pure blocking read, so there is no
busy-wake cost for ordinary connections, and the
idle-in-transaction-session-timeout deadline is preserved across the
poll slices.

#### Fixed

- `sql/pgserver.py`: the idle read loop pushes queued notifications to
  listening sessions (~250 ms delivery latency) instead of holding them
  until the next query cycle.
- `sql/pgnotify.py`: `NotifyHub.is_listening` — the poll applies only to
  sessions with at least one active LISTEN.

### Garbage SQL fails at parse time, and multi-statement errors stream partial results

sqlglot is a permissive parser: it reads `bad` as a column reference and
`SYNTAX ERROR` as an aliased expression, so preparing or executing a
non-statement quietly "succeeded" where real PostgreSQL raises a syntax
error. A bare expression at the top level is now rejected at parse time
with PG's `42601 syntax error at or near "..."` across every entry point
— simple protocol, extended-protocol Parse, and pipelined Parse.

A multi-statement simple query (`select 1; select 1/0; select 2`) also
now matches PG's streaming shape: the completed statements' results are
delivered before the ErrorResponse, and the statements after the error
never run. Previously a mid-batch error discarded the already-computed
results, so the client saw only the error.

#### Fixed

- `sql/engine.py`: top-level bare expressions raise `42601`; the
  expression-shaped commands sqlglot mis-parses the same way (`CLOSE`,
  `DISCARD`, `DEALLOCATE`) are exempted and keep working.
- `sql/pgextended.py`: the extended protocol's Parse applies the same
  check, so Prepare and pipelined SendPrepare error at parse time like
  real PG.
- `sql/engine.py` / `sql/pgserver.py`: a mid-batch `SQLError` carries the
  completed statements' results, and the wire layer renders them before
  the ErrorResponse, like real PG.

### pg_type grows real array-type rows

Every type that advertises a `typarray` now has the paired array-type row
in `pg_catalog.pg_type` — `_int4` with `typelem = 23` and friends, for
built-ins, enums, domains, composites and table row types — where before
the advertised oid resolved to nothing. The `typelem` column exists at
all now, `'pg_catalog.array_in'::regproc` strips the schema the way
PostgreSQL renders search-path-visible functions (so pgjdbc's standard
is-array probe matches), and `pg_class` carries a `relacl` column (null,
single-user server). pgjdbc's EnumTest enum-array resolution now works;
psycopg's `TypeInfo.fetch` finds array types by oid.

#### Added
- Array-type rows in `pg_type` (typname `_<elem>`, `typelem`,
  `typinput = array_in`) for every type with a `typarray`.
- `pg_type.typelem`, `pg_class.relacl` columns; `::regproc` casts.

### The pgjdbc weekly lane's red now means something

The weekly pgjdbc gauge returned gradle's raw exit code, and gradle exits
non-zero while any test fails — so with ~200 documented standing failures
the lane was red by construction and its conclusion carried no signal. The
lane now compares the run's failures against a committed baseline
(`pgjdbc_validation/baseline.json`, seeded from the latest weekly run) and
fails only on regression: a failing test the baseline doesn't list, or a
parameterized test failing more times than recorded. Runs with fewer
failures stay green and print the newly-passing entries so the baseline can
be tightened (`python -m pgjdbc_validation.baseline --update`).

#### Changed
- `pgjdbc_validation/runner.py` exits by baseline comparison, not gradle's
  raw code; a gradle failure that produced no test results at all is still
  a hard failure, and a truncated run still refuses a verdict (124).

#### Added
- `pgjdbc_validation/baseline.py` (compare / verdict / `--update` CLI) and
  the committed `baseline.json` (204 standing failures, 2026-08-11 weekly).

### Pipelined statements run in one implicit transaction, like Postgres

Statements a client pipelines before a single Sync now run in ONE
implicit transaction, exactly as PostgreSQL treats them: a mid-pipeline
error rolls back the earlier statements' effects, a clean Sync commits
them, and an explicit BEGIN inside the pipeline takes the transaction
over. pgjdbc's batch semantics depend on this — a failed batch must not
leave its earlier inserts behind — and its 184-variant BatchFailureTest
and 140-variant BatchExecuteTest both now pass in full. The first
statement of a pipeline retries internally on write-write races, so
single-statement autocommit behaves exactly as before.

Two describe/planner gaps closed alongside: SELECTs joining derived
VALUES tables (no real table anywhere) now Describe their shape instead
of answering NoData before emitting rows (a protocol violation pgjdbc
rejects), and CrystalReports' `{oj ((( … )))}` grouping-paren join
chains plan correctly. pgjdbc's OuterJoinSyntaxTest passes in full.

#### Fixed
- Extended protocol: implicit transaction from first pipelined statement
  to Sync (commit / rollback-on-error at Sync; BEGIN takeover;
  transaction-control and VACUUM-class statements exempt).
- Describe over joins of derived VALUES tables returns the row shape.
- Grouping parens around join chains unwrap through multiple layers, and
  an aliased VALUES parsed as a Table-wrapped node normalizes.

- A mixed-mode lost-update race the implicit transaction exposed: a
  pipeline's Sync-commit could land inside a bare autocommit computed
  update's read-compute-write window and be silently overwritten (every
  statement still reported `UPDATE 1`). Computed updates outside a
  transaction block now run their whole read-compute-write as one
  storage snapshot transaction, so a mid-window commit surfaces as a
  write conflict and the statement retries from a fresh read. Pinned by
  a deterministic regression test.

#### Changed
- The feature is ON by default (`SECANTUS_PIPELINE_TXN=0` is an escape
  hatch).

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

### Rust server: renaming a huge collection can no longer wedge the engine

`renameCollection` re-keyed every row in one WiredTiger statement
transaction — the same unbounded-dirty-content livelock class as the
(already fixed) one-transaction drop purge, and the last DDL path that
could wedge the engine on a collection larger than the cache's dirty
budget. The rename is now a chunked two-phase move that reuses the drop
tombstones: tombstone the destination, copy the rows across in bounded
transactions (fresh RecordIds preserving insertion order, index entries
and unique claims rebuilt per batch), then one small switch transaction
registers the destination, unregisters the source, moves the tombstone,
and emits the rename oplog entry, and the source's rows purge in bounded
batches. Both crash windows recover through the existing open-time
tombstone recovery — on either server — as a plain drop: a crash
mid-copy purges the partial destination (the rename never happened); a
crash after the switch purges the leftover source (it did). A
deterministic regression test renames a collection larger than a small
cache — the shape that previously spun forever.

#### Fixed
- Rust server: `renameCollection` of a collection larger than the WT
  cache's dirty budget livelocked the engine (one unbounded re-key
  transaction + unbounded write-conflict retry); now a chunked two-phase
  move with crash-safe tombstone recovery. Inside a user transaction the
  atomic single-transaction path remains, bounded by the transaction
  dirty-budget guard. The batched copy also drops the old whole-collection
  in-memory materialization.

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

### Concurrent sessions get their own temp-table namespaces

Postgres gives every backend a private `pg_temp_<n>` schema, so two open
connections can each `CREATE TEMPORARY TABLE bar` without colliding.
SecantusDB's SQL server shared one namespace: the second concurrent create
failed with `42P07 relation "bar" already exists`, a real divergence that
connection-pooled applications and driver test suites hit immediately.

Each session now allocates its own `pg_temp_<n>` namespace the first time it
creates a temp table. Unqualified names resolve against the session's temp
namespace ahead of `public` — so a temp table shadows a permanent one of the
same name, exactly like real Postgres — and an explicit `pg_temp.<name>`
qualifier resolves to the session's own namespace (`CREATE TABLE pg_temp.t`
creates a temp table, and `CREATE TEMP TABLE` aimed at any other schema is
rejected with `42P16`). COPY and extended-protocol Describe resolve through
the same path, temp-table SERIAL sequences are per-session too, and
`pg_class` / `information_schema.tables` report the bare relation name under
its session's temp schema.

#### Fixed

- `sql/session.py`: per-session `pg_temp_<n>` namespace, allocated lazily on
  first temp-table creation (pid-seeded so a crashed process's stale entries
  can't collide with a new one's).
- `sql/planner.py`: `qualify_from_search_path` resolves the session temp
  namespace first (unless `pg_temp` is placed explicitly on `search_path`),
  rewrites `pg_temp.<name>` to the session's namespace, and
  `qualify_temp_create_target` homes `CREATE TEMP TABLE` targets there;
  `pg_table_is_visible` lowers against bare relnames.
- `sql/engine.py`: `copy_plan` and extended-protocol Describe apply the same
  search-path / temp-namespace resolution as execution.
- `sql/executor.py`: duplicate temp-table errors name the bare relation
  (`relation "bar" already exists`); error diagnostics report the session's
  actual `pg_temp_<n>` schema.

### Read and write concerns inside a transaction are refused, not ignored

A transaction's concerns are settled when it begins: its read concern rides
the statement that starts it, and its write concern belongs to the commit.
Attaching either to a statement in the middle is meaningless, and real
MongoDB says so with an `InvalidOptions` error. SecantusDB accepted them and
quietly did nothing, so a caller could believe a statement had run at a
durability or isolation level it never had.

Drivers already refuse this on the client side, which is why no driver test
suite ever caught it. It surfaces for anyone issuing raw commands — the one
audience with no other way to tell us apart from a real server.

#### Fixed

- A `writeConcern` on an in-transaction statement is rejected with
  `InvalidOptions` (72) on both servers, matching MongoDB's wording.
- A `readConcern` on a statement that continues (rather than starts) a
  transaction is likewise rejected. The starting statement may still carry
  one, since that is how a transaction's read concern is chosen.

### JDBC clients get their real time zone

A pgjdbc connection tells the server its JVM time zone through a `TimeZone`
**startup parameter** — and the PG server dropped it, leaving every JDBC
session on UTC. For clients west of Greenwich that shifted date reads back a
day (`1950-02-07` came back `1950-02-06`). Startup GUC parameters are now
applied and echoed in the opening ParameterStatus burst, the way PostgreSQL
treats them.

Four smaller conformance gaps closed with it: `SET timezone = 'gmt-3'` now
reports the normalized `GMT-3` spelling (pgjdbc's ParameterStatus parser is
case-sensitive and silently fell back to UTC on the lowercase echo);
POSIX-style zone specs accept minutes (`GMT+3:30` is UTC-03:30, pgjdbc's
half-hour-zone test); `tstz::text` casts render the session-zone offset and
`tz::text` renders PostgreSQL's `+01` spelling; and a BC-era timestamptz
literal without an offset is stamped with the session zone's offset so the
stored instant is correct. pgjdbc's TimezoneTest is now **16/16** and
DateTest **192/192** — and this time measured with a fixed tally (the
release-note claim that DateTest was already clear traced to an XML-parsing
bug in the measurement script, not the server).

#### Fixed
- `TimeZone` (and other reportable GUCs) sent as startup parameters are
  applied to the session and reported in the initial ParameterStatus burst.
- `TimeZone` values normalize to PostgreSQL's reported spelling
  (`gmt-3` → `GMT-3`).
- POSIX GMT/UTC offsets accept minutes and seconds (`GMT+3:30`).
- `timestamptz::text` renders the session-zone offset; `timetz::text`
  renders whole-hour offsets as `+01`.
- An out-of-range (BC) timestamptz literal without an offset takes the
  session zone's offset instead of UTC.

### SELECT * over a USING join merges the join column, like Postgres

`SELECT * FROM a JOIN b USING (k)` now returns `k` once — from the left
side (the right for RIGHT joins, `COALESCE` for FULL) — followed by each
source's remaining columns, exactly as PostgreSQL expands it. Previously
the star emitted the column once per side, a long-pinned divergence. The
fix is one AST rewrite before the USING-to-ON desugar, not a change to
every star-expansion path. `tbl.*` items over joins also work now (they
previously crashed with `column "*" does not exist`), and — matching
Postgres — `tbl.*` does NOT merge; only the bare `*` does.

#### Fixed
- Bare `*` over `USING` joins merges the join columns (left / right /
  coalesce per join side; chained USING joins in the all-inner case).
- `tbl.*` in a join select expands to the table's columns instead of
  crashing.

### `validationLevel` finally does something

A collection can tell the server how strictly to apply its validator, and
SecantusDB recorded the answer and then ignored it. `validationLevel: "off"`
— an explicit request for no validation at all — still had every write
checked. `"moderate"` behaved like `"strict"`, which defeats the reason the
level exists: it lets you attach a validator to a collection that already
holds rows predating it, without freezing those rows. Under our behaviour
those legacy documents became un-updatable.

Both levels now work, on both servers. `off` disables validation outright.
`moderate` exempts a document that ALREADY failed the validator from
update-time checks, while a document that currently satisfies it is still
held to it — so an update can no longer turn a valid document invalid, and
inserts are validated as before.

#### Fixed

- `validationLevel: "off"` disables document validation on the Python and
  Rust servers.
- `validationLevel: "moderate"` exempts already-invalid documents from
  update-time validation on both servers, on the single-document and
  multi-document update paths and through `findAndModify`.

## [0.6.0b10] — 2026-08-13

### Large objects over Fastpath, a drop that can't wedge the engine, and TCP_NODELAY everywhere

This release closes two of the oldest gaps a PostgreSQL client could hit.
The PG server now implements the Large Object API the way pgjdbc's
`LargeObjectManager` (and therefore JDBC `Blob`/`Clob`) actually drives it —
the Fastpath sub-protocol dispatching `lo_open` / `loread` / `lowrite` and
friends by their real `pg_proc` OIDs, backed by chunked sparse storage that
joins the session's transaction. Around it landed the pieces callable
statements need: user-defined functions in FROM position typed by their
declared return type, Describe that derives result shapes without executing
a side-effecting function body, PostgreSQL's void-argument convention for
the JDBC OUT-parameter slot, and plpgsql `RAISE`. Four pgjdbc test classes
that were previously zeroed — BlobTest, BlobTransactionTest,
CallableStmtTest, CleanupSavepointsWithFastpathTest — now pass in full.
(A claim in the original release notes that DateTest was cleared at 192/192
traced to an XML-parsing bug in the measurement script — the real remaining
failures are fixed in the next release's time-zone slice.)

On the storage side, dropping a very large collection could livelock the
Rust server: the whole row purge ran as one WiredTiger transaction, and once
its delete volume exceeded the cache's dirty budget the engine rolled it
back for cache pressure and the retry loop re-ran it forever — an eviction
storm that survived client disconnects and ignored SIGTERM. Drops are now
chunked and two-phase, with a tombstone that makes a crash mid-purge
recover cleanly at the next open, on both servers. Both wire servers also
now set TCP_NODELAY on every connection, which removes Linux delayed-ACK
stalls worth ~40ms per round-trip — one pgjdbc generated-keys batch test
went from 41.5 seconds to 0.2. The performance and concurrency reports on
secantusdb.com were re-measured from scratch with hardened harnesses, and
the Rust server gained a background oplog pruner and a 4G embedded cache
default that keep sustained write throughput off the request path.


### Document validation you can actually stage, and an admin UI that reaches the rest of the server

Setting `validationAction: "warn"` on a collection is how you stage a
validator against live traffic — mongod logs the violations and stores the
document anyway. The Python server accepted the option, reported success,
and then rejected the write with code 121 regardless, so the one workflow
the setting exists for was the one it broke. `collMod` had the same shape
of problem from the other end: it replied `ok: 1` to `validationAction`
and `validationLevel` and quietly discarded both, leaving callers
convinced they had relaxed enforcement that was still fully armed. Both
are fixed, on every write path, and the Rust server — which already got
this right — is now matched exactly.

The admin UI also stopped hiding features the server has shipped for a
while. Collections can be created with validators and capped options,
modified with `collMod`, and renamed (across databases, with an optional
`dropTarget`); custom roles can be created and dropped. The change-stream
page gained the options that make it a real debugging tool: `fullDocument`
and `fullDocumentBeforeChange`, all three start points, and a pipeline
filter — plus a **Resume from here** button on every event, which finally
closes a loop the page had left open by offering a "Copy resume token"
button with nowhere to paste the token.

#### Added

- Admin: create / `collMod` / rename panels on the collection list, and
  create / drop for custom roles on `/roles`. Options are entered as one
  Extended-JSON document, so any option the target server understands
  works without waiting for a matching form field.
- Admin: `fullDocument`, `fullDocumentBeforeChange`, `resumeAfter`,
  `startAfter`, `startAtOperationTime` and pipeline controls on the
  change-stream page, with a **Resume from here** action per event.
  Options round-trip through the URL, so a shared link reproduces the
  same stream.

#### Fixed

- `validationAction: "warn"` and `"off"` now accept violating writes
  instead of rejecting them with `DocumentValidationFailure` (121), on
  insert, update, replace and `findAndModify` alike. Only the default
  `"error"` rejects.
- `collMod` now applies `validationAction` and `validationLevel` rather
  than accepting and discarding them.
- Admin: a rejected change-stream option is reported as a readable error
  frame instead of a bare websocket close, and the message is no longer
  overwritten by the disconnect handler that followed it.

### Advisory locks now actually exclude

`pg_advisory_lock` and friends used to be session-local bookkeeping that
always granted — two connections could both "hold" the same exclusive lock,
so leader-election and migration-fencing patterns (alembic's lock, cron
fencing) silently provided no mutual exclusion. The PG server now runs a
server-wide advisory-lock table shared by every connection: exclusive and
shared modes with PostgreSQL's grant rules, re-entrant holds, blocking
`pg_advisory_lock` waits with deadlock detection (`40P01 deadlock
detected`), truthful `pg_try_*` results, and release on unlock, at
transaction end for `xact` locks, and when a connection ends.

#### Added

- `secantus.sql.pgadvisory.AdvisoryLockHub`: the server-wide lock table,
  attached to every wire session; per-session state remains the `pg_locks`
  reflection layer. Pinned by cross-connection tests covering exclusion,
  blocking waits, shared/exclusive interaction, deadlock detection,
  transaction-end and connection-teardown release — including a wire-level
  two-connection psycopg test.

### Large batch inserts no longer risk a storage livelock

A single insert message can carry up to 48MB of documents, and the Python
server used to write the entire batch — document rows, their full-document
oplog entries, and every index entry, roughly two to three times the message
bytes — inside one WiredTiger transaction. A transaction's dirty content is
unevictable, so a large enough batch could pin the storage cache past its
dirty-stall threshold and livelock the engine: every thread drafted into
eviction, nothing evictable, and only the stuck writer's own commit able to
free the cache. This is what wedged the mongo-rust-driver conformance
gauge's `large_insert` test (35,000 tweet-sized documents) in weekly CI —
and once wedged, the server never recovered.

Inserts now commit in bounded chunks of at most 1,000 documents or 4MB per
statement transaction, mirroring what real mongod does with its internal
insert batches. MongoDB batch inserts are per-document atomic only — a batch
has never been all-or-nothing — so the extra commit points are invisible to
clients: ordered batches still stop at the first error with the correct
per-document index, unordered batches still report every error, and capped
collections still never evict documents from the batch being inserted.

#### Fixed

- `secantus.storage.insert`: one wire batch no longer runs as one WiredTiger
  statement transaction; chunks are bounded at 1,000 docs / 4MB with the
  write-conflict retry scoped per chunk. Reproduced and pinned by
  `test_storage.py::test_large_batch_insert_survives_a_small_cache` (35k ×
  1.1KB documents against a deliberately small 128M cache) plus
  ordered/unordered cross-chunk semantics tests.

### A finished job no longer shows an empty log

The opsboard runs each job as a detached child on a pseudo-terminal and
tees everything it prints to a logfile the UI tails. That tee loop asked
the pty whether it had anything to read and, on a quiet answer, left as
soon as the child had exited. A child that wrote its output *and* exited
inside that window left its bytes sitting in the pty buffer, and leaving
discarded them — so the job finished with exit 0 and a completely empty
log. The shorter the job, the likelier it was to lose everything it said.

The loop now drains the buffer before it leaves. The comment that used to
justify the old behaviour ("a timed-out select with the child reaped means
everything has been drained") was simply untrue, and is gone.

#### Fixed

- `jobkit`'s pty tee no longer discards output written in the window
  between polling the terminal for readability and observing that the
  child has exited. This is the second race of its kind on this path; the
  regression test forces the losing interleaving deterministically rather
  than relying on timing.

### Numeric division carries PostgreSQL's result scale

Dividing numerics now produces the display scale real PostgreSQL derives:
`SELECT 5.52 / 2.4` answers `2.3000000000000000` (scale 16), `1/3::numeric`
answers `0.33333333333333333333`, and a driver reading
`getBigDecimal().scale()` sees exactly what it would on Postgres. The rule is
`select_div_scale` from Postgres' own `numeric.c`, ported into the numeric
division path and verified against a live PostgreSQL 14.13 across twenty
division cases — every text render byte-identical. Values were already exact
after the numeric-exactness work; this closes the last recorded divergence,
the displayed scale. Integer division still truncates and float8 mixes still
coerce to float8, as before.

#### Fixed

- `secantus.sql`: `numeric / numeric` results are quantized to PG's derived
  division scale (`typemap.numeric_div`, half-away-from-zero rounding).
  Pinned by `tests/test_sql_numeric_div_scale.py` — a twenty-case battery
  whose expectations are byte-exact captures from PostgreSQL 14.13.

### The oplog prune moves off the write path entirely

Under sustained write load the oplog reaches its entry cap within seconds,
and from then on the opportunistic prune has to delete rows as fast as
they arrive. That sweep — a key merge across the shard tables, PITR
archiving, per-row deletes — ran inline on whichever thread crossed the
cadence: the writer itself in the default mode (measured at roughly a
third of the whole insert path under cap pressure), or a drainer in
async mode. A dedicated background pruner now owns the sweep in both
modes; write paths just set a flag. mongod does the same job on its
OplogCapMaintainerThread, for the same reason.

Oplog reads got cheaper alongside: shard tables are created lazily and
most never exist, but every oplog merge probed all sixteen plus the
legacy table, paying a failed cursor-open per absent table per read. A
shard-existence mask seeded at open skips them outright. The embedded
Rust `Storage` library also now defaults to the same 4G WiredTiger
cache *cap* as the daemon and the Python handle (the cache fills
lazily, so small test instances stay small) — closing the gap where a
library user hit eviction pressure at 256M that the daemon never would.

Measured on the standard concurrency methodology (8 KiB docs, batch
100, sync oplog, interleaved A/B): **+7.7% single-writer and +2.9% at
eight writers**, with every single-writer rep separating cleanly.

#### Changed

- Rust storage: the opportunistic oplog prune runs on a dedicated
  background pruner thread (signalled by the write-path cadence, with a
  10s retention backstop) instead of inline on writer / drainer
  threads. Explicit `prune_oplog` calls are unchanged (synchronous).
- Rust storage: oplog merges (reads, floor, prune scans, archiving)
  skip shard tables known absent via an existence mask seeded at open.
- Rust storage: `Storage::open`'s default WiredTiger cache is a 4G cap
  (was 256M), matching the daemon and the embedded Python handle.

### Fresh performance and concurrency numbers — and honest harnesses

Both benchmark reports are re-measured on current code (post
TCP_NODELAY, batched sequences, and the parse cache). Per-operation
latency: the Rust server runs at **0.7×–2.2× of mongod**, with three
workloads now beating mongod outright (change-stream drain 0.7×,
delete and single-stage `$group` 0.9×); the Python server spans
1.2×–24×. Write scaling is unchanged in shape and confirmed healthy:
the Rust server scales monotonically to **2.5× at eight writers
(~93k docs/s fully durable)**, the async oplog stack reaches ~107k.

Getting trustworthy numbers surfaced two real defects. The concurrency
harness handed writer 0 a `drop` that raced the other writers' insert
stream — a drop starved behind continuous batches for the whole window,
died summary-less on SIGTERM, and rows silently averaged a dead writer,
manufacturing a 3.4× phantom regression. Writers now target fresh
per-row collections (no drops near the measurement), install signal
handlers before any I/O, and a missing writer summary fails the run
instead of shipping a corrupt row. Second, dropping a heavily-churned
collection can wedge the Rust server behind a WiredTiger eviction storm
that survives client disconnect and SIGTERM — captured with native
stacks and filed in `tasks/backlog.md` for its own slice.

#### Added

- `bench/latency_chart.py` + `bench/results/latency.json`: the latency
  chart, markdown table, and site table are now regenerated
  mechanically from one results file (they were hand-edited SVGs).

### The PG server now ships a default idle-in-transaction timeout

A PostgreSQL client that opens a transaction and then goes quiet — a failed
test that never rolls back, a leaked pooled connection — used to pin the
storage engine's oldest snapshot indefinitely. WiredTiger then had to keep
every subsequent write's history reachable, so each operation got slower in
proportion to total churn until a large statement (a 100k-row TRUNCATE in
pgjdbc's own suite) stalled in page reads and wedged the whole server. That
single mechanism was the root cause of the pgjdbc conformance lane's
two-hour hang.

`SecantusPGServer` now applies a server-config default of 120 seconds for
`idle_in_transaction_session_timeout` (PG ships 0/disabled, but PG's MVCC
degrades gracefully where WiredTiger's cache-bound history does not). The
GUC hierarchy is faithful: a session `SET` overrides the server default,
`SET … = 0` opts out entirely, `RESET` falls back to the server value, and
`SHOW` reports the effective setting. The `secantusd-py-pg` daemon grows a
matching `--idle-in-transaction-timeout` flag.

#### Added

- `Session.server_gucs` — a postgresql.conf-tier defaults layer between
  session `SET` overrides and the built-in GUC defaults, honoured by
  `get_setting`, `SHOW`, `SHOW ALL`, and `RESET`.
- `SecantusPGServer(idle_in_transaction_timeout_s=…)` constructor knob and
  the `--idle-in-transaction-timeout` daemon flag (default 120s, 0 disables).

#### Fixed

- An abandoned open transaction on a live connection no longer degrades all
  later writes without bound (linear-with-churn slowdown, ending in a
  server-wide page-read stall). Idle-in-transaction sessions are terminated
  with PG's own FATAL 25P03 after the timeout, unpinning the snapshot.

### PostgreSQL Large Object API over Fastpath

The PG server now implements PostgreSQL's Large Object surface the way
pgjdbc's `LargeObjectManager` (and therefore JDBC `Blob`/`Clob`) drives it:
the Fastpath sub-protocol ('F' FunctionCall / 'V' FunctionCallResponse)
dispatching `lo_open` / `lo_close` / `loread` / `lowrite` / `lo_lseek` /
`lo_creat` / `lo_create` / `lo_tell` / `lo_unlink` / `lo_truncate` and their
64-bit variants by their real `pg_proc` OIDs, reflected into
`pg_catalog.pg_proc` so drivers can resolve them by name. Object bytes live
in chunked, sparse per-database collections (a 2GB `lo_truncate` extension
stores nothing and reads back as zeros, like PG's own representation), and
reads/writes join the session's open transaction so `ROLLBACK` discards
`lowrite` data. `lo_creat` / `lo_create` / `lo_unlink` are also SQL-callable.

Around it, the pieces pgjdbc's CallableStatement and Blob tests need: a
user-defined function call in FROM position (`select * from f($1) as
result`, pgjdbc's rewrite of `{? = call f(?)}`) evaluates as a one-row
source typed by the function's declared return type; extended-protocol
Describe derives that shape from the catalog **without executing the
function body** (a side-effecting UDF in a pgjdbc batch previously ran
twice — once at Describe, once at Execute); a NULL parameter declared
`void` (oid 2278) is dropped from the call's argument list, matching PG's
accommodation of the JDBC OUT-parameter slot; plpgsql gains `RAISE`
(NOTICE/WARNING/etc. flow to the wire as NoticeResponse, EXCEPTION raises
`P0001`); and contrib/lo's `lo_manage` trigger DDL is accepted as a
recognized no-op. pgjdbc's `BlobTest` (28), `BlobTransactionTest`,
`CallableStmtTest` (14), and `CleanupSavepointsWithFastpathTest` (10) all
pass fully — all four were previously zeroed.

#### Added
- `secantus/sql/largeobjects.py`: chunked sparse LO store + Fastpath
  dispatch with PG's real `pg_proc` OIDs; per-session descriptors.
- Fastpath sub-protocol handling in the PG wire server
  (`parse_function_call` / `function_call_response`).
- plpgsql `RAISE` statement (levels, `%` formatting, notice delivery over
  both simple and extended protocol).
- UDF and built-in function calls (`now()`, `version()`) as one-row FROM
  sources, typed by declared return type.

#### Fixed
- Extended-protocol Describe no longer executes side-effecting UDF bodies
  to derive the result shape (pgjdbc batched `{call f(?)}` ran every
  insert twice).
- `Storage.use_user_transaction` is re-entrant (nested entry from a
  SQL-callable `lo_creat` inside a transactional INSERT no longer breaks
  the outer transaction).

### Read-only transactions are enforced; isolation level round-trips

Writes inside a read-only transaction now fail with PostgreSQL's 25006
(`cannot execute INSERT in a read-only transaction`) — whether the
read-only-ness came from `BEGIN READ ONLY`, `SET TRANSACTION READ
ONLY`, or `SET SESSION CHARACTERISTICS AS TRANSACTION READ ONLY`. DML,
DDL, TRUNCATE, MERGE, and GRANT are gated; reads are untouched. And
`SHOW TRANSACTION ISOLATION LEVEL` — the multi-word spelling pgjdbc's
`getTransactionIsolation` issues verbatim (previously resolving to an
unknown GUC and an empty string) — now reports the level a
`SET SESSION CHARACTERISTICS` planted, as does `SHOW TIME ZONE`.

pgjdbc: ConnectionTest 15/15 (was 12/15), DatabaseMetaData
TransactionIsolationTest 14/14 (was 8/14), AutoSaveTransactionSettings
4/6 (was 0/6). Known divergences, none gauge-exercised, recorded in
`tasks/backlog.md`: temp-table writes are also blocked (PG allows them
under read-only), and `SELECT … FOR UPDATE` / `nextval()` are not yet
gated (PG blocks both).

### A conformance run that was cut short no longer reads as a clean sweep

The pgjdbc gauge wiped its results directory at startup and only aggregated
them once Gradle returned, so a run that hit the wall-clock budget reported
zero tests — which looks identical to a flawless run at a glance, and was read
that way once. A truncated run now keeps whatever did complete, records that it
was cut short, and exits with the conventional timeout status.

The report generator refuses to render a truncated run at all. That is the
important half: a partial run's per-class numbers are every bit as correct as a
complete one's, and only the *set of classes* is short — so publishing it
produces a healthy-looking pass rate quietly measured over less of the suite.
There is no caveat that reliably survives being pasted into a summary, so the
artifact simply isn't produced.

The budget itself is raised and made overridable via `SECANTUS_PGJDBC_TIMEOUT`,
because CI hardware runs several times slower than a development machine and
the suite legitimately grew once the crashes that used to end tests in
milliseconds were fixed.

#### Added

- `SECANTUS_PGJDBC_TIMEOUT` overrides the gauge's Gradle budget (default two
  hours, up from one).

#### Fixed

- A timed-out pgjdbc gauge aggregates partial results and reports the run as
  truncated instead of silently summarising zero tests.
- `generate_report` refuses to publish a conformance rate computed from a
  truncated run.

### The pgjdbc lane runs sharded — CI wall clock drops from ~70 to ~20 minutes

The pgjdbc conformance gauge runs ~5,500 JUnit tests over a real wire and
took the better part of an hour as a single CI job. The lane now fans out
as four parallel jobs, each running a deterministic round-robin quarter of
the class list (the vendored suite stays byte-for-byte unmodified — only
Gradle's `--tests` selection differs per shard), and a merge job combines
the shards' JUnit results into the same single conformance report.

The merge enforces the same publish discipline as the truncation guard: a
missing, duplicate, or truncated shard refuses the report outright rather
than rendering a pass rate measured over part of the suite. `only=pgjdbc`
dispatches select all four shards; a single shard is addressable as
`only=pgjdbc-1`. Locally, `invoke validate-pgjdbc` is unchanged (one full
run), with `--shard K/N` + `validate-pgjdbc-report` available for the
split flow.

#### Changed

- `.github/validate-lanes.json` gains lane `group`s; the plan job's filter
  matches groups as well as names.
- `pgjdbc_validation.runner` honours `SECANTUS_PGJDBC_SHARD=K/N`;
  `generate_report` merges a complete shard set (refusing anything less);
  shard-math and merge-guard tests in
  `tests/test_pgjdbc_gauge_truncation.py`.

### Tailable cursors wait, rewritten resume tokens are refused, and `_id` leads again

Three unrelated fidelity gaps, each found by mongo-php-library's suite
asking a question no unit test had thought to ask.

A `TAILABLE_AWAIT` cursor is supposed to park on the server until data
arrives or `maxAwaitTimeMS` expires. SecantusDB's capped-collection
tailables returned in about a fifth of a millisecond, because the wait's
wake condition was keyed on a change-stream position counter that plain
tailables never maintain — leaving it permanently satisfied. Clients
polling a capped collection were spinning instead of waiting.

A change-stream pipeline may not tamper with an event's `_id`: that field
is the resume token, and an altered one silently breaks resumption. The
server already rejected a pipeline that *removed* it, but a pipeline that
*rewrote* it passed straight through, and the error surfaced client-side
in the driver rather than from the server. Both are fatal now, as they are
in mongod.

Finally, a replacement-style update put the preserved `_id` at the end of
the stored document rather than the front. BSON keeps field order on the
wire, so the bytes a client got back differed from mongod's for the same
operation — invisible until something compared raw documents, which is
exactly what the PHP codec tests do.

#### Fixed

- `getMore` on a capped-collection tailable cursor with `awaitData` now
  blocks for up to `maxTimeMS` instead of returning immediately.
- A change-stream pipeline that modifies (not only removes) an event's
  `_id` now fails server-side with `ChangeStreamFatalError`, matching the
  Rust server, which already did this.
- Replacement updates place `_id` first in the resulting document, on both
  the Python and Rust engines.

The mongo-php-library gauge goes from 42 failures to 1 — and the one that
remains is a text-index test, a feature that is explicitly out of scope.

### updateMany and deleteMany commit in bounded chunks on the Python server too

The Python server gains the same bounded multi-document write transactions
the Rust server just did: updating or deleting everything a broad filter
matches no longer runs as a single WiredTiger transaction whose unevictable
dirty content grows with the matched set. Chunks re-read their documents
inside their own transaction, every document is transformed exactly once
even across conflict retries, and single-document writes, upserts, and
writes inside multi-document transactions are unchanged. With this, the
storage-engine livelock class is closed on both servers across all three
surfaces: batch inserts, multi-document updates and deletes, and
multi-document transactions.

#### Fixed

- `secantus.storage`: `update_matching` (multi) and `delete_matching`
  (unbounded) run chunked statement transactions (≤1000 docs / ≤4MB each)
  instead of one unbounded transaction — mongod-faithful, since updateMany
  and deleteMany are per-document write units and documented non-atomic.
  Pinned by a 35,000-document rewrite + deleteMany against a deliberate
  128M cache, exactly-once `$inc` across chunk boundaries, and unchanged
  bounded paths.

### The pymongo gauges now separate "unsupported" from "broken"

Both pymongo gauges — the sync suite and the `AsyncMongoClient` one — had
a handful of red tests that were never going to go green, because every
one of them exercises something SecantusDB deliberately does not
implement: hashed indexes, text indexes, and `$where`, which needs the
embedded JavaScript runtime mongod ships and SecantusDB does not. The
server already answers each with a faithful "not supported" error; the
tests fail because they asked, not because anything is wrong.

Two of them are worth naming precisely, because their titles suggest
otherwise. `test_maxtime_ms_message` and `test_to_list_csot_applied` are
about timeouts, not about `$where` — they merely use `$where` to make a
query slow enough to time out. Since the query is rejected up front, they
never reach the behaviour they are named for. They are recorded as
unverified rather than as passing: the gauge tells us nothing about
maxTimeMS message shape or CSOT either way.

#### Changed

- The six pymongo / pymongo-async failures are now classified as expected,
  each with its rationale, so the summary counts them separately from
  failures that need a fix. Both gauges report zero actionable failures.
- The async gauge is wired to the shared expected-failures list; it runs
  the same upstream tests and hit the same gaps under different node IDs.

### A 2dsphere index reports its format version

MongoDB stamps every `2dsphere` index with the index format version it was
built at, and drivers read it back through `listIndexes` — the PHP library
exposes it as `IndexInfo::is2dSphere()` and `$index['2dsphereIndexVersion']`.
The Rust server left the field off entirely, so a client asking which 2dsphere
format an index used got no answer. It now reports version 3, matching both
the Python server and MongoDB 3.2 onwards. A `2d` index carries no such field
and still doesn't.

#### Fixed

- `listIndexes` reports `2dsphereIndexVersion` for a `2dsphere` index on the
  Rust server.

### A change stream survives a transient error, as it should

A change stream is meant to be durable across a hiccup: when the server hits a
transient problem mid-stream, the client is supposed to quietly reconnect and
carry on from where it left off. That never happened here, because the server
gave the client no way to tell a transient failure from a fatal one.

MongoDB marks the errors a change stream may recover from, and drivers act on
that marking alone — never on the error code by itself. The Rust server sent
neither the marking nor, in fact, the errors: the mechanism test suites use to
provoke a mid-stream failure was accepted and then ignored, so nothing ever
went wrong to recover from. Both halves are now in place, so a change stream
interrupted by a transient error resumes instead of surfacing the failure to
the application.

The distinction MongoDB draws is preserved: an error injected inside the
change-stream path is recoverable, while the same error code injected at the
command boundary is not, and a fatal error stays fatal.

#### Fixed

- A change stream resumes after a transient server error rather than failing.

### A change stream stops forgetting where it got to

Reading to the end of a change stream threw away the position it had reached.
While events were arriving, the stream reported each one's position faithfully;
the moment a read came back empty it replaced that with a bare positional
marker — one that named neither the collection nor the document last seen. A
client that then reconnected resumed from something less precise than it had
already been told, and the token it had been carefully tracking went backwards.

The position now only ever moves forward. An idle stream still advances as the
server's clock does, so a quiet collection doesn't strand a reader behind the
oplog window, but it never rewinds past an event already delivered.

Separately, `$currentOp` did not report which application a connection belonged
to, so tools that look up their own operation — by the `appName` given in the
connection string — found nothing to inspect.

Both were invisible until now: the C++ driver's suite is the one that covers
them, and it had never been run against this server because its tests bind a
fixed port.

#### Fixed

- A change stream's resume position no longer regresses to a positional marker
  when a read returns no events.
- `$currentOp` reports `appName` and the connection's driver metadata.

### Transactional DDL and consistent scans for the Rust server

The Rust server's storage engine now runs every namespace-level DDL —
createIndexes, dropIndexes (single and `"*"`), create, drop and rename
collection, and dropDatabase — inside the same per-statement WiredTiger
transaction machinery its CRUD path has used since the collection-locks work.
Registry rows, index entries, collection options and the DDL's oplog entry now
commit or vanish together, so a crash mid-DDL can no longer strand orphan
index-entry rows behind a missing registry row. dropDatabase commits one
transaction per collection — the same unit real mongod uses — so a huge
database can't blow the storage cache with a single monolithic transaction.

That atomicity also closes the long-standing DDL-vs-scan wobble: a lock-free
read racing a drop or rename could previously return a partial result set,
splicing rows read before the DDL with the post-DDL view. Reads now run under
a seqlock-style namespace-generation check — DDL holds the generation counter
odd for its duration, and a scan that observed an odd or moved generation
re-runs against the settled state, so every result is a point-in-time answer.
A concurrent-stress test pins the new invariant: scans racing drops and
renames observe the full collection or none of it, never a partial splice.

Two smaller items land alongside: single-document updates no longer clone the
post-image document unless the caller actually asked for it (only
`findAndModify` does — plain updates skip a full per-document clone), and the
`anyhow` dependency moved past RUSTSEC-2026-0190 in all four lockfiles.

#### Changed

- `secantus-storage`: `create_collection[_with_options]` / `drop_collection` /
  `drop_database` / `rename_collection` / `create_index` / `drop_index` /
  `drop_all_indexes` wrap their row writes in `with_statement_txn` +
  `retry_write_conflicts`; dropDatabase is per-collection transactions. DDL
  invoked inside a user (multi-document) transaction now joins it uniformly
  and rolls back with it (pinned by `tests/ddl_txn.rs`).
- `secantus-storage`: `update_matching` / `update_matching_pipeline` (and the
  `secantus-commands` storage seam's `update_matching_array_filters` /
  `update_matching_pipeline`) take a `want_post_image` flag;
  `UpdateOutcome::post_image` is captured only for `findAndModify`, sparing
  every plain single-doc update a full `Document` clone.
- `anyhow` 1.0.102 → 1.0.104 in `crates/`, `secantusdb`, `secantus-storage`
  and `secantus-storage-py` lockfiles, clearing the RUSTSEC-2026-0190
  unsoundness advisory from the cargo-audit log.

#### Fixed

- `secantus-storage`: a lock-free `find_matching_with` / `count_matching`
  racing a `renameCollection` / `dropCollection` / `dropDatabase` /
  `dropIndexes` can no longer return a partial result set. Namespace DDL runs
  under a drop-guarded seqlock generation (`ddl_generation_scope`, serialised
  by the global lock) and readers re-run a scan whose generation was odd or
  moved (bounded, so a DDL storm can't livelock a reader). Pinned by
  `tests/concurrent_reads.rs::scans_racing_namespace_ddl_are_never_partial`.

### Rust server: dropping a huge collection can no longer wedge the engine

Dropping a collection ran its whole row purge as one WiredTiger statement
transaction. Because collections share the sharded document tables, a drop
is a row-by-row purge — and a collection whose delete volume exceeds the
cache's dirty budget got a cache-pressure `WT_ROLLBACK`, which the write-
conflict retry loop re-ran forever while the eviction threads spun. That is
the livelock the 2026-08-11 concurrency sweep hit: a drop that sat for 40+
minutes at full CPU, survived client disconnect, and ignored SIGTERM. The
same unevictable-dirty-content class was already fixed for batch inserts,
updateMany, and deleteMany; drop (and dropDatabase) were the remaining
unbounded transactions.

Drops are now chunked and two-phase. A small first transaction unregisters
the collection, writes a drop tombstone, and emits the drop oplog entry —
after it commits the namespace is gone for every reader and writer. The row
purge then runs in bounded 4000-row transactions and finally clears the
tombstone. A crash mid-purge is finished at the next open, before any
traffic can re-create the name, so leftover rows can never resurface inside
a re-created collection. Inside a user transaction, drops keep the old
atomic single-transaction path, which the transaction dirty-budget guard
(`TransactionTooLargeForCache`) already bounds. A deterministic regression
test drops a collection larger than a deliberately small cache — the exact
shape that previously wedged — and a recovery test pins the crash-left
tombstone path.

#### Fixed
- Rust server: `drop` / `dropDatabase` of a collection larger than the WT
  cache's dirty budget livelocked the engine (unbounded purge transaction +
  unbounded write-conflict retry); now chunked, with crash-safe tombstone
  recovery at open.

#### Added
- `table:secantus_drop_tombstones` (additive to the shared on-disk layout):
  pending-drop markers that make the chunked purge crash-safe.

### The Rust server matches the Python one on the C and Ruby driver suites

Four more behaviours the Rust server was missing, found by regenerating the
gauges rather than reasoning about the code — every one of them was invisible
from the source and obvious from a single line of driver output.

`serverStatus` omitted its `connections` section entirely, so a driver asking
how many connections had been created got no answer. That is what the C
driver's exhaust-cursor tests were failing on all along: they open a cursor and
check that a connection was created, and the failure looked for all the world
like an exhaust-cursor bug. It took three passes to fix properly — the section
was missing, then present but the wrong integer width for a driver that
type-checks rather than coerces, then present and correctly typed but always
zero, which cannot satisfy a test asserting the count went up. It now reports
the server's real counters.

A capped collection's `$collStats` still didn't report its bounds, because the
values arrive as 32-bit integers and were read as 64-bit only. And
`listIndexes` accepted a negative `batchSize` instead of rejecting it, which is
the deliberate failure a Ruby session spec uses to check that errors surface.

#### Fixed

- `serverStatus` reports `connections` (with live counts), `opcounters` and
  `network`.
- `$collStats` reports `maxSize` / `max` for a capped collection regardless of
  the integer width the driver used.
- `listIndexes` rejects a negative `batchSize` rather than accepting it.

### The Rust server rejects the specs it should, and owns up to Atlas-only commands

Three behaviours the Python server had and the Rust one didn't, found by
splitting the C and Ruby driver-conformance failures against the Python
server's own results so only the Rust-specific ones remained.

Unknown fields on `create` and on an index spec were silently accepted rather
than rejected. Real MongoDB fails them, and drivers rely on that: three
mongo-ruby-driver specs deliberately pass `invalid: true` and assert the
operation fails, which is how a typo in an index option gets caught at the
point it is made rather than becoming an index that quietly isn't what was
asked for. Both now answer with the same unknown-field error MongoDB gives.

The Atlas Search index commands — `createSearchIndexes`, `updateSearchIndex`,
`dropSearchIndex` — went unanswered entirely, so a client heard "no such
command" rather than "this needs Atlas". A non-Atlas MongoDB registers them and
fails them with a message naming Atlas, which is the difference between a
driver reporting a missing feature and reporting a broken server. Finally,
`$collStats` reported that a capped collection was capped but not what its
bounds were; the `max` and `maxSize` fields are now present.

#### Fixed

- `create` rejects unknown top-level options, and `createIndexes` rejects
  unknown fields on an index spec, with MongoDB's `Location40415`.
- `createSearchIndexes` / `updateSearchIndex` / `dropSearchIndex` report
  `CommandNotSupported` naming Atlas, instead of `CommandNotFound`.
- `$collStats` reports `maxSize` and `max` for a capped collection alongside
  `capped`.

### The Rust server's batch inserts are chunk-committed too

The Rust storage engine had the same latent hazard the Python server's
`large_insert` CI wedge exposed: one wire message's inserts ran as one
WiredTiger statement transaction, whose unevictable dirty content could in
principle cross the cache's dirty-stall threshold and livelock the engine.
Its 4G embedded default cache kept the worst 48MB-message case comfortably
inside the budget — but a daemon configured with a smaller `--cache-size`
had no such protection.

Batch inserts now commit in the same bounded chunks as the Python server
(at most 1,000 documents or 4MB per statement transaction), keeping the
dirty footprint independent of the client's batch size on any cache
configuration. As on the Python side, MongoDB batch inserts are
per-document atomic only, so the commit points are invisible to clients.

#### Fixed

- `secantus-storage`: `Storage::insert` chunks one wire batch into bounded
  statement transactions (write-conflict retry per chunk; capped-FIFO
  fresh-key protection spans the whole client batch). Pinned by
  `batch_insert.rs::large_batch_insert_survives_a_small_cache` (35k × 1.1KB
  documents against a deliberate 128M cache) plus ordered/unordered
  cross-chunk semantics tests.

### updateMany and deleteMany commit in bounded chunks on the Rust server

The last unbounded-transaction surface on the Rust server is closed:
updating or deleting every document a broad filter matches used to run as a
single WiredTiger transaction, whose unevictable dirty content grows with
the matched set — the same storage-livelock class the chunked batch inserts
and the transaction dirty budget already closed. Multi-document updates and
deletes now commit in bounded chunks (at most 1,000 documents or 4MB per
statement transaction), each chunk re-reading its documents inside its own
transaction so concurrent transaction commits are never overwritten from a
stale read, and each document is transformed exactly once even across
conflict retries.

Real MongoDB's updateMany and deleteMany are per-document write units and
documented as non-atomic, so the chunk boundaries match its semantics —
single-document writes, upserts, and writes inside multi-document
transactions are unchanged.

#### Fixed

- `secantus-storage`: `update_matching` (multi) and `delete_matching`
  (unbounded) run chunked statement transactions instead of one unbounded
  transaction. Pinned by `multiwrite_chunk.rs`: a 35,000-document rewrite
  and deleteMany against a deliberately small 128M cache, exactly-once
  `$inc` across chunk boundaries, and unchanged bounded paths.

### The C driver can finally exercise change streams

`replSetGetStatus` said this server was a standalone while `hello`, on the very
same connection, described a single-node replica set. Real MongoDB is never
both, and the disagreement had a cost: the C driver's test fixture reads the
member roster to decide whether replica-set behaviour is available, saw an
empty one, and skipped every change-stream test as inapplicable. The strictest
wire-protocol suite we run had no change-stream coverage at all.

`replSetGetStatus` now reports the same one-member primary that `hello` already
advertised. A server started without a replica-set name still answers as a
standalone, which is the honest reply for one.

Thirty-two change-stream tests run as a result, and four real defects came out
of them: the error for a pipeline that discards the resume token had the wrong
message, the error for a malformed pipeline stage had the wrong code and
message, and — the substantive one — a pipeline that *rewrote* the resume token
rather than removing it was accepted. MongoDB permits only transformations that
leave the token untouched, so a rewritten token now fails the same way a removed
one does, instead of reaching the client as a confusing driver-side error.

#### Fixed

- `replSetGetStatus` reports a one-member primary roster when a replica-set
  name is configured, agreeing with `hello`.
- A change-stream pipeline that modifies the resume token is rejected, not just
  one that removes it.
- The resume-token and pipeline-stage errors carry MongoDB's own codes and
  messages.

### A tailing cursor is told why its collection went away

Dropping a collection while a client is tailing it left the client with
"cursor not found" — technically true, but it doesn't say what happened, and a
tailing application can't tell a dropped collection from an expired cursor or a
server restart. MongoDB reports that the query plan was killed and names the
dropped namespace, and the Rust server now does the same.

The three kinds of cursor a drop can hit are handled differently, matching the
Python server. An ordinary cursor is discarded, so the next fetch reports the
cursor is gone. A tailing cursor is kept just long enough to explain itself.
Change streams are left alone entirely: they already announce a drop through
their own invalidation event, and turning that into an error would replace a
normal end-of-stream with a failure.

The cursors are also now killed *before* the collection is removed rather than
after — a tail parked waiting for new data is woken by the drop itself, and it
has to find the explanation already in place or it goes back to waiting on a
collection that no longer exists.

#### Fixed

- A `getMore` on a tailable cursor whose collection was dropped reports
  `QueryPlanKilled` naming the collection, instead of `CursorNotFound`.

### The Rust server disables Nagle too

Mirror of the Python servers' `TCP_NODELAY` fix: the Rust server's
accept loop now calls `set_nodelay(true)` on every accepted connection,
closing the same ~40ms-per-round-trip delayed-ACK stall on Linux that
cost pgjdbc's chatty batch tests a 200x slowdown in CI against the
Python server. Best-effort (a failed setsockopt on a dying socket never
kills the accept loop), matching mongod's and PostgreSQL's own
unconditional NODELAY.

### The Rust server rejects oversized transactions too

The Rust server now enforces the same transaction dirty budget the Python
server gained: a multi-document transaction whose written volume exceeds a
cache-derived threshold (about 15% of the storage cache, mirroring real
MongoDB's `TransactionTooLargeForCache` guard) fails with code 313 before
its unevictable content can stall WiredTiger. The error carries no
transient label and the transaction aborts, matching mongod. With this,
the storage-engine livelock class is closed on both servers: batch inserts
commit in bounded chunks, and transactions are bounded by the cache budget.

#### Added

- `secantus-storage`: `StorageError::TransactionTooLargeForCache` + a
  per-transaction dirty budget (~15% of the configured `cache_size`,
  default 4G) enforced across `with_user_transaction` statements; mapped
  to mongod's 313 by the command seam. Pinned by
  `txn_budget.rs::transaction_dirty_budget_guard` against a deliberate
  128M cache.

### Unique indexes hold across a transaction

A unique index on the Rust server could be persuaded to accept two documents
with the same value. If one writer was inside a transaction and another was
not, each checked for a clash by reading its own snapshot of the data — and
neither snapshot showed the other's pending write. Both were told they were
fine, both were written, and the index that was supposed to guarantee
uniqueness quietly held a duplicate. Nothing failed, nothing was logged; the
damage only became visible later, in the data.

The clash check no longer relies on reading. Each unique value is now claimed
in a table keyed by the value itself, so the storage engine refuses the second
claim outright, whoever makes it and whenever they started. A writer that
arrives while a transaction holds the value now waits for it, exactly as
MongoDB does, and is then told the value is taken — or, if the transaction was
rolled back, quietly takes it.

Claims are released when the row that owns them is deleted, and cleared when
the collection, database or index they belong to is dropped, so a value can
always be used again once nothing is using it.

#### Fixed

- A unique index no longer accepts a duplicate when one of the writers is
  inside a transaction.

### Sequences allocate in batches — bulk SERIAL ingest is 3x faster

Every `nextval` used to pay a full read plus durable-update transaction
against the sequence's stored document, which dominated bulk-ingest
profiles: a 100k-row `COPY` into a SERIAL table spent roughly three
quarters of its time advancing the sequence, capping ingest around
5,000 rows/s. `nextval` now pre-allocates a batch of 128 values with a
single persisted write and hands the rest out from memory under the
same statement-write lock that already serialized it — PostgreSQL's own
`CACHE` mechanism applied server-side. The same `COPY` now runs at
13,000–15,800 rows/s, and per-statement SERIAL inserts gain about 20%.

Values remain gapless while the server runs (the cache is server-wide,
not per-backend). The stored document carries the batch's high-water
mark, so a restart resumes past the unhanded values — the identical gap
PostgreSQL's `CACHE` and crash semantics produce. `setval`,
`ALTER SEQUENCE`, `DROP`, and re-`CREATE` all discard the prefetched
run, so their effects stay immediate.

#### Changed

- `Catalog.sequence_nextval` allocates `SEQUENCE_ALLOC_BATCH` (128)
  values per persisted write; `tests/test_sql_sequences.py` pins the
  gapless run, the high-water persistence and reopen gap, and the
  invalidation on `setval` / `ALTER … RESTART` / re-create paths.

### `serverStatus` tells drivers which storage engine it is

Real driver test suites branch on `serverStatus.storageEngine.name` before
they will even attempt a transaction. SecantusDB never reported the field,
so mongo-php-library's `skipIfTransactionsNotSupported` helper threw
`UnexpectedValueException: Could not determine server storage engine` and
took roughly twenty-seven transaction tests down with it — not because any
transaction misbehaved, but because the suite could not establish what it
was talking to. One absent sub-document read as dozens of independent
failures.

Both servers now report the engine, and the answer is the true one:
SecantusDB is WiredTiger-backed, the same engine mongod uses. The
`persistent` flag is wired to the actual store rather than hard-coded, so
an `:memory:` instance reports itself as non-persistent instead of
claiming durability it does not have.

#### Fixed

- `serverStatus` now carries the `storageEngine` sub-document (`name`,
  `supportsCommittedReads`, `supportsPendingDrops`,
  `supportsSnapshotReadConcern`, `readOnly`, `persistent`,
  `backupCursorOpen`) on both the Python and Rust servers. The
  mongo-php-library gauge goes from 42 failures to 4 over the same 3130
  tests.

### BC timestamps, and parameters that kept their declared type

A date before year 1 stored in a `timestamp without time zone` column came back
carrying a time-zone offset it should never have had — `0101-01-01 00:00:00+00
BC` where Postgres writes `0101-01-01 00:00:00 BC`. Ordinary dates already
dropped the offset; only the ones outside the range Python can represent kept
it.

Separately, a parameter the client declared as `timestamp with time zone` lost
that declaration on the way to the column. Stored into a `timestamp` column it
was treated as though it had been typed out as a literal — offset discarded,
clock face kept — instead of being converted through the connection's zone, so
the value moved by the zone's offset. A client in New York writing midnight got
five in the morning back.

#### Fixed

- A BC or far-future timestamp in a `timestamp without time zone` column no
  longer reports an offset.
- A `timestamp with time zone` parameter keeps its type when stored into a
  `timestamp` column, and converts through the session's zone.

### A date written with a time-zone offset and no clock time

`1950-02-07 -05` — a calendar date, an offset, and no time of day — is what a
JDBC client sends for a date when it has been given a calendar. We read the
offset as though it were the time itself, so the value quietly became five in
the morning with no zone at all, and a `timestamp` column stored it that way.

Postgres reads the implicit midnight, and so do we now: the date lands on the
day it names, a `timestamp` column keeps midnight, and a `timestamp with time
zone` column keeps the instant that midnight refers to.

Dates at the very edge of the representable range are handled alongside this.
Now that the offset is understood, shifting one of those to UTC can fall off
the end of the calendar — the first instant of year 1 is in year zero once you
move it west. Those keep their clock face rather than failing.

#### Fixed

- A date literal carrying a time-zone offset but no time of day is read as
  midnight at that offset, rather than as a time.

### Unquoted identifiers fold to lower case, as Postgres does

`SELECT r.table_name FROM (SELECT id AS TABLE_NAME …) r` reported that the
column did not exist. Postgres lower-cases an unquoted identifier, so writing
an alias in upper case and reading it back in lower case names the same column;
we compared every spelling exactly, so the two forms were two different names.

Quoted identifiers keep their spelling exactly, which is what quoting is for —
`"Mixed"` and `mixed` remain different columns.

Matching case always worked, which is why this went unnoticed: code that writes
an alias one way and reads it back the same way never trips it. Generated SQL,
and anything written in the SQL-standard upper case, does — JDBC's metadata
queries are how it surfaced.

Folding happens once, immediately after parsing, so table names, column
references and aliases all agree on one canonical spelling.

Note for existing databases: a table created unquoted with a mixed-case name is
now addressed lower-cased, matching what Postgres would have stored in the
first place.

#### Fixed

- An unquoted identifier written in one case and read in another now names the
  same table, column or alias.

### Five ways a SQL connection could drop with "internal error"

Every crash the PostgreSQL front end reported as a bare `internal error` came
from a distinct, small cause, and each one killed the connection rather than
returning a message the client could act on. All five are fixed, along with a
quadratic cost in parameter binding that made large statements look like hangs.

The protocol's 16-bit count fields — parameter counts, column counts — were
read and written as *signed*. Postgres allows up to 65535 parameters in a
single Bind, and a JDBC driver rewriting a batch into one statement really does
send tens of thousands; above 32767 the count came back negative, walked the
parse offset backwards, and the connection died. Binding those parameters was
also `O(N²)`, because each placeholder was replaced one at a time and the
expression library re-parents every sibling on each replacement. A statement
with 40000 parameters took over two minutes; it now takes well under a second.

Geometric values had no binary decoder at all, so a `point`, `box` or `polygon`
sent in the binary format — which drivers do by default — arrived at the *text*
parser as raw bytes and failed as "no coordinate pairs in geometry". The `line`
type could not be parsed even as text: its canonical form is three coefficients
`{A,B,C}` rather than coordinate pairs, and the branch that handled it sat
after the pair parse it could never survive. `time + interval` was simply
missing, and an interval inside a `WHERE` clause was pushed down into an
aggregation expression that has no interval type, where it surfaced as a
`$multiply` type error. Finally, the catalog builders behind `pg_class` and
friends enumerated the table list twice — once to assign OIDs and once to emit
rows — so a table created by another session in between produced a `KeyError`
part-way through a catalog scan.

#### Added

- Binary parameter decoders for every geometric type: `point`, `lseg`, `path`,
  `box`, `polygon`, `line`, `circle`.
- `pggeo.line_from_points`, converting the two-point spelling of a `line` to
  its `{A,B,C}` canonical text the way Postgres does.
- `virtual._tables_with_oids`, the single-snapshot accessor catalog builders
  use instead of enumerating the tables twice.

#### Fixed

- 16-bit count fields are read and written unsigned, so a Bind carrying more
  than 32767 parameters no longer drops the connection. Fields that can
  legitimately be negative — attnum, type size, format codes — stay signed.
- Binding N parameters is linear rather than quadratic.
- `line` values parse, and an open `path` keeps its `[…]` spelling through a
  round trip instead of being rewritten as closed.
- `time ± interval` returns a `time`, wrapping into a single day and dropping
  the month/day components, as Postgres does. `timetz ± interval` does the same
  and carries the zone offset through untouched.
- A `date` compared against a computed `timestamp` promotes to midnight the way
  Postgres does, instead of failing to compare ISO text against a datetime.
- `'23:59:60'::time` carries forward to `24:00:00` rather than storing a second
  that nothing downstream could parse — which had made `time - time` fail too.
- An unknown-type operand beside an interval resolves numerically, so
  `$1 * $2::interval` works with the typeless parameters JDBC drivers bind.
- Interval arithmetic in a `WHERE` clause falls back to per-row evaluation
  instead of lowering to an aggregation expression that cannot express it.
- Catalog builders take one snapshot of the table list, so concurrent DDL no
  longer aborts a `pg_class` / `pg_attribute` / `pg_attrdef` / `pg_description`
  / `pg_index` scan.

### Leap seconds are accepted, and a bad timestamp says what is wrong

`'2015-06-30 23:59:60'` — a real leap second, and a value Postgres accepts by
rolling it forward to the next minute — crashed with an internal error, because
Python has no room for a second numbered 60. It now rolls forward the same way,
carrying across the minute, day and year boundaries.

The same path had a wider problem: *any* timestamp that could not be parsed
reached the client as an internal error rather than saying so. Even
`'not-a-date'` did. Unparseable timestamps now report invalid input syntax,
naming the value, and the out-of-range near-misses Postgres also rejects —
`23:59:61`, or a fractional leap second like `23:59:60.5` — are among them.

#### Fixed

- A `:60` leap second in a timestamp literal no longer fails with an internal
  error.
- An unparseable timestamp reports `invalid input syntax` instead of an
  internal error.

### `numeric` is exact again — `0.1 + 0.2 = 0.3` is true

Decimal literals were read as floats, so Postgres' arbitrary-precision exact
`numeric` behaved like a double. `0.1 + 0.2 = 0.3` answered false,
`SELECT 0.000000` came back as `0` with its scale discarded, and a value wider
than a double silently dropped digits — `12345678901234567890.12345 + 1`
returned `1.2345678901234567E+19`, which for money-shaped data is corruption
rather than rounding.

A literal is now the same exact decimal a `numeric` column already stored, so
values written, computed and read back all agree. Integers are unaffected, and
so is integer division.

Comparisons involving a decimal were wrong in a quieter way: the operators
could not compare a decimal against an int or a float at all, and answered
false instead. Any predicate mixing the two — a column against a decimal
expression, a stored `numeric` against a literal — silently matched nothing.

#### Added

- `typemap.number_literal`, the single mapping from a numeric literal to its
  Postgres type. The planner and the scalar evaluator carried separate copies
  of this, which is why an earlier attempt at this fix left arithmetic on
  floats.
- `typemap.unwrap_numeric` / `typemap.negate` / `typemap.to_decimal128`.

#### Fixed

- Decimal literals are exact and keep their scale, so `numeric` arithmetic no
  longer inherits floating-point error or loses digits.
- Comparison operators handle decimals instead of silently answering false.

### Repeated SQL statements skip the parser

`planner.parse` now caches parsed statements by text, handing out fresh
copies of the cached trees (`Expression.copy()` measures 3–4× cheaper
than a parse, which profiled at ~29% of embedded statement time).
Entries are cached on second sight — the first occurrence only leaves a
marker — so workloads of mostly-unique statements (sqllogictest's
corpus, inline-literal DML) pay nothing beyond a dict probe, while
repeated text (per-connection re-parse of prepared statements, fixture
DDL repeated across thousands of tests) hits from the second occurrence
on: +26% embedded statement throughput on repeated-text workloads,
no measurable cost on unique-text ones. The cached trees never leave
the cache uncopied, so downstream mutation cannot poison them — pinned
by `tests/test_sql_parse_cache.py` alongside second-sight, eviction,
and error-path semantics.

### `_pg_expandarray` and composite field access in the select list

`information_schema._pg_expandarray(arr)` yields one `(x, n)` record per array
element — the value and its 1-based subscript. JDBC's metadata queries lean on
it heavily, selecting it two ways in the same statement: the whole record, and
a single field via `(…).n`. Neither shape was recognised, so those queries
failed outright rather than returning primary-key or index information.

Both now work, including the schema-qualified spelling, and the record stays a
composite rather than being flattened to text so that a field can still be read
from it a level up — which is exactly how the driver uses it, producing the
record in a subquery and selecting a field from it in the outer query.

#### Added

- `information_schema._pg_expandarray` in the select list, whole or by field.
- `(expr).field` against a record-returning function.

#### Fixed

- Set-returning functions are recognised when written with a schema
  qualification in the select list.

### Result columns report the table and column they came from

Every result column described itself as having no source: the table OID and
column number that Postgres puts in each field of a row description were sent
as zero. JDBC clients use exactly those to map a result column back to the
column it was selected from, so an updatable `ResultSet` could not name the
column it was asked to update — it built `UPDATE t SET "" = ?` and the server
rejected it.

Columns selected from a table now carry their source table and position, and
they keep it through aliasing and reordering, since the position describes the
table rather than the select list. Computed columns still report none, which is
what Postgres reports for them.

#### Fixed

- Updating a row through a JDBC updatable `ResultSet` no longer fails with
  `column "" does not exist`.

### `SET TIME ZONE` actually sets the time zone

Written the two-word way — `SET TIME ZONE 'Europe/Dublin'` — the statement did
nothing at all. It takes no `=` or `TO`, so it slipped past the handler that
reads name-and-value settings, and `SHOW TIME ZONE` answered with an empty
string because that spelling was not recognised either. A client that pinned
its connection's zone this way, as JDBC drivers do, silently stayed on the
default and had no way to tell.

Both spellings now set and report the same setting, `DEFAULT` resets it, and
the change is announced to the client the way other tracked settings are.

Worth being clear about the limit: this makes the *setting* stick. Values of
type `timestamp with time zone` are still stored and displayed without regard
to it — that conversion is a larger piece of work and is written up in the
backlog.

#### Fixed

- `SET TIME ZONE <value>` sets the `TimeZone` setting; `SHOW TIME ZONE` reports
  it.

### `timestamp with time zone` respects the session's time zone

A value written without an offset — `'2005-01-01 12:00:00'` — was read as UTC
rather than as local time in the connection's own time zone, so it was stored
at the wrong instant by however far that zone sits from Greenwich. Reading it
back showed the same skew, which for a value near midnight moved it to the
previous or the following day.

Such a value is now interpreted in the session's zone, as Postgres does, and
displayed back in that zone. A value that arrives carrying its own offset is
already unambiguous and is left alone.

Two smaller things came with it. Zone names written with an offset, like
`GMT+13`, previously resolved to nothing and fell back to UTC; they now resolve,
keeping the POSIX convention Postgres follows where `GMT+13` means thirteen
hours *behind* UTC. And offsets are written the way Postgres writes them — `+00`
and `-05` rather than `+00:00`, widening to `+05:30` only where the minutes
matter — which clients that compare the rendered text depend on.

Values of type `date` and `timestamp without time zone` are unaffected, as they
should be: neither has an instant behind it to move.

#### Fixed

- A `timestamptz` written without an offset is interpreted in the session's
  time zone instead of UTC, and displayed in that zone.
- Zone settings of the form `GMT±N` resolve, with Postgres' sign convention.
- Offsets render in Postgres' spelling.

### A failed statement now aborts its transaction, whatever raised it

Postgres aborts a transaction block on any error: every later statement fails
until the block is rolled back. That held for errors raised while running a
statement, but not for errors the protocol layer raised on its own — asking for
a prepared statement or portal that no longer exists, or a parameter that could
not be decoded. Those left the block looking healthy, so work that a client
believed had been discarded went on to commit.

Rolling back, including to a savepoint, still recovers the block, and statements
outside a transaction are unaffected.

`DEALLOCATE ALL` also now reports the command tag Postgres reports —
`DEALLOCATE ALL` rather than a bare `DEALLOCATE`. Drivers watch for that exact
tag to learn their server-side statement cache has been discarded and to
re-prepare; without it they kept using names the server had already dropped.

The two go together. Aborting the transaction on its own made a JDBC driver's
recovery worse, not better: the block now died where the driver expected to
carry on, because it still had no idea its cache was stale.

#### Fixed

- An error raised by the extended query protocol aborts the open transaction.
- `DEALLOCATE ALL` reports the `DEALLOCATE ALL` command tag.

### UNIQUE constraints are enforced by the storage engine

A `UNIQUE` constraint was upheld by looking for a clashing row before writing
one. That look happens against the snapshot the writing transaction is reading,
so it could not see a value another transaction had just committed, nor one a
second writer was inserting at that moment. Either way a duplicate was stored,
and the constraint quietly did not hold.

Declaring a constraint now creates the index that enforces it, so the storage
engine decides: a value already present is refused whoever wrote it and
whenever, and two transactions reaching for the same value collide so that only
one keeps it. Adding a constraint to an existing table does the same, and
dropping it removes the index.

The SQL rules around NULL are preserved: any number of NULLs satisfy a `UNIQUE`
constraint, and a constraint over several columns does not apply to a row where
any of them is NULL.

Constraints declared `DEFERRABLE` are unchanged. Those are allowed to be
violated part-way through a transaction and are judged when it commits — a
swap of two values being the usual case — so they continue to be checked at
commit rather than on every write.

#### Fixed

- A `UNIQUE` constraint no longer admits a duplicate written by a transaction
  that began before the value was committed, or by two transactions at once.

### Unique indexes are enforced by the storage engine

A unique index used to be upheld by looking for a clashing value before writing
one. That look happens against the snapshot the writer is reading, which cannot
show a value another transaction committed a moment earlier, and cannot show a
value a second writer is inserting right now. Both cases stored a duplicate.

Unique indexes now also record each indexed value under a key that is the value
itself, so WiredTiger decides. A value already present is refused by the engine
whoever wrote it and whenever; two writers reaching for the same value collide
and only one keeps it. Creating a unique index over rows that already exist
claims their values too.

Nothing else changes: the existing index entries, and every query path that
reads them, are untouched, and a database written by an earlier version stays
readable.

This covers unique indexes as used through the MongoDB interface. A `UNIQUE`
constraint declared in SQL is still upheld the older way and keeps the same two
gaps; the groundwork for closing that is now in place.

#### Fixed

- A unique index no longer admits a duplicate written by a transaction that
  began before the value was committed, or by two writers at once.

### Both wire servers disable Nagle — a 200x CI stall on chatty round-trips

Neither server set `TCP_NODELAY` on accepted sockets. Reply paths write
small frames back-to-back (a reply then ReadyForQuery, one batch item's
result then the next), and with Nagle enabled the second write waits for
the peer's delayed ACK — roughly 40ms per round trip on Linux, invisible
on macOS loopback where ACKs are immediate. pgjdbc's generated-keys
batch tests, which perform 1,000 single-row round trips each, measured
41.5 seconds per test in CI against 0.2 seconds locally from exactly
this — about 20 minutes of the pgjdbc lane's in-test time on ~30 tests.
Both servers now set `TCP_NODELAY` unconditionally on every accepted
connection, as mongod and PostgreSQL do.

### Oversized transactions are rejected before they can stall the engine

A multi-document transaction's statements all join a single WiredTiger
transaction, whose written content stays unevictable from the storage cache
until commit. A client that pushed enough data through one transaction
could therefore pin the cache past its dirty threshold and livelock the
engine — the same stall class the chunked-insert fix closed for plain batch
writes, where chunking cannot apply.

The Python server now enforces the guard real MongoDB has for this exact
condition: a transaction whose buffered write volume exceeds a budget
derived from the cache size (about 15%, mirroring mongod's threshold) fails
with `TransactionTooLargeForCache` (code 313). The error carries no
`TransientTransactionError` label — retrying the same oversized transaction
would hit the same wall — and, as with any failed in-transaction statement,
the transaction is aborted server-side. Transactions under the budget, and
plain writes of any size, are unaffected.

#### Added

- `secantus.storage`: `TransactionTooLargeError` + a per-transaction
  dirty-bytes budget (~15% of `cache_size`) enforced in the oplog-buffering
  path; surfaced by the command layer as mongod's
  `TransactionTooLargeForCache` (313, unlabeled). Pinned at both the
  storage and wire levels against a deliberately small cache.

### Unique-key claims no longer survive their table

The storage-backed unique-index enforcement introduced a week ago kept its
claims table alive across namespace teardown: dropping a table (or index, or
database) left the dropped namespace's unique-key claims behind, so
recreating the table and inserting a previously-used value was falsely
rejected as a duplicate. Caught by the weekly conformance sweep — the
sqllogictest corpus cycles drop/create with unique indexes constantly — and
reproduced in eight lines. Every teardown path now releases the namespace's
claims: drop table, drop index, drop all indexes, drop database, and rename.

#### Fixed

- `secantus.storage`: `table:secantus_unique_keys` rows are purged wherever
  their index or collection dies. Pinned by per-path regression tests
  (`TestClaimsDieWithTheirNamespace`) and the previously-failing
  `index/delete` sqllogictest file, which passes again in both protocols.

### Idle connections can no longer pin WiredTiger's transaction horizon

The pgjdbc conformance lane's two-hour hang had a second, deeper cause beyond
the idle-in-transaction timeout shipped previously: a connection whose last
statement left its cached WiredTiger session with a positioned cursor held an
*implicit* transaction — invisible to every PostgreSQL-level accounting — and
pinned the storage engine's oldest-transaction horizon while it idled. Every
write after that pin kept its history unevictable, so per-operation cost grew
linearly with churn until a 100k-row TRUNCATE stalled in page reads and wedged
the server. The wedge needed a specific mix of prior traffic to arm, which is
why it only appeared mid-way through the full pgjdbc suite.

Both wire servers now call the new `Storage.release_thread_snapshot()` before
blocking for the next client message: `WT_SESSION.reset()` releases the
snapshot and every cursor position in one cheap call, so an idle connection
holds nothing by construction. Inside an open user transaction the release is
a deliberate no-op — a transaction's pinned snapshot is its semantics, and the
transaction-lifetime / idle-in-transaction timeouts bound that case. The
previously-deterministic pgjdbc wedge reproduction now runs clean with the
pinned-transaction-range statistic flat at zero.

#### Added

- `Storage.release_thread_snapshot()` — releases the calling thread's WT read
  snapshot and cursor positions; called by both the PG and Mongo wire servers
  at the end of every request, before the idle wait.
- `tests/test_storage_snapshot_release.py` — statistics-backed regression
  tests: a positioned cursor measurably pins the horizon and the release
  clears it; the release is a no-op inside a user transaction; a wire-level
  invariant that an idle PG connection never accumulates a pinned range.

#### Fixed

- An idle connection's stale read snapshot no longer degrades all later
  writes without bound (the pgjdbc `CopyLargeFileTest` wedge / 2-hour CI lane
  timeout). The Rust server's equivalent idle-session behaviour is tracked as
  a follow-up in `tasks/backlog.md`.

## [0.6.0b9] — 2026-08-01

### Async oplog hardened: transactions can no longer leak ghost events

The Rust server's opt-in async oplog (`RustServer(oplog_async=True)` /
`secantusd-rs --oplog-async`) closed out its prototype caveats. The
important one was a correctness bug the hardening audit caught: a write
inside a multi-document transaction handed its oplog entry to the
background drainer *before* the transaction committed, so a rollback
left a persisted entry for data that never existed — a phantom change
event and a wrong PITR row. Entries now buffer on the transaction handle
and reach the drainer only after the commit succeeds; a rolled-back
transaction leaves no oplog trace.

Two smaller async-mode gaps closed with it. Reading `local.oplog.rs`
now drains the writer's queue first, so a client that just got its
write acknowledged sees the entry in the oplog view — read-your-own-write,
as on mongod. And the opportunistic prune cadence moved from the write
path to the drainers themselves: the old trigger could only prune rows
already persisted, so a lagging drainer queue escaped every sweep and a
burst of writes could leave the oplog over its cap until the next
explicit prune. CI gains an async-oplog lane that runs the whole
storage suite with the drainer pool live.

#### Fixed

- Async oplog: multi-document transaction writes minted + enqueued their
  oplog entries mid-transaction; a rollback persisted a ghost entry
  (phantom change-stream event, wrong PITR). Entries now buffer on the
  transaction handle and are minted + enqueued only after a successful
  commit; rollback / commit-failure / handle drop discard them
  (`crates/secantus-storage/tests/async_txn.rs` pins both directions).
- Async oplog: `local.oplog.rs` reads raced the drainer — an
  acknowledged write's entry could be missing from the view. The view
  read path now flushes the drainer first (no-op in sync mode; skipped
  inside a user transaction, where mongod forbids reading `local`
  anyway).
- Async oplog: the opportunistic prune fired on minted volume but could
  only doom persisted rows, so drainer-queue lag escaped the sweep and
  the counter reset deferred the retry a full interval — an oplog
  temporarily unbounded past `oplog_max_entries` under bursts. The
  cadence now lives with the drainers (triggered as rows land).
- Async oplog: an explicit `prune_oplog` call racing the drainer pruned
  a timing-dependent subset of acknowledged writes (cap-excess rows
  still queued escaped the sweep, shifting the pruned count and the
  resulting oplog floor / PITR segment contents). The public entry
  point now drains the queue first, so explicit prunes
  deterministically cover every acknowledged write.

#### Changed

- `tests/oplog_visibility.rs` pins `oplog_async: Some(false)` (it tests
  the sync in-flight-mint window, which async mode does not have) and
  storage-crate oplog tests pin the async read-after-write contract with
  explicit `flush_oplog()` calls, so the whole suite is meaningful in
  both modes.

#### Added

- CI: an async-oplog parity lane in the `rust-storage` job —
  `cargo test` re-run under `SECANTUS_OPLOG_ASYNC=1` +
  `SECANTUS_OPLOG_NONLOGGED=1` — the stated precondition for the mode
  ever becoming a default.

### Change streams no longer skip an event that commits mid-lookup

Resuming a change stream from a point in time could permanently miss an event.
Mapping a `startAtOperationTime` to a position scans the committed oplog and
then checks that nothing is still in flight below the answer — but it read
those two things in the wrong order. A write that committed between the scan
and the check produced a stale answer naming the position *above* it, while
the check had already advanced to cover that position, so the answer was
accepted and the event was never delivered.

The two reads are now ordered so the in-flight check is sampled first, which
is conservative in the safe direction: the visible position only ever moves
forward, so an earlier reading can only make the check stricter, never let an
unresolved write slip past.

The window was narrow enough to surface only as an intermittent CI failure on
Windows, where the coarser scheduling quantum happened to land inside it. It
is reproducible on demand once the interleaving is forced, and the regression
test does exactly that rather than racing for it. Both the Python and the Rust
storage engines carried the same ordering and both are fixed.

#### Fixed

- `startAtOperationTime` could resolve to a position past an in-flight write
  whose entry qualified, permanently skipping that event once it committed.

### Set-returning functions work as join and derived-table sources

`generate_series`, `unnest` and friends worked only as the *sole* `FROM` item.
Used anywhere else — joined to a table, or inside a derived table — they
failed with `relation "" does not exist`, an error naming a relation nobody
had written. The empty name was the tell: sqlglot models a table function as
a table whose name lives in a function node rather than an identifier, so the
planner fell through to a catalog lookup for the empty string.

Such a source is now reduced to the base-less shape the engine already knows
how to materialize, and handed to the executor as a raw sub-plan. That matters
for more than tidiness: the rows are produced at execution time, so an SRF
whose arguments read session state — `generate_series(1,
array_upper(current_schemas(false), 1))` — resolves against the real session
instead of being guessed at while planning.

`pg_type` also gained `typinput`, the column drivers compare against
`array_in` to decide whether a type is an array.

Together these let the JDBC driver's type-lookup query run, which had been the
single largest source of failures in its conformance suite; the gauge moves
from 92.5% to 93.7%.

#### Added

- `pg_catalog.pg_type.typinput`.

#### Fixed

- A set-returning function in `JOIN` position, or in the body of a derived
  table, no longer fails with `relation "" does not exist`.

### UNIQUE constraints hold against rows committed after your snapshot

A `UNIQUE` constraint could be violated from inside a transaction. Enforcement
worked by looking for an existing row through the transaction's own snapshot,
which is fixed when the transaction begins — so a row another connection
committed after that point was invisible, the check passed, and the duplicate
was stored. PostgreSQL rejects the same sequence, because a unique index is
checked against committed data even though your reads stay on your snapshot.

Enforcement now consults committed state as well as the transaction's own view.
Both are needed: the transaction's view sees rows it has inserted itself and
respects rows it has deleted, and the committed view sees what everyone else
has landed in the meantime.

Autocommit statements were never affected — each is its own short transaction —
which is why this went unnoticed. Nothing changes for them, and the extra check
costs nothing outside a transaction.

Two narrower cases still get through and are recorded in the backlog: a
transaction that has already written to the table before inserting, and two
transactions inserting the same value simultaneously. Both are closed properly
by making unique index entries collide in the storage engine, which is a
change to the on-disk layout.

#### Added

- `Storage.find_matching_committed`, a committed-state read for constraint
  enforcement (not for user-visible reads, which must keep their snapshot).

#### Fixed

- A `UNIQUE` constraint no longer accepts a value another transaction committed
  after the inserting transaction's snapshot was taken.

## [0.6.0b8] — 2026-08-01

### A kill -9 crash window in the data-nonlogged mode could lose acknowledged writes — fixed

The opt-in log-only-the-oplog mode (`data_nonlogged`) wrote its stable
marker — the seq recovery replays from — *before* running the checkpoint it
describes. The marker lives in an always-WAL-logged table, so it became
crash-durable immediately: a `kill -9` landing after the marker's WAL write
but before the checkpoint completed recovered with a marker *above* what the
last checkpoint actually contained, and replay started too high — every
acknowledged write between the old checkpoint and the marker was silently
lost as a mid-history hole (the oplog rows themselves all survived). The
window is a few milliseconds on an idle machine but stretches with checkpoint
duration under load, which is how the hard-kill harness caught it live: 2,300
of 7,200 acknowledged documents missing after recovery, with all 7,200 oplog
entries present.

Both checkpoint sites (the periodic anchor thread and explicit/close-time
`stable_checkpoint`) now checkpoint first and write the marker after. A crash
between the two leaves the *old* marker, and replay covers extra
already-applied entries — the idempotent-replay path that has always existed
absorbs exactly that. Stale-marker is safe; eager-marker loses data. The
hard-kill harness also gained self-diagnosis: on any future loss it reports
whether the missing documents' oplog entries survived, separating WAL loss
from replay-window bugs at a glance.

#### Fixed
- `secantus-storage`: stable-marker row written after (not before) its
  checkpoint in both the periodic checkpoint thread and `stable_checkpoint`;
  the recovery floor can now only ever be conservative.
- `tests/test_crash_recovery.py`: loss assertions carry a diagnosis dict
  (doc count, oplog row count and tail, whether the first missing id's oplog
  entry exists).

### Numeric comparisons stop allocating on the hot path

Every numeric comparison in the Rust engines — a find filter's
`$gt`/`$eq`/range test, a sort comparator call, an `$expr` compare — used to
build the value's exact decimal-digit form on the heap (a `String` plus a
digit vector per operand) before comparing. A new allocation-free fast path
answers the common int32/int64/double pairs directly, falling back to the
digit form only for Decimal128 and for int64↔double pairs beyond ±2^53
(where the engines' shortest-repr decimal semantics and exact binary
comparison can diverge — the boundary is proven and pinned by an
edge-corpus equivalence test). Measured on COLLSCAN drains: +11% on an
integer range filter, +49% when an integer query bound meets a double
field; all seven Rust↔Python parity suites unchanged.

#### Changed
- `secantus-core`: `numeric::fast_cmp` / `fast_eq` / `fast_cmp_numberish`
  answer int/double comparisons without allocating; the query matcher,
  `order::cmp` / `bson_lt`, and the expression engine's compare/eq paths
  try them first. Decimal128 and out-of-range pairs keep the exact
  digit-form path; verdicts are byte-for-byte unchanged.

### Change-stream and exhaust replies stop re-encoding every document

The last two survivors of the reply-path materialization (Finding 2) are
gone. A change stream's tailable getMore decoded every event blob into a
document and re-encoded it onto the wire — even though the only thing the
handler needed from the batch was the last event's `_id` for the
postBatchResumeToken. And the exhaust streamer round-tripped every batch
through an owned document array (plus a full clone of each batch) between
pulling it from the cursor registry and framing it. Both now splice the
pre-encoded blobs straight onto the wire like the ordinary find/getMore
path has since the RecordId era. Measured: change-stream drain +22%
(105k → 128k events/s), exhaust-cursor drain +26% (1.20M → 1.52M docs/s).

#### Changed
- `secantus-commands`: the tailable getMore hands its event blobs to the
  wire encoder undecoded; the postBatchResumeToken decodes only the final
  blob (as it always did).
- `secantus-server`: the exhaust streamer threads the pre-encoded batch
  through every `moreToCome` frame (`encode_cursor_reply` splice) instead
  of materialising and cloning it per frame; `materialize_batch` is gone.

### The Python server compiles a projection once per cursor, not once per document

Every projected document re-ran the whole projection front-end: meta
validation, spec partitioning, inclusion/exclusion mode detection, and —
worst — rebuilding the dotted-path trie from scratch, per row. The spec is
constant for a cursor's lifetime, so all of that now compiles once into a
projection plan and only the per-document work runs per document. Alongside
it, the expression engine stops shallow-copying the entire document on every
`$field` reference (the copy only existed to satisfy a type annotation — the
path walk is read-only), the matcher stops rebuilding a constant frozenset
per operator clause, and the pure-Python FNV shard-name hash is memoised.
Measured on the Python server: projected find drain +46%, exclusion
projection +19%, a `$group` pipeline +2.8%.

#### Changed
- `secantus.projection`: new `compile_projection` / `apply_projection_plan`
  split; `apply_projection` and the batch path are unchanged in behaviour
  (all seven Rust↔Python parity suites pass untouched — the Python engine
  stays the oracle).
- `secantus.expressions`: `$field` resolution no longer copies the document;
  `secantus.query`: `_SIBLING_MODIFIERS` hoisted to module scope;
  `secantus.storage`: shard-name lookup memoised, projected reads use the
  batch (compile-once) path.

### Write ops decode the collection-options row once, not three times

Every insert decoded the collection-options blob twice (the timeseries
check, then the UUID fetch for the oplog entry), and every replace/delete
decoded it twice more (UUID, then the pre/post-image flag) — the same tiny
BSON row, searched and decoded repeatedly within one operation. A one-decode
`CollMeta` view now feeds all three consumers; the collection UUID stays
lazily minted only when the oplog actually needs it, so a server running
with the oplog disabled mints exactly as few UUIDs as before. Measured
paired A/B on batch inserts into a two-index collection: +2.3% (5/5 positive
pairs).

#### Changed
- `secantus-storage`: `coll_meta` / `meta_uuid` replace the per-op
  `is_timeseries` + `collection_uuid` + `pre_post_images_enabled` call
  chains on the insert/replace/delete paths. Behaviour is unchanged —
  same facts, one decode.

### The benchmark page now covers the paths that differentiate — and the PGO profile catches up

The published nine-workload latency table gains two rows the old six-row
table never measured: a **filtered collection scan** (the per-document
compare path — the one the new allocation-free numeric fast path
accelerates; the unfiltered scan and the indexed range never touch it) and
a **change-stream drain**, where the Rust server now clocks **0.8× of
mongod — faster than mongod at its own change streams** — after the reply
path stopped re-encoding event blobs. The aggregate multi-stage workload
joins the published table too. The committed PGO profile is regenerated on
the post-review hot paths (a stale profile silently forfeits its 12–19%),
and every surface that quotes the ×mongod ranges — the benchmark page, the
website performance page, the Rust-server docs, the README — is re-baselined
from the same fresh five-rep run.

#### Changed
- `bench/compare_servers.py`: new `find_filtered_scan` and
  `change_stream_drain` workloads; the change-stream reference spawns a
  single-node replica-set mongod (its change streams require one) while
  every other row keeps the standalone reference; the Rust server arm
  advertises the replica-set persona to match the Python server.
- `crates/pgo/_secantus_server.profdata.tar.gz`: retrained via
  `invoke rust-pgo-refresh` on the post-micro-opt hot paths.
- `docs/benchmark.md`, `docs-rust/index.md`, `README.md`, website
  performance page: nine-row table + refreshed charts and ×mongod ranges
  (Rust ~0.8×–2.3×; three rows beat mongod outright).

### Concurrency graphs are now generated, refreshed per release

The N-writer scaling charts on secantusdb.com/performance and in the
docs' concurrency deep-dive are no longer hand-authored SVG. A new
`invoke concurrency-refresh` task re-measures all four series (Python
server, Rust server, Rust async stack, mongod) with `bench.concurrency`
— now able to drive the async-oplog stack directly (`--server
rust-async`), take medians over interleaved runs (`--runs`), and write
machine-readable results (`--json`) — and `bench.concurrency_chart`
regenerates the chart and data-table blocks in both surfaces from those
results. The committed results live at `bench/results/concurrency.json`,
and a test pins the committed charts to exactly what that file renders
to, so the graphs can no longer silently drift from the measurements.
The refresh is part of the per-release website update.

#### Added
- `bench.concurrency`: `--server rust-async` (async + non-logged oplog
  stack), `--runs N` interleaved-median sweeps, and `--json PATH`
  structured output; `--server all` now sweeps four servers.
- `bench.concurrency_chart`: renders the website and docs concurrency
  chart + table blocks from the results JSON into marker-delimited
  regions.
- `invoke concurrency-refresh`: benchmark + regenerate in one step
  (`--skip-bench` re-renders from the committed results).
- `tests/test_concurrency_chart.py`: pins the render/replace logic and
  fails if the committed charts are stale relative to the committed
  results JSON.

### The wire-protocol gauge lands — CockroachDB's pgtest corpus runs verbatim

The SQL server's conformance portfolio gains its strictest instrument: G3,
the pgwire message-level gauge. `invoke validate-pgtest` drives CockroachDB's
`pkg/sql/pgwire/testdata/pgtest` corpus — ~54 datadriven files of raw
Parse/Bind/Describe/Execute/COPY/error exchanges with byte-exact expected
responses — using CockroachDB's own `pkg/testutils/pgtest` runner,
completely unmodified. It is the SQL analogue of the mongo-c-driver gauge:
where the driver gauges tolerate server slop, this one asserts the framing
itself.

The monorepo problem is solved by not vendoring at all: both corpus and
runner are fetched at a pinned commit through a sparse, blob-filtered clone
(about 25 MB, cached) at gauge time — the same fetch-at-runtime pattern as
the sqllogictest runner's `cargo install` — which also keeps the CockroachDB
Software License outside the repository tree. The only committed Go code is
a thin `go test` driver and a ten-line shim for one internal helper the
runner imports. SecantusDB presents as non-CockroachDB, so the corpus'
`crdb_only` exchanges skip themselves.

The opening baseline is **8 of 58 files** — honest and low by design, since
every file stops at its first byte-level mismatch; the number climbs
cluster-by-cluster the way the psycopg gauge went from 42% to 91%. The first
finding is already fixed: an unaliased cast's output column is now named
after the type's `typname` (`SELECT 2::int8` → column `int8`), where it
previously reported `?column?`.

#### Added

- `pgtest_validation/` (pinned-commit sparse fetch, verbatim upstream
  runner staging, Go driver module, report generator), `invoke
  validate-pgtest`, weekly `validate.yml` row sharing the Go toolchain step.

#### Fixed

- `sql/planner.py`: unaliased top-level cast projections are named after the
  cast target's `typname` like real PG, across the constant, single-table,
  grouped, and RETURNING paths.

### The JDBC driver's own suite now measures the SQL server — and one fix moved it nine points

pgjdbc, the official PostgreSQL JDBC driver, joins the portfolio as the G5
gauge: `invoke validate-pgjdbc` runs the driver's own test suite —
unmodified, from a vendored submodule at REL42.7.13 — against a daemon
SecantusDB server. Targeting uses pgjdbc's stock `build.local.properties`
mechanism, which the project itself gitignores, so pointing the suite at us
leaves the vendored tree pristine. Scope opens at the `jdbc2` core package
(75 test classes, 5,500-odd tests) and grows package by package.

The opening baseline was 4,462 passed / 1,068 failed (80.7%) — and half of
those failures were a single protocol bug. Describe answered NoData for any
query with a CTE, then Execute sent DataRows anyway; pgjdbc refuses that
outright with "Received resultset tuples, but no field structure for them",
and a data-modifying CTE (`WITH x AS (INSERT … RETURNING …) SELECT * FROM x`)
tripped it every time. Describe now derives a CTE query's shape by planning
the outer SELECT against synthetic tables standing in for each CTE — the
data-modifying ones described from their RETURNING clause, nothing executed,
no side effects. That one fix took the gauge to **4,962 passed / 568 failed
(89.7%)**.

This is the third distinct form of the same protocol violation the SQL
gauges have surfaced this week (computed WHERE clauses, views, now CTEs),
each caught by a different client — which is exactly the argument for
running several strict drivers rather than one.

#### Added

- `pgjdbc_validation/` (runner with JDK-21 discovery, per-class enumeration
  so exclusions are effective, JUnit-XML aggregation, report generator),
  `vendor/pgjdbc` submodule at REL42.7.13, `invoke validate-pgjdbc`, and a
  weekly `validate.yml` row reusing the java/kotlin JDK + Gradle cache steps.

#### Fixed

- `sql/engine.py`: extended-protocol Describe reported NoData for every CTE
  query while Execute emitted rows — a protocol violation that made
  data-modifying CTEs unusable from strict clients.

### The sqllogictest gauge grows a second protocol lane — and catches a wire bug doing it

`invoke validate-slt` now runs every corpus file through **both** PostgreSQL
wire protocols: sqllogictest-rs's `postgres` engine (simple query) and
`postgres-extended` (Parse/Bind/Execute), completing the two-lane design the
gauge plan called for. 52 of 60 lane-files pass; the only failures are the
four declared SQLite-vs-Postgres divergences, doubled across lanes.

The new lane immediately earned its keep: a `SELECT` from a view over the
extended protocol answered Describe with NoData and then sent DataRows — a
protocol violation strict libpq clients reject outright. Describe now
expands view references (on a copy, leaving the stored prepared statement
pristine) so the declared row shape always precedes the rows.

#### Added

- `slt_validation/`: the `postgres-extended` lane (both engines per include
  file, lane-tagged report).

#### Fixed

- `sql/engine.py`: extended-protocol Describe of a SELECT-from-view
  answered NoData while Execute emitted DataRows.

### pgbench and psql run clean — the SQL server's stress smoke lands

Unmodified `pgbench` now drives SecantusDB end to end: the full init cycle
(multi-table `DROP TABLE`, table creation, a 100,000-row client-side `COPY`,
`VACUUM`, and `ALTER TABLE … ADD PRIMARY KEY`), then the TPC-B transaction
script in all three protocol modes — simple, extended, and prepared — plus a
concurrent select-only lane. `psql`'s catalog family (`\dt`, `\d table`,
`\di`, `\l`, `\dn`) runs without error. All of it is packaged as `invoke
sql-stress` (the G7 gauge of the SQL conformance portfolio), weekly in CI,
with the invariant that any error or dropped connection is a bug.

Getting there closed a string of real gaps: multi-name `DROP TABLE a, b, c`;
`VACUUM` accepted; `ALTER TABLE ADD PRIMARY KEY` as a true migration
(validates NOT NULL and uniqueness, then re-keys every existing row onto the
column value); PG's unknown-type literal coercion in arithmetic (`abalance +
$1` with an untyped text parameter — how pgbench binds everything); the
`OPERATOR(pg_catalog.~)` regex spelling with `COLLATE`; schema-qualified
`array_to_string`; comma-join scalar subqueries (psql's collation lookup);
literal `IN` lists in `JOIN ON`; and the pg_catalog surface psql reads —
owner/toast/statistics columns on `pg_class`, encoding and collation on
`pg_database`, `pg_policy`, and present-but-empty `pg_trigger` /
`pg_statistic_ext` / `pg_inherits` / `pg_rewrite` / publication catalogs.

One documented boundary: under concurrent writers to the same row,
WiredTiger's optimistic concurrency surfaces a PG-SERIALIZABLE-style `40001`
serialization failure rather than blocking like READ COMMITTED. Retry-capable
clients handle this normally; the smoke keeps its write lanes single-client
and the retry-semantics question is tracked in the backlog.

#### Added

- `sqlstress_validation/` + `invoke sql-stress` + weekly `validate.yml` row
  (installs postgresql-contrib for pgbench/psql).
- `sql/executor.py`: `ALTER TABLE … ADD [CONSTRAINT] PRIMARY KEY` with row
  re-keying and 23502/23505/42P16 validation.
- `sql/planner.py` + `sql/engine.py`: multi-name `DROP TABLE`; `VACUUM`;
  `OPERATOR(pg_catalog.~ / ~*)` (+ negations) rewritten to regex matches;
  literal `IN` lists in join `ON`.
- `sql/scalar.py`: unknown-text numeric coercion in arithmetic (22P02 on
  garbage), `pg_get_userbyid`, `pg_encoding_to_char`, schema-qualified
  `array_to_string`, comma-join (cartesian) scalar subqueries.
- `sql/virtual.py`: `pg_class` owner/toast/check/flag columns, `pg_database`
  encoding/collation/ACL, `pg_namespace` owners, `pg_index` validity flags,
  `pg_policy`, and empty `pg_trigger` / `pg_statistic_ext` / `pg_inherits` /
  `pg_rewrite` / `pg_publication*` catalogs.

### Chasing the JDBC driver's failures turns up six real server bugs

Working the pgjdbc conformance gauge's failure clusters took it from 89.7% to
**92.4%** of the driver's `jdbc2` suite — but the point is what the failures
were hiding. Six of them were genuine correctness bugs, two of which produced
wrong answers rather than errors.

The starkest: an **ungrouped aggregate returned no rows when its WHERE
excluded everything**. `SELECT count(*) WHERE 1=2` answered "no rows" where
PostgreSQL answers `0`, and `SELECT max(3) WHERE 1=2` answered nothing where
PostgreSQL answers one NULL row. This was verified against a real PostgreSQL
14.13 rather than from memory — and it means `SELECT 0/count(*) WHERE 1=2`
now raises division-by-zero, which is precisely how pgjdbc's batch tests
inject a runtime failure. A pre-existing test had encoded the wrong
behaviour; it has been corrected with the verification noted in place.

Also fixed: BC-era timestamps are accepted with the era marker either side of
the zone offset (pgjdbc sends `0101-01-01 BC +00`, PostgreSQL's datetime
input is field-order flexible), and a BC value stored in a `date` column no
longer silently loses its era and becomes an AD date. `time` and `timetz`
accept a full timestamp and keep the time-of-day, as PostgreSQL does.
Multi-dimensional enum arrays (`flag[][]`) no longer crash the server, and
nested arrays render with nested braces instead of quoted JSON. `x = ANY(…)`
works in per-row evaluation, `current_schemas()` is implemented, and
`ALTER DATABASE … SET` stores database-level GUC defaults applied to new
sessions with PostgreSQL's precedence. Finally, extended-protocol Describe no
longer needs parameter *values*: `SELECT $1::inet` has a shape fixed by its
cast target.

#### Added

- `sql/scalar.py`: `current_schemas(include_implicit)`, `x = ANY(<array>)` in
  per-row evaluation, `pg_encoding_to_char`.
- `sql/engine.py` + `sql/catalog.py` + `sql/session.py`: `ALTER DATABASE …
  SET / RESET [ALL]` database-level GUC defaults, merged into new sessions
  (explicit session settings still win).
- `sql/engine.py`: value-free Describe fallback for cast projections over
  unbound parameters.

#### Fixed

- `sql/planner.py`: an ungrouped aggregate now yields exactly one row when the
  WHERE excludes the implicit row (COUNT 0, others NULL) — previously zero
  rows, a wrong answer. Verified against PostgreSQL 14.13.
- `sql/datetimes.py`: the BC era marker is accepted before or after a zone
  offset; a BC/out-of-range value with a time part keeps its era in a `date`
  column (previously became an AD date); `time` / `timetz` accept a full
  timestamp and a trailing offset.
- `sql/scalar.py`: multi-dimensional enum arrays (`flag[][]`) raised an
  internal error; labels are now validated at every depth.
- `sql/typemap.py`: nested array text rendering inferred its element type from
  the outer list, rendering sub-arrays as quoted JSON instead of nested braces.

### Unqualified SQL names resolve through `search_path`

The PostgreSQL front end resolved an unqualified relation name to the
`public` schema and nowhere else. `SET search_path TO reporting` followed by
`SELECT * FROM orders` raised `relation "orders" does not exist` even though
`reporting.orders` was right there — the schema was addressable only by
spelling it out on every reference. Unqualified names now walk `search_path`
in order and bind to the first schema that holds them, which is what every
tool that sets a search path and then writes plain SQL expects.

Resolution only consults the path when the bare name misses, so a relation
that already resolved is never redirected, and the rewrite happens on the
statement itself — a read and a write of the same unqualified name are
guaranteed to address the same schema. `CREATE TABLE` is deliberately exempt:
Postgres creates into the path's first schema rather than binding to a
same-named relation further along it.

Separately, a fixed wrong answer: a nested `SELECT` inside a `FROM`-less one
had its aggregates folded against the outer statement's single implicit row,
so `SELECT (SELECT count(*) FROM t)` reported `1` for any table regardless of
its contents, and the other aggregates raised `column … does not exist`. The
subquery now aggregates over its own rows.

#### Added

- `Session.search_path`, the resolution-ordered schema list (`"$user"`
  collapsed to `public`, repeats dropped). `Session.current_schema` is now
  its first entry.
- `planner.qualify_from_search_path`, which binds unqualified table
  references to a `search_path` schema in place, skipping CTE names and
  `CREATE TABLE` / `CREATE VIEW` targets.

#### Fixed

- Unqualified relation names now resolve through every `search_path` entry
  instead of only `public`.
- Aggregates in a subquery nested inside a `FROM`-less `SELECT` are no longer
  folded against the outer implicit row.

## [0.6.0b7] — 2026-07-31

### The async oplog stack graduates to first-class options

The Rust server's storage write-path modes — the background oplog drainer,
non-logged oplog tables, and the mongod-style log-only-the-oplog data mode
with its stable-checkpoint cadence — were until now reachable only through
process-wide `SECANTUS_*` environment variables. They are now real,
per-store options at every layer: a `StorageOptions` struct on the storage
crate, `RustServer(oplog_async=…, oplog_nonlogged=…, data_nonlogged=…,
checkpoint_seconds=…)` kwargs on the embedded handle, and `--oplog-async` /
`--oplog-nonlogged` / `--data-nonlogged` / `--checkpoint-seconds` flags plus
matching `[storage]` TOML keys on the `secantusd-rs` daemon. Unset options
defer to the environment variables, so existing env-driven workflows are
unchanged; an explicit option wins for that store only.

Two async-mode gaps closed on the way: an async store now prunes its oplog
opportunistically from write volume (the every-1000-emits cadence the sync
path always had — previously an async store only pruned on explicit calls),
and `create_archive` drains the oplog queue before its checkpoint so a
backup taken under the async drainer can no longer miss acknowledged writes.

#### Added
- `secantus_storage::StorageOptions` + `Storage::open_with_options` — per-store
  `wt_config` / `durable` / `oplog_async` / `oplog_nonlogged` / `data_nonlogged` /
  `checkpoint_seconds`; `None` defers to the matching `SECANTUS_*` env var.
- `RustServer` kwargs `oplog_async` / `oplog_nonlogged` / `data_nonlogged` /
  `checkpoint_seconds` (embedded handle).
- `secantusd-rs` flags `--oplog-async` / `--oplog-nonlogged` / `--data-nonlogged` /
  `--checkpoint-seconds N` and `[storage]` keys `oplog_async` / `oplog_nonlogged` /
  `data_nonlogged` / `checkpoint_seconds` (Rust-daemon-only; `secantusd-py`
  rejects them).

#### Fixed
- Async-mode change streams could surface **pre-open events**: a write
  acknowledged before `watch()` could still be queued at the drainer, so the
  open position (seeded at the drainer's watermark) sat below it and the event
  leaked into the new stream (pymongo's `test_kill_cursors`, async-only). The
  open path now waits (bounded) for the drainer to reach the minted tail
  captured at open (`Storage::oplog_open_seq`); sync mode is unchanged — an
  open transaction's pinned visible tail is already the correct open position,
  and flushing there would block opens behind long transactions.
- Async-oplog stores never pruned the oplog from write volume; the drain path
  now mirrors the sync emit path's opportunistic every-1000-emits prune.
- `create_archive` under the async drainer could snapshot before queued oplog
  entries landed; it now calls `flush_oplog()` first.
- `docs/rust/embedded.md` documented `replica_set_name=None` as defaulting to
  the replica-set persona; the embedded handle's default is a plain standalone
  `hello` (pass `replica_set_name="secantus"` for change streams).

### The SQL server gets its ORM gauge — and a primary-key fidelity fix to go with it

SQLAlchemy's own dialect-compliance suite now runs against SecantusDB's
PostgreSQL server as a first-class conformance gauge (`invoke
validate-sqlalchemy`), joining the psycopg and sqllogictest gauges in the
weekly validation run — the sqllogictest gauge itself also graduates to weekly
CI in the same stroke. Nothing is vendored: the suite ships inside the
sqlalchemy package, pointed at a daemon server through the stock
`postgresql+psycopg` dialect, with SecantusDB's capabilities declared in a
requirements class the suite is designed to read. The opening baseline is 572
of 738 executed tests passing (77.5%), published in the new
`docs/validation-report-sqlalchemy.md`.

Standing the gauge up flushed out a real correctness bug: a table-level
`CONSTRAINT <name> PRIMARY KEY (…)` was silently dropped — the column was
never mapped to the document `_id`, so primary-key uniqueness was not
enforced and duplicate keys were accepted. Declared PK constraint names are
now honored end-to-end: enforcement, catalog reflection (in place of the
synthesized `<table>_pkey`), and duplicate-key error messages. The suite's
provisioning also forced two smaller statement gaps closed: `CREATE / DROP
EXTENSION` (citext, hstore, and plpgsql accepted — the extensions whose
functionality ships built in; anything else is honestly unavailable) and
`COMMENT ON CONSTRAINT` for check, unique, foreign-key, and primary-key
constraints.

#### Added

- `sqlalchemy_validation/`: the G6 ORM gauge of `tasks/sql-gauges-plan.md` —
  runner, capability declarations (`requirements.py`), report generator, and
  an `invoke validate-sqlalchemy` task; weekly in `validate.yml`.
- `.github/workflows/validate.yml`: the sqllogictest gauge (`validate-slt`)
  runs weekly too, with a pinned cached `sqllogictest-bin 0.29.1`.
- `sql/engine.py`: `CREATE EXTENSION [IF NOT EXISTS]` / `DROP EXTENSION
  [IF EXISTS]` for citext / hstore / plpgsql (no-op success); unknown
  extensions raise `0A000`, unknown drops `42704`.
- `sql/engine.py` + `sql/planner.py`: `COMMENT ON CONSTRAINT <c> ON <t>`
  (check / unique / FK / PK), stored in the catalog; `IS NULL` removes.

#### Fixed

- `sql/planner.py`: a table-level `CONSTRAINT <name> PRIMARY KEY (…)` was
  silently ignored — no `_id` mapping, no uniqueness enforcement. The PK now
  applies regardless of clause position, and the declared constraint name is
  recorded (`TableDef.pk_name`) and surfaced by `pg_constraint` /
  `pg_class` reflection and duplicate-key errors instead of the synthesized
  `<table>_pkey`.

### The SQLAlchemy compliance gauge climbs from 77% to 97% — and takes a pile of SQL fixes with it

One day after the SQLAlchemy dialect-compliance gauge landed at 77.5%, a
sweep through its failure clusters brought the PostgreSQL server to **713 of
735 executed suite tests passing (97.0%), with zero errors**. As with the
gauge's first landing, the score is a by-product: each cluster traced to a
real server gap, and each fix is ordinary engine behavior any client
benefits from.

The catalog now tells the truth about more things: temp tables carry
`relpersistence 't'`, are visible only to their creating session
(`pg_table_is_visible` is session-aware), and are dropped when that
session's connection closes — real Postgres temp-table lifecycle. Declared
type modifiers survive into reflection (`varchar(52)` reports its length and
`character varying(52)` from `format_type`; numeric precision/scale
likewise), `pg_get_expr` returns stored default expressions (so a SERIAL
column reflects its `nextval` default and `autoincrement`), plain views
expose their output columns through `pg_attribute`, constraint comments
reflect through `pg_description`, `pg_get_constraintdef` quotes identifiers
the way `quote_ident` does (fixing every "bizarro character" reflection
case), and a composite primary key reflects its declared column order.

The expression engine grew `LIKE … ESCAPE` (with PG's `22025` invalid-escape
error and `ESCAPE ''` disabling escaping), computed LIKE patterns over the
extended protocol (Describe no longer fails on a WHERE that will be
evaluated per-row), `IS [NOT] DISTINCT FROM` in per-row evaluation, exact
numeric division for int-to-numeric casts (`CAST(15 AS NUMERIC) / 10` is
`1.5`, while `15 / 10` stays integer division), float⊕numeric operand
harmonization, constant expressions in `LIMIT` / `OFFSET`, `INSERT …
DEFAULT VALUES`, and `CREATE SEQUENCE … NO MINVALUE NO MAXVALUE`.

#### Added

- `sql/planner.py` + `sql/scalar.py`: `LIKE … ESCAPE` (pushdown + per-row),
  `IS [NOT] DISTINCT FROM` (per-row), constant expressions in
  LIMIT/OFFSET, `INSERT … DEFAULT VALUES`, `CREATE SEQUENCE NO
  MINVALUE / NO MAXVALUE`.
- `sql/virtual.py`: plain-view columns in `pg_attribute`; constraint
  comments in `pg_description`; `quote_ident` semantics in
  `pg_get_constraintdef`; temp tables report `relpersistence 't'` /
  `pg_temp_1`.
- `sql/session.py` + `sql/engine.py` + `sql/pgserver.py`: session-scoped
  temp-table lifecycle — visibility limited to the creating session, drop at
  connection teardown.
- `sqlalchemy_validation/requirements.py`: temp-table, constraint-index,
  and include-columns capabilities declared.

#### Fixed

- `sql/scalar.py`: `CAST(<int> AS NUMERIC)` now yields numeric, so division
  is exact instead of silently truncating; mixed float/Decimal arithmetic
  no longer raises `TypeError` (float8 wins, as in PG); `pg_get_expr`
  returned NULL for every stored default, hiding SERIAL defaults and
  `autoincrement` from reflection; `format_type` ignored type modifiers.
- `sql/engine.py`: extended-protocol Describe failed outright on a WHERE
  clause that Execute would evaluate per-row (computed LIKE patterns over
  bound parameters errored under psycopg); `CREATE SEQUENCE … NO MINVALUE`
  crashed with an internal error.
- `sql/planner.py`: a composite PK's declared column order
  (`PRIMARY KEY (name, id, attr)`) was lost in reflection.

### The SQLAlchemy compliance gauge reaches 100% — every executed suite test passes

The final round on the SQLAlchemy dialect-compliance gauge closes the
residual tail: **731 of 731 executed suite tests pass, with zero failures
and zero errors**, up from 77.5% at the gauge's first landing. Nothing is
deselected; the only declared divergence is `datetime_microseconds`
(BSON datetimes are int64 milliseconds, and the shared dual-protocol
document store is the product), closed through the suite's own capability
mechanism — the same switch MySQL-family dialects close.

As before, the score is a by-product of real engine work. A FROM-less
`SELECT … WHERE EXISTS (…)` now routes through the constant path with the
subquery evaluated against real storage; parenthesized set-operation arms
carry their own ORDER BY and LIMIT; derived tables can be set operations,
`VALUES` lists with column aliases (the shape SQLAlchemy's insertmanyvalues
sentinel emits), or FROM-less selects; `INSERT` accepts any constant
expression in a VALUES cell (`nextval('seq')` included); and covering
indexes (`CREATE INDEX … INCLUDE (…)`) store their columns and reflect
through `pg_index`'s `indnkeyatts` split.

One fix in this round was a silent wrong-answer, the worst kind: a scalar
subquery ignored its ORDER BY and LIMIT, so `(SELECT id FROM t ORDER BY id
DESC LIMIT 1)` returned the *first* row in storage order instead of the
last. Ordered, limited, grouped, or joined scalar subqueries now run through
the full query engine, and a subquery returning more than one row raises
PG's `21000` instead of picking one arbitrarily.

#### Added

- `sql/engine.py` + `sql/planner.py` + `sql/executor.py`: FROM-less
  `WHERE EXISTS`; parenthesized union arms; set-operation / VALUES /
  FROM-less derived tables; constant expressions (incl. `nextval`) in
  INSERT VALUES cells; covering-index `INCLUDE` metadata + reflection
  (`pg_index.indnkeyatts`).
- `sqlalchemy_validation/requirements.py`: `supports_distinct_on` opened;
  `datetime_microseconds` closed with the BSON-millisecond rationale.

#### Fixed

- `sql/scalar.py`: a scalar subquery with ORDER BY / LIMIT / GROUP BY /
  joins silently ignored them (wrong row returned); it now runs through the
  engine, and >1 result row raises `21000`.
- `sql/engine.py`: extended-protocol Describe answered NoData for a set
  operation whose first arm was parenthesized, then Execute sent DataRows —
  a protocol violation that crashed libpq clients.

### The pgx gauge lands — Go's strictest pgwire client now measures the SQL server

jackc/pgx joins the SQL server's conformance portfolio as its fourth external
gauge. `invoke validate-pgx` runs the vendored pgx v5.9.2 `pgconn` and
`pgproto3` test packages — the hand-rolled wire client and message codecs,
the Go analogue of the mongo-c-driver gauge on the Mongo side — completely
unmodified, pointed at a daemon server through `PGX_TEST_DATABASE`. It runs
weekly in CI alongside the psycopg, sqllogictest, and SQLAlchemy gauges.

The opening baseline is **291 passed / 87 failed / 22 skipped (77.0%)**:
the `pgproto3` wire codecs pass 99.4%, while `pgconn` (55.7%) exposes two
clear feature clusters worth their own follow-ups — pipeline mode (Sync-less
extended-protocol batching) and CancelRequest handling — now recorded in the
backlog as the next levers.

#### Added

- `pgx_validation/` (runner, package list, `go test -json` report generator),
  `vendor/pgx` submodule pinned at v5.9.2, `invoke validate-pgx`, and a
  weekly `validate.yml` row sharing the Go gauge's toolchain step.

### Tables get real schemas — and NOT LIKE stops lying

Relations in a user schema are now first-class: `CREATE TABLE
test_schema.users` coexists with `public.users`, resolves qualified from any
statement (DML, views, sequences, indexes, comments, foreign keys), reflects
under its own `pg_namespace` row, and is invisible to unqualified lookups —
`pg_table_is_visible` now enforces the default search path, exactly like real
Postgres. `DROP SCHEMA … CASCADE` takes the schema's tables with it, creating
into a nonexistent schema raises `3F000`, and a cross-schema foreign key
renders its target as separately-quoted identifiers in
`pg_get_constraintdef`. Internally a schema-qualified relation stores under a
dotted catalog key (`test_schema.users`) — the same mapping user-defined
types adopted — so the dual-protocol Mongo view addresses the backing
collection as `db["test_schema.users"]`.

With that in place the SQLAlchemy compliance gauge's `schemas` capability
opens, unlocking the suite's entire schema-qualified surface: **978 of 978
executed tests pass (100%)**, up from 731 executed before, still with zero
failures and zero errors.

Standing the schema surface up flushed out a genuine wrong-answer bug:
sqlglot parses `NOT LIKE` as `Like(negate=True)` rather than wrapping it in
`NOT`, and both the pushdown translator and the per-row evaluator ignored the
flag — so `WHERE n NOT LIKE 'pg_%'` silently behaved as `LIKE`. Both engines
now honor the negation.

#### Added

- `sql/planner.py` (`qualified_table_name`) + resolution sites across
  `engine`/`executor`: schema-qualified tables, views, and sequences stored
  under dotted catalog keys; `3F000` on unknown target schemas; DROP SCHEMA
  CASCADE drops contained tables.
- `sql/virtual.py`: relations split into (schema, relname) for `pg_class` /
  `information_schema` reflection; `pg_temp_1` namespace row; cross-schema
  FK targets quoted per part in `pg_get_constraintdef`.
- `sql/planner.py`: `pg_table_is_visible` lowers to a search-path check
  (default namespaces + the session's own temp tables).
- `sqlalchemy_validation/`: the `schemas` capability opens; the runner
  pre-provisions `test_schema` / `test_schema_2` (SQLAlchemy's documented
  DBA setup step).

#### Fixed

- `sql/planner.py` + `sql/scalar.py`: `NOT LIKE` behaved as `LIKE` — sqlglot
  encodes the negation as `Like(negate=True)`, which both engines ignored.
- `sql/executor.py`: `COMMENT ON` a schema-qualified table or column landed
  on the same-named public relation.
- `sql/planner.py`: an auto-named foreign key on a schema-qualified table
  minted `schema.table_col_fkey` instead of PG's `table_col_fkey`.

## [0.6.0b6] — 2026-07-31

### Log-only-the-oplog becomes crash-safe: replay-on-open recovery lands

The `SECANTUS_DATA_NONLOGGED` mode — the mongod storage architecture, where
only the oplog is WAL-journaled and the data tables are checkpoint-durable —
graduates from a measure-only benchmark probe to a recoverable
configuration. A periodic stable checkpoint (60s cadence, the mongod
default; `SECANTUS_CHECKPOINT_SECONDS` overrides) anchors a marker in the
always-logged oplog-meta table, the oplog prune never touches entries above
the marker (they are the recovery source), and `Storage::open` replays the
oplog above the marker through the ordinary write paths — idempotently, so
the deliberately conservative marker can never double-apply work. A clean
close anchors a final checkpoint even under the fast-storage test
environment, whose skip-the-close-checkpoint optimisation would otherwise
lose unlogged tables' data with no crash involved.

The contract is proven by a hard-kill harness
(`tests/test_crash_recovery.py`): a writer subprocess is `SIGKILL`ed
mid-load and every acknowledged write must be present after the reopen —
including with no checkpoint ever taken, where the entire dataset comes
back from oplog replay alone. Durability matches the logged default at
each `sync_on_commit` setting: with per-commit fsync every acknowledged
write survives a hard kill; without it, a hard crash can lose the unsynced
WAL tail — in either mode, exactly as before.

The default is unchanged, and deliberately so: with the durability
anchoring live, the mode's own measurements moved. A single writer gains
~5%, and a workload whose oplog stays under the retention cap keeps the
probe-era headroom (~122k docs/s at eight writers measured with anchoring
idle) — but a sustained eight-writer load at cap pressure pays the
periodic checkpoint of a hot, unlogged working set and lands at roughly
half the logged default's throughput. Finding 14 records the decomposition;
the default flip stays parked until the checkpoint cost is tamed. The mode
is correct and recoverable today; choose it for read-heavy, single-writer,
or bounded workloads.

#### Added

- Rust server: replay-on-open crash recovery for `SECANTUS_DATA_NONLOGGED`
  stores — stable-checkpoint marker + periodic checkpoint thread +
  idempotent oplog replay + prune clamp; the mode is recorded per-store at
  create time (existing stores are unaffected by the env var).
- `tests/test_crash_recovery.py`: the hard-kill recovery harness (SIGKILL
  mid-load → reopen → every acknowledged write present), plus WT-level
  stable-marker tests.

## [0.6.0b5] — 2026-07-30

### The Rust server's change streams get a real oplog visibility point

Concurrent writers on different collections could permanently lose a change
event. The Rust server's tailable cursors treated the highest *minted* oplog
seq as the readable tail, but a seq is minted inside its writer's still-open
transaction — so a writer on one collection could commit a *later* seq while
an earlier one was still in flight, and a change stream that polled in that
window advanced its resume position past the hole. When the in-flight
transaction then committed, its event sat behind the stream's position:
dropped from the live stream and unreachable on resume. Same-collection
writers never hit this (the per-collection lock serializes them), which is
why it survived every single-collection test.

The fix is the analogue of WiredTiger/mongod's `all_durable` timestamp: an
in-flight window tracks every minted-but-unresolved seq range, and readers —
`wait_for_oplog`, `read_oplog`, change-stream open positions, post-batch
resume tokens — are bounded by its floor. A commit releases its range and
the tail advances; a rollback releases it silently, leaving a permanent seq
hole the shard merge already tolerates, so an aborted transaction can never
stall the stream. `flush_oplog` in sync mode now genuinely waits for the
window to drain, and abandoned transaction handles release their ranges on
drop so a reaped session cannot pin the tail.

#### Fixed

- Rust server: change streams no longer lose events when writers on
  different collections commit out of oplog-mint order (live and on
  resume). New `Storage::oplog_visible_tail_seq()` is the bound every
  reader uses; three WT-level pinning tests and a cross-collection
  database-watch exactly-once test guard the invariant.

#### Added

- `tests/test_mongo_server_concurrency.py::test_db_change_stream_exactly_once_across_collections`
  — N per-collection writers under a database-wide watch, asserting
  exactly-once delivery (no duplicates, no losses), on both servers.

### The Python server's change streams get the oplog visibility point too

The Rust server's oplog visibility fix has a twin on the Python server.
Since the per-collection lock split, Python writers on different
collections mint their oplog sequence numbers and commit their WiredTiger
transactions independently — so a writer could commit a *later* sequence
while an earlier one was still inside an open transaction, and a change
stream polling in that window advanced its resume position (the
`scan_high` skip bound) past the hole. When the in-flight transaction
committed, its event sat behind the stream's position: dropped live and
unreachable on resume. Multi-document transactions were already protected
by commit-time minting, but the flush between mint and WiredTiger commit
had the same narrow window.

The fix is the same `all_durable`-style design: an in-flight mint window
pins the visible tail at its floor, registered when sequences are minted
and released when the owning transaction commits or rolls back (batch
transactions, the user-transaction commit flush, and bare autocommit
emits each resolve at their own point). Every reader is bounded by it —
the tailable-getMore wake predicate, change-stream open positions,
`read_oplog` and its `scan_high`, the PITR archive head, and
`startAtOperationTime` (which now waits briefly for the window to drain
past its answer instead of finalising a position an in-flight event could
land behind). A rolled-back mint leaves a permanent, tolerated hole and
can never stall the stream.

#### Fixed

- Python server: change streams no longer lose events when writers on
  different collections commit out of oplog-mint order (live and on
  resume); `startAtOperationTime` can no longer skip an event minted
  inside a still-open transaction. Five WT-level pinning tests
  (`tests/test_oplog_visibility.py`) mirror the Rust suite.

### The oplog prune stops taxing every write — +25% single-writer, +62% at eight writers

Phase-0 profiling of the concurrency-parity program (Finding 12) caught the
Rust server's opportunistic oplog prune consuming ~36% of the sustained
write path: once a workload passes the 100k-entry oplog cap — about four
seconds into any sustained run — every sweep re-read the full 8 KiB value
of every doomed row through the shard merge, copying ~8 MB per sweep just
to learn which seqs to delete, on the writer's own thread. The sweep now
walks keys only, peeking a row's timestamp just in the retention tail
beyond the cap excess, and the emit path stops re-running WiredTiger's
schema-locked `create` for its oplog shard on every batch (a
first-touch bitmask remembers what exists). Measured on the Finding-12
baseline rig: sync single-writer 25.4k → 31.6k docs/s (+25%), eight
writers 41.5k → 67.3k (+62%), lifting durable-path scaling from 1.65× to
2.13× and oplog retention from 22% to 36% of the no-oplog ceiling.

The slice also closes the `startAtOperationTime` residual recorded by the
visibility-point fix: `find_seq_for_ts` no longer finalises a resume
position past a minted-but-uncommitted oplog entry whose timestamp
qualifies — it waits (bounded) for the in-flight window to drain past its
committed-view answer and rescans, so a transaction committing mid-open
surfaces the earlier event instead of losing it.

#### Fixed

- Rust server: `startAtOperationTime` can no longer skip an event whose
  oplog entry was minted inside a still-open transaction (bounded wait on
  the in-flight window; falls back to the committed view at the deadline —
  today's behaviour — only for long-open transactions).

#### Changed

- Rust server: the opportunistic oplog prune identifies doomed rows with a
  key-only shard merge (values peeked only for the retention tail), and
  oplog shard tables are created on first touch instead of per-batch.

### The Finding-13 winners become the defaults — another +14% at eight writers, no knobs required

The oplog append-path sweep's measured winners now ship as defaults on the
Rust server. Oplog writes route across two shard tables instead of sixteen —
the wide split existed to spread a rightmost-page append hotspot that the
RecordId keying and the prune fix eliminated, and the sweep measured every
narrower width beating sixteen; the read side still scans all sixteen, so
stores written under any width stay fully readable and interchangeable. The
oplog and pre-image btrees are created append-tuned (`split_pct=100,
leaf_page_max=128KB` — rows arrive in ascending seq order and are never
updated, so pages fill completely before splitting), and the daemon and the
Python `RustServer` handle raise their WiredTiger cache default from a 1G to
a 4G *cap* — WiredTiger fills cache lazily, so idle test servers stay as
small as before while sustained writers stop thrashing eviction
(`--cache-size` / `cache_size=` still override; the low-level
`Storage::open` library default is unchanged).

Interleaved A/B against the previous defaults on the reference box: sync
single-writer 31.8k → 35.1k docs/s (+10%), eight writers 78.1k → 88.7k
(+14%) — on top of the prune-fix release's +62%. Oplog block compression
stays on deliberately: the sweep measured turning it off cratering
throughput to a fifth of the ceiling (bigger uncompressed pages mean more
eviction IO, and IO volume — not CPU — is the constraint).

#### Changed

- Rust server: oplog write routing defaults to 2 shard tables (was 16);
  `SECANTUS_OPLOG_SHARDS` still overrides 1–16; reads scan all tables
  regardless, so on-disk compatibility is unaffected.
- Rust server: oplog/pre-image tables are created with
  `split_pct=100,leaf_page_max=128KB` (fresh stores; existing stores keep
  their config).
- Rust server: `secantusd-rs` and the Python `RustServer` handle default
  `cache_size` to `4G` (a lazy cap, was `1G`); `docs/concurrency.md`'s
  tuning guidance updated (log `prealloc` now hurts at eight writers
  post-prune-fix; never disable oplog compression).

### Oplog experiment hooks and the sweep that found 54%-retention durable writes

Three measure-oriented env hooks land on the Rust server so oplog append-path
experiments no longer need a rebuild: `SECANTUS_OPLOG_SHARDS` overrides how many
shard tables the write path routes across (1–16; reads always consider all, so
any store stays fully readable), `SECANTUS_OPLOG_TABLE_EXTRA` appends
last-key-wins WiredTiger config to the oplog/preimage table creates (the
`SECANTUS_WT_CONFIG_EXTRA` trick at table scope), and `SECANTUS_DATA_NONLOGGED`
is a loudly-documented, crash-unsafe, measure-only probe of the mongod
architecture (journal only the oplog). `bench/oplog_sweep.py` drives the arms
interleaved and reports retention against the same-session no-oplog ceiling.

The sweep's headlines (recorded as Finding 13): the 16-way oplog sharding is
now pure overhead — every lower shard count beats it at eight writers;
turning oplog compression off craters throughput to 19% retention (zlib is
load-bearing under write pressure); cache size is the strongest single knob;
and the winning stack (2 shards + append-tuned oplog pages + 4G cache) reaches
**102.8k docs/s at eight writers fully durable — 54% of the no-oplog ceiling**,
up from 43% on the defaults. The mongod-architecture probe adds only the last
+11% on top (60%, matching mongod's own 61% oplog-retention ratio), so the
replay-on-open recovery project is parked until the config winners ship.

#### Added

- Rust server: `SECANTUS_OPLOG_SHARDS`, `SECANTUS_OPLOG_TABLE_EXTRA`, and
  `SECANTUS_DATA_NONLOGGED` (measure-only, crash-unsafe) experiment hooks —
  all default-off, create/routing-time only.
- `bench/oplog_sweep.py`: the interleaved oplog append-path sweep runner.

### Rust concurrency: steal telemetry, a read-under-load bench, and the CI that runs it

The per-collection write-lock work turned the Rust server's multi-writer
story into real scaling — it climbs to about 2.6× its single-writer rate
at four concurrent writers before a WiredTiger ceiling (specifically the
oplog's WAL append and checkpoint share) bends the curve back down, and
the opt-in async + non-logged oplog stack lifts even that to a monotonic
~2.4× at eight writers. That ceiling lives inside WiredTiger, not in a
SecantusDB lock; `docs/concurrency.md` carries the measured curve and the
attribution. This slice adds the tooling and telemetry *around* that
result rather than the measurement itself.

The new `bench/read_concurrency.py` harness measures the property the lock
split most directly buys and that a raw write-throughput curve hides: a
read-heavy workload keeps 60–75% of its standalone query throughput while
eight writers saturate the server, where before every read queued behind
every write. The `findAndModify` steal telemetry makes a concurrent-steal
storm on a hot job-queue document visible in the server log instead of
surfacing only as CPU. And the Rust parametrization of the `#451`
concurrency stress suite now actually runs in CI, where it previously
skipped itself in every lane.

#### Added

- Rust server: `findAndModify` logs a warning every few seconds of
  continuous re-picking (a concurrent writer repeatedly stealing the
  matched document), so a steal storm on a hot job-queue document is
  visible in the server log instead of surfacing only as CPU — mirroring
  the storage layer's write-conflict retry telemetry.
- `bench/read_concurrency.py`: a read-under-write-load benchmark for the
  Rust server — measures the query throughput a read-heavy client retains
  while N writers saturate the server.
- CI: the Rust parametrization of the `#451` concurrency stress suite
  (exactly-one-winner races, exact final counts, typed-errors-only) now
  runs in the `storage-engine` job — previously it `importorskip`ed in
  every lane because no other job builds the embedded Rust server.

### Refreshed PGO profile for the reworked write path

The committed profile-guided-optimization profile
(`crates/pgo/_secantus_server.profdata.tar.gz`) is regenerated against the
current hot paths — the oplog visibility point, the key-only prune, and the
new routing defaults all reshaped the write path since the profile was last
trained, and a stale profile silently forfeits PGO's gains (measured: the
refresh recovered `update_many` 1.2×→1.1×, `$group` 1.3×→1.0×,
`delete_many` 1.5×→0.9× of mongod on the six-workload benchmark).

#### Changed

- `crates/pgo/_secantus_server.profdata.tar.gz` retrained on the post-#702
  write path (wheel builds consume it; a stale profile is safe but slower).

### The admin console, documented in pictures — and four bugs it was hiding

The admin UI's documentation has always described 22 pages in prose and shown
none of them. It does now: every page in the console has a screenshot, generated
rather than hand-captured. `invoke admin-screenshots` starts a throwaway
SecantusDB on a fixed port, seeds it with a fictional shop — invented customers,
`example.com` addresses, public landmark coordinates, indexes of every shape,
users, profiler entries, backup archives — then drives all 22 pages through a
real browser with Playwright, filling and submitting forms where a bare page load
would only show an empty one. Machine-specific strings are rewritten out of the
DOM before each shot, so a committed image carries nothing about the machine that
made it. The same run publishes the four shots the marketing site uses, so the
docs, the README and secantusdb.com can't drift apart.

Driving the console through a real browser turned out to be the first time
anyone had. It found four live bugs, all invisible to the existing tests because
the templates render identically whether or not their JavaScript runs. Alpine was
loading before Chart.js, and since this Alpine build starts the moment its script
executes, the dashboard threw `Chart is not defined` during `init()` — which
aborted the component before it opened the metrics websocket. The dashboard has
been showing zeros, no charts, and a permanent "connecting…" status. Behind that
sat three more: every Alpine page called its own `init()` twice, so the
change-stream tail opened two sockets and displayed every event twice; the
sparkline canvases had no sized parent and grew until they filled the viewport;
and the geo map's markers 404'd on Leaflet image assets this package doesn't
vendor, so map pins rendered as broken images. All four are fixed, and each is
pinned by a regression test.

Regenerating the screenshots is now a release step. A browser-free test keeps
every documented page wired to an image on disk, but it can't tell a fresh
screenshot from a stale one — so the release procedure regenerates them, and the
capture itself fails loudly if any page logs a JavaScript error or is
photographed showing an empty state.

#### Added

- `scripts/admin_screenshots.py` and `invoke admin-screenshots`: Playwright-driven
  capture of all 22 admin-UI pages against a seeded throwaway server, with DOM
  anonymisation, JS-error detection, empty-state detection, and publication of
  the website-tagged subset into the Pelican theme. Flags: `--only`, `--list`,
  `--headed`, `--scale`, `--server-port`, `--keep-data`, `--skip-website`, and
  `--from-checkout` for rendering a working tree's templates and static assets
  instead of the installed package's.
- A `screenshots` optional extra carrying Playwright (kept out of `dev` so CI
  lanes don't install a browser stack they never drive).
- Screenshots throughout `docs/admin.md`, an admin-UI section in the README, and
  an admin console section on the secantusdb.com landing page.
- `tests/test_docs_screenshots.py` and `tests/test_admin_asset_order.py`.

#### Fixed

- **The admin dashboard never worked.** `alpine.min.js` loaded before
  `chart.umd.min.js`, and this Alpine build calls `Alpine.start()` as soon as its
  own deferred script runs, so `Chart` was undefined when the dashboard's
  `init()` executed. The thrown error aborted the component before `_connect()`,
  so the live-metrics websocket never opened: every tile read 0 and the status
  stayed on "connecting…". Alpine now loads last.
- Every Alpine page (`dashboard`, `changestream`, `query`, `insert`) carried a
  redundant `x-init="init()"` alongside a component that already defines
  `init()`, which Alpine invokes itself. Each page therefore initialised twice —
  two Chart instances per canvas, two metrics websockets, two change-stream
  sockets (so every event appeared twice), and duplicate collection-suggestion
  fetches.
- The dashboard's sparkline canvases had no fixed-height positioned parent, so
  Chart.js's `maintainAspectRatio: false` sizing loop grew each chart until it
  overflowed the viewport.
- Chart instances were stored in Alpine's reactive state; reached through its
  Proxy, Chart.js's internal per-chart lookups missed and every `update()` threw
  `Cannot set properties of undefined (setting 'fullSize')`. They now live in the
  component factory's closure.
- The geo map drew points with Leaflet's default marker, which loads
  `images/marker-icon.png` and `images/marker-shadow.png` relative to
  `leaflet.css` — files this package doesn't vendor. Every point 404'd twice and
  rendered broken; points are now vector `circleMarker`s needing no assets.

#### Changed

- `docs/admin.md` no longer claims the UI "never makes outbound network calls of
  its own". It makes one: the Geo page fetches basemap tiles from OpenStreetMap.
  That's now stated up front and called out in a note on the page's own section,
  since it tells a third party your IP and roughly where your data is.

### A standalone Windows binary for the Rust server

The `secantusd-rs` standalone binary now ships for Windows alongside Linux and
macOS. Every `secantusdb-v*` release attaches an `x86_64-pc-windows-msvc` archive
— a `.zip` for Explorer, and a `.tar.gz` for anyone who'd rather use the same
command on all three platforms — each with a `.sha256` beside it. The Windows
build links the C runtime statically, so the `.exe` runs on a clean machine with
no Visual C++ redistributable installed.

Windows had been listed as blocked on the MSVC WiredTiger build "producing no
static library". That turned out to be a statement about a filename rather than a
capability: MSVC emits `wiredtiger.lib` where Unix emits `libwiredtiger.a`, and
`build.rs` grew the second name some time ago. CI had quietly been linking that
static library and building `secantusd-rs.exe` on every push ever since — the
note simply outlived its cause. Enabling the release lane was mostly packaging.

Two Windows-specific problems did surface, and both were worth finding. The
linker had been warning `LNK4098: defaultlib 'LIBCMT' conflicts`, which is not
cosmetic: WiredTiger's static library uses the static C runtime while Rust's MSVC
target defaults to the dynamic one, and two C runtimes in one process means two
heaps — memory allocated inside WiredTiger and freed on the Rust side is
undefined behaviour. Building with `+crt-static` matches them. Separately, the
binary's smoke test asserted a clean exit after SIGTERM, which can't work on
Windows at all: `send_signal` maps SIGTERM to `TerminateProcess`, an immediate
kill that exits 1 and runs no handler. It now sends `CTRL_BREAK_EVENT` to a
process-group child, which the binary's console handler turns into the same
graceful shutdown Unix gets — and that test now runs on Windows in CI on every
push, so the release binary is exercised continuously rather than only at tag
time.

#### Added

- `x86_64-pc-windows-msvc` in the `release-binaries` matrix, publishing
  `secantusdb-<version>-x86_64-pc-windows-msvc.zip` + `.tar.gz` (each with a
  `.sha256`). Built with `+crt-static`; no PGO on this target yet, so it is a few
  percent slower on write-heavy paths than the other two archives and otherwise
  identical.
- The wheel-bundled `secantusd-rs` smoke test now runs on Windows in `test.yml`'s
  `storage-engine` job (it was skipped on the stale grounds that the binary
  wasn't built there).

#### Fixed

- **CRT mismatch in the Windows binary** (`LNK4098`): WiredTiger's static CRT vs
  Rust's default dynamic CRT put two C runtimes, and two heaps, in one process.
  Now built with `-Ctarget-feature=+crt-static`.
- `tests/test_rust_binary_smoke.py` asserted a graceful SIGTERM exit that Windows
  cannot deliver; it now uses `CTRL_BREAK_EVENT` with `CREATE_NEW_PROCESS_GROUP`
  there, leaving the Unix path unchanged.

#### Changed

- `docs-rust/installation.md`, `docs-rust/releases.md`, the marketing site's
  Rust-server page, and the two "Windows is blocked" comments in
  `release-binaries.yml` / `test.yml` all corrected — they described a limitation
  that no longer existed.

## [0.6.0b4] — 2026-07-29

### Restore full PGO on the arm64-macOS standalone binary

The arm64-macOS `secantusd-rs` binary's on-target profile-guided optimization is
working again, after a run of macos-14-runner-specific failures. Two quirks (both
Linux-immune) are now handled: the PGO instrumented build drops mimalloc (whose
instrumented allocator internals segfault at startup), and — the crux — the
instrumented binary writes its own profile on shutdown, because the profiling
runtime on that runner never wires up its `LLVM_PROFILE_FILE` at-exit write (a
clean `--version` exit produced no `.profraw` at all). A CI diagnostic pinned
that down; the binary now calls `__llvm_profile_set_filename` +
`__llvm_profile_write_file` to a known path (behind the instrumented-only
`pgo-instrument` feature), yielding a valid, mergeable profile (23.9k functions).
Both binary targets ship with full two-stage PGO again.

#### Fixed

- `secantusdb` arm64-macOS binary: full on-target PGO restored — the instrumented
  stage self-writes its profile (the runtime's env-driven at-exit write is inert
  on the macos-14 runner) and drops mimalloc to avoid an instrumentation segfault.

### Fix the arm64-macOS binary's PGO build crashing on mimalloc instrumentation

The standalone `secantusd-rs` binary's two-stage profile-guided-optimization
build segfaulted at startup on the arm64-macOS CI runner: the PGO **instrumented**
stage compiles mimalloc's own allocator internals with profiling counters, and
`__llvm_profile_instrument_target` faults (EXC_BAD_ACCESS) when it runs *inside*
mimalloc's first page allocation (from `LogBuffer::new` in the server bind path),
re-entering the half-initialized global allocator. The instrumented stage now
builds with the system allocator (`--no-default-features`, gating mimalloc behind
a default-on `mimalloc` cargo feature); the optimized final binary still ships
mimalloc, and the collected profile — a hint whose unmatched functions are
ignored — is unaffected by the allocator swap. The PyPI wheel's embedded server
was never affected (it consumes a committed profile, not on-target instrumentation).

#### Fixed

- `secantusdb` binary: the arm64-macOS two-stage PGO release build no longer
  segfaults in the instrumented stage. mimalloc is now a default-on cargo feature
  so the instrumented build can opt out (`--no-default-features`) while the
  shipped binary keeps mimalloc.

### Flush the PGO profile explicitly so the arm64-macOS binary build completes

With mimalloc no longer instrumented, the arm64-macOS binary's PGO instrumented
stage ran the profiling workload cleanly but wrote no `.profraw`: the LLVM
profiling runtime's atexit flush doesn't fire under the release workflow's
SIGTERM shutdown on that runner (Linux flushes normally), so the profile-merge
step had nothing to merge. The instrumented stage-1 build now compiles an
explicit `__llvm_profile_write_file()` into the shutdown path (behind a
`pgo-instrument` cargo feature, off for every normal build), flushing the profile
deterministically before exit.

#### Fixed

- `secantusdb` binary: the arm64-macOS two-stage PGO build now writes its profile
  under the workflow's SIGTERM shutdown (explicit `__llvm_profile_write_file()`
  behind the instrumented-only `pgo-instrument` feature), so the profile-merge
  and optimized stages complete.

## [0.6.0b3] — 2026-07-28

### Fix `drop` (and other ops) on a never-written collection under lazy shards

Lazy shard creation (0.6.0b2) makes a collection's documents shard exist only
once something is written to it, and the Rust server's `drop_collection` ran
`purge_collection_tables` unconditionally — which opened the collection's shard
cursor and failed with a WiredTiger "No such file or directory" error when the
collection had never been written (dropping an empty / never-created collection,
a no-op in MongoDB). The purge now treats an absent shard as "no rows to remove".
The gap escaped the test suite because tests always create a collection before
dropping it; it was caught by the standalone binary's PGO-profile workload, whose
first operation is `coll.drop()` on a fresh collection. A regression test now
exercises every operation (drop / find / count / distinct / delete / update /
aggregate / findAndModify / create-index-then-drop) against never-written
collections on the Rust server.

#### Fixed

- Rust server: `drop` on a collection whose shard was never written no longer
  errors with a WiredTiger `ENOENT`; `purge_collection_tables` tolerates an
  absent documents shard (matching the other lazy-shard read/scan/merge paths).

## [0.6.0b2] — 2026-07-28

### Rust server: 2.2× multi-writer throughput with the async + non-logged oplog stack

The Rust server's concurrent-write ceiling was the oplog: every write paid a
WAL-logged oplog append, holding 8-writer throughput to ~56k docs/s (sync
default) or ~88k with the async-oplog prototype, against a ~191k no-oplog
ceiling. Two new opt-in levers close most of that gap. Setting
`SECANTUS_OPLOG_NONLOGGED=1` creates the oplog and pre-image tables with
WiredTiger WAL logging disabled, so oplog rows are checkpoint-durable only — in
async mode that removes the drainer's WAL volume from the writers' path and
lifts 8-writer throughput to ~125k docs/s, 2.2× the sync default (and ~1.9× a
single writer's async rate), while change streams stay exactly-once. The async
drainer also now coalesces queued batches into one WiredTiger transaction (up
to 32 batches / 16 MB; `SECANTUS_OPLOG_ASYNC_COALESCE=0` disables it). The
durability trade is explicit and opt-in: a hard crash loses the oplog tail
written since the last checkpoint (data tables stay fully logged and durable; a
clean shutdown flushes and checkpoints a complete oplog). Defaults are
unchanged — the synchronous, fully-logged oplog remains the out-of-the-box
behaviour.

#### Added

- `SECANTUS_OPLOG_NONLOGGED=1` — create the oplog + preimage tables with
  `log=(enabled=false)` (checkpoint-durable oplog; data unaffected). Applies at
  table-create time on a fresh store.
- Async-oplog drainer batch coalescing: queued `DrainBatch`es are written in a
  single WT transaction (caps: 32 batches / 16 MB), on by default in async
  mode; `SECANTUS_OPLOG_ASYNC_COALESCE=0` restores per-batch commits.
- `SECANTUS_DISABLE_OPLOG=1` on `secantusd-rs` — run the daemon with oplog
  emission off entirely (the "drop the oplog" throughput lever from
  `docs/concurrency.md`, previously reachable only via the embedded API).

### Lazy shard creation cuts Storage open cost ~2×, and a shutdown-hang fix

Opening a store eagerly created all ~37 WiredTiger tables up front — 16
per-collection document shards plus 16 oplog shards dominating — even for an
ephemeral store that touches a single collection. At ~10.6 ms per WT `create`
that is ~500 ms and 51 files per open, and under a highly parallel test run it
saturated disk I/O badly enough to stall (workers stuck in uninterruptible I/O
wait). Both servers now create shards **on demand**: a document shard is made on
first creation of a collection that hashes to it, and an oplog shard on first
write to it (the Python server, which never writes the sharded oplog, creates no
oplog shards at all). A fresh store now creates ~13 base tables plus only the
shards actually used, roughly halving open cost (open + one collection + insert
dropped from ~500 ms to ~300 ms; 51 files → 20). Every read / scan / merge / drop
/ rename / `$out` / delete path on both servers treats an absent shard as empty
(Python `_cursor_optional`; Rust `WtError::is_missing_table`), so a store written
with a subset of shards stays byte-compatible with an eager store and across
servers for backup / PITR — a missing shard simply reads as empty.

Separately, `Storage.close()` could hang forever inside WiredTiger's
`WT_CONNECTION->close`: it joined the background TTL sweeper (on by default,
`ttl_sweep_seconds=60`) with only a 2-second timeout and then tore WiredTiger
down anyway, so under load — when a sweep outran the 2 s budget — it closed the
sweeper's still-live WT session from the wrong thread and `conn.close()` blocked.
It now joins the sweeper and heartbeat threads to completion before any
WiredTiger teardown, and each loop closes its own thread-local session on exit.

#### Changed

- Both the Python and Rust servers create documents / oplog shard tables lazily
  (on first write; the Python server creates no oplog shards at all) instead of
  all ~37 eagerly at open, cutting Storage open cost ~2× and the per-open file
  count from 51 to ~20. Read, scan, merge, drop, rename, `$out`, and delete paths
  on both servers tolerate an absent shard, so the on-disk layout stays
  cross-server byte-compatible (a missing shard reads empty).

#### Fixed

- `Storage.close()` no longer hangs in `WT_CONNECTION->close` when the TTL
  sweeper or noop-heartbeat thread is active: the threads are joined to
  completion before WiredTiger teardown, and each closes its own thread-local
  WiredTiger session on exit rather than leaving it for a cross-thread close.

#### Internal

- Test harness: the session `_hang_watchdog` now routes its traceback to the
  per-worker crash file (`SECANTUS_FAULTHANDLER_DIR`) and stays armed through
  shutdown, so a shutdown-time wedge self-diagnoses instead of dying anonymously
  as "node down". Hang timeout tunable via `SECANTUS_HANG_SECONDS`.

#### Fixed

- Wheel builds: the embedded extension's PGO wiring passed cargo a fresh
  `RUSTFLAGS` (`-Cprofile-use=…`), clobbering the ambient flags cibuildwheel
  sets for the Linux containers — `-Ctarget-feature=-crt-static`, without which
  the musllinux target cannot produce a cdylib. Every manylinux/musllinux wheel
  build since the PGO change failed with "cannot produce cdylib … does not
  support these crate types". The PGO flags now append to the ambient
  `RUSTFLAGS` instead of replacing them (an empty ambient value composes to the
  previous behaviour, so macOS/Windows builds are unchanged).

### A much faster Rust server: mimalloc, thin LTO, and profile-guided optimization

The Rust server — both the standalone `secantusd-rs` binary and the copy
embedded in the Python wheel — is now built with the mimalloc allocator, thin
link-time optimization, and profile-guided optimization (PGO). Profiling had
pinned `malloc`/`realloc`/`free` churn from BSON materialization as a top CPU
cost on every write and aggregate path; a faster allocator attacks that
directly, LTO inlines across the crate boundaries the hot path crosses, and PGO
lays the final build out around the branches a benchmark run showed are hot.

Together they move single-client writes to **beat** standalone `mongod`,
`aggregate $group` to parity, and the whole six-workload benchmark into the
~0.8×–2.1× band — a cumulative ~30–40% off the write and aggregate paths since
the previous release, with no change in behaviour (it is entirely an allocator
and compiler-optimization story). This is primarily a Rust-server speed release
— it ships in both the wheel's embedded server and the standalone binary.

It also carries a change-stream correctness fix on the Python server: a stream
could silently skip a write that committed while it was polling (or one whose
sequence number was assigned but not yet committed), because the empty-poll skip
was bounded by the oplog tail rather than by what the poll had actually
examined. And it hardens the driver-conformance gauges so a gauge that never ran
can no longer leave behind a report that looks like it passed.

#### Added
- `invoke rust-pgo-refresh` regenerates the committed PGO profile for the
  embedded extension (instrument → run the benchmark workloads → merge → commit
  → rebuild). Needs `rustup component add llvm-tools-preview`.
- `crates/pgo/_secantus_server.profdata.tar.gz`: the committed, sparse PGO
  profile the wheel build consumes.

#### Changed
- Rust server: mimalloc is the global allocator for the embedded
  `_secantus_server` extension and the standalone `secantusd-rs` binary.
- Rust server: release builds use `lto = "thin"` and `codegen-units = 1`
  (the allocator is the dominant lever; LTO is roughly additive on top).
- Rust server: both distributions are built with PGO. The embedded extension
  build (CMake) applies a committed profile via `-Cprofile-use`
  (`SECANTUS_PGO_DISABLE=1` turns it off, `SECANTUS_PGO_GENERATE=<dir>` switches
  to instrumentation); the standalone binary is built two-stage per architecture
  in its release workflow for an on-target profile. Cumulative measured gain
  over the previous release (mongod-normalized): writes and aggregates ~−30–40%,
  moving writes past `mongod` and `aggregate $group` to parity. Raw-scan reads
  are unchanged (already at the wire floor).
- Java and Kotlin conformance gauges move from an exit-code guard to the shared
  artifact guard, so a legitimately failing run reports instead of being
  suppressed.

#### Fixed
- Python server: a change stream no longer skips past a write that commits while
  it is polling, or one whose sequence number has been assigned but not yet
  committed. The empty-poll skip is now bounded by the highest position the poll
  actually examined, not the oplog tail, so a write racing the poll is delivered
  on the next poll instead of being stepped over permanently. The Rust server
  was never affected.
- Driver-conformance gauges no longer regenerate a validation report from a
  previous run's results — a gauge that cannot start (port in use, missing
  toolchain, failed build) now exits non-zero and leaves the prior report
  untouched, instead of restamping stale numbers under today's date. Applies to
  all thirteen gauges (go, node, ruby, rust, java, kotlin, php-lib, php-ext, c,
  cxx, dotnet, psycopg, slt).

## [0.6.0b1] — 2026-07-26

### RecordId storage on both servers, faster writes, a decoupled oplog, and an Ops Board

The headline change is that **both servers now use the same on-disk document
layout, byte for byte.** The Python server's document table is keyed by RecordId —
a monotonic insertion counter — exactly as the Rust server has been since its own
RecordId work, and its index entries carry the RecordId too. That drops a write
per insert and a lookup per index-scan result (measured Python inserts ~15%
faster, unsorted scans ~35%, `$group` ~24%), makes an unsorted `find()`, tailable
cursors, and `multi:false` updates follow true insertion order like mongod, and —
because the two servers now agree on the whole format, documents and indexes both
— lets a store written by one server be read, backed up, and point-in-time
restored by the other. (There is no in-place upgrade from the older layout: a
pre-RecordId data directory is refused at open with a clear error naming the
mismatch; start from a fresh directory or downgrade to the build that wrote it.)

This release is also a Rust-server write-performance push plus a new tool for
running the project. The oplog — the write every change stream, point-in-time
recovery, and `local.oplog.rs` read depends on, and which a bare standalone
`mongod` doesn't keep at all — stopped re-encoding documents it had already
serialized for the collection write. That one change brought single-writer inserts
to within ~10% of a real `mongod` (1.1×) and updates and deletes to ~1.3×, on the
same WiredTiger engine `mongod` ships.

For concurrent writers there's a new **opt-in async oplog** (`SECANTUS_OPLOG_ASYNC=1`):
it moves the oplog write off the writer's critical path onto a background drainer,
lifting multi-writer write throughput ~1.4× while keeping change streams correct —
validated exactly-once under concurrent writers. The investigation behind it pinned
the remaining multi-writer ceiling squarely to WiredTiger's own aggregate write
rate: a parallel drainer pool (also shipped) does not beat it, and neither does WT
config tuning beyond ~10%, so the honest guidance for sustained concurrent-write
workloads stays "run a real `mongod`, or drop the oplog if you don't need change
streams." The trade for the async path is that the oplog is no longer atomic with
the data — a hard crash loses drainer-queued entries (the data itself stays fully
durable; a clean shutdown flushes) — which is why it is off by default.

The other headline is the **Ops Board**: a local web app that drives the whole
build / test / release / validate cycle from one place — a matrix of all thirteen
driver-conformance gauges with per-driver scores, a CI monitor with version-drift
detection and startable runs, graphical job progress with full-tree cancel, a
confirm-gated release page, and per-task explanations and time estimates. Alongside
these, a batch of correctness fixes brought more edges in line with `mongod`:
capped collections now evict in true FIFO order, indexes on arrays of subdocuments
actually get used, malformed wire frames surface as typed errors instead of raw
exceptions, and a panic in one collection can no longer wedge it until restart.

#### Added
- Python server: indexes record `entryFormat` in the catalog. Like the internal
  `multikey` flag it is stripped from `listIndexes`, so clients never see it.
- `invoke validate --jobs N` / `invoke validate-pymongo-async --jobs N`: run
  the gauge on N xdist workers, each with its own embedded server
  (`--dist loadfile`, so files stay whole). Default `1` — unchanged serial
  behaviour and an unchanged published number.
- `pymongo_validation/plugin.py`: `SECANTUS_GAUGE_PER_WORKER` makes each xdist
  worker start (and tear down) its own embedded server and overwrite
  `DB_IP` / `DB_PORT` with its own address before pymongo's conftest import.
  The controller still runs the full identity tripwire, so every process
  verifies the server it is about to measure.
- `explain` reports `isMultiKey` on the `IXSCAN` stage, on both servers.
- `/ci` page: recent workflow runs (bounded, cached) plus per-server version
  drift.
- `secantus.opsboard.github`: read-only `gh` wrapper with an injectable runner,
  TTL cache, bounded limits and graceful degradation.
- `secantus.opsboard.versions`: local version + latest-tag reader for both
  servers (no network).
- `/gauges` page: 13 gauges × 2 servers, data-driven from `registry.GAUGES`,
  with per-gauge toolchain requirements, time estimates and info dialogs.
- `secantus.opsboard.reports`: validation-report parser + gauge/server filename
  mapping, handling all three report shapes.
- Per-gauge, per-server scores on the `/gauges` matrix.
- `secantus.opsboard.activity`: merged local + CI feed with an explicit origin.
- `GitHubClient.workflows()` / `.dispatch()` and a confirm-gated `/ci/dispatch`.
- `Journal.list(include_running=False)` and a self-refreshing running block.
- `secantus.opsboard.progress`: log→progress parser (phase markers + pytest %)
  driving an overall bar + phase stepper in the job view.
- `Cancel all running` control; per-job cancel now tears down the whole process
  group + escaped descendants (SIGINT→SIGTERM→SIGKILL) and reaps children.
- `py-gate` / `rust-gate` emit `==> [k/N] label` phase-step banners.
- **All gauges** button per server (Python / Rust) running `validate-all`, with
  a parallelism input that sets `--jobs N` (dispatches the gauges over a thread
  pool; capped, 4 or fewer recommended).
- `/release` page: readiness checklist (fail-safe — unknown blocks), version +
  typed-confirmation gate, explicit override for blocking checks.
- `secantus.opsboard.readiness`: local git/changelog checks with an advisory CI
  check that never blocks.
- `secantus.opsboard.discovery`: Tier-3 process-table scan for untracked build
  processes, filtered against journal-tracked pids.
- Per-task info dialogs on the dashboard with long-form detail, the exact
  command, an irreversibility warning for release-class tasks, and a time
  estimate.
- `secantus.opsboard.estimates`: median-of-past-successful-runs estimation with
  an explicit `measured` / `rough` / `unknown` provenance shown in the UI.
- `Journal.completed_durations()`: bounded, exact-argv duration history
  (successful runs only, so an early-aborting failure can't skew the estimate).
- `secantus.jobkit`: shared job runner + sqlite journal (cursor-paginated) with
  a pty-tee `run_tracked`; the `./inv` wrapper routes through it (import-light,
  `SECANTUS_NO_TRACK=1` to bypass).
- `secantus.opsboard`: FastAPI + HTMX + pywebview app — dashboard cards per
  server, job history, live log tail, cancel; token middleware; `invoke
  opsboard` task; `opsboard` extra; `secantus-opsboard` console script.
- `secantus.opsboard.config`: layered configuration (CLI > env > saved
  `~/.secantus/opsboard.json` > default) with an env var for every persistable
  setting and `--save` / `--print-config`.
- `SECANTUS_OPLOG_ASYNC=1` (Rust server, opt-in, prototype): oplog entries are
  persisted by a background drainer off the writer's transaction. `Storage::flush_oplog`
  blocks until the drainer has caught up (read-after-write oplog visibility).
- `secantusd-rs --log-file-max SIZE` and `[storage] log_file_max` in the config
  file (unit-suffixed, e.g. `128MB` / `1GB` / `2GB`), threaded into the WiredTiger
  connection config. Daemon default: `2GB`.
- `SECANTUS_WT_CONFIG_EXTRA` (Rust server / daemon): raw WiredTiger connection-config
  appended to the built config string, overriding defaults via last-key-wins.

#### Changed
- Python server: the documents table is keyed `(db, collection, RecordId)` with
  a framed `[u32-LE id_key_len][id_key][blob]` value, matching the Rust server's
  on-disk format exactly. Insert write-amplification drops from four rows to
  three, and an unsorted scan no longer walks a side index to find insertion
  order.
- Python server: an unindexed `update`/`delete` candidate scan now visits
  documents in insertion order rather than `_id` order, so `multi: false` picks
  the oldest matching document like mongod.
- Python server: index entries store the document's RecordId (8-byte
  big-endian) instead of its encoded `_id`, removing a lookup per result from
  every index scan. Matches the Rust server's entry format byte for byte.
- Python server: uniqueness checks exclude the document being updated by
  RecordId rather than by encoded `_id`.
- `docs/benchmark.md`, the README and the Rust server's docs index carry the
  new figures; the latency chart is regenerated from them.
- `listIndexes` no longer echoes the internal `multikey` catalog flag, matching
  mongod. The admin console's multikey badge is gone with it — the flag isn't
  wire-visible, and the console reports what the wire says; `isMultiKey` in the
  explain visualiser carries the same information.
- `secantus/storage.py`: `_prune_oplog_locked` streams only the oldest oplog
  entries and stops early instead of scanning and decoding the entire oplog;
  a new in-memory `_oplog_live_count` (seeded by a one-time key-only count on
  open) drives the entry-cap decision without a counting scan. Doomed rows are
  deleted from the one table the oldest-first walk found them in, not probed
  across all shard tables.
- The Rust `Storage` holds its `Connection` / oplog state behind `Arc` so the drainer
  thread can share them; `wait_for_oplog` blocks on the drainer's `written_seq`
  watermark in async mode and on `next_seq - 1` (unchanged) in the synchronous
  default.
- The daemon's WiredTiger WAL `log=(file_max=...)` defaults to 2GB instead of
  128MB. `wt_config` takes the value as a parameter; the embedded `RustServer`
  handle and `Storage::open`'s test-default config are unchanged at 128MB.
- `Storage::emit_oplog` (Rust) takes an `OplogEntry` that is either an owned
  `Document` (the rare DDL / noop / `findAndModify` paths, encoded as before) or a
  pre-assembled raw `RawDocumentBuf` (the hot `insert` / `update` / `delete` /
  capped-eviction paths). The raw builder writes `op` / `ns` / `ui` / `o` / `o2` in
  mongod field order and splices the pre-encoded `o` / `o2` bytes, so the document
  body is never re-serialised; `ts` and `wall` are appended last, matching the
  historical byte layout.
- `secantus_core::diff::compute_update_description` walks the pre-/post-images
  directly (`walk_docs`) instead of wrapping each in an owned `Bson::Document`
  clone.
- `Storage::find_matching` short-circuits an empty filter (`find({})`) instead of
  running a foregone `RawDocument::from_bytes` + `matches_raw` per document, and the
  read-only collection scan (`scan_blobs_natural`) reuses each value's allocation
  rather than cloning the blob a second time.
- Rust server oplog persisted across sixteen shard tables
  (`secantus_oplog_sh0..15`), each write batch routed to one shard by its start
  sequence; all oplog readers merge the shards (plus the legacy single table) in
  seq order via a k-way merge.
- Opportunistic oplog prune bounded via a maintained live-entry count: an early-out
  (one timestamp read) when under the cap with the oldest row still in-window, and a
  bounded walk of only the doomed rows otherwise — replacing the full-oplog scan
  that dominated the single-writer write path.
- Python `Storage` oplog readers (`read_oplog`, `oplog_floor_seq`,
  `find_seq_for_ts`, prune, recovery, PITR archive) merge the sharded oplog so the
  Python server can read/recover/prune a Rust-written store; Python writes stay on
  the legacy single table.
- The Rust server routes an `insert`'s `OP_MSG` kind-1 `documents` sequence to the
  handler as un-decoded byte slices (a new `CommandContext.raw_insert_documents`
  side-channel), skipping the merge-decode and the command-layer re-encode. The
  handler pre-checks `_id` `$`-prefixed keys over `RawDocument` and passes the raw
  bytes to storage; documents inline in the command body, and collections with a
  `validator`, still take the decoded path.
- `Storage::insert` / `Storage::insert_one` (Rust) store the caller's BSON verbatim
  when `_id` already leads the document, skipping the `encode_doc` re-serialisation;
  they fall back to `encode_doc` when an `ObjectId` is assigned or `_id` must be
  reordered to the front. Stored bytes are unchanged in every case (verified
  byte-for-byte against the client-sent encoding across ObjectId / string / int
  `_id`, nested documents, arrays, Decimal128, dates, and binary), and the pymongo
  conformance gauge is non-regressing (1020/1500, 99.5%).

#### Fixed
- Python server: a store written before this change is now refused at open with
  a clear message naming the format mismatch, instead of being opened and read
  with the wrong key format.
- Python server: a store whose indexes predate this change is refused at open
  with a message naming the index and the format mismatch, instead of running
  index scans that quietly match nothing.
- Python server: a tailable cursor on a capped collection with non-monotonic
  `_id` values no longer drops documents inserted after the cursor opened, and
  no longer redelivers documents it already returned.
- Python server: capped-rollover detection (`CappedPositionLost`) is based on
  the oldest-inserted document rather than the smallest `_id`, matching what
  capped eviction actually removes.
- Python server: capped-collection eviction is now strict FIFO
  (insertion order) even for non-monotonic user `_id` values, matching
  mongod, instead of evicting in `_id` byte order.
- Rust server: per-collection write locks, the cursor registry, and
  per-statement transaction locks are poison-tolerant, so a panic inside a
  critical section no longer leaves the collection, cursor, or transaction
  permanently unusable.
- `paths.py` / `secantus-core`: new `get_path_values` resolves a dotted path
  through arrays, returning every reachable value plus whether an array was
  traversed.
- `storage.py` / `secantus-storage`: index-key generation, the multikey flag, and
  the sparse-index gate all use that walk, so a path descending into an array is
  indexed per element.
- `storage.py` / `secantus-storage`: unique enforcement (`_unique_conflict`, the
  `createIndexes` pre-check, and `find_index_duplicates`) probes every key a
  document contributes instead of one canonical key, and a duplicate-key error
  reports the value actually behind the conflicting key.
- `$jsonSchema` with pathological schema nesting returns `FailedToParse`
  (code 9) rather than a generic internal error (security review I21).
- A malformed PostgreSQL startup packet (short `CANCEL`, non-UTF-8
  parameter) surfaces as `PGProtocolError` instead of a raw
  `struct.error` / `UnicodeDecodeError` (I16).
- The Mongo wire framing translates a `RecursionError` during body parse
  into a `BadValue` reply, keeping the connection alive (I1).
- `secantus-storage` (Rust): `Storage`'s close (`Drop`) now checkpoints when
  `durable` is set, mirroring Python `Storage.close`; the flag is resolved on
  open via `resolve_durable` with Python's precedence. Adds
  `Storage::open_with_config_durable` for an explicit override.
- Oplog retention prune deletes each doomed row from its exact source table
  (WiredTiger cursors are overwrite-mode, so a `remove()` of an absent key silently
  succeeds — a shard-then-legacy fallback would have leaked pruned rows on a
  Python-written store).
- `secantus-storage` (Rust): WiredTiger connection config now sets
  `log=(prealloc=false)`, eliminating the ~256 MB per-instance pre-allocated
  journal that was exhausting CI runner disks.
- Admin UI: the change-stream tail stops polling and releases its cursor as
  soon as the client disconnects, instead of only on the next event send.
- Bounded the per-poll awaitData wait so an orphaned poll thread left behind
  on disconnect frees promptly, hardening against an intermittent CI worker
  crash under load.

#### Removed
- Python server: the forward natural-order table (`secantus_natural`) is no
  longer written. The reverse table (`secantus_natural_seq`) remains as the
  `_id` index.

## [0.6.0b0] — 2026-07-19

### Say no like mongod: an operator-fidelity sweep, and a Rust server that clears every gauge

The single largest theme in this release is learning to fail correctly.
Roughly sixty slices went through the query, update, and aggregation
operators asking one question of each: when the argument is wrong, does
SecantusDB raise the same error, with the same code, that `mongod`
raises? Very often it did not — `$all` silently mis-matched a bad
argument, `$rename` could corrupt a document, `$concat` quietly coerced
non-strings, `$bucket` dropped out-of-range documents on the floor, and
a long tail of operators leaked a raw Python exception to the wire.
Those are now mongod-shaped errors with mongod's codes. Alongside the
rejections, numeric fidelity got the same treatment: whole-number
doubles are accepted where mongod accepts them, `$toInt` and `$convert`
enforce int32/int64 overflow bounds, `$mod` matches on floats and bools,
and `$substrBytes` is genuinely byte-based — including refusing a range
that would split a UTF-8 character.

The Rust server crossed its headline milestone: it now clears **all
thirteen driver-conformance gauges**, the same unmodified upstream suites
the Python server runs. Getting there took both correctness work
(MONGODB-X509 authentication, `$project` error fidelity, full `$min`/`$max`
ordering) and a sustained performance push — writers no longer queue
behind each other, reads no longer queue behind writers, oplog bookkeeping
came off the write path, and four separate paths (scanning, filtering,
projection, and the wire reply) now work over raw BSON instead of
decoding whole documents just to re-encode them.

New surface landed too. `$median` and `$percentile` ship on both servers
without a t-digest; `$sum` / `$avg` / `$max` / `$min` work as expression
operators, not just accumulators; `$toLong` joins the conversion family;
`$jsonSchema` grew mongod's full keyword surface; and expanded
change-stream events now match mongod field-for-field. The SQL server
kept its own pace with binary `COPY`, composite records end to end,
binary and scrollable server-side cursors, `DO` blocks, savepoint
rollback that reverts DDL, type modifiers on the wire, and
`idle_in_transaction_session_timeout`. The admin console caught up with
all three servers, the benchmark pages gained charts, and new concurrency
stress suites found and fixed real races.

#### Highlights

- Operator fidelity: ~60 slices aligning argument validation and error
  codes with `mongod` across query, update, and aggregation operators —
  rejections, numeric coercion rules, and byte-vs-codepoint string
  semantics.
- Rust server: clears all thirteen driver-conformance gauges; MONGODB-X509
  auth; per-collection write locks, lock-free reads, oplog off the write
  path, raw-BSON scan / match / projection / reply paths, and `$group`
  field pushdown so wide documents decode only what the stage reads.
- New operators: `$median`, `$percentile`, `$toLong`, `$sum` / `$avg` /
  `$max` / `$min` as expressions, full `$jsonSchema` keyword surface,
  `$bucketAuto` preferred-number granularity.
- Change streams: expanded events match mongod field-for-field.
- SQL server: binary `COPY`, composite records, binary + scrollable
  server-side cursors, `DO` blocks, savepoint DDL rollback, wire type
  modifiers, `idle_in_transaction_session_timeout`.
- Tooling: concurrency stress suites (and the races they caught),
  benchmark charts, a three-server admin console, and a green docs build.

### The Rust server decodes only the fields a `$group` actually reads

The `aggregate_group` workload was the worst of the six in the
post-raw-BSON profile (3.1× of mongod) for a structural reason: `$group`
received fully-decoded documents while reading only its `_id` and
accumulator-argument fields, so input materialization survived into the
heavy stage. `secantus_core::referenced_top_level_fields` now walks a
`$group` spec and returns the top-level fields it reads, and the command
layer pushes that field set down into the fetch when the first heavier
stage of a pipeline is such a `$group`. Wide documents no longer pay to
materialize fields the pipeline never looks at.

The analysis bails to a full decode wherever the referenced set can't be
determined statically — `$$ROOT` / `$$CURRENT`, computed-field access,
and non-simple accumulators like `$top` / `$topN` whose `sortBy` names a
field by bare key.

#### Changed

- Rust server: a leading `$group` pushes its referenced top-level field
  set into the fetch, decoding only those fields from wide documents.

### Admin console docs catch up, and the docs build goes green again

The admin web UI documentation still described the console as it was
before the Rust server and the SQL server existed. Most conspicuously it
carried a per-server feature table asserting that the Rust server lacked
archive restore, oplog and TTL pruning, role grant/revoke, `killOp`,
logs, and profiling — a table that had been wrong for months, and whose
in-code counterpart has since been removed. The page now explains why
there is no such table any more, and what replaced it: SecantusDB
targets start permissive, and a feature is withdrawn only when the
server itself reports the command missing.

The rest of the page caught up with what shipped alongside that —
point-in-time recovery, launching the Rust server from the embedded
control, index collation, target-sourced roles, the wider set of `_id`
types the collection browser can paginate, and the fact that `admin.db`
is a credential store and is now permissioned like one. Several stale
claims went with it, including a "one target server per launch" line that
predated the target hot-swap, and a limitations entry for a saved-
connections page that has since shipped.

Separately, `invoke docs` had been failing. Eleven Rust-server driver
validation reports were never added to the toctree, and since docs-only
commits are deliberately skipped by CI, nothing caught it. All thirteen
reports are now listed and the build is clean again.

#### Fixed

- `invoke docs` builds warning-free. Eleven `validation-report-*-rust-server`
  pages were missing from the toctree, failing the warnings-as-errors build.

#### Changed

- The admin UI docs describe the current capability model, the PITR panel,
  the Rust embedded-server option, index collation, target-sourced roles,
  `admin.db` permissions, and the supported `_id` types for pagination.

### The admin console catches up with three servers

The admin console was written when SecantusDB had one server. Since then a
Rust server and a PostgreSQL-wire SQL server shipped, and the console
quietly fell behind: a hardcoded table of "what the Rust server can't do
yet" went stale within days of being written and spent months hiding six
feature groups — archive restore, oplog and TTL pruning, role
grant/revoke, `killOp`, the server log, and profiling — behind disabled
buttons, on a server that implements every one of them.

That table is gone. Only a real `mongod` keeps a static capability
profile, because its negatives are definitional rather than a snapshot of
a moving target: no `mongod` will ever serve the proprietary
`secantusAdmin.*` commands. Both SecantusDB servers now start fully
permissive, and a feature is withdrawn only when the target itself
answers `CommandNotFound` — negative knowledge learned from the live
server instead of guessed in advance, so the console cannot drift out of
step with either server again.

The same review closed the rest of the gap. Point-in-time recovery, the
largest shipped subsystem with no interface at all, gets a panel on the
backup page. The embedded-server button can start the Rust server, not
just the Python one. The role picker asks the connected target what roles
exist rather than consulting the Python server's own table. Collections
keyed by `Decimal128`, `UUID`, or `Binary` `_id` can be browsed at last,
and a tampered pagination cursor now returns a clean error instead of an
unhandled crash.

#### Added

- Point-in-time recovery on `/backup`: take a base snapshot, and recover
  an archive to a wall-clock moment into a fresh directory.
- The embedded-server control can launch either the Python or the Rust
  server; the picker appears when the Rust extension is installed.
- Collation input on the index-create form, and a collation badge in the
  index list.
- `Decimal128`, `UUID`, and `Binary` `_id` values are supported by the
  collection browser's pagination cursor, `Binary` round-tripping its
  subtype.

#### Changed

- Capability detection no longer keeps a per-flavour feature table for
  SecantusDB servers. Features are hidden only after the target reports
  `CommandNotFound`.
- The role picker and `/roles` catalogue are sourced from the connected
  target via `rolesInfo`, falling back to the built-in names. Role names
  submitted from the form are no longer filtered against a local table,
  so a valid custom role is no longer silently discarded.

#### Fixed

- `~/.secantus/admin.db`, which stores target URIs verbatim including
  credentials, is created `0600` with its directory `0700`. Previously it
  was left at the process umask while the token file beside it was
  already locked down.
- A malformed pagination cursor returns a `ValueError`-shaped error
  rather than an unhandled `InvalidId` / `InvalidOperation` from a bson
  constructor.
- Pointing the console at a `postgresql://` URI explains that the SQL
  server has no admin UI, instead of failing with an opaque pymongo
  parse error.

### $all validates its argument instead of silently mis-matching

`$all` accepted a malformed argument. A non-array leaked / mis-parsed, and a
`$`-expression element that wasn't the all-`$elemMatch` form — mixing `$elemMatch`
with a scalar, or using another `$`-operator document — was silently treated as an
equality clause (matching nothing) rather than erroring. mongod rejects both with
`BadValue`: "$all needs an array" and "no $ expressions in $all". Both servers now
match.

A pure-scalar `$all`, an all-`$elemMatch` form, regex elements, and plain
subdocument elements remain valid. The Python server carries mongod's `BadValue`;
the Rust core defers these cases so the Rust server rejects them too. Three-way
mongod 7.0.12-verified.

#### Fixed

- `$all` rejects a non-array argument ("needs an array") and a `$`-expression
  element outside the all-`$elemMatch` form ("no $ expressions in $all") with
  `BadValue`, instead of silently mis-matching (both servers).

### $all against a scalar field now matches, like mongod

The `$all` array query operator silently missed documents whose field held a
*scalar* value rather than an array — `{tags: {$all: ["red"]}}` matched
`{tags: ["red", ...]}` but not `{tags: "red"}`, on both the Python and Rust
servers. mongod treats a scalar field like a one-element array for `$all`
(equality and regex elements alike), so those documents should have matched.
This dual-server correctness bug was found while triaging the driver-gauge
results and is verified fixed against a live mongod 7.0.12 probe (three-way:
Python == Rust == mongod). In the same fix, `$all: []` now correctly matches
nothing (it previously matched every array-valued document), and `$elemMatch`
clauses inside `$all` still correctly require an actual array.

#### Fixed

- `$all` matches a scalar field value like a one-element array (both servers).
- `$all: []` matches nothing rather than every array-valued document.

### Range operators order array elements by full BSON type order, like mongod

A third comparison bug from the driver-gauge triage: `$gt` / `$lt` against an
array bound compared elements pairwise, but a *cross-type* element pair made
both servers return no match — `{a: {$gt: [1, 2]}}` skipped `{a: [1, "x"]}`
even though mongod matches it (a string element outranks a number element in
BSON order, so `[1, "x"] > [1, 2]`). Python's native list comparison raises
`TypeError` on `"x" > 2` (swallowed to a no-match) and the Rust matcher
returned no-match on any incomparable element pair. Both now order array
elements by full BSON order (type rank first) via the shared `_bson_lt`
comparator, verified three-way against a live mongod 7.0.12 probe.

#### Fixed

- Range operators order two arrays element-by-element in full BSON order, so a
  cross-type element pair still orders instead of silently no-matching (both
  servers).

### Array operators reject a non-array input instead of silently yielding null

`$first`, `$last`, `$reverseArray`, `$concatArrays`, `$slice`, `$map`, `$filter`,
and `$reduce` all silently returned `null` when their input wasn't an array.
mongod errors on a non-array (non-null) input, each with its own code: `$first` /
`$last` `28689`, `$reverseArray` `34435`, `$concatArrays` `28664`, `$slice`
`28724`, `$map` `16883`, `$filter` `28651`, `$reduce` `40080`. A null or missing
input still yields `null`. Both servers now match.

The Python server carries mongod's codes; the Rust core defers a non-array input
(so the Rust server rejects it) and now distinguishes a null input (→ null) from a
non-array one. Three-way mongod 7.0.12-verified.

#### Fixed

- `$first` / `$last` / `$reverseArray` / `$concatArrays` / `$slice` / `$map` /
  `$filter` / `$reduce` reject a non-array input with the operator's mongod code,
  instead of silently returning `null`; a null / missing input still yields `null`
  (both servers).

### Array / set / string operator type-guards match mongod's error codes (and stop silently accepting)

A discovery sweep of ~120 aggregation-operator error cases against real mongod
7.0.12 found a bounded set of type-guard divergences, several of them **silent
accepts** — operators that returned a value where mongod errors. They now match
mongod: `$arrayElemAt`, `$in`, and `$regexMatch`/`$regexFind`/`$regexFindAll`
were silently returning `null`/`false`/`[]` on a bad argument and now error, and
the rest returned a generic `TypeMismatch` (14) where mongod uses a specific
`Location` code. Both the Python and Rust servers are fixed (the Rust core defers
each case — `$in`, `$arrayElemAt`, and the regex ops needed Rust-side fixes to
stop computing a value), verified against real mongod.

#### Fixed

- **Silent accepts, now errors (both engines):** `$arrayElemAt` non-array →
  `Location28689`; `$in` non-array second argument → `Location40081`;
  `$regexMatch` / `$regexFind` / `$regexFindAll` non-string `input` →
  `Location51104` (a `null`/missing input stays valid — `false`/`null`/`[]`).
- **Generic `TypeMismatch` (14) → mongod's `Location` code:** `$size` (17124),
  `$indexOfArray` (40090), `$setUnion` (17043), `$setIntersection` (17047),
  `$setDifference` (17048), `$setIsSubset` (17046), `$anyElementTrue` (17041),
  `$allElementsTrue` (17040), `$mergeObjects` (40400), `$range` non-numeric bound
  (34443), `$indexOfBytes` non-string (40091/40092), `$binarySize` (51276),
  `$bsonSize` (31393).

### `arrayFilters` nested-identifier extraction

`arrayFilters` identifiers are now extracted recursively through `$and` / `$or`
/ `$nor`, completing the arrayFilters validation. A filter like
`{$and: [{"x.a": {…}}, {"x.b": {…}}]}` correctly resolves the single identifier
`x` (so a `$[x]` update path applies to the matching elements), and mongod's
"exactly one identifier per filter" rule is now enforced: a filter carrying two
distinct identifiers — top-level or nested — is rejected, as is a bare `$expr`.
Both the Python and Rust servers behave identically, verified against real
mongod 7.0.12.

#### Fixed

- A single arrayFilter identifier nested inside `$and`/`$or`/`$nor` (e.g.
  `{$and: [{"x.score": {$lt: 50}}]}` for `$[x]`) is now extracted and applied,
  instead of failing with "arrayFilters has no entry for identifier x".
- An arrayFilter referencing two or more distinct identifiers now raises code 9
  ("Expected a single top-level field name, found 'x' and 'y'") — previously a
  second top-level identifier was silently ignored.
- An arrayFilter that is a bare `$expr` (no field identifier) now raises code 224
  ("$expr is not allowed in this context").

### `arrayFilters` validation

`arrayFilters` (the `$[<identifier>]` filter documents passed to an update) are
now validated the way real mongod validates them, instead of silently accepting
malformed input. A filter that isn't an object, is empty, carries an
identifier that isn't a lowercase-letter-led alphanumeric name, repeats an
identifier, or isn't actually referenced by any `$[<id>]` path in the update is
rejected with mongod's exact error code. Covered on both the Python and Rust
servers (the Rust core defers each invalid case), verified against real mongod
7.0.12.

#### Fixed

- A non-object array filter now raises code 14 ("BSON field
  'update.updates.arrayFilters.N' is the wrong type …, expected type 'object'").
- An empty array filter (`{}`) now raises code 9 ("Cannot use an expression
  without a top-level field name in arrayFilters").
- An identifier that isn't an alphanumeric string beginning with a lowercase
  letter (e.g. `1x`, `X`) now raises code 2 ("Error parsing array filter …").
- Two array filters with the same top-level identifier now raise code 9
  ("Found multiple array filters with the same top-level field name …").
- An array-filter identifier that no `$[<id>]` path in the update references now
  raises code 9 ("The array filter for identifier '<id>' was not used in the
  update …").

#### Notes

- An array filter whose only top-level keys are `$`-operators (e.g.
  `{$and: [{x: …}]}`) carries a *nested* identifier that SecantusDB doesn't
  extract yet; such a filter is left unvalidated rather than wrongly rejected
  (tracked in `tasks/backlog.md`).

### $bitsAllSet / $bitsAllClear / $bitsAnySet / $bitsAnyClear validate their argument, like mongod

The bitwise query operators mishandled several non-integer arguments. A negative
bit position (`$bitsAllSet: [-1]`) raised an *uncaught* `ValueError` (from
`1 << -1`) that surfaced without a code, a negative or fractional non-array
bitmask was silently accepted or reported the wrong code, and the Rust server
*rejected a valid whole-number-double* mask/position (`$bitsAllSet: 6.0`) because
its coercion didn't accept doubles.

All four operators now match mongod on both servers: a whole-number double is
accepted (truncated), and a fractional double, a bool, or a negative value is
rejected — a bad *bit position* with code 2, a bad non-array *mask* with code 9 —
on the Python server with mongod's messages, the Rust core deferring to
`BadValue`. Three-way mongod 7.0.12-verified.

#### Fixed

- `$bits*` accept a whole-number-double mask / bit position and reject a
  fractional / negative / bool one with mongod's exact code, instead of raising
  an uncaught `ValueError` on a negative position, silently accepting a negative
  mask (Python), or rejecting a valid `6.0` (Rust server).

### $bucket errors on an out-of-range value instead of silently dropping the document

`$bucket` did almost no validation. Worst of all, a document whose `groupBy` value
fell outside every bucket and had no `default` was **silently dropped** — silent
data loss. mongod errors (7158303). It now does too, on both servers.

`$bucket` also now validates the rest of its spec like mongod, instead of
silently accepting it: missing `groupBy` (40198), non-array `boundaries` (40200),
fewer than two boundaries (40192), boundaries of mixed type (40193) or not
strictly ascending / duplicated (40194), a `default` that falls inside the bucket
range (40199), and a non-document `output` (40196). The Python server carries
mongod's codes; the Rust core defers those cases to `BadValue`. Valid buckets are
unaffected. Three-way mongod 7.0.12-verified.

#### Fixed

- `$bucket` errors (7158303) on an out-of-range value with no `default` instead
  of silently dropping the document, and rejects an invalid spec (missing
  groupBy, bad/unsorted/mixed boundaries, in-range default, non-doc output) with
  mongod's codes instead of silently accepting it (both servers).

### Argument validation for `$bucketAuto`, projection `$elemMatch`, and `$pull` / `$pullAll`

Three more type-guard divergences from real mongod are closed. `$bucketAuto`
now validates its `buckets` argument (a bool or non-numeric value, a fractional
double, a non-positive count, or a missing `groupBy`/`buckets` each raise
mongod's exact code, while a whole-double count is accepted); a non-document
`$elemMatch` projection argument is rejected; and `$pull` / `$pullAll` against a
field that is present but not an array now errors instead of silently doing
nothing. All three are covered on both the Python and Rust servers (the Rust
core defers each invalid case) and verified against real mongod 7.0.12.

#### Fixed

- `$bucketAuto` `buckets` now raises `Location40241` (non-numeric or bool),
  `Location40242` (fractional double — not representable as a 32-bit integer),
  `Location40243` (not greater than 0), and `Location40246` (missing `groupBy`
  or `buckets`) instead of silently accepting `buckets: true` or leaking an
  uncoded error. A whole-double `buckets` (e.g. `2.0`) is accepted, matching
  mongod.
- A non-document `$elemMatch` projection argument (e.g. `{arr: {$elemMatch: 5}}`)
  now raises `Location31274` instead of being silently accepted.
- `$pull` / `$pullAll` on a field that exists but is not an array (a scalar or
  `null`) now raises code 2 ("Cannot apply $pull to a non-array value") instead
  of silently doing nothing. A missing field remains a no-op.

### `$bucketAuto` `granularity` validation

The optional `granularity` argument to `$bucketAuto` is now validated against
real mongod's rules instead of being silently ignored. A non-string value
raises code 40261, and an unknown series name raises 40257 — both matching
mongod 7.0.12 exactly. A *valid* preferred-number series (`R5`, `R10`, …,
`POWERSOF2`, `1-2-5`, `E6`, …) is rejected as not-yet-supported (code 2) rather
than silently producing count-chunked, unrounded boundaries: reproducing
mongod's boundary rounding byte-for-byte would require its exact internal
float series constants (its `6.3` is the f64 `6.3000000000000007`, not
`float("6.3")`), which aren't recoverable by black-box probing — so a faithful
error is preferred over a silently-divergent result (see `tasks/backlog.md`).
Both the Python and Rust servers behave identically.

#### Fixed

- `$bucketAuto` with a non-string `granularity` now raises `Location40261`, and
  with an unknown series name `Location40257`, instead of accepting them.

#### Changed

- `$bucketAuto` with a valid but unsupported `granularity` series now raises an
  explicit "not yet supported" error (code 2) instead of silently ignoring the
  field and returning unrounded boundaries.

### $bucketAuto `granularity` rounds boundaries to preferred-number series

`$bucketAuto` now honours the `granularity` option (`R5`/`R10`/`R20`/`R40`/`R80`,
`E6`/`E12`/`E24`/`E48`/`E96`/`E192`, `1-2-5`, and `POWERSOF2`), rounding bucket
boundaries to the ISO preferred-number series exactly as mongod does — instead of
rejecting a valid series as unsupported. The rounding is **hex-exact against real
mongod 7.0.12**, including mongod's non-standard floating-point results (its `R5`
boundary at 6.3 is the double `6.300000000000001`, i.e. `63 * 0.1`).

The rounder and the boundary walk were ported verbatim from mongod's
`granularity_rounder_preferred_numbers.cpp` and `document_source_bucket_auto.cpp`
to both the Python server and the Rust core (which backs the Rust server), so the
two agree bit-for-bit and both match mongod. A `granularity` groupBy value must be
a non-negative number: a non-numeric value, a `NaN`, or a negative number is
rejected (mongod codes 40258 / 40259 / 40260 on the Python server). A
Decimal128-valued groupBy is deferred (the standing Decimal128 precision
limitation); the int/double path is complete.

#### Added

- `$bucketAuto` `granularity` boundary rounding for every preferred-number series
  and `POWERSOF2`, hex-exact to mongod 7.0.12 on both the Python and Rust servers.

#### Fixed

- A `$bucketAuto` `granularity` groupBy value that is non-numeric (40258), `NaN`
  (40259), or negative (40260) is now rejected with mongod's code instead of the
  previous blanket "unsupported" error.

### CI stops cancelling its own answers

Two blind spots on the same theme — checks that quietly produced no result,
so a breakage could sit on `main` looking green.

The docs had no CI at all. `test.yml` and both wheel workflows carry
`paths-ignore: ['**.md', 'LICENSE*', 'docs/**']`, which is right for a test
matrix but means a docs-only commit skips CI entirely — and that is exactly
how the Sphinx build came to be failing on `main` through many green pushes.
A new `Docs` workflow builds both trees with warnings-as-errors. It has no
`paths` filter on purpose: `conf.py` runs autodoc over the package, so a
malformed docstring in a code-only commit can break the docs build, and
filtering to `docs/**` would recreate the same blind spot facing the other
way. The build compiles nothing — the WiredTiger extension is mocked — so
running it on everything is the cheapest job in the repo.

The second was subtler. All three of `Tests`, `Build wheels` and `Build
secantus-core wheels` cancelled in-progress runs per ref. On a feature branch
that is what you want, since the newest push should win. On `main` every
merge lands on the same ref, so on a busy day each merge cancelled the
previous commit's post-merge run: several consecutive merges each showed
`cancelled`, meaning the default branch went long stretches with no completed
result and a wheel-only regression would not have surfaced until a release
tag. Cancellation is now disabled on `main` only, so those runs queue and
each finishes, while pull requests keep newest-push-wins.

#### Added

- A `Docs` workflow building `docs/` and `docs-rust/` with `sphinx-build -W`,
  covering the gap left by every other workflow's `paths-ignore`.

#### Fixed

- `Tests`, `Build wheels` and `Build secantus-core wheels` no longer cancel
  their own post-merge runs on `main`. Runs on the default branch queue and
  complete; pull-request branches still cancel superseded runs.

### $concat rejects non-string operands instead of coercing them

`$concat` silently `str()`-coerced any operand — `{$concat: ["x=", 5]}` produced
`"x=5"` — and treated a null / missing operand as an empty string. mongod requires
every operand to be a string: a non-string operand is `Location16702` ("$concat
only supports strings, not <type>"), and a null or missing operand short-circuits
the whole expression to `null` (evaluated left-to-right, so a non-string that
precedes a null still errors). Both servers now match.

The Python server carries mongod's code; the Rust core defers a non-string operand
(so the Rust server rejects it) and now returns `null` on a null operand rather
than skipping it. Three-way mongod 7.0.12-verified.

#### Fixed

- `$concat` rejects a non-string operand with `Location16702` and returns `null`
  for a null / missing operand, instead of `str()`-coercing operands or treating
  null as an empty string (both servers).

### Concurrency stress suites hammer the servers — and the races they caught are fixed

Two new concurrency harnesses hammer the servers with barrier-synchronized
thread storms — one drives the Mongo-wire servers (the Python server and the
embedded Rust server, every test parametrized over both) through real pymongo
clients, the other drives the PostgreSQL-wire server through psycopg —
same-key insert races, transactional increment hammers, bank-transfer
invariants under concurrent readers, findAndModify ticket dispensers, unique-
index races, DDL churn against live writers, and connection churn under load.
Every test asserts a hard integrity invariant (exact counts, exactly one race
winner, a conserved total) plus error hygiene: the only errors a loser may see
are the typed, retriable signals a real server would send.

The harnesses caught five real concurrency bugs, now fixed on every affected
server. A SQL write-write conflict escaped as a generic `XX000 internal
error`; it now surfaces as SQLSTATE `40001 serialization_failure`, the
retriable signal drivers key their retry loops on, and the losing connection
stays fully usable. SQL DML statements are read-modify-write sequences
spanning several storage calls, so concurrent inserts could double-satisfy a
`UNIQUE` constraint (134 rows landed for 30 distinct values in the
reproducer) and concurrent `SET n = n + 1` updates lost increments (83 of 400
survived); DML statements — and bare `nextval()` draws — now serialize per
shared storage, closing both. `findAndModify {new: true}` on both Mongo-wire
servers re-found the document after updating it, so two concurrent callers
could be handed the same post-image (8 duplicate tickets in 400 measured);
the write now captures its own post-image while it holds the storage lock,
on the Python server and the Rust server alike. And findAndModify's write
now re-asserts the original query (keyed by the matched `_id`, in a re-pick
loop) on both servers, so job-queue claims and fam removes are exclusive —
two workers can no longer both "take" the same document.

#### Added

- `tests/test_pgserver_concurrency.py` — 11 psycopg-driven stress tests:
  autocommit insert storms, same-PK and UNIQUE-constraint races (exactly one
  winner, losers see `23505`), transactional and autocommit increment hammers,
  a deterministic two-transaction `40001` conflict, bank transfers conserving
  the total under concurrent readers, concurrent `nextval()`, DDL churn
  alongside DML, connection churn under write load, extended-protocol prepared
  statements across threads, and a bounded txn-vs-autocommit stall check.
- `tests/test_mongo_server_concurrency.py` — one pymongo-driven harness
  parametrized over BOTH Mongo-wire servers (the pure-Python
  `SecantusDBServer` and the embedded Rust server): insert storms, `$inc`
  hammers, findAndModify ticket dispensers, upsert races, unique-index races,
  readers paginating (`getMore`) through churn, index builds under write
  load, multi-collection writers, delete/insert churn, client connection
  churn, and a change stream observing every insert from four concurrent
  writers, plus job-queue claim-exclusivity and remove-exclusivity storms.
  Every test runs against both servers.
- `Storage.update_matching(..., return_post_images=True)` — returns the
  post-image of each write, captured while the statement holds the storage
  lock, so command handlers never re-read what they just wrote.

#### Fixed

- SQL: a storage-level write-write conflict (`WriteConflictError` /
  `WT_ROLLBACK`) now maps to SQLSTATE `40001 serialization_failure` on both
  the simple and extended protocol paths, instead of escaping as `XX000
  internal error`; the losing connection survives, `ROLLBACK` works, and
  retry converges.
- SQL: DML statements serialize per shared storage, so concurrent inserts can
  no longer double-satisfy a `UNIQUE` constraint and concurrent computed
  updates (`SET n = n + 1`) no longer lose increments. Bare
  `SELECT nextval('seq')` draws are serialized the same way and never repeat
  a value.
- `findAndModify {new: true}` — on BOTH Mongo-wire servers — returns the
  post-image of its own write instead of a racy re-read, so concurrent
  callers can no longer be handed the same ticket. Python captures it via
  `Storage.update_matching(..., return_post_images=True)`; the Rust storage
  grew the matching primitive (`UpdateOutcome::post_image`). The upsert path
  returns the upserted document from the write itself.
- findAndModify (both servers) re-asserts the original query at write time —
  the update/delete is keyed by the matched `_id` *plus* the query, in a
  re-pick loop — so the job-queue pattern (`{state: "new"}` →
  `{$set: {state: "taken"}}`) can no longer double-claim a document, and a
  fam remove checks its deleted-count so two removes can never both claim
  the same pre-image. This mirrors mongod re-evaluating the predicate when
  it acquires the document write.

### mongod-specific error codes for conversion / string-length / sort expressions

Several aggregation expressions raised a generic `TypeMismatch` (code 14) where
real mongod returns a specific error code. They now match mongod 7.0.12, so a
`pymongo` client sees the same `code` on a failed operation. This is a Python
server refinement — the Rust server already surfaced `BadValue` for these.

#### Fixed

- `$toInt` / `$toLong` / `$toDouble` / `$toDecimal` on an unparseable numeric
  string now raise `ConversionFailure` (241) instead of 14.
- `$convert` with an unknown target type name now raises code 2
  ("Unknown type name: …") and is **not** swallowed by `onError` (a query-compile
  error, matching mongod), instead of 14.
- `$sortArray` on a non-array input now raises `Location2942504` instead of 14.
- `$strLenCP` / `$strLenBytes` on a non-string argument now raise
  `Location34471` / `Location34473` instead of 14.

### $toInt and $convert enforce int32 / int64 overflow bounds

`$toInt` and `$convert` (to `int` / `long`) never range-checked their result:
`$toInt: 1e30` returned an unbounded Python integer, and a value larger than the
target type silently widened instead of overflowing. mongod errors (241,
"Conversion would overflow target type in $convert") — or routes to `$convert`'s
`onError`. SecantusDB now does the same on both servers.

`$toInt` also now yields an int32 (a plain int on the wire) rather than
preserving an int64 input's type, matching mongod, which always narrows to int.
Non-finite doubles (`inf` / `nan`) overflow rather than raising an uncaught
Python error. The Python server carries mongod's 241 code; the Rust core defers
the overflow cases to `BadValue`. Valid in-range conversions are unaffected.
Three-way mongod 7.0.12-verified.

#### Fixed

- `$toInt` / `$convert` (int/long) error on an out-of-range or non-finite value
  (mongod 241, caught by `$convert`'s `onError`) instead of returning an
  unbounded / silently-widened integer, and `$toInt` narrows an int64 input to
  int32 like mongod (both servers).

### Expanded change-stream events now match mongod field-for-field

`showExpandedEvents` change streams now reproduce mongod 7.0.12's event
shapes exactly, on both servers. Expanded update events always carry
`disambiguatedPaths` — an empty document when nothing was ambiguous — and
plain streams never do. `dropIndexes` events describe the dropped index in
full (`{v, key, name}`, with the key spec captured at drop time), matching
`createIndexes`. And the Rust server now emits `dropIndexes` events on the
`dropIndexes: "*"` path at all — its `drop_all_indexes` previously skipped
the oplog, so `drop_indexes()` from a driver produced no event.

#### Fixed

- Expanded update events carry `disambiguatedPaths` (both servers); the key
  is correctly absent without `showExpandedEvents`.
- `dropIndexes` events describe the dropped index in full on both servers,
  and the Rust server emits them for `dropIndexes: "*"`.

### $dateAdd / $dateSubtract / $dateTrunc validate their integer arguments

The date-arithmetic operators mishandled a non-integer `amount` / `binSize`: a
whole double (`2.0`) was over-rejected, and a bool was silently coerced to `1`.
mongod accepts an integer or a whole double, and rejects everything else: a
fractional double / bool / non-numeric `amount` is `Location5166405` ("$dateAdd
expects integer amount of time units"), a non-integer `binSize` is
`Location5439017`, and a non-positive `binSize` is `Location5439018`. Both servers
now match.

The Python server carries mongod's codes; the Rust core (via a new `date_int`
helper) now accepts a whole-double argument rather than deferring — so the Rust
server no longer rejects a valid `amount: 2.0` — and defers the invalid cases.
Three-way mongod 7.0.12-verified.

#### Fixed

- `$dateAdd` / `$dateSubtract` accept a whole-double `amount` and reject a
  fractional / bool / non-numeric one (`Location5166405`); `$dateTrunc` accepts a
  whole-double `binSize` and rejects a non-integer (`Location5439017`) or
  non-positive (`Location5439018`) one (both servers).

### Date and misc aggregation operators match mongod's error codes

Continuing the operator error-code sweep, a set of date and miscellaneous
aggregation operators now raise mongod 7.0.12's exact error code instead of a
generic `TypeMismatch` (14) — and two more **silent accepts** are closed
(`$dateToString` on a non-date, and `$dateDiff` with a missing `endDate`
parameter, both of which returned a value where mongod errors). Both the Python
and Rust servers are fixed (the Rust core defers each case — `$dateToString`
and `$dateDiff` needed Rust-side fixes to stop computing `null`).

#### Fixed

- `$dateToString` / `$dateToParts` on a non-date `date` → `Location16006`
  (`$dateToString` was a silent `null`; a `null`/missing date stays valid).
- `$dateFromString` with a non-string `dateString` → `ConversionFailure` (241).
- `$dateAdd` / `$dateSubtract` / `$dateTrunc` with an unknown `unit` → code 9
  ("unknown time unit value").
- `$dateDiff` with a missing `startDate`/`endDate`/`unit` **parameter** →
  `Location5166303`/`5166304`/`5166305` (was a silent `null` for a missing
  `endDate`; a present-but-null parameter still yields `null`).
- `$let` referencing an undefined variable → `Location17276`.
- `$switch` with no branches → `Location40068`.
- `$ifNull` with fewer than two arguments → `Location1257300`.
- `$getField` / `$setField` with a non-string `field` → `Location5654602` /
  `Location4161107`.
- `$sortArray` with an invalid `sortBy` → `Location2942507`.
- `$convert` with a missing `input`/`to` parameter → code 9.

### $densify validates its range spec

The `$densify` stage didn't validate its `range`. A date `unit` applied to a
numeric field leaked a raw Python `TypeError` (adding a `timedelta` to an int), a
bool `step` was silently coerced to `1`, a non-positive `step` and malformed
`bounds` (a bad string, a wrong-length array, a descending array) were quietly
accepted or mis-handled. mongod rejects each with a specific code: a numeric value
under a date unit is `6053600`, a bool step is `14`, a non-positive step is
`5733401`, a bounds string that isn't `"full"` / `"partition"` is `5946802`, a
bounds array that isn't exactly two elements is `5733403`, and a non-ascending
bounds array is `5733402`. A fractional `step` (`1.5`) is still accepted. Both
servers now match.

The Python server carries mongod's codes; the Rust core defers every invalid case
(bool step included) so the Rust server rejects them too. Three-way mongod
7.0.12-verified.

#### Fixed

- `$densify` rejects a date unit on a numeric value (`6053600`), a bool step
  (`14`), a non-positive step (`5733401`), and a malformed `bounds` string /
  array (`5946802` / `5733403` / `5733402`), instead of leaking a Python
  `TypeError`, coercing a bool, or silently mis-handling the range (both servers).

### Range operators now order embedded documents, like mongod

`$gt` / `$gte` / `$lt` / `$lte` against an embedded-document bound returned
*nothing* on both the Python and Rust servers — `{a: {$gt: {x: 1}}}` matched
no documents at all — because Python's `operator.gt` raises `TypeError` on two
dicts (swallowed to a silent no-match) and the Rust matcher treated any
document operand as an unconditional no-match. mongod orders embedded
documents field-by-field (first differing key compares as a string, else
recurse into the value, else the shorter document sorts first), so those
queries should have matched. Found while triaging the driver-gauge results and
verified against a live mongod 7.0.12 probe; now three-way parity (Python ==
Rust == mongod). The type bracket is preserved — a document-valued field still
never matches a scalar bound, and vice versa.

#### Fixed

- Range operators order two embedded documents field-by-field (both servers)
  instead of matching nothing.

### Docs: the benchmark pages get charts

`docs/benchmark.md` and `docs/concurrency.md` now open their result
sections with inline SVG charts — grouped latency-multiplier bars against
a mongod = 1x reference, and the three-server concurrency-scaling lines —
theme-aware for furo's light and dark modes (palette validated for both
surfaces), with native tooltips and the tables kept as the data view.
Matches the charts on secantusdb.com/performance.html.

### Aggregation expressions reject a bool where a number is expected, like mongod

The bool-as-int cluster reaches the aggregation expression engine. Because
Python's `bool` is an `int` subclass (and the Rust core mapped `Boolean`
straight to 0/1), a bool argument slipped through the numeric checks of eight
operators and was *computed* instead of *rejected*: `$round`/`$trunc` treated
`true` as a decimal place, `$arrayElemAt`/`$slice`/`$indexOfArray`/`$substrCP`
treated it as an index, and `$sortArray` as a sort direction. Every one is a
parse error in real mongod — a bool is not a number — and both servers now say
so.

The Python server reports mongod's exact per-operator codes (`$round`/`$trunc`
16004, `$arrayElemAt` 28690, `$slice` 28725/28727, `$sortArray` 2942507,
`$substrCP` 34450/34452, `$range` 34443/34445/34447, `$indexOfArray` 40096) and
messages; the Rust server surfaces `BadValue`. Found while sweeping the
aggregation surface for the same root cause as the `$inc`/`$mul` and
`$pop`/`$position`/`$slice`/`$bit` clusters; three-way mongod 7.0.12-verified.
`$range` already rejected a bool (with a generic code) and now carries mongod's
per-argument code.

#### Fixed

- `$round`, `$trunc`, `$arrayElemAt`, `$slice`, `$sortArray`, `$substrCP`,
  `$range`, and `$indexOfArray` reject a bool argument with mongod's exact
  error code instead of coercing it to 0/1 (both servers).

### `$sum` / `$avg` / `$max` / `$min` as expression operators

MongoDB 5.0 made `$sum`, `$avg`, `$max`, and `$min` usable as ordinary
expression operators — over an array or a single value, anywhere an expression
is accepted (e.g. inside `$project`/`$addFields`), not only as `$group`
accumulators. SecantusDB previously rejected these as unknown expression
operators (code 168); they now compute, matching real mongod 7.0.12.

#### Added

- `$sum` / `$avg` / `$max` / `$min` as expression operators. An array argument
  reduces over its elements; a scalar is a single value; a missing/absent
  argument contributes nothing. `$sum`/`$avg` ignore non-numeric elements
  (`$sum` of an empty/all-non-numeric input is `0`, `$avg` is `null`);
  `$max`/`$min` order by BSON cross-type order and ignore `null` (empty →
  `null`).

#### Notes

- Implemented on the Python server (the pymongo conformance target). The Rust
  core defers these to Python, so the embedded Rust server does not yet compute
  them — tracked in `tasks/backlog.md`.

### $facet validates its spec instead of leaking on a malformed sub-pipeline

The `$facet` stage didn't validate its spec. A sub-pipeline element that wasn't a
stage document (`{a: [5]}`) leaked a raw Python `TypeError`, an empty `{}` spec and
a nested `$facet` were silently accepted, and a non-array sub-pipeline gave a
generic error. mongod rejects each: an empty / non-object spec is `40169`, a
non-array sub-pipeline is `40170`, a non-object stage element is `40171`, and a
`$facet` nested inside a `$facet` is `40600`. Both servers now match.

An empty sub-pipeline (`{a: []}`) remains valid. The Python server carries mongod's
codes; the Rust core defers every invalid case (empty spec and nested `$facet`
included) so the Rust server rejects them too. Three-way mongod 7.0.12-verified.

#### Fixed

- `$facet` rejects an empty / non-object spec (`40169`), a non-array sub-pipeline
  (`40170`), a non-object stage element (`40171`), and a nested `$facet` (`40600`),
  instead of leaking a Python `TypeError` or silently accepting the malformed spec
  (both servers).

### Array-index operators accept a whole-number double, like mongod

`$arrayElemAt`, `$slice`, and `$indexOfArray` rejected *every* non-integer index,
but mongod accepts a **whole-number double** (`$arrayElemAt: [[...], 2.0]` →
element 2; `-1.0` → last element) and rejects only a *fractional* one. SecantusDB
now matches: a whole double is coerced to the integer index (so both servers
compute the same element), and a fractional double raises mongod's per-operator
code — `$arrayElemAt` 28691, `$slice` 28726 (second arg) / 28728 (third arg),
`$indexOfArray` 40096.

This mattered beyond fidelity: the fix had to land on both servers together —
had only the Python engine learned to accept `2.0`, the two servers would have
*disagreed* on a valid index (Python returning the element, the Rust server
erroring). Ground truth probed against mongod 7.0.12; three-way verified.

#### Fixed

- `$arrayElemAt`, `$slice`, and `$indexOfArray` accept a whole-number double
  index (coerced to int) instead of returning null/-1, and reject a fractional
  double with mongod's exact per-operator error code (both servers).

### Whole-number-double acceptance completed for $substrCP, $range, and $round/$trunc

Finishes the whole-number-double sweep begun for the array-index operators. Like
mongod, `$substrCP` (start/length), `$range` (start/end/step), and the precision
argument of `$round`/`$trunc` now accept a whole-number double (coerced to the
integer) and reject a *fractional* one with mongod's exact per-argument code
rather than rejecting every non-integer. So `$range: [0.0, 5.0, 1.0]` yields
`[0,1,2,3,4]` on both servers, while `$range: [0, 5.7]` raises 34446.

The Python server carries mongod's codes — `$substrCP` 34451/34453, `$range`
34444/34446/34448, `$round`/`$trunc` place 51082 — and the Rust core coerces the
whole double (computing the same result) or defers the fractional case to
`BadValue`. As with the array operators, the coercion had to land on both engines
together so they never disagree on a valid whole-double argument. Three-way
mongod 7.0.12-verified.

#### Fixed

- `$substrCP`, `$range`, and `$round`/`$trunc` (precision) accept a whole-number
  double argument (coerced to int) and reject a fractional one with mongod's
  exact error code, instead of rejecting all non-integer values (both servers).

### $group accumulators stop coercing non-numeric values

`$sum` and `$avg` accumulated whatever the input expression produced — a bool
folded in as `1`, and other non-numeric values either coerced or leaked a Python
error. mongod ignores non-numeric operands entirely: `$sum` of a group with no
numeric value is `0`, `$avg` is `null`. Both servers now match.

`$min` and `$max` compared values with Python's native `<` / `>`, which raises
on a cross-type pair (a number vs a string) — so a mixed-type field errored
where mongod returns a real extreme. They now ignore null / missing and order
every other value by BSON cross-type order (number < string < bool < …), so
`$max` over `[10, "hi", true]` is `true`, matching mongod. On the Rust server
these mixed-type groups previously deferred to a `BadValue`; they now compute.
Three-way mongod 7.0.12-verified.

#### Fixed

- `$sum` / `$avg` ignore non-numeric operands (string / bool / null / missing)
  instead of coercing or erroring — an all-non-numeric group yields `0` / `null`
  like mongod (both servers).
- `$min` / `$max` order mixed-type values by BSON cross-type order and skip
  null / missing, instead of raising on a cross-type comparison (both servers).

### $in / $nin validate their argument instead of leaking or silently no-matching

`$in` and `$nin` never checked their argument. A non-array (`{a: {$in: 5}}`)
leaked a raw Python `TypeError`, and an array element that was a document with a
`$`-prefixed key (`{$regex: …}` or `{$x: 1}`) silently matched nothing. mongod
rejects both with `BadValue`: "$in needs an array" and "cannot nest $ under $in".
Both servers now do the same.

A BSON regex *literal* (`/x/`) and a plain subdocument element remain valid. The
Python server carries mongod's `BadValue`; the Rust core defers these cases so
the Rust server rejects them too. Three-way mongod 7.0.12-verified.

#### Fixed

- `$in` / `$nin` reject a non-array argument ("needs an array") and an array
  element that is a document with a `$`-prefixed key ("cannot nest $ under $in")
  with `BadValue`, instead of leaking a Python `TypeError` or silently matching
  nothing (both servers).

### $inc / $mul reject a non-numeric argument, like mongod

`$inc` and `$mul` silently computed with a bool argument — `{$inc: {n: true}}`
added 1 and `{$mul: {n: false}}` multiplied by 0, because Python's `bool` is an
`int` subclass — and the Python engine raw-raised a `ValueError`/`TypeError`
on a string or null argument instead of a clean coded error. mongod rejects
any non-number argument with `Cannot increment with non-numeric argument:
{field: value}` (code 14). Both servers now reject it: the Python server
raises mongod's exact message and code; the Rust server surfaces `BadValue`
(the standing update error-code gap), but neither silently computes a wrong
result. Found while triaging the driver-gauge update operators; three-way
mongod-verified.

#### Fixed

- `$inc` / `$mul` by a bool, string, or null argument is rejected instead of
  computing a wrong value (both servers); the Python server reports mongod's
  code 14 and message.

### $indexOfBytes / $indexOfCP validate their start / end index

The `$indexOfBytes` and `$indexOfCP` operators mishandled a non-integer start /
end index: a whole double (`2.0`) was silently ignored (the whole expression
returned `-1`), and a bool was coerced to an integer. mongod accepts an integer or
a whole double, and rejects everything else: a fractional double, a bool, or a
non-numeric index is `Location40096` ("requires an integral … index"), and a
negative index is `Location40097` ("requires a nonnegative … index"). Both servers
now match.

The Python server carries mongod's codes (reproducing its verbatim missing-space
message quirk); the Rust core defers the invalid cases and now computes a
whole-double index rather than returning `-1`. Three-way mongod 7.0.12-verified.

#### Fixed

- `$indexOfBytes` / `$indexOfCP` accept a whole-double start / end index and reject
  a fractional / bool / non-numeric index (`Location40096`) or a negative one
  (`Location40097`), instead of silently returning `-1` or coercing a bool (both
  servers).

### $jsonSchema grows mongod's full keyword surface — and rejects what mongod rejects

The `$jsonSchema` query operator now covers every keyword real mongod accepts,
on both servers, with semantics pinned by a live probe against mongod 7.0:
`multipleOf` (fmod semantics, fractional divisors included), tuple-form
`items` with `additionalItems` (false / schema / absent), and the `title` /
`description` metadata keywords (accepted and ignored, with mongod's string
type check). Exclusive bounds move to the draft-4 semantics mongod actually
implements — `exclusiveMinimum` / `exclusiveMaximum` are booleans that
sharpen `minimum` / `maximum` to a strict bound, and the draft-6 numeric form
is rejected at parse time (the previous numeric treatment was a silent
divergence).

Just as important is what gets rejected: schema keywords are now validated at
parse time, recursively through every sub-schema, with mongod's verbatim
codes and messages — an unknown keyword or a known-but-unsupported one
(`$ref`, `$schema`, `default`, `definitions`, `format`, `id`) is
`9 FailedToParse`, and a type violation (non-number `multipleOf`, non-boolean
exclusive bound, non-string metadata, non-object schema) is
`14 TypeMismatch` — before a single document is scanned, even on an empty
collection. Previously both servers silently ignored anything they didn't
recognise, so a typo'd keyword matched everything.

#### Added

- `$jsonSchema` keywords `multipleOf`, tuple-form `items` +
  `additionalItems`, and `title` / `description`, on both servers, with
  curated parity coverage.
- Parse-time recursive keyword validation on both servers
  (`query._check_json_schema_keywords` / `secantus_core::query::
  json_schema_keyword_error`), with mongod's verbatim errors.
- `QueryError` carries a `code` / `code_name` (default `2 BadValue`), so
  parse-time errors with documented distinct codes surface faithfully
  through find, update, and delete write-error paths.

#### Changed

- `$jsonSchema` exclusive bounds follow draft-4 (boolean) semantics, matching
  mongod; the draft-6 numeric form now errors instead of silently applying.

#### Fixed

- A `$jsonSchema` with a mistyped or unsupported keyword no longer silently
  matches every document — it errors exactly as mongod does.

### $limit and $skip validate their argument like mongod

The `$limit` and `$skip` stages coerced their argument naively (`int(spec)`), so a
range of invalid inputs silently produced wrong results instead of the error
mongod raises: `$limit: 0` returned nothing (mongod: "the limit must be
positive"), `$limit: -1` did a Python negative-slice, and a bool or fractional
double was quietly truncated/coerced. The Rust server had the mirror-image bug —
it *rejected a valid whole-number double* (`$limit: 2.0`) because its integer
coercion didn't accept doubles, while still coercing a bool to 1.

Both stages now match mongod: a whole-number double is accepted (coerced to the
count), and a bool, a fractional double, or a negative value is rejected — the
Python server with mongod's exact codes (`$limit` 5107201, `$skip` 5107200,
plus 15958 for a zero `$limit`), the Rust core deferring to `BadValue`. `$skip: 0`
stays valid. Three-way mongod 7.0.12-verified.

#### Fixed

- `$limit` / `$skip` accept a whole-number double and reject a bool / fractional /
  negative argument (and `$limit` a zero) with mongod's exact error code, instead
  of silently coercing it (Python) or rejecting a valid `2.0` (Rust).

### Log-family domain errors now match mongod

An out-of-domain argument to the log family — `$ln` or `$log10` of a
non-positive number, `$log` with a non-positive argument or a base that is
non-positive or 1, `$sqrt` of a negative number — now raises mongod's exact
Location error (28766 / 28761 / 28758 / 28759 / 28714, messages verbatim from
a mongod 7.0.12 probe) on both servers, instead of silently returning null.
NaN inputs now propagate as NaN (IEEE, matching mongod); null and missing
still yield null.

#### Fixed

- `$ln` / `$log` / `$log10` / `$sqrt` out-of-domain arguments error exactly
  as mongod does, on both servers (the Rust engine defers those cases so the
  Python error surfaces).

### $log rejects a non-numeric argument or base

`$log` type-checked neither of its operands: a string argument or base leaked a
raw Python `TypeError`, and a bool was silently coerced to `1` / `0`. mongod
rejects both — a non-numeric argument is `Location28756`, a non-numeric base is
`Location28757` — before the positive-domain check, while `null` still passes
through as `null`. Both servers now match, completing the math-operator type-guard
family (the unary ops landed in the previous slice).

The Python server carries mongod's codes; the Rust core defers these cases (bool
included, reusing the `math_float` helper) so the Rust server rejects them too.
Three-way mongod 7.0.12-verified.

#### Fixed

- `$log` rejects a non-numeric (incl. bool) argument (`Location28756`) or base
  (`Location28757`) instead of coercing a bool or leaking a Python `TypeError`
  (both servers).

### $median and $percentile land on both servers — no t-digest required

The `$median` and `$percentile` group accumulators — and their expression
forms over arrays — now run on both the Python and Rust servers, with
semantics pinned by a live probe against real mongod 7.0.12. On bounded data
mongod's "approximate" method resolves to a discrete percentile —
`sorted[max(0, ceil(p·n) − 1)]`, returned as a double — so no approximate
t-digest sketch is needed and the two engines agree exactly: values collect
from int, long, double, and Decimal128 inputs (as doubles), bool and NaN are
excluded, and an empty input yields null (median) or per-`p` nulls
(percentile), all exactly as mongod behaves.

Spec validation carries mongod's verbatim codes and messages: a missing
`method` / `input` / `p` field is `40414`, a method other than
`"approximate"` is rejected with mongod's exact wording, a non-array `p` is
`7750301`, and an out-of-range `p` value is `7750303`.

#### Added

- `$median` / `$percentile` as `$group` accumulators and as expression
  operators, on both servers, with curated parity coverage, unit tests, and
  wire tests against each server.

### $mod matches mongod on floats, bools, and error cases

A fourth query-operator bug from the driver-gauge triage, this time in `$mod`.
mongod truncates both the field value and the divisor toward zero to integers,
excludes bool (bool is not a number for `$mod`), and uses C-style truncated
modulo (sign of the dividend). Both servers diverged: they matched a bool
field (`{a: {$mod: [2, 1]}}` wrongly matched `a: true`), didn't truncate
non-integer floats, and Python's floored `%` disagreed on negatives — and the
Rust server outright errored (`BadValue`) on a double-valued field, aborting
the whole query. Now both engines truncate value and divisor, exclude bool,
compute C-style modulo, and raise mongod's errors for a zero divisor and a
malformed spec — verified three-way against a live mongod 7.0.12 probe.

#### Fixed

- `$mod` truncates float values and divisors toward zero, excludes bool, and
  uses C-style (truncated) modulo, matching mongod on both servers.
- The Rust server no longer errors on a `$mod` query against a double-valued
  field.
- `$mod` with a zero divisor or a malformed spec raises like mongod.

### More mongod error codes for `$zip` / `$arrayToObject` / `$replaceOne` / `$dateDiff`

A second batch of aggregation expressions that raised a generic `TypeMismatch`
(code 14) now return mongod 7.0.12's specific error code, so a `pymongo` client
sees the same `code`. This clears the named-operator error-code rows from the
divergence catalog's Tier 3. Python-server refinement (the Rust core already
defers each case, so the Rust server surfaced `BadValue` — unchanged).

#### Fixed

- `$zip` with a non-array `inputs` now raises `Location34461`, and with a
  non-array element inside `inputs` `Location34468`, instead of 14.
- `$arrayToObject` on a non-array input now raises `Location40386`, and
  `$objectToArray` on a non-document input `Location40390`, instead of 14.
- `$replaceOne` / `$replaceAll` with a non-string argument now raise mongod's
  per-argument code — `input` → 51746, `find` → 51745, `replacement` → 51744 —
  instead of a single generic 51745.
- `$dateDiff` with an unknown `unit` now raises code 9 ("unknown time unit
  value: …") instead of 14.

### $not and $elemMatch validate their arguments

`$not` accepted any argument: `{$not: 5}` silently degraded to "not equal to 5"
instead of erroring, and an empty `{$not: {}}` was accepted. `$elemMatch` accepted
a non-object argument and mis-parsed it. mongod rejects both with `BadValue`: `$not`
"needs a regex or a document" (a non-empty one — an empty document is "cannot be
empty"), and `$elemMatch` "needs an Object". Both servers now match.

A regex or an operator document under `$not`, and an object under `$elemMatch`,
remain valid. The Python server carries mongod's `BadValue`; the Rust core defers
these cases so the Rust server rejects them too. Three-way mongod 7.0.12-verified.

#### Fixed

- `$not` rejects a scalar / array / bool / empty-document argument ("needs a regex
  or a document" / "cannot be empty"), and `$elemMatch` rejects a non-object
  argument ("needs an Object"), with `BadValue` — instead of silently degrading to
  an equality check or mis-parsing (both servers).

### The SQL server reports IntervalStyle at startup

`IntervalStyle` is one of postgres's `GUC_REPORT` parameters — a real
server announces it in the startup `ParameterStatus` set so clients that
decode intervals themselves know which style to parse. SecantusDB's SQL
server tracked the setting internally, defaulting to `postgres`, but
never announced it.

That gap is invisible to psycopg's binary backend, because libpq keeps
its own copy of the value, which is why the pinned test configuration
never noticed. psycopg's pure-Python backend trusts the server instead:
with the parameter absent it sees a style of `unknown` and raises
`NotImplementedError` on any query returning an `interval`, so a client
configuration that works against real postgres failed against
SecantusDB. Anywhere psycopg falls back to the pure-Python
implementation — a platform with no binary wheel, or an explicit
`psycopg` install without the `[binary]` extra — hit it.

The server now sends `IntervalStyle`, and the guarantee is pinned at the
wire level rather than through a client, so it holds regardless of which
psycopg implementation is installed.

#### Fixed

- The SQL server reports `IntervalStyle` in its startup
  `ParameterStatus`, as real postgres does. Without it, psycopg's
  pure-Python backend could not decode an `interval` value and raised
  `NotImplementedError`.

### $pow no longer crashes on a negative base with a fractional exponent

`$pow` with a negative base and a fractional exponent (e.g. `$pow: [-2, 0.5]`)
produced a Python **complex** number, which is unencodable — it crashed BSON
serialization of the response. It now returns `NaN`, matching mongod. `$pow` also
now validates its operands like mongod: a non-numeric base raises 28762, a
non-numeric exponent (including a bool) raises 28763, and a zero base with a
negative exponent raises 28764 — instead of silently coercing a bool, or leaking
a raw Python `TypeError`/`ZeroDivisionError`. Both servers; the Rust core already
returned `NaN` for the complex case (`f64::powf`) and now defers the bool /
zero-negative-exponent cases so both servers agree. Three-way mongod 7.0.12-verified.

This is the first fix from a **parallel divergence sweep** (recorded in
`tasks/divergence-catalog.md`) that probed the full operator surface against real
mongod and turned up a queue of type-coercion / argument-validation gaps.

#### Fixed

- `$pow` returns `NaN` for a negative base with a fractional exponent instead of
  crashing BSON encode, and rejects a non-numeric / bool operand (28762 / 28763)
  or a zero base with a negative exponent (28764) instead of coercing or leaking
  a raw Python exception (both servers).

### Projection $slice validates its argument

A projection `$slice` silently accepted a malformed argument and returned the
full or a wrong array. mongod validates it: the valid forms are a number
(first / last n) or `[skip, limit]` with a **positive** limit; anything else is
evaluated as the aggregation `$slice` *expression* and errors. A non-number
scalar or an array with fewer than two / more than three elements is
`Location28667` (wrong argument count); a two- or three-element array whose first
element isn't a number is `Location28724`. A negative `[skip, limit]` limit and a
three-element array are rejected the same way. Both servers now match.

Valid forms — a number, and `[skip, limit]` with a positive limit (the skip may be
negative) — are unaffected. The Python server carries mongod's codes; the Rust
core defers the invalid shapes so the Rust server rejects them too. Three-way
mongod 7.0.12-verified.

#### Fixed

- A projection `$slice` rejects a non-number scalar / short / long array
  (`Location28667`) and a two/three-element array that isn't `[skip, positive
  limit]` (`Location28724`), instead of silently returning the full or a wrong
  array (both servers).

### `$push` `$sort` direction validation and `$currentDate` boolean acceptance

Two more update-operator divergences from real mongod are closed. A `$push`
with a `$sort` modifier now rejects any direction that isn't exactly `1` or
`-1` (previously an out-of-range value such as `2` silently sorted anyway), and
`$currentDate` now accepts a boolean `false` (like `true`, it sets the current
Date) instead of wrongly rejecting it. Both are verified against real mongod
7.0.12.

#### Fixed

- `$push` `$sort` now raises code 2 when the whole-element sort direction is a
  number other than `±1` ("The $sort element value must be either 1 or -1"),
  when a `{field: dir}` direction is not `±1` ("The sort element value must be
  either 1 or -1"), or when the spec is a non-numeric value such as a string,
  bool, or array ("The $sort is invalid: use 1/-1 …"). A whole-double `±1`
  (e.g. `1.0`, or `{field: 1.0}`) is accepted and sorts, matching mongod —
  previously a whole-double scalar sort was wrongly rejected.
- `$currentDate: {field: false}` now sets the current Date (a boolean `false`
  is the same set-Date form as `true`) instead of raising. A non-boolean scalar
  argument and a bad or missing `{$type: …}` now raise code 2 with mongod's exact
  message instead of an uncoded "not understood" error.

### $gte/$lte: null and $exists match mongod's semantics

Two query-match correctness bugs that silently returned the wrong documents (no
error): a range comparison against null and `$exists`'s argument truthiness.

`{f: {$gte: null}}` (and `$lte: null`) matched nothing; mongod matches documents
where `f` is null **or missing** — the same set as `$eq: null` — because null only
orders equal to null. Both now do. (`$gt`/`$lt: null` correctly match nothing.)

`$exists` used Python's truthiness for its argument, so `{$exists: ""}` /
`{$exists: []}` / `{$exists: {}}` were read as `$exists: false`. mongod uses its
own truthiness — only `false`, `0`, and `null` are falsy; an empty string, array,
or document is truthy — so those all mean `$exists: true`. Both servers now match
(the Rust `truthy` no longer treats empty containers as falsy, and its comparison
routes `$gte`/`$lte: null` to null-equality). Three-way mongod 7.0.12-verified.

#### Fixed

- `$gte`/`$lte: null` match null and missing (like `$eq: null`) instead of nothing.
- `$exists` uses mongod's argument truthiness (empty string/array/document are
  truthy), not Python's, so `{$exists: ""}` means exists-true (both servers).

### The Rust server clears all thirteen driver-conformance gauges

Every one of SecantusDB's thirteen driver-conformance gauges now runs against
the Rust server, not just pymongo — pymongo (sync + async), the mongo-go /
node / java / kotlin / ruby / rust / php-library / php-driver / c / c++ / .NET
drivers — and the Rust server reaches effective conformance parity with the
mature Python server across every ecosystem. Three gauges pass perfectly
(mongo-rust-driver, .NET, kotlin at 100%), nothing scores below 98%, and no
failure is a new Rust-specific divergence: each one traces to a gap that is
already out of scope for a single-node surrogate (text / hashed indexes,
`$where`, multi-node transactions and sessions, Atlas search-index
management, IPv6) or to a documented driver-side or test-harness artifact the
Python server exhibits too. The per-gauge scoreboard and the follow-up triage
notes (a handful of assertion failures to diff against the Python-server runs)
live in `tasks/backlog.md` under the R8 entry; each run's full report is
committed as `docs/validation-report-<driver>-rust-server.md`.

#### Added

- Committed Rust-server conformance reports for all thirteen gauges under
  `docs/`, and a full-sweep scoreboard in the backlog's R8 entry.

### $regex / $options validate their arguments instead of silently ignoring

A `$regex` / `$options` query condition wasn't validated. An unknown option flag
(`{$options: "z"}`) was silently ignored, a non-string `$options` was interpreted
as raw regex flags, `$options` with no sibling `$regex` silently matched, and a
non-string `$regex` value leaked a Python error. mongod rejects each: an unknown
flag is `Location51108` ("invalid flag in regex options: X"), and the other three
are `BadValue` ("$options has to be a string" / "$options needs a $regex" /
"$regex has to be a string"). Both servers now match.

Valid flags (`imsxu`), an empty option string, a plain `$regex` string, and a BSON
regex literal are all unaffected. The Python server carries mongod's codes; the
Rust core defers these cases so the Rust server rejects them too. Three-way mongod
7.0.12-verified.

#### Fixed

- A `$regex` query validates its options: an unknown flag is rejected with
  `Location51108`, and a non-string `$options`, an `$options` without a sibling
  `$regex`, or a non-string `$regex` value with `BadValue` — instead of silently
  ignoring the flag, coercing, matching, or leaking a Python error (both servers).

### $rename validates its spec instead of corrupting the document

`$rename` performed no validation, so several invalid specs silently corrupted
data or leaked a raw Python exception rather than raising mongod's error:

- `{$rename: {"arr.0": "x"}}` (source is an array element) rewrote the array to
  `[null, 2, 3]`; `{a: "arr.0"}` (destination an array element) wrote into the
  array. mongod rejects both (code 2) — the field cannot be an array element.
- `{a: "a"}` (same field) and `{a: "a.b"}` (source/target on the same path) were
  applied; mongod rejects both (code 2).
- `{a: ""}` (empty target) created a field named `""`; mongod → code 56.
- `{a: 5}` / `{a: true}` (non-string target) leaked an `AttributeError`
  (`'int' object has no attribute 'split'`); mongod → code 2.

All now raise mongod's codes on the Python server (2 for the field/path/type
cases, 56 for the empty path) and defer to `BadValue` on the Rust server — the
document is left untouched. Valid renames (including into a new nested path) are
unaffected. Three-way mongod 7.0.12-verified.

#### Fixed

- `$rename` rejects an array-element source/destination, a same-field or
  same-path rename, an empty target, and a non-string target — instead of
  silently corrupting the document or leaking a Python exception (both servers).

### The Rust server's writers stop queueing behind each other

Writes on the Rust server now serialise per collection instead of per
server. Each collection gets its own write lock (created on first
reference, stable across drop-and-recreate); inserts, updates, deletes,
replaces and TTL prunes take only their collection's lock, so writers to
different collections run in parallel where previously every write in the
process queued on one global mutex — the flat ~0.5× concurrency scaling
the three-way benchmark measured. DDL (index builds, create/drop/rename,
collMod) takes the global lock plus the affected collection lock(s), so an
index build excludes in-flight writes on its namespace — which the Rust
server genuinely needs, because its write path is autocommit per operation
and WiredTiger would not surface a DDL-vs-write overlap as a conflict the
way the Python server's per-statement transactions do.

Getting the exactness guarantees right under real overlap took two more
pieces, both ported from the Python server's concurrency work. Every write
statement now runs inside its own WiredTiger snapshot transaction —
without one, an update could read a document in one implicit transaction
and write it in another, and a competitor committing in between was
silently overwritten with a value computed from the stale read (a lost
update the new stress tests catch reliably). The statement transaction
also makes each write atomic: document row, index entries, natural-order
rows and oplog rows commit or vanish together, closing a
crash-mid-statement window that could previously leave a dangling index
entry. And a write that loses a race retries: statement-level WT_ROLLBACK
and the bare-EINVAL commit-time conflict (a competitor marking the
transaction rollback-only after its last operation) both map to the typed
WriteConflict, which plain writes retry unbounded — matching mongod's
writeConflictRetry, with a warning logged every few seconds of continuous
retrying — while statements inside a user transaction surface it
immediately so the client sees mongod's statement-time WriteConflict with
the TransientTransactionError label.

#### Changed

- Rust server: CRUD writes serialise per collection (own lock per
  namespace); DDL takes the global lock plus the affected collection
  lock(s); the opportunistic oplog prune moved to its own mutex so the
  write path never takes the global lock. Lock-order rules are documented
  on the storage struct.
- Rust server: every write statement runs in its own WiredTiger snapshot
  transaction — statement atomicity (doc row + index entries +
  natural-order rows + oplog rows commit together) and write-conflict
  detection across the statement's read-modify-write.
- Rust server: tailable change-stream waiters are woken after the
  statement (or user-transaction) commit makes the oplog rows visible,
  not at emit time inside the still-open transaction.

#### Fixed

- Rust server: a plain write racing a multi-document transaction on the
  same document can no longer be lost to a stale-snapshot read-modify-
  write — the statement transaction turns it into a detected conflict and
  the write retries to completion, so both increments land.
- Rust server: a commit-time transaction conflict (bare EINVAL from
  WiredTiger after a competitor marked the transaction rollback-only)
  now surfaces as the retriable WriteConflict instead of a generic
  internal error, matching the Python server's #444 mapping.
- Rust server: two callers racing the lazy collection-UUID mint can no
  longer mint different UUIDs for the same namespace — the mint runs
  under the collection write lock with a double-check, and the
  already-minted fast path is a lock-free read.

#### Added

- `crates/secantus-storage/tests/concurrent_writes.rs` — cross-collection
  writer storms (exact counts), same-collection `$inc` hammers (exact
  final value), unique-index races (exactly one winner, typed loser
  errors), `createIndex` under write load (index-routed reads must reach
  every document), a plain-write-vs-transaction race (retries to
  completion, both effects land), and a transaction-vs-transaction
  statement conflict (typed WriteConflict, no retry inside the
  transaction).

### `$sum` / `$avg` / `$max` / `$min` expression operators on the Rust server

The expression-operator forms of `$sum`, `$avg`, `$max`, and `$min` (added to the
Python server in the previous release) now also compute on the embedded Rust
server, so both servers support the MongoDB 5.0+ feature. The Rust
implementation reuses the group-accumulator numeric-width logic (int32 → int64 →
double promotion) and BSON cross-type ordering, so its result — value *and* type
— is byte-for-byte identical to the Python server (pinned by the parity suite).

#### Added

- `$sum` / `$avg` / `$max` / `$min` as expression operators in the Rust core.
  An array argument reduces over its elements, a scalar is a single value, and a
  missing/absent argument contributes nothing; a `Decimal128` element or an
  extreme that isn't BSON-orderable still defers to Python.

### The Rust server's reads stop queueing behind writers

Every read the Rust server served used to take the same global storage lock
as every write — concurrent readers serialised not just against writers but
against each other. Read-only storage methods (`find`, `count`, the `_id`
point lookup, collection scans, listers, planners and stats) now run
lock-free: each call's own WiredTiger session gets a consistent MVCC view
without blocking, so reads no longer queue behind a bulk insert and a mixed
read/write workload stops paying the writer's lock hold. All mutable Rust
state lives in WiredTiger tables or under the dedicated oplog mutex, so the
lock was buying readers nothing — correctness under concurrency is carried
by the storage schema instead (a fixed set of shared tables that DDL never
drops) plus the invariant that index-routed candidates are always
re-verified by the exact matcher and doc fetches tolerate not-found.

Making that invariant airtight surfaced three write-ordering fixes worth
having on their own. Index maintenance for updates is now a set diff — an
update inserts only the entry keys the new document adds and removes only
the ones it drops, so a `$set` of an unindexed field performs zero
index-table operations (the old scheme deleted and rewrote every entry,
opening a window where a document vanished from an index whose value the
update never touched). `createIndex` backfills its entry rows before
writing the registry row, so a reader can never route through a
half-built index and miss documents. And every delete-shaped path (delete,
TTL prune, capped eviction) removes the document row first and its index
entries after, so a stale entry resolves to a skipped not-found rather
than an index miss of a still-live document.

#### Changed

- Rust server: read-only storage methods no longer take the global storage
  lock — reads run concurrently with writes and with each other under
  WiredTiger MVCC. The lock now serialises only writes and DDL.
- Rust server: update index maintenance writes the set *difference* of
  entry keys (additions before the doc-row write, removals after) instead
  of delete-all-then-rewrite; updates that don't change indexed values do
  no index writes at all.

#### Fixed

- Rust server: `createIndex` now makes the index-registry row the commit
  point of a fully-backfilled index (entries first, registry last), so a
  concurrent reader can no longer route through a half-built index and
  return incomplete results.
- Rust server: delete-shaped writes (delete, TTL prune, capped-collection
  eviction) remove the doc row before its index/natural-order entries, so
  a concurrent index-routed read can no longer miss a document that is
  still live.

#### Added

- `crates/secantus-storage/tests/concurrent_reads.rs` — four reader
  threads hammer `find` / `findOne` / scans / counts / `listIndexes`
  against a live writer doing replaces, delete/re-insert churn and
  drop/recreate index churn; every served document must decode and match
  the filter it was returned for.

### The Rust server's oplog bookkeeping gets off the write path

The Rust server's oplog housekeeping no longer taxes the hot paths. The
opportunistic oplog prune that runs on the write path (every 1000 emits,
under the writer's lock) used to decode every oplog row in full just to read
its timestamp — an O(entire-oplog) stall for every concurrent writer each
time it fired. The retention scan now peeks the timestamp out of the raw
BSON bytes without materialising the document, stops dating rows at the
first in-window entry (timestamps are monotone in seq, so the expired rows
form a prefix), and walks the rest keys-only. `hello` replies stopped
writing to storage entirely: the per-call oplog-meta persist that every
driver heartbeat used to pay — a single-row WiredTiger hotspot every
concurrent writer contended on — is gone, matching the cure the Python
server shipped in 0.5.4b236.

Crash recovery got structurally safer at the same time. The oplog meta row
is now written once at close, and recovery treats it as a hint, not the
truth: the recovered counters are clamped up past what the oplog and
natural-index tables actually contain, so a crash can never lead to a
re-minted (duplicate) oplog seq — previously a stale meta row could
overwrite live oplog rows after an unclean shutdown. Restart monotonicity of
the cluster clock is guaranteed the same way the Python server does it:
recovery bumps the clock one full second past everything it can see, which
covers any `hello`-minted timestamp that was never persisted.

#### Changed

- Rust server: `current_cluster_time` (every `hello` reply under the
  replica-set persona) no longer persists the oplog meta row, and no longer
  takes the global storage lock — it is a pure in-memory mint under the
  dedicated oplog mutex.
- Rust server: the write-path opportunistic oplog prune peeks timestamps
  from raw BSON (no full-document decode), early-stops at the first
  in-retention row, and collects the remainder of the walk keys-only;
  `startAtOperationTime` seq resolution uses the same raw peek.
- Rust server: oplog-meta recovery reconstructs from the newest oplog row
  with a single reverse cursor step instead of a full-table decode walk.

#### Fixed

- Rust server: a stale oplog-meta row (the on-disk state a crash leaves
  behind, since the meta snapshot is written at close, not per emit) can no
  longer rewind `next_seq` / `next_nat_seq` — recovery clamps both counters
  up past the table maxima, so a reopen can never re-mint an already-used
  oplog seq (which would silently overwrite a live oplog row) or collide a
  natural-order entry.
- Rust server: the cluster clock can no longer step backwards across an
  unclean restart — recovery bumps it one second past the recovered
  timestamp, the wall clock, and the oplog tail, covering mints that were
  never persisted.

### The Rust server thins aggregation input before decoding it

The aggregation pipeline fetched its input, then decoded **every** document
into an owned `bson::Document` before the pipeline ran — even documents a
leading `$skip` / `$limit` / `$match` was about to drop. A pipeline that
narrows its input early (a limit-then-group, a sample, a second filter after
the lifted one) paid to materialize rows it never used.

The leading pass-through prefix of a pipeline — `$skip`, `$limit`, and a
non-leading `$match` — now runs over the raw BSON before anything is decoded
(`$match` via the same `query::matches_raw` `find` uses), so only the survivors
that actually reach the first heavier stage (`$group` / `$sort` / computed
`$project` / `$unwind` / …) are decoded. On a 5000-row scan of wide documents
feeding `[{$limit: 50}, {$group: …}]`, that measured ~4× faster (it decoded 50 rows instead of 5000). The result is
identical to decoding everything and running the same stages — the prefix is
order-preserving and reuses the parity-pinned matcher — and any stage the
prefix doesn't handle (or a `$match` filter the raw matcher defers on) flows
through the full decode-and-run path unchanged. The heavier stages themselves
still materialize; accelerating those is separate, larger work.

#### Changed

- Rust server: an aggregation pipeline's leading `$skip` / `$limit` / `$match`
  prefix is applied over raw BSON before `decode_docs`, so the heavier stages
  decode only the documents that survive the prefix.

### The Rust server projects simple field lists without decoding whole documents

Phase 3 of the raw-BSON serving-path work takes the return path off
materialization for the common projection shape. A `find` with a projection
still decoded every returned document into an owned `bson::Document` before
projecting it — the last big materialization site now that projection-free
`find` and `count` run fully on raw BSON.

A pure top-level inclusion projection (`{a: 1, b: 1}`, optionally with
`_id: 0`) now projects straight off the raw document, decoding **only the
included fields** rather than the whole thing. On a 5000-row scan of wide
documents projecting two of twelve fields, that measured **~2× faster**. The
fast path is byte-identical to the full projection — same fields, same order
— so no result changes; anything it doesn't cover (exclusion, dotted paths,
`$slice` / `$elemMatch` / `$meta`, positional, mixed inclusion/exclusion)
transparently falls back to the full projection on a decoded document.

#### Changed

- Rust server: a pure top-level inclusion projection is applied over raw BSON
  (`projection::apply_projection_raw`), decoding only the projected fields
  instead of the whole document. All other projection shapes fall back to the
  full decode + `apply_projection` unchanged.

#### Added

- `secantus_core::projection::apply_projection_raw(&RawDocument, spec)` — the
  raw-BSON inclusion-projection fast path, exposed to the parity harness as
  `_secantus_core.apply_projection_raw` and cross-checked byte-for-byte
  against `apply_projection` on every projection parity case.

### The Rust server stops decoding documents just to re-encode them for the wire

The Rust server used to decode every document it served into an owned
`bson::Document` purely to build the wire reply — then immediately
re-encode it. The storage scan and the cursor registry already speak raw
BSON bytes end-to-end, so a `find`'s `firstBatch` and every `getMore`'s
`nextBatch` were round-tripping through a decode→`IndexMap`→re-encode step
that produced exactly the bytes they started from. That reply-path
materialization was one of the two dominant hot spots the profiler found
(`tasks/rust-perf-findings.md`).

Cursor replies now splice the pre-encoded document blobs straight onto the
wire. A new `secantus_wire::encode_cursor_reply` assembles the
`cursor.firstBatch` / `cursor.nextBatch` BSON array from the stored blobs
without decoding them, and the `find` (no-projection) and non-tailable
`getMore` handlers hand their batches to the server as raw bytes instead
of an owned array. The output is byte-for-byte identical to the old path
(pinned by a unit test), so no driver can tell the difference — the work
saved is pure overhead. This is Phase 1 of the raw-BSON serving-path plan;
the change-stream (tailable) path, projected `find`, and exhaust-cursor
streaming keep their existing behaviour for now.

#### Changed

- Rust server: `find` (without a projection) and non-tailable `getMore`
  no longer materialize their document batch into the reply — the
  pre-encoded blobs are spliced onto the wire by
  `secantus_wire::encode_cursor_reply`, eliminating the reply-path
  decode→re-encode round-trip. The batch is carried to the server
  out-of-band via `CommandContext::pending_batch` (the same idiom as
  `close_connection`); the exhaust-getMore streamer reconstructs the
  batch it needs to reframe.

### The Rust server stops decoding whole documents just to filter them

Phase 2 of the raw-BSON serving-path work takes the scan path off owned-BSON
materialization. The Rust server's collection scan used to decode every
candidate document into an owned `bson::Document` — a heap allocation per
field — purely to run the query filter over it, then throw that document
away for any document the filter rejected. For a selective filter over wide
documents that is almost all wasted work: the profiler put this scan-side
materialization (with the reply-path decode fixed in the previous release)
at the larger share of the serving path's on-CPU time.

The filter now runs over the raw BSON bytes. A new
`secantus_core::query::matches_raw` walks the document by field name and
decodes **only the fields the filter actually reaches** — a filter on one
field of a ten-field document never touches the other nine — reusing every
existing operator unchanged. Documents the filter rejects are never fully
decoded, and a filter-only find (no in-memory sort) decodes nothing at all;
only the matched documents of a post-sorted find are decoded, for their
sort keys. The raw matcher is pinned bool-for-bool to the owned matcher (and
transitively to the pure-Python matcher) across the entire curated + fuzz +
regex + collation parity corpus, so no query answers change.

A selective filter over wide documents that this optimizes — a `count` or
`find` that scans many rows and keeps few — measured **~2.8× faster** on a
5000-row collection scan (one filter field of eleven, rejecting 99.8%).

#### Changed

- Rust server: `find` and `count` scan filtering runs over raw BSON
  (`query::matches_raw`) instead of materialising each candidate into an
  owned `Document`. Selective filters over wide documents skip decoding the
  fields they don't touch; rejected documents and no-sort finds skip the
  owned-document build entirely. (The update/delete candidate scans still
  materialise, since a matched document is needed for the write — a later
  phase.)

#### Added

- `secantus_core::query::matches_raw(&RawDocument, …)` — the raw-BSON query
  matcher, exposed to the parity harness as `_secantus_core.query_matches_raw`
  and cross-checked against `query_matches` on every parity case.

### The Rust server matches update/delete candidates over raw BSON too

Completing the raw-BSON match work: `update` and `delete` scanned their
candidate documents by decoding each one in full to run the filter, then
discarded that document for every candidate the filter rejected — the same
waste `find` and `count` already avoid. Both now match candidates over raw
BSON (`query::matches_raw`, decoding only the filter's fields) and decode the
full document only for a candidate that actually matches (which the write
needs anyway). A selective `update` / `delete` over a collection scan of wide
documents no longer decodes the rows it isn't going to touch.

On a 5000-row collection scan of wide documents (11 fields) updating the ~10
that match one unindexed filter field, this measured **~4× faster** — the
baseline decoded all 5000 candidates to update ten. This reuses the same
matcher `find` and `count` use, already pinned bool-for-bool to the owned
matcher (and pure Python) across the query parity corpus — so no query
semantics change.

#### Changed

- Rust server: `update` and `delete` candidate scans filter over raw BSON
  (`query::matches_raw`) instead of decoding every candidate; only a matched
  candidate is fully decoded (for the write / oplog). Every scan-matching
  path — `find`, `count`, `update`, `delete` — now skips decoding the
  documents a selective filter rejects.

### Rust-server residues closed: $project error fidelity, WT knobs, full $min/$max order

Three long-tracked Rust-server gaps from the rewrite backlog are closed. An
unknown expression operator inside an aggregation `$project` now reports
mongod's stage-specific `Location31325` on the Rust server too (a parse-time
scan that never mislabels the projection-only `$slice` / `$elemMatch` /
`$meta` shapes), completing the context-specific unknown-operator codes on
both servers. The embedded `RustServer` handle grew the WiredTiger knobs the
daemons already exposed — `cache_size`, `session_max`, and `sync_on_commit`
constructor parameters — so tests can drive non-default storage configs
in-process. And the Rust engine's `$min`/`$max` update operators moved onto a
direct port of Python's `_bson_lt` (`order::bson_lt`), a single strict-less
relation that needs none of the `$sort` comparator's transitivity guarantees:
bool, Decimal128, NaN, Binary, Timestamp, Regex, Min/MaxKey, and the decoded
exotic text types all compute natively now, with only a DBPointer operand
still deferring. Range operators accept the exotic text types the same way
(Symbol / JS code compare as strings, mirroring pymongo's decode), so
otherwise-fine queries no longer error with `BadValue` on the Rust server.

#### Added

- `RustServer(..., cache_size=, session_max=, sync_on_commit=)` — the
  embedded handle's WiredTiger knobs, threading into `wt_config` exactly like
  `secantusd-rs`'s `--cache-size` / `--session-max` / `--sync-on-commit`.
- `order::bson_lt` in `secantus-core` — the direct `ordering._bson_lt` port
  backing `$min`/`$max`, covering the types `is_sortable` must bar from sorts.

#### Fixed

- Rust server: an unknown expression operator inside `$project` reports
  `Location31325` ("Invalid $project :: caused by :: Unknown expression $op")
  instead of a generic `2 BadValue`, matching the Python server and mongod.
- Rust matcher: JS code / Symbol range operands compare as text and DBPointer
  / undefined operands are a clean no-match, instead of erroring `BadValue`.
- Rust engine: `$min`/`$max` with bool / Decimal128 / NaN / Binary /
  Timestamp / Regex operands compute instead of deferring; curated parity
  cases pin every newly-computed shape.
- `invoke rust-parity` installs `shapely` / `s2sphere` / `python-dateutil`
  into its isolated environment, so the geo curated cases run instead of
  erroring with `ModuleNotFoundError`.

### Rust server: MONGODB-X509 authentication actually works now

The Rust server's TLS, mTLS, and MONGODB-X509 machinery was in place, but the
first end-to-end test showed the peer-certificate DN was extracted in
x509-parser's raw display form — least-specific-first with comma-space
separators — so the identity a client's certificate asserted never matched
the user record provisioned for it, and X509 authentication always failed.
The extraction now produces the mongod-style RFC 4514 string
(most-specific-first, bare commas, short OID names, value escaping),
byte-identical to the Python server's conversion, so a user provisioned
against either server authenticates on the other. A full two-stage
bootstrap-then-authenticate test now runs against the Rust server, mirroring
the Python suite.

#### Fixed

- Rust server: the MONGODB-X509 peer DN matches mongod's RFC 4514 form; X509
  auth verified end-to-end (TLS handshake, mTLS client-cert requirement, DN
  identity, and the no-client-cert refusal path).
- Rust server: tailable cursors on capped collections verified live and
  pinned by a smoke test (the shipped `find.rs` producer had outlived its
  "still deferred" note).
- `secantus-wt`'s build probe also accepts MSVC's `wiredtiger.lib` (and
  `.dylib`), unblocking the standalone `secantusd-rs` build on Windows.

#### Changed

- A 2026-07-17 backlog audit closed five stale Rust-rewrite entries: the
  `find` command entry (shipped long since), Phase-4 sub-phase 5e and the
  storage-keystone engine-selection half (superseded by the two-server
  model), the keystone wheel-flag flip (already ON in the shipping matrix),
  and the standalone-binary half of the Rust-package entry (`secantusd-rs`
  ships). The crates.io publish and the "recommend Rust by default" call are
  explicitly flagged as product decisions.
- R8: the mongo-go-driver gauge now runs against the Rust server — 398
  passed / 3 failed / 52 skipped (99.3%), unified suite 42/42; the single
  real failure is the documented, accepted go-harness `try_next` load-timing
  artifact. Report at `docs/validation-report-go-rust-server.md`.

### $sample validates its size argument, like mongod

The Python server's `$sample` stage coerced its `size` with a naive `int()`, so a
bool size was treated as 1 and a negative size crashed with a raw `ValueError`.
mongod rejects both — a non-number size with 28746 ("size argument to $sample
must be a number") and a negative size with 28747 ("must not be negative") — while
accepting a fractional double and truncating it. The Python server now matches;
the Rust server already rejected these (its `$sample` lives in the server crate and
validated), so this closes the gap on the Python side.

With this, the aggregation numeric-argument trio — `$limit`, `$skip`, and
`$sample` — matches mongod on both servers.

#### Fixed

- `$sample` rejects a bool `size` (28746) and a negative `size` (28747) instead of
  coercing the bool to 1 or crashing on the negative (Python server).

### $size validates its argument like mongod

`$size` accepted or silently ignored arguments mongod rejects: a negative size
returned no match instead of erroring, a bool was accepted as `1` (Python's
`bool` is an `int`), and an integer-valued float like `2.0` was wrongly
rejected even though mongod accepts it as `2`. Both engines now validate the
argument the way mongod 7.0.12 does — it must be a number, integer-valued, and
non-negative — raising the corresponding parse error (code 2) otherwise, and
accepting an integer-valued float. Found while triaging the driver-gauge
results; three-way mongod-verified.

#### Fixed

- `$size` errors on a negative, non-integer, string, or bool argument (code 2)
  instead of silently matching nothing or accepting a bool, and accepts an
  integer-valued float — on both servers.

### $sort stage validates its direction values

The `$sort` aggregation stage didn't validate its spec. A string direction
(`{v: "asc"}`) leaked a raw Python `ValueError`, a bool was silently coerced to
ascending, a numeric value other than ±1 (`0`, `2`) was treated as ascending, and
an empty `{}` spec was a silent no-op. mongod rejects each: a non-numeric
direction is `Location15974` ("Illegal key in $sort specification"), a numeric
non-±1 is `Location15975` ("must be 1 … or -1"), and an empty spec is
`Location15976` ("must have at least one sort key"). A whole double (`1.0`) is
still accepted as ±1. Both servers now match.

The Python server carries mongod's codes; the Rust core defers these cases (bool
included — it no longer coerces `true` to `1`) so the Rust server rejects them too.
Three-way mongod 7.0.12-verified.

#### Fixed

- The `$sort` stage rejects a non-numeric direction (`15974`), a numeric non-±1
  direction (`15975`), and an empty spec (`15976`), instead of leaking a Python
  `ValueError`, coercing a bool, or silently no-op-ing (both servers).

### $split validates its arguments instead of leaking a Python error

`$split` with an empty separator leaked a raw Python `ValueError` (`empty
separator`), and its type / arity errors surfaced with a generic code. mongod
rejects each with a specific Location code: an empty separator is `40087`, a
non-string first / second argument is `40085` / `40086`, and the wrong number of
arguments is `16020`; a null string or separator still yields `null`. The Python
server now carries these codes; the Rust core already defers every invalid case,
so the Rust server rejects them too. Three-way mongod 7.0.12-verified.

#### Fixed

- `$split` reports mongod's Location codes — empty separator `40087`, non-string
  first / second argument `40085` / `40086`, wrong arity `16020` — instead of
  leaking a Python `ValueError` or using a generic code (both servers).

### SQL server: binary COPY, Parse-time inference, full array-literal grammar

`COPY … (FORMAT binary)` works in both directions: COPY OUT emits the PGCOPY
stream (signature/flags header bundled with the first row the way real PG
frames it, one CopyData per row, int16 -1 trailer) with each field encoded by
its column type through the existing binary result encoders, and COPY IN
parses the same layout, decoding fields by the target column's type.

Untyped parameters get real Parse-analysis type inference — a client that
binds a value in binary format with no declared type (psycopg's
`Range(empty=True)` dump sends OID 0) takes its type from the AST (a cast on
the parameter, a cast or range-constructor operand it's compared with) — and
an untyped parameter fed straight to a VARIADIC "any" function (`concat`)
raises 42P18 like real Postgres.

The array machinery reaches PG's full literal grammar: nested `{{…}}`
multi-dimensional arrays parse, render, and encode/decode in binary
(row-major with per-dimension headers), `[l:u]=` bounds prefixes parse,
`box[]`'s `;` delimiter is honoured both ways, `int[][][]` collapses to the
one array type like PG, `'{a,b}'::text[]` casts materialise real lists (so
subscripting and `unnest` work), array concatenation `||` concatenates
lists, and `= any('{1,2}')` accepts array-literal operands. E-string
literals are now decoded by the engine *before* sqlglot parses (sqlglot's
half-decoding was lossy for `E'\\x5c'`), `json` and `jsonb` columns carry
their distinct OIDs (114/3802 — plain json's binary form has no version
byte), table row types appear in `pg_type` (typtype `c`, with `typarray`,
resolvable via regtype so psycopg's `TypeInfo.fetch(conn, "<table>")`
works), and the `name`/`aclitem` types exist with their real OID pairs.

Range text literals follow PG's quoting rules exactly: quoted bound tokens
with `""`/`\X` escapes parse, embedded quotes/backslashes double on render,
ASCII-only whitespace trimming (Python's unicode-aware `.strip()` corrupted
NBSP/NEL bounds), and a user-declared range constructor result describes
with its minted OID. psycopg's range, multirange, json, array, and string
suites all pass.

#### Added

- `GROUP BY 1` positional references resolve to the select-list expression
  (42P10 when out of range); computed GROUP BY keys beyond the aggregation
  engine's operators (`GROUP BY col = ascii(x)`, `substr(…)`) evaluate
  per-doc in Python before the pipeline, typed from the source expression.
- bytea params substitute through `::bytea` / `::bytea[]` casts so equality
  against computed bytea values compares bytes (text-format arrays decode at
  Bind); `(x::box[])::text` renders with the element's rules.

#### Fixed

- Multi-statement `COPY` strings raise 42601 (ProgrammingError), bytea cast
  failures 22P02 (was XX000), and the binary array encoder covers
  varchar[]/bpchar[] (1015/1014).

### SQL server: composite records end to end, typed array parameters, NaN semantics

The composite/record machinery closes its remaining conformance gaps.
Array-of-composite elements binary-encode as real records (not JSON), a
binary composite parameter with a NESTED composite field recurses by the
field's minted OID instead of crashing, a registered dumper's record text
literal (`'("(foo,10)",20)'::"-x-€"`) parses on INSERT — including nested
and zero-field (`'()'`) types — and a declared-composite table column
reports its minted OID so a registered psycopg loader parses nested fields
by their reflected types. Anonymous `row(…)` records now carry each field's
SQL type OID from the source expression: an untyped literal embeds
unknown (705, loads as bytes like real PG), `::text` embeds 25, `::bytea`
17 with real binary bytea, and int literals type int4. `VALUES` rows of
records describe as RECORD, and `array_agg` types as the element's real
array type (jsonb_agg/json_agg keep json) — psycopg's
`CompositeInfo.fetch` of a zero-field type depends on it. Parse, Bind, and
Describe now run inside the open transaction's storage scope, so a type
created earlier in an uncommitted block is visible to parameter-OID
resolution.

Typed array parameters generalize: a text- or binary-format array param
with a known array OID (int2[]/numeric[]/inet[]/bytea[]/…) decodes into a
typed list and substitutes through a `::tag[]` cast, so equality against
`array[…]` constructor values compares element-wise (this closed the numpy,
uuid, and network dump/load clusters wholesale). Array casts coerce LIST
values to the element's canonical form, bare `array[x::inet, …]`
constructors describe as the element's array type, uuid/inet/cidr/macaddr
text parameters canonicalise at Bind (psycopg dumps uuids as bare hex), and
`NaN = NaN` is true like Postgres. psycopg's composite, numpy, uuid, net,
and numeric suites now pass (numeric's exhaustive wide-digit test remains —
the Decimal128 34-digit cap).

### SQL server: DO blocks, backend termination, richer diagnostics, typed-param polish

A batch of protocol-conformance closers across the psycopg gauge's error,
connection, prepared-statement, cursor, and adapter suites.

`DO $$ … $$` blocks run through a minimal plpgsql interpreter: `RAISE
NOTICE`/`WARNING`/`INFO` surface as NoticeResponse messages (via the new
`SQLResult.notices`), `RAISE EXCEPTION` raises with its `USING ERRCODE`
(default P0001), and `EXECUTE format(…)` runs dynamic SQL whose errors keep
their real SQLSTATE. `pg_terminate_backend` / `pg_cancel_backend` close the
target connection through the live-session registry. ErrorResponse now
carries the optional diagnostic identity fields (schema/table/column/
constraint) and a statement position, so a CHECK violation reports its
constraint name and a name error renders the `LINE 1: …` caret context.

Typed parameters and introspection sharpen: `pg_prepared_statements`
reports each statement's original query text, real prepare time, and
regtype parameter names (with array typing); `DEALLOCATE ALL` clears the
extended-protocol registry; INSERT parameters infer their type from the
target column at Parse (so an untyped `%s` into a jsonb column types
correctly); `->`/`#>` type as jsonb and `->>`/`#>>` as text, and integer
JSON subscripts (`-> 1`) index arrays. `numeric` renders in plain
positional form (`1.1E+2` → `110`, matching `numeric_out`), `NaN = NaN`
holds, `generate_series(…)::int4` casts each element, the East-Asian client
encodings Python can convert are accepted, `format('%s/%I/%L', …)` works,
and `max_prepared_transactions` reports non-zero so drivers' 2PC probes
pass.

#### Added

- `pgwire.error_response` diagnostic fields + statement position;
  `notice_response` full severity/sqlstate.
- `errors.SQLError` carries `diag` / `position`.
- `SQLResult.notices`, rendered as NoticeResponse in both protocols.
- `TableDef.temp` (CREATE TEMP TABLE — reflected in error schema).

### SQL server: idle_in_transaction_session_timeout

A connection left idle inside an open transaction block longer than the
`idle_in_transaction_session_timeout` GUC (milliseconds; 0 = disabled, the
default) is now terminated with a FATAL `25P03` — the connection's blocked
read for the next command is bounded by the timeout, and exceeding it aborts
the open transaction and closes the socket, exactly as Postgres does.
psycopg's `test_right_exception_on_session_timeout` (which sets the GUC,
sleeps, and expects `IdleInTransactionSessionTimeout`) passes.

#### Added

- `idle_in_transaction_session_timeout` GUC (default 0); the wire loop bounds
  the next-command read by it while a transaction is open and terminates on
  timeout.

### SQL server: savepoint rollback reverts DDL; wide binary numerics round-trip

`ROLLBACK TO SAVEPOINT` now undoes schema changes made after the savepoint,
not just data writes. A `CREATE TYPE` / `CREATE TABLE` / `DROP` / `ALTER`
inside a savepoint snapshots every catalog collection (they're tiny), so the
rollback restores the pre-savepoint schema — a re-`CREATE` of the same type
then succeeds, and a `DROP` is undone. Previously only DML target
collections were snapshotted, so the catalog change leaked past the abort
(psycopg's `test_change_type_savepoint`, which creates and rolls back an enum
three times, hit "type already exists").

The binary `numeric` decoder handles arbitrarily wide values: a wide
integral magnitude combined with a large declared scale sized the Decimal
context too small and made the final quantize raise `InvalidOperation`
(surfacing to the client as an internal error). The context now spans the
full integer + fractional digit count, so `test_dump_numeric_exhaustive`'s
50-plus-digit values round-trip.

#### Added

- `catalog.ALL_CATALOG_COLLECTIONS`; `engine._is_ddl` drives the catalog
  snapshot for savepoint rollback.

#### Fixed

- `pgextended._decode_numeric` context precision spans the whole value.

### SQL server: binary and scrollable server-side cursors

psycopg's `ServerCursor` / `RawServerCursor` (DECLARE … CURSOR / FETCH /
MOVE over the wire) work in binary as well as text, and honour scroll
semantics. A binary `FETCH … FROM <name>` rides the extended protocol, so
Describe on the FETCH portal now reports the cursor's columns instead of
NoData — previously the server sent DataRows with no prior RowDescription, a
protocol violation the client rejected. `DECLARE … NO SCROLL` is enforced
(backward movement raises 55000, a psycopg `OperationalError`) and `SCROLL`
allows it; a negative bare count (`FETCH -2`, `MOVE -1`) scans backward in
the default direction, `FORWARD -n` / `BACKWARD -n` flip direction, and
`MOVE ABSOLUTE 0` repositions before the first row (not at the end) like
Postgres. A DECLARE body that isn't a row-returning query (`wat`, a DDL
statement) raises 42601 (ProgrammingError) rather than 0A000. psycopg's
`test_cursor_server.py` goes from 15 failed / 7 errored to 0.

#### Added

- `_Cursor.scrollable` (SCROLL / NO SCROLL); `_moves_backward` gates NO
  SCROLL cursors.
- `describe_statement` reports a FETCH portal's columns (binary server
  cursors) and NoData for MOVE.

#### Fixed

- Negative FETCH/MOVE counts scan backward; `MOVE ABSOLUTE 0` positions
  before-first; a non-query DECLARE body is a syntax error.

### SQL server: type modifiers on the wire, E-string fidelity, transaction characteristics

RowDescription now carries real PG type modifiers: `select null::varchar(42)`
describes with typmod 46 (and varchar/bpchar keep their distinct OIDs
1043/1042 instead of folding onto text), `numeric(p,s)` packs precision and
scale — including negative scales, which sqlglot can't parse and the engine
now pre-rewrites through a sentinel — and the bit/varbit/time-family
precisions all flow through, so psycopg's `Column.display_size` /
`precision` / `scale` / `type_display` report like real Postgres. psycopg's
`test_column.py` goes 35 failed → 0.

Two more conformance holes close alongside. `E'…'` escape strings
interpolated by psycopg's ClientCursor (any string containing a backslash)
were double-unescaped — sqlglot already decodes the simple escapes, and the
second pass corrupted `\\b` into a backspace or raised 0A000 in the INSERT
value path; the decoder now finishes only the octal/hex/unicode forms
sqlglot leaves raw. And transaction characteristics are honoured end to end:
`BEGIN ISOLATION LEVEL … / READ ONLY / DEFERRABLE` (every spelling,
including the ones sqlglot rejects), `SET TRANSACTION`, and `SET SESSION
CHARACTERISTICS AS TRANSACTION` apply to the transaction (via the SET LOCAL
revert machinery) or the session defaults, and the `transaction_*` GUCs
mirror their `default_transaction_*` values until overridden — psycopg's
`set_isolation_level` / `set_read_only` / `set_deferrable` suite goes 13
failed → 0.

#### Added

- `result.py` / `pgwire.py`: `ColumnDesc.typmod` carried through both the
  simple-query and extended-protocol RowDescription emitters.
- `typemap.py`: `cast_type_identity` — (oid, typmod) for modifier-bearing
  cast targets, including arrays (element typmod with the array OID).
- `engine.py`: `_parse_txn_characteristics` shared by BEGIN / SET
  TRANSACTION / SET SESSION CHARACTERISTICS; `session.get_setting` falls
  back dynamically from `transaction_*` to `default_transaction_*`.

#### Fixed

- `scalar._unescape_estring` no longer re-decodes escapes sqlglot already
  resolved (the `test_leak` corruption); `planner._literal` accepts
  ByteString values in INSERT position.

### Aggregation stage-argument validation for `$count`, `$project`, and `$sortByCount`

Three more aggregation stages now reject malformed arguments with mongod's exact
error code instead of silently computing a wrong result. `$count` enforces that
its field is a non-empty string that is neither `$`-prefixed, dotted, nor the
reserved `_id`; an empty `$project` specification is rejected up front; and
`$sortByCount` requires a `$`-prefixed path string or a single-`$`-key expression
object. Both the Python and Rust servers are covered (the Rust core defers each
invalid case so the exact code is raised), and each is verified against real
mongod 7.0.12.

#### Fixed

- `$count` now raises `Location40156` (non-string field), `Location40157`
  (empty), `Location40158` (`$`-prefixed), `Location40160` (contains `.`), and
  `Location15948` (`_id`) instead of accepting the malformed field name.
- `$project` with an empty specification now raises `Location51272`
  ("projection specification must have at least one field") instead of returning
  the input documents unchanged.
- `$sortByCount` now raises `Location40149` (non-string/non-object argument),
  `Location40148` (bare, non-`$` path string), and `Location40147`
  (non-expression object) instead of grouping on a constant.

### `$strcasecmp` coerces its operands like mongod

`$strcasecmp` previously required both operands to be strings and raised a
generic `TypeMismatch` (14) otherwise. Real mongod `$toString`-coerces each
operand first — a number becomes its string form, `null` becomes the empty
string — and rejects only a boolean. SecantusDB now does the same, matching
mongod 7.0.12.

#### Fixed

- `$strcasecmp` now coerces a non-string operand to a string (`null` → `""`,
  numbers → their string form) instead of raising, so `{$strcasecmp: [5, "a"]}`
  returns `-1` like mongod. A boolean operand raises `Location16007` (mongod's
  code). Integer coercion computes on both the Python and Rust servers; the
  Rust core defers double/date coercion to Python.

### $substrBytes rejects a bool index, and $substr is byte-based like mongod

Completing the aggregation bool-as-int sweep: `$substrBytes` computed a bool
start/length index (`as_int_like(Boolean) → 0/1`) instead of rejecting it. Both
servers now reject — the Python server with mongod's exact codes (16034 for the
starting index, 16035 for the length), the Rust core defers to `BadValue`.

While verifying against mongod, `$substr` turned out to be mis-aliased: mongod
treats `$substr` as a deprecated alias of `$substrBytes` (byte-based), but
SecantusDB routed it to `$substrCP` (code-point-based). On multi-byte strings
the two diverge, and a bool index reported the wrong code (34450 instead of
16034). `$substr` now aliases `$substrBytes` on both servers, fixing the byte
semantics and the bool code together. Three-way mongod 7.0.12-verified.

#### Fixed

- `$substrBytes` rejects a bool start/length argument with mongod's codes
  (16034 / 16035) instead of coercing it to 0/1 (both servers).
- `$substr` is now a byte-based alias of `$substrBytes` (matching mongod),
  rather than code-point-based `$substrCP`.

### $substrBytes rejects a byte range that splits a UTF-8 character

A `$substrBytes` (or its `$substr` alias) range whose start or end falls inside a
multi-byte UTF-8 character used to return a Unicode replacement character (Python
server) or an empty string (Rust server) rather than the error mongod raises. Both
servers now reject: the Python server with mongod's exact codes — 28656 when the
starting index is a UTF-8 continuation byte, 28657 when the ending index lands in
the middle of a character — and the Rust core defers to `BadValue`.

The subtlety a fuzz run surfaced: mongod rejects a continuation-byte start *even
for an empty (length 0) range*, which the Rust core's "is the slice valid UTF-8?"
check missed (an empty slice is always valid), so both engines needed an explicit
boundary check. A negative start keeps the legacy slice semantics on both engines.
Clean character boundaries and clamped past-the-end ranges still compute. Three-way
mongod 7.0.12-verified.

#### Fixed

- `$substrBytes` / `$substr` reject a byte range that splits a UTF-8 character
  (mongod's 28656 / 28657 on the Python server, `BadValue` on the Rust server),
  including an empty range that starts on a continuation byte, instead of
  returning a replacement character or empty string.

### $substrBytes truncates a double index, completing substr numeric fidelity

`$substrBytes` rejected a non-integer start/length, but mongod accepts any double
there and truncates it toward zero (`1.7`→1, `2.9`→2, `0.9`→0) — unlike
`$substrCP`, which rejects a fractional double. Both servers now truncate and
compute the same substring; a truncated-negative start (`-1.7`→-1) still falls
into the negative-start rejection (50752), and a negative length still means "to
the end".

With this, `$substrBytes` / `$substrCP` numeric-argument handling matches mongod
across the board — bool rejection, byte-vs-code-point aliasing, whole-double /
fractional / truncation semantics, UTF-8-split rejection, and negative indices.
Three-way mongod 7.0.12-verified.

#### Fixed

- `$substrBytes` accepts a double start/length and truncates it toward zero
  (matching mongod), instead of rejecting all non-integer values (both servers).

### $substr* reject a negative index, like mongod

A negative start (or, for `$substrCP`, a negative length) silently produced a
Python-style negative-index slice — usually an empty or wrong substring — instead
of the error mongod raises. Both servers now reject: the Python server with
mongod's exact codes, the Rust core defers to `BadValue`.

- `$substrBytes` / `$substr` negative start → **50752** ("starting index must be
  non-negative"). A negative *length* is still fine — it means "to the end".
- `$substrCP` negative start → **34455**, negative length → **34454**.

This completes `$substrBytes` / `$substrCP` numeric-argument fidelity (bool
rejection, byte-vs-code-point aliasing, whole-double / fractional handling,
UTF-8-split rejection, and now negative indices). Three-way mongod 7.0.12-verified.

#### Fixed

- `$substrBytes` / `$substr` / `$substrCP` reject a negative start (and
  `$substrCP` a negative length) with mongod's exact error code instead of
  returning a Python-style negative-index slice (both servers).

### $toDate rejects a bool instead of coercing it to a date

`$toDate` (and `$convert` to `date`) silently coerced a bool to a date by treating
it as `1` / `0` milliseconds. mongod rejects it: a bool is a `ConversionFailure`
(241, "Unsupported conversion from bool to date"), which `$convert`'s `onError`
still catches. Every other supported source (int / long / double / string /
objectId / decimal → date, null → null) is unchanged. Both servers now match.

The Python server carries mongod's 241 code (through `$toDate`, which previously
re-wrapped it as a generic error); the Rust core now classifies bool → date as a
supported-but-failed conversion so `$convert`'s `onError` applies on the Rust
server too. Three-way mongod 7.0.12-verified.

#### Fixed

- `$toDate` / `$convert` to `date` reject a bool with `ConversionFailure` (241)
  instead of coercing it to a date, and `$convert`'s `onError` handles the failure
  (both servers).

### `$toLong` aggregation operator

The `$toLong` conversion operator is now implemented, completing the `$to*`
conversion family (`$toInt` / `$toDouble` / `$toDecimal` / `$toBool` /
`$toString` / `$toDate` were already present, and `$convert: {to: "long"}` too).
It converts numbers (truncating a double toward zero), numeric strings, and
booleans to a 64-bit `long`, matching real mongod 7.0.12 — so a value beyond the
32-bit range that `$toInt` rejects converts cleanly, while a value beyond the
64-bit range overflows (code 241, catchable by `$convert`'s `onError`). Covered
on both the Python and Rust servers (the Rust core computes the numeric cases
and defers string / Decimal128 parsing to Python).

#### Added

- `$toLong` — previously an unrecognized expression operator (code 168), now
  converts int / long / double (truncating toward zero) / bool / numeric string
  to a BSON `long`; a result outside `[-2^63, 2^63-1]`, or a non-finite double,
  overflows with code 241.

### $trim / $ltrim / $rtrim validate their input and chars arguments

The trim operators silently ignored a non-string `chars` argument (falling back to
whitespace trimming) and reported a non-string `input` with a generic error.
mongod validates both: a non-string `input` is `Location50699` and a non-string
`chars` is `Location50700` (each message names the offending value and type). A
null / missing `input` yields `null`, and — unlike the whitespace default — an
explicit `chars: null` also yields `null`. Both servers now match.

The Python server carries mongod's codes; the Rust core defers the non-string
cases (so the Rust server rejects them) and now returns `null` for a `chars: null`
rather than deferring. Three-way mongod 7.0.12-verified.

#### Fixed

- `$trim` / `$ltrim` / `$rtrim` reject a non-string `input` (`Location50699`) or
  `chars` (`Location50700`) instead of erroring generically or silently ignoring
  `chars`, and yield `null` for a `chars: null` (both servers).

### $type validates its argument and accepts whole-double codes

The `$type` query operator didn't validate its argument: an unknown alias, an
out-of-range or fractional numeric code, and a bool all silently matched nothing
instead of erroring, and the Rust engine additionally rejected a valid whole-double
code (`{$type: 2.0}`) that mongod accepts. mongod validates it: a known alias or a
numeric code in `{-1, 1..19, 127}` (a whole double counts) is valid; an unknown
alias or an out-of-range / fractional code is `BadValue` (2, with a `{$exists:
false}` hint for code `0`), and a bool / other type is `TypeMismatch` (14). Both
servers now match.

The Python server carries mongod's codes; the Rust core defers the invalid cases
so the Rust server rejects them too, and now computes whole-double codes rather
than deferring (so the Rust server no longer rejects a valid `{$type: 2.0}`). All
22 aliases (including the deprecated ones and `number`) are recognised. Three-way
mongod 7.0.12-verified.

#### Fixed

- `$type` rejects an unknown alias / out-of-range / fractional code (`BadValue`)
  and a bool (`TypeMismatch`) instead of silently no-matching, and accepts a valid
  whole-double numeric code on both servers (previously the Rust server rejected
  it).

### Unary math operators reject non-numeric operands instead of coercing or crashing

`$abs`, `$ceil`, `$floor`, `$sqrt`, `$exp`, `$ln`, `$log10`, `$round`, and
`$trunc` never type-checked their operand. A string leaked a raw Python
`TypeError` (surfacing as a generic error, not a clean server error), and a bool
was silently coerced to `1` / `0` and computed on. mongod rejects both: a
non-numeric operand is `Location28765` (`$round` / `$trunc` use `51081`), while
`null` still passes through as `null`. Both servers now match.

The Python server carries mongod's exact codes; the Rust core defers these cases
to `BadValue` (so the Rust server rejects them too, rather than coercing a bool).
Whole-double operands and every valid numeric input are unaffected. Three-way
mongod 7.0.12-verified.

#### Fixed

- `$abs` / `$ceil` / `$floor` / `$sqrt` / `$exp` / `$ln` / `$log10` reject a
  string or bool operand with `Location28765`, and `$round` / `$trunc` with
  `51081`, instead of coercing a bool to `1`/`0` or leaking a Python `TypeError`
  (both servers).

### $unwind validates its path, includeArrayIndex, and preserveNullAndEmptyArrays

The `$unwind` stage silently accepted a malformed spec: a non-`$`-prefixed `path`,
a non-string `path`, a non-string / empty / `$`-prefixed `includeArrayIndex`, and a
non-bool `preserveNullAndEmptyArrays` (which it coerced with Python's `bool()`).
mongod rejects each: a non-string path is `Location28808`, a bare path is
`Location28818`, a non-string / empty `includeArrayIndex` is `Location28810`, a
`$`-prefixed one is `Location28822`, and a non-bool `preserveNullAndEmptyArrays` is
`Location28809`. Both servers now match.

The Python server carries mongod's codes (including its verbatim double-space
message quirk for `28810`); the Rust core defers the invalid cases so the Rust
server rejects them too. Three-way mongod 7.0.12-verified.

#### Fixed

- `$unwind` rejects a non-string / bare `path` (`28808` / `28818`), a non-string /
  empty / `$`-prefixed `includeArrayIndex` (`28810` / `28822`), and a non-bool
  `preserveNullAndEmptyArrays` (`28809`), instead of silently accepting or coercing
  them (both servers).

### $pop / $position / $slice / $bit reject a bool argument, like mongod

A cluster of update-operator bugs from the same root cause as `$inc`/`$mul`:
Python's `bool` being an `int` subclass. `$pop: true` was treated as `$pop: 1`
(pop the last element) on both servers, and `$push` with `$position: true` or
`$slice: true` computed on the Rust server (insert at index 1 / keep 1) — all
of these are parse errors in mongod. Every one now rejects a bool argument:
the Python server reports mongod's exact codes (9 for `$pop`, 2 for
`$position` / `$slice` / `$bit`) and messages, and the Rust server surfaces
`BadValue`. `$pop` now also errors on a number other than ±1 (it silently did
nothing before). Found while triaging the driver-gauge update operators;
three-way mongod 7.0.12-verified.

#### Fixed

- `$pop`, `$push` `$position` / `$slice`, and `$bit` reject a bool argument
  instead of coercing it to 1 (both servers); `$pop` errors on a non-±1 value.

## [0.5.4b237] — 2026-07-17

### SQL: user-defined range types, and enum OIDs in every plan shape

A focused SQL-server release, both slices driven by the psycopg
conformance gauge. `CREATE TYPE ... AS RANGE` brings user-defined range
types end to end — catalog rows, the v3 extended protocol's binary codec
path, and range operators over the new types — building on the
range/multirange parameter support that shipped in the previous release.
And enum result OIDs now survive **every** plan shape: joins, subqueries,
set operations, and computed projections all report the enum's real
`pg_type` OID in RowDescription (previously only simple scans did), with
`mood[]`-style enum-array columns decoding correctly on the binary path.

#### Added

- `CREATE TYPE ... AS RANGE` / `DROP TYPE`: catalog rows, binary codecs on
  the extended protocol, range operators over user-defined range types.
- Enum result OIDs in `RowDescription` across joins, subqueries, set
  operations, and computed projections; enum-array (`mood[]`) columns on
  the binary path.


### SQL server: CREATE TYPE … AS RANGE

User-declared range types land: `CREATE TYPE textrange AS RANGE (subtype =
text)` mints the type and its auto-created companion multirange
(`textmultirange`, following Postgres' naming rule) with allocation-stable
OIDs, reflects both through `pg_type` (typtype `r` / `m`, real `typarray`)
and `pg_range` (`rngtypid` / `rngsubtype` / `rngmultitypid`), and wires the
full value path: literal casts (`'[a,b)'::textrange`,
`'{[a,b)}'::textmultirange`) parse with the declared subtype's coercion, the
type gets its constructor (`textrange(lo, hi, bounds)`), parameters a
registered psycopg dumper declares with the minted OID round-trip in text and
binary (PG's range wire layout), results describe with the minted OID and
render/encode as ranges in both formats, and `DROP TYPE` removes the pair.
psycopg's `RangeInfo.fetch` → `register_range` → typed `Range` values works
end-to-end, as does the multirange counterpart.

The statement itself exceeds sqlglot's parser (it falls back to a raw
Command), so the engine intercepts the command tail — the same pattern
`CREATE DOMAIN` uses. Together with the earlier waves this clears psycopg's
custom-range fixtures, which previously errored out of thirty-one range and
multirange tests before any assertion ran.

#### Added

- `catalog.py`: `create_range_type` / `get_range_type` (by range or companion
  multirange name) / `drop_range_type` / `list_range_types`, minted from the
  stable user-type OID counter; `multirange_name_for` (Postgres' rename rule).
- `engine.py`: the `CREATE TYPE … AS RANGE (…)` Command interception (subtype
  resolved via regtype spelling; collation/opclass options accepted, ignored);
  `DROP TYPE` drops range types.
- `ranges.py`: `custom_elem` parsing/construction for non-builtin subtypes.
- `scalar.py`: custom range/multirange casts and constructors.
- `pgextended.py`: binary custom-range/multirange parameter decode and the
  generic binary range/multirange result encoders; user-type binary params
  route by catalog kind.
- `virtual.py`: `pg_type` + `pg_range` rows for user ranges; `regtype` /
  `user_type_*` resolution covers them.

### SQL server: enum OIDs through every plan shape, and enum-array columns

The minted enum OID now survives every SELECT plan shape. GROUP BY keys,
JOIN projections, `SELECT DISTINCT`, and per-row-evaluated selects (a scalar
function alongside the enum column) previously described enum result columns
as plain `text` (25) because the pipeline/evaluated planners flatten output
columns to string type tags; the enum identity now travels in a parallel
`out_enum_types` position map so RowDescription reports the mint — and a
psycopg `register_enum` loader fires on those results — in both the simple
and extended protocols. `array['sad'::mood, …]` constructors describe with
the minted array-companion OID like the `::mood[]` cast already did.

`mood[]` table columns land too: an array of a declared enum type was
previously rejected outright (`unsupported column type`); it now stores a
text array, validates every element against the enum's labels at write time
(22P02), and reports the array-companion OID so a registered loader returns
lists of enum members. An array of an undeclared type raises 42704.

#### Added

- `planner.py`: `out_enum_types` on `PipelineSelectPlan` / `EvaluatedSelectPlan`,
  populated by the DISTINCT / GROUP BY / JOIN / evaluated builders;
  `_enum_array_element_name` recognises `mood[]` column declarations; the
  constant-select array-constructor override gains an enum branch.
- `executor.py`: `_tagged_out_column_descs` resolves minted enum OIDs for the
  string-tag plans (shared by Execute and Describe); `_out_column_descs`
  reports the array-companion OID for enum-array columns; enum write
  validation checks each element of an array value.

## [0.5.4b236] — 2026-07-17

### Measured everywhere: a concurrent test suite that caught real bugs, three-way benchmarks, and self-hosted docs

SecantusDB now measures itself against real `mongod` in both dimensions.
The per-operation benchmark became a three-way comparison — the Rust
server lands at **2.1×–4.5× of mongod**, roughly 2.7×–5.2× faster than
the Python server workload-for-workload — and a new **concurrent test
suite** (`bench.concurrency --server all`) sweeps 1–8 parallel writers
across the Python server, the Rust server, and mongod. That harness paid
for itself immediately: its first runs caught a write-path bug where a
WiredTiger transaction marked rollback-only by a concurrent competitor
surfaced to clients as a generic `InternalError` instead of a retryable
write conflict, plus a Windows-only ordering bug in the admin console's
recent-connections list.

The conflict story now matches mongod end to end. Commit-time conflicts
map to the same retryable `WriteConflict` machinery as operation-time
ones; plain writes retry **without a deadline** — a client never sees
`WriteConflict` outside a multi-document transaction, exactly like
mongod's `writeConflictRetry`; and the two hot-path writes to the shared
oplog-metadata row (per oplog emit, and per cluster-time mint — the
latter ran on every driver heartbeat) are gone, with restart
monotonicity guaranteed structurally by a one-second recovery bump of
the cluster clock.

The documentation moved home. Both docs trees — the main reference and a
new dedicated Rust-server tree — are now built and deployed with the
website at `secantusdb.com/docs/` and `/docs/rust/`, wearing the site's
banner; readthedocs.org keeps a pointer banner to the new location, and
the release pipeline dropped its four Read-the-Docs legs. The SQL server
kept marching on the psycopg gauge: range/multirange parameters as
first-class values, enum result OIDs in RowDescription, composite-type
materialization, JSON/datetime binary codec fidelity, and a batch of
protocol quick-wins.

#### Highlights

- Concurrency: `bench.concurrency --server python|rust|mongod|all` with
  failure-diagnosis instruments (writer log tails, `--server-log`);
  measured table in [Concurrency](concurrency.md).
- Fixed by it: commit-time WT conflicts are retryable `WriteConflict`
  (was `InternalError`); unbounded `writeConflictRetry` for plain writes;
  heartbeats no longer write the oplog meta row; admin recent-targets
  ordering is deterministic under timestamp ties.
- Benchmarks: three-way `docs/benchmark.md` — Rust server 2.1×–4.5× of
  mongod per operation.
- Docs: self-hosted at secantusdb.com/docs (main) and /docs/rust (the new
  nine-page Rust-server tree); RTD carries a moved banner; `[project.urls]`
  metadata on PyPI; the weekly validate run regenerates the cross-driver
  summary and the Rust-server gauge reports.
- SQL: range/multirange parameters, enum OIDs in RowDescription,
  composite materialization, psycopg JSON/datetime codec fidelity,
  protocol quick-wins.


### Benchmark: the Rust server measured — 2.1×–4.5× of mongod

`docs/benchmark.md` is regenerated as a three-way comparison
(`bench.compare_servers`): real `mongod`, the Rust server, and the Python
server, six workloads end-to-end through `pymongo` on on-disk WiredTiger.
The Rust server lands at **2.1×–4.5× of mongod** per operation and
~2.7×–5.2× faster than the Python server workload-for-workload; the Python
server sits at 6×–20.5× of mongod on this run. The Rust docs tree cites the
numbers, and its releases page now links the `secantusdb-v`-filtered
GitHub listing (binary releases are pre-releases, so the bare releases page
leads with the source-only PyPI release).

### Concurrent writers: heartbeats stop writing the oplog meta row; conflict retries are unbounded

Two follow-ups from the concurrency harness. `current_cluster_time()` no
longer persists the oplog meta row on every call — it runs on every
`hello` reply under the replica-set persona (driver heartbeats) and on
change-stream high-water-mark minting, so it was a single-row write
hotspot; restart monotonicity is now guaranteed structurally (recovery
bumps the cluster clock one second past the meta hint, the oplog tail,
and the wall clock, so mints that were never persisted can't be
re-minted). And the non-transaction write-conflict retry loop loses its
5-second deadline: real mongod's `writeConflictRetry` loops until the
write goes through, so a client never sees `WriteConflict` (112) for a
plain write — ours now matches, logging a warning during long retry
stretches. Post-fix sweeps show zero client-visible conflict errors at
1–8 concurrent writers.

#### Fixed

- `Storage.current_cluster_time()` is write-free; recovery bump keeps
  restart cluster time strictly monotonic (regression-tested with a
  simulated crash).
- `_retry_write_conflicts`: unbounded retry with capped backoff outside
  user transactions (inside one, conflicts still surface immediately as
  mongod's statement-time `WriteConflict`).

### Concurrent writers: commit-time conflicts retry instead of erroring

Under concurrent writers, a WiredTiger batch transaction can be marked
rollback-only by a competitor after its last operation succeeded; the
conflict then surfaces at `commit_transaction` as a bare EINVAL with no
`WT_ROLLBACK` marker, which escaped the write-conflict retry wrapper and
reached clients as a generic `InternalError` (code 1). Found by the new
three-server `bench.concurrency` harness. Commit failures now map to the
retryable `WriteConflictError` when WiredTiger reports a rollback reason or
the documented rollback-required EINVAL shape; commit failures that are
neither (I/O errors, panics) stay loud, per the never-swallow rule. The
remaining structural contention (every batch transaction updates the shared
oplog-meta row, so writers on different collections still conflict) is
recorded in `tasks/backlog.md` for the WT concurrency plan.

#### Fixed

- `storage._commit_batch_transaction`: commit-time rollback-required
  failures become `WriteConflictError` (retried outside user transactions;
  mongod's statement-time `WriteConflict` inside them) instead of
  `InternalError`.

### The concurrency benchmark measures all three servers

`bench.concurrency` grows a `--server python|rust|mongod|all` switch —
`all` sweeps the three back-to-back and prints a combined
throughput-vs-writers table — plus two diagnosis instruments the first
run immediately paid for: failing writers dump their log tails instead of
silently zeroing, and `--server-log` captures the server's own
stdout/stderr. [Concurrency](https://secantusdb.com/docs/concurrency.html)
now carries the measured end-to-end table (mongod scales to 4.1× at 8
writers; the Rust server holds flat behind its global write mutex; the
Python server degrades under the shared oplog-meta hotspot — with
conflicts retried and surfaced honestly since the commit-conflict fix this
harness uncovered).

### Docs: the site banner tops every self-hosted docs page

Both self-hosted docs trees (secantusdb.com/docs/ and /docs/rust/) now carry
the standard site banner — SecantusDB · Python DB · Rust DB · Blog ·
Python docs · Rust docs — via furo's announcement bar, so the documentation
reads as part of secantusdb.com rather than a detached sub-site. The
readthedocs.io copies keep their "docs have moved" banner instead (the two
are the same announcement slot, switched on the READTHEDOCS build env var).

### Docs move to secantusdb.com; Read the Docs carries a pointer banner

The documentation is now self-hosted: the main tree at
`secantusdb.com/docs/` and the Rust server's tree at
`secantusdb.com/docs/rust/`, both deployed atomically with every website
publish. The release pipeline drops its four Read the Docs legs
(`release-finalize` now waits only for the publish workflow and the PyPI
listing, and no longer requires `READTHEDOCS_TOKEN`); README, and the new
`[project.urls]` PyPI metadata, point at the self-hosted locations. The
readthedocs.io copies stay online but every page there now carries a
banner linking to the up-to-date docs (furo's announcement bar, enabled
only when `READTHEDOCS=True` is in the build environment, so the
self-hosted build never shows it).

### Docs: a dedicated Rust-server documentation tree

The Rust server gets its own Sphinx tree (`docs-rust/`, built with
`invoke docs-rust`): installation from the prebuilt `secantusdb-v*` binary
archives, the full `secantusd-rs` CLI-flag and `secantusd.toml` reference,
the embedded `RustServer` handle, security (SCRAM / X509 / RBAC / rustls
TLS), backup and point-in-time recovery via `secantusd-rs restore`, the
crates architecture, conformance numbers, and the binary release track.
The tree is pure Markdown (no autodoc — its version is read from the
lockstep crate version), so it builds in any bare worktree, and it deploys
to secantusdb.com alongside the main docs.

### SQL server: composite types materialize — row(), record casts, typed field access

Composite values were half-real: a `'(foo,42,3.14)'::testcomp` cast passed raw
text through with a text OID, so psycopg's `register_composite` loaders never
fired, `row(…)` didn't exist, and `(value).field` access failed on anything
but a table column. The whole path is now materialized: `row(a, b, …)` builds
an anonymous record (rendered `(a,b)`, described as RECORD 2249, with the PG
binary record layout on binary cursors); casts to a declared composite parse
the record text literal — including quoted/escaped fields and nested records —
into the typed, field-named subdocument; a parameter a registered psycopg
dumper declares with the minted composite OID round-trips in both text and
binary formats; `array[…::testcomp]` describes with the paired array OID;
`pg_typeof` prints the type's name; and `('…'::testcomp).bar` types as the
declared field, not text.

Composite and domain OIDs also switched to the allocation-stable mint that
enums got earlier (assigned at `CREATE TYPE`/`CREATE DOMAIN` from a persisted
counter, never renumbered or reused) — positional minting shifted every type's
OID whenever a lexically-earlier name appeared, sending registered client
loaders decoding the wrong type. `oid::regtype` output now also double-quotes
reserved words (`"order"`), which psycopg's `sql.Literal` pastes verbatim.
psycopg's `tests/types/test_composite.py` goes from 66 failing to 17 (the
remainder: binary record edge samples and suite-order effects).

#### Added

- `scalar.py`: `row(…)` anonymous records; composite cast materialization
  (`_composite_from_text` / `_composite_from_seq` with positional remap for
  `row(…)::type`); `typemap.parse_pg_record_literal`.
- `pgextended.py`: PG binary record encode (`_encode_record`) and param decode
  (`_binary_record_to_text`); minted user-type binary params keep raw payloads
  until the catalog resolves them at Bind; `pg_typeof($N)` resolves minted
  user-type OIDs.
- `catalog.py`: allocation-stable `composite_type_oids` / `domain_type_oids`
  (shared `_mint_user_type_oid` counter machinery).

#### Fixed

- `planner.py`: constant-select RowDescription overrides for composite casts
  (minted OID, `composite` tag), `array[…::testcomp]` (paired array OID), and
  composite field access (the field's declared tag); user-defined type names
  build as `udt` DataTypes in parameter substitution.
- `virtual.quote_type_name`: reserved words double-quote in regtype output.

### SQL server: a round of protocol-fidelity fixes from the psycopg gauge

A batch of small wire-protocol and error-surface divergences, each found by
running psycopg's own test suite against the server: `to_regtype` now accepts
double-quoted identifiers (psycopg's `TypeInfo.fetch(conn,
sql.Identifier("text"))` was returning None); garbage input (`"wat"`) raises
a real syntax error (42601) instead of feature-not-supported, so clients map
it to ProgrammingError; a non-numeric string bound to an integer or float
column surfaces `22P02 invalid input syntax` instead of an internal error;
COPY TO STDOUT sends one CopyData message per row like a real server (a
single all-rows blob made every row after the first vanish in psycopg's
`Copy.rows()`); a client's CopyFail aborts the enclosing transaction
(INERROR); a bare `VALUES (…)` answers extended-protocol Describe with its
row shape instead of NoData-then-DataRows (a protocol violation that crashes
libpq's stream mode); RowDescription reports fixed-width types' `typlen` and
encodes column names in the client's encoding; `pg_sleep()` sleeps;
`pg_tables` exists; and the transaction-characteristics GUCs
(`transaction_isolation` etc.) report their honest single-node constants.

Together these clear ~60 tests across psycopg's `test_typeinfo` (18 → 0),
`test_cursor_common` (27 → 3), `test_copy` (37 → 26) and `test_column`
(42 → 35) files.

#### Fixed

- `typemap.oid_for_regtype` / `planner._to_regtype`: double-quoted identifier
  resolution with Postgres case rules (quoted names keep case; built-ins only
  match lowercase).
- `engine.py`: bare expression statements → `42601`; extended-protocol
  Describe of `VALUES` returns the row shape.
- `typemap.coerce`: int/float coercion failures raise `22P02` (as an
  exception that is also a `ValueError`, so soft-fallback callers keep their
  behaviour).
- `pgserver.py`: COPY OUT chunks per row; CopyFail marks the transaction
  failed.
- `pgwire.row_description`: static `typlen` table; client-encoding column
  names (threaded from both the simple and extended paths).
- `functions.py`: `pg_sleep` (capped at 30s — our connection threads have no
  cancel path); `session.py`: transaction-characteristics GUC defaults.
- `virtual.py`: the `pg_tables` system view.

### SQL server: the psycopg JSON and datetime suites go fully green

Two of the three biggest failure clusters in the psycopg conformance gauge —
`tests/types/test_json.py` (181 failing) and `tests/types/test_datetime.py`
(259 failing) — now pass completely, taking the gauge headline from 2900
passed (70.3%) to 3473 passed (84.2%) under deterministic test order. The
third cluster, `test_typing.py` (125), was purely environmental: it shells out
to a bare `mypy`, which the gauge venv didn't carry — mypy now rides the `dev`
extra and all 125 pass.

The JSON cluster came down to one root cause with wide blast radius: json and
jsonb values were never parsed at ingress. A `'{"a":1}'::jsonb` cast passed
raw text through, so `->`/`->>` navigation returned NULL and output
double-encoded. Casts and json-declared parameters now parse into real JSON
values, `array[…]::text` renders Postgres' `array_out` literal instead of a
JSON list, `E'…'` escape strings evaluate (psycopg's `sql.Literal` emits them
for any string containing a backslash), and the plain-json OIDs (114/199)
alias the jsonb tag.

The datetime cluster decomposed into seven root causes, all fixed: temporal
parameters substituting as bare text (a datetime param silently compared
false against an equal cast literal); interval literals rejecting PG's unit
abbreviations (`1s`, `5 min`, `1d 3h`); parser gaps for `epoch`, `infinity`,
BC dates, non-padded fields and loose UTC offsets; the session `TimeZone` GUC
being ignored on both input and output (including POSIX-inverted numeric
zones and `set_config()`, which now emits ParameterStatus); `DateStyle`-aware
text rendering (German/SQL/Postgres orders); binary encoders using float
seconds (a 1µs error at year 9999) and lacking infinity sentinels; and
PG-range values beyond Python's datetime limits, now carried as text via
proleptic-Gregorian ordinal math so `'9999-12-31'::date + 1` returns
`10000-01-01` like a real server. Intervals also gained PG's justified
duration comparison (`-1 day +23:59:59.999999 = -0.000001s`).

#### Added

- `datetimes.py`: proleptic-Gregorian ordinal helpers valid outside
  [year 1, 9999], `infinity`/`-infinity`/`epoch` sentinels, wide/BC timestamp
  canonical text + binary wire values, `TimeZone`-GUC tzinfo resolution
  (POSIX sign convention, zoneinfo names), loose-input widening.
- `intervals.py`: PG unit abbreviations (`s`/`sec`/`min`/`h`/`d`/`w`/`y`/
  `ms`/`us`, attached forms like `1d`), justified `total_micros`.
- `typemap.py`: session-bound render context (TimeZone/DateStyle GUCs honoured
  at output), typed parameter carriers (`JsonText`/`DateText`/`TimeText`/
  `TimeTzText`) that substitute as casts, `json` OID aliases.
- `session.py`: case-insensitive GUC name canonicalization (`set timezone`
  hits `TimeZone`); `set_config()` on a reportable GUC emits ParameterStatus.
- `pyproject.toml`: `mypy` in the `dev` extra (psycopg's `test_typing.py`
  shells out to it).

#### Fixed

- `planner._value_to_node`: datetime / date / time / timetz / interval / json
  parameters substitute as typed casts, not bare string literals — the same
  treatment `Decimal` already had.
- `pgextended.py`: temporal text params convert per their declared OID; binary
  interval params decode to the interval subdoc; binary timestamp/date
  encoders use integer-µs arithmetic and PG's infinity sentinels.
- `scalar.py`: `'nope'::timestamp` raises `22007` instead of silently passing
  raw text into the binary encoder; `ts::text` renders through the
  session-aware renderer; mixed naive/aware datetime comparisons treat naive
  as UTC; multi-value `SET name = v1, v2` (DateStyle) is stored and reported.
- `engine.py` / `functions.py`: `client_encoding` canonicalises on every SET
  path (`utf-8` → `UTF8` in ParameterStatus).

### SQL server: range and multirange parameters become first-class values

Range-typed parameters used to arrive as raw text and never become range
values: `select 'empty'::int4range = %s` with a psycopg `Range` parameter
silently compared a subdocument against a string and returned false. A
parameter declared with a range or multirange OID (or their array forms) now
travels as tagged text and substitutes as a `::type` cast, so the existing
cast coercion turns it into the structured value. Array casts
(`'{empty,"[1,3)"}'::int4range[]`) coerce their elements, untyped literals
compared against a range value take the range's type (Postgres' context
inference), and `range::text` renders the `[a,b)` literal.

Equality itself also got Postgres semantics: range bounds store in the
subtype's canonical form regardless of construction path (a
`daterange(date, date)` constructor bound now matches the text cast's bound;
`numrange` bounds unify int / Decimal / Decimal128), and comparisons go
through a representation-independent canonical identity. psycopg's range and
multirange suites drop from 149 failing + 31 errors to 10 + 31 — the
remainder being untyped binary parameters (psycopg dumps a bound-less
`Range(empty=True)` with OID 0 in binary; needs Parse-time parameter-type
inference) and `CREATE TYPE … AS RANGE` (both recorded in `tasks/backlog.md`).

#### Added

- `ranges.canonical` / `canonical_multirange`: representation-independent
  range identity used by comparisons.
- `typemap.TaggedText`: the typed-parameter carrier for range/multirange
  (and array-of-range) declared OIDs.

#### Fixed

- `pgextended.py`: text and binary range/multirange parameters (and binary
  arrays of them) substitute as typed casts; `ParameterDescription` resolves
  an undeclared parameter to `text` like Postgres' parse analysis instead of
  echoing 0.
- `ranges.make_range`: bounds coerce to the subtype's canonical storage form.
- `scalar.py`: untyped-literal context coercion against range values;
  `::range[]` element coercion; `range::text` rendering.

### SQL server: enum result OIDs in RowDescription, and real array OIDs for user types

An enum-typed result column used to describe itself as plain `text` (OID 25),
which broke the catalog-driven type registration flow every Postgres driver
builds on: psycopg's `EnumInfo.fetch` would find the type's minted OID in
`pg_type`, but no result column ever carried it, so `register_enum` loaders
never fired and enum values always came back as bare strings. `RowDescription`
now reports the same minted OID that `pg_type` / `pg_enum` / `pg_attribute`
reflect — the mint moved onto `Catalog.enum_type_oids` so reflection and the
wire layer cannot drift — and the full psycopg round-trip works: fetch the
type, register a Python `enum.Enum`, and SELECT / RETURNING rows come back as
enum members.

Chasing the conformance numbers surfaced a second, far larger bug: every
user-declared type (enum / domain / composite) reported `pg_type.typarray = 0`.
Clients key array-type registrations on that value, and 0 is `INVALID_OID` —
psycopg's own suite pops the loader registered under `array_oid`, which
deleted psycopg's *global unknown-oid fallback loader* and poisoned every
subsequent unknown-OID text load in the process. User types now mint a derived
paired array OID (`oid + 100000`).

Enum values also flow through expressions and parameters, not just table
columns, so the cast and Bind paths grew the same fidelity: `SELECT %s::mood`
describes with the enum OID and validates the label (`22P02 invalid input
value for enum` on a label the type doesn't have), a parameter a registered
psycopg dumper declares with the enum OID is label-validated at Bind,
`oid::regtype::text` quotes mixed-case type names the way real Postgres does
(psycopg's ClientCursor pastes that string verbatim as a cast suffix), and
`%s::mood[]` round-trips as a list through the minted array OID in both text
and binary formats. psycopg's enum-adaptation suite (`tests/types/test_enum.py`,
197 tests) passes completely. On the full psycopg conformance gauge the work
takes the headline from 2554 passed (61.9%) to 2900 passed (70.3%) under
deterministic test order — +346 tests, including the entire 212-test
"unknown oid loader not found" cluster and all 152 enum failures.

#### Added

- `catalog.py`: `Catalog.enum_type_oids(db)` — the single enum-OID mint,
  shared by `pg_catalog` reflection (`virtual._enum_oids` now delegates) and
  result-column description. OIDs are **allocation-stable**: assigned from a
  persisted counter at `CREATE TYPE`, kept across `ALTER TYPE … ADD VALUE`,
  and never renumbered or reused after `DROP TYPE` — the previous positional
  mint (base + sorted-name index) shifted every enum's OID whenever a
  lexically-earlier type appeared, which would send a client's registered
  loader decoding the wrong type.
- `executor._out_column_descs`: enum-aware `(name, Column)` → `ColumnDesc`
  resolution, used by SELECT (plain, correlated), INSERT / UPDATE / DELETE /
  MERGE `RETURNING`, and extended-protocol Describe (statements and
  RETURNING).
- `virtual._pg_type`: enum / domain / composite rows carry a derived
  `typarray` (`oid + USER_TYPE_ARRAY_OID_OFFSET`) instead of 0.
- `scalar.py` / `planner.py`: casts to a declared enum (`'ok'::mood`,
  `%s::mood`, `%s::mood[]`) validate labels (`22P02`) and describe with the
  enum's OID (arrays: the paired array OID) in constant selects.
- `pgextended.py`: a Bind parameter declared with an enum OID is
  label-validated (`22P02`); binary array parameters and results handle
  user-type array OIDs (elements travel as text — an enum's wire form is its
  label).

#### Fixed

- `executor.py` / `engine.py`: enum columns in `RowDescription` report the
  enum's OID instead of 25 across the simple-SELECT, RETURNING, and Describe
  paths. JOIN / GROUP BY / evaluated-expression plans still describe enum
  outputs as `text` (their column shape drops the enum tag at plan time) —
  recorded in `tasks/backlog.md`.
- `scalar.py`: `oid::regtype::text` of a user type quotes names that need it
  (`"CamelCaseEnum"`) — an unquoted mixed-case name pasted back as a cast
  suffix folds to lowercase and misses the type.
- `virtual.user_type_oid`: `::regtype` / `to_regtype()` resolution of a
  user-declared type name now applies Postgres identifier folding — an
  unquoted part folds to lowercase (`'StrTestEnum'::regtype` finds
  `strtestenum`), a quoted part keeps its case. psycopg's
  `EnumInfo.fetch(conn, "MixedCaseName")` was returning `None`, which
  poisoned its entire enum-adaptation suite.

### Admin UI: recent-connections order is deterministic under timestamp ties

`TargetStore.recent()` ordered by `last_used_at` alone; on Windows,
`time.time()`'s ~15.6ms resolution makes back-to-back records tie, so the
recent-targets list (and the trim that caps the table) could return them in
arbitrary order — caught as a Windows-only CI flake. Both queries now break
ties by `rowid DESC` (the later insert), pinned by a regression test that
freezes the clock.

## [0.5.4b235] — 2026-07-16

### Point-in-time recovery, a SQL server with its own gauges, and operator parity across both servers

This is the largest SecantusDB release to date — a month of parallel work,
125 changelog entries. The headline capability is **point-in-time
recovery**: every write already flowed through the oplog, and
`secantusAdmin.restoreToTimestamp` now replays it to reconstruct the
database exactly as it stood at any moment inside the retention window —
with hot backup archives, base snapshots, and archives portable between the
Python and Rust servers. The other headline is housekeeping with teeth: the
daemons got distinguishable names (`secantusd-py`, `secantusd-rs`,
`secantusd-py-pg` — the old `secantusdb` console script is gone), and
Python 3.10 is now genuinely supported and genuinely tested in CI.

The PostgreSQL-wire SQL server graduated from experiment to measured
surface. It now has two external conformance gauges of its own — psycopg
3's unmodified test suite and the SQLite-originated sqllogictest corpus —
and the long tail they surfaced landed alongside them: server-side cursors
over the wire, `COPY` inside transaction blocks, `CREATE SCHEMA`,
`LANGUAGE plpgsql` function bodies, the full binary codec surface with real
Postgres type OIDs, per-statement RBAC reusing the Mongo role model, and
SQL's three-valued NULL semantics carried all the way down the
filter-pushdown path.

On the MongoDB side, both servers picked up a wide operator-fidelity batch
— the `$setWindowFields` operator set completed (`$derivative` /
`$integral` with time units, `$locf`, `$linearFill`, `$expMovingAvg`, range
windows), the N-ary accumulators, trigonometric and set expressions, a
much larger date toolbox, and dozens of exact-error-code alignments — and
the Rust server reached pymongo-suite parity with the Python server (99.5%
each). Measurement grew to match: sixteen driver-conformance gauges now run
weekly (C, C++, C#, Kotlin, pymongo async, psycopg, and the Rust-server
gates joined this cycle), feeding a regenerated cross-driver summary and a
new three-way feature-comparison page. One genuine bug that machinery
caught — an awaitData wake race that could delay change-stream delivery by
a full `maxTimeMS` — is fixed, alongside security hardening (two admin-UI
CVEs, SCRAM-credential leak paths closed, constant-time token comparison).

#### Highlights

- Point-in-time recovery: `secantusAdmin.backupArchive` /
  `archiveBaseSnapshot` / `restoreArchive` / `restoreToTimestamp` on the
  Python server; the same archives restore on the Rust server via
  `secantusd-rs restore`. See [Recovery](recovery.md).
- Daemon renames: `secantusd-py` (MongoDB wire, Python), `secantusd-rs`
  (MongoDB wire, Rust), `secantusd-py-pg` (PostgreSQL wire). The legacy
  `secantusdb` / `secantus` console scripts are removed.
- SQL server conformance gauges: `invoke validate-psycopg` (psycopg 3's own
  suite) and `invoke validate-slt` (sqllogictest corpus), with the
  wire-protocol, codec, cursor, `COPY`, schema, and plpgsql work they drove.
- Both servers: completed `$setWindowFields` operators, `$topN` /
  `$bottomN` / `$firstN` / `$lastN` / `$maxN` / `$minN` accumulators,
  `$mergeObjects` as an accumulator, trigonometric / set / bitwise
  expression operators, `$dateFromParts`, `$toDate`, timezone-aware date
  extraction, and mongod-exact error codes for unknown operators.
- Rust server catch-up to parity: index-driven `$lookup`, views, `getLog`,
  `killOp`, role grants, oplog maintenance commands, IANA timezones in
  date formatting, and the pymongo suite at 99.5% — level with the Python
  server.
- Change streams: the awaitData wake race is fixed (a write landing between
  the producer drain and the wait could stall delivery a full `maxTimeMS`);
  resume tokens advance per event even at `batchSize` 1.
- Security: two admin-UI CVEs fixed plus a stored-XSS; `admin.system.users`
  no longer leaks SCRAM credentials; constant-time secret comparison in the
  PostgreSQL SCRAM and admin-token checks.
- Conformance measurement: sixteen gauges (C, C++, C#, Kotlin, pymongo
  async, psycopg, sqllogictest, and the two Rust-server gates joined),
  weekly report + cross-driver-summary refresh, and the
  [Feature comparison](feature-comparison.md) page.
- Python 3.10 support, tested per-version in CI.
- Process: changelog fragments (`changelog.d/`) and release-time version
  assignment ended cross-PR conflicts on `docs/changelog.md` and
  `pyproject.toml`.


### Change streams: awaitData wake no longer misses a write landing mid-getMore

A tailable `getMore` baselined its awaitData wake predicate on a fresh
oplog-tail snapshot taken *after* draining the change-stream producer. A
write landing in the gap between the drain and the wait was counted into
that snapshot and never tripped the predicate — the `getMore` slept its
full `maxTimeMS` with the event already in the oplog, surfacing it only on
the post-wait re-drain. On a loaded machine that pushed delivery past the
client's await window (seen as a one-off
`test_await_data_blocks_then_wakes_on_insert` failure in the durable CI
lane). The predicate now baselines on the producer's own consumed position
(`entry.position_seq`, which the drain advances to the tail it actually
observed) — any write after that observation wakes or skips the wait,
mirroring the Rust server's `wait_for_oplog(position, ...)`, which was
never affected. A regression test pins the interleaving deterministically
by landing an insert inside the former race window. A side benefit: a
resuming cursor that drains a full filtered batch no longer sleeps its
whole `maxTimeMS` before fetching the next backlog page.

#### Fixed

- `commands.py` tailable `getMore`: wake predicate compares the oplog tail
  against `entry.position_seq` instead of a post-drain tail snapshot.

### `$bit` update applies multiple operations (both servers)

The `$bit` update operator now accepts more than one bitwise operation per field
and applies them in order, matching mongod: `{$bit: {n: {and: X, or: Y}}}`
computes `(n & X) | Y`. Both servers previously rejected any `$bit` document with
more than a single sub-operation. Found by a three-way update differential vs
real `mongod` 6.0.

#### Fixed

- `update.py` / `secantus-core`: `$bit` iterates every `and`/`or`/`xor` entry in
  the per-field document (in order) instead of requiring exactly one; an empty
  `$bit` document is still rejected, and the int32/int64 result width is preserved
  as before.

### Changelog fragments and release-time version assignment

Concurrent development got much less painful. Previously every PR edited the top
of `docs/changelog.md`'s `[Unreleased]` section and bumped the single `version`
line in `pyproject.toml` — two shared lines that made *any* two in-flight PRs
conflict, so merging one forced the others to rebase and hand-resolve the same
files. Feature PRs now add a `changelog.d/<slug>.md` fragment (one entry per
file) instead of touching `docs/changelog.md`, and they no longer bump the
Python package version at all — the version is assigned once, at release time, by
`release-prepare`. New fragment files never collide, so parallel sessions stay
independent.

#### Added

- `changelog.d/` fragment convention (`changelog.d/README.md`), a
  `changelog.fragments` collator, and an `invoke changelog-collate` task that
  folds fragments into `## [Unreleased]`. `release-prepare` runs the collation
  automatically before it stamps the version.

#### Changed

- Feature PRs no longer bump the Python `version` / `__version__` (assigned at
  release) or edit `docs/changelog.md` directly. The Rust crate version is still
  bumped per-PR (its `buildInfo` traceability handle; rare same-session-only
  collisions). See the Versioning and Conventions sections of `CLAUDE.md`.

### Date extractors error on a non-date input (both servers)

All thirteen date-component extractors — `$year` / `$month` / `$dayOfMonth` /
`$hour` / `$minute` / `$second` / `$dayOfWeek` / `$dayOfYear` / `$week` /
`$isoWeek` / `$isoDayOfWeek` / `$isoWeekYear` / `$millisecond` — now raise
mongod's `Location16006` ("can't convert from BSON type … to Date") when given a
present non-date value (a string, a number, a bool, …), instead of silently
returning `null`. A `null` or a missing field still yields `null`, as before.

#### Fixed

- `expressions.py` / `secantus-core`: the shared date-operand resolver
  (`_date_operand` / `date_operand_millis`) distinguishes a null / missing operand
  (→ null) from a present non-date value. The Python server raises `Location16006`;
  the Rust server surfaces a generic `BadValue` on that path (the documented
  error-code gap). Verified three-way vs real `mongod` 6.0 (Python zero
  divergences).

### Six more date-component extractors and `$dateToParts` ISO mode

The aggregation date toolbox picks up the components MongoDB exposes but
SecantusDB was still missing: `$dayOfYear` (1-366), `$week` (US week number,
0-53, weeks starting Sunday), `$isoWeek` (ISO-8601 week 1-53), `$isoDayOfWeek`
(1=Monday … 7=Sunday), `$isoWeekYear` (the ISO week-numbering year), and
`$millisecond`. Each slots in alongside the existing extractors and accepts the
same two shapes — a bare date expression or a `{date, timezone}` object — so a
fixed `±HH:MM` offset or a named IANA zone (`America/New_York`) shifts the
instant before the component is read. The year-boundary edge cases match
mongod: `2026-01-01` (a Thursday) is US week 0, and `2027-01-01` (a Friday) is
ISO week 53 of ISO year 2026.

`$dateToParts` now honours `iso8601: true`, returning `{isoWeekYear, isoWeek,
isoDayOfWeek, hour, minute, second, millisecond}` instead of the calendar
`{year, month, day, …}` shape. The `timezone` option applies in both modes, and
`iso8601: false` (or absent) keeps the existing output unchanged.

Both servers gain the operators together, pinned byte-for-byte by the Rust ↔
Python expression parity harness. The named-IANA-zone cases compute natively on
the Rust side via `chrono-tz`.

#### Added

- `expressions.py` / `secantus-core`: `$dayOfYear`, `$week`, `$isoWeek`,
  `$isoDayOfWeek`, `$isoWeekYear`, and `$millisecond` aggregation-expression
  operators, each supporting the `{date, timezone}` object form.
- `expressions.py` / `secantus-core`: `$dateToParts` now supports
  `iso8601: true`, emitting the ISO week-based parts document.

### Docs: three-way feature comparison (MongoDB vs Python server vs Rust server)

A new [Feature comparison](https://secantusdb.readthedocs.io/en/latest/feature-comparison.html)
docs page decomposes the validation-report pass rates into a per-feature
matrix: commands, query/update/expression operators, aggregation stages,
accumulators and window functions, index types, collections, change streams,
transactions, auth, backup/PITR, and the SQL frontend — each marked
supported / partial / missing for real `mongod`, the Python server, and the
Rust server.

#### Changed

- `docs/servers.md`: refreshed the stale "what the Rust server doesn't
  support" list — the pymongo-suite gap has closed to parity (99.5% both) and
  the DDL-change-stream-event, large-event-splitting, and timeseries-`_id`
  bullets described already-shipped features; replaced with the current gap
  set (SQL frontend, `mapReduce`/`top`, wire-level `restoreToTimestamp`,
  session lifecycle no-ops, oracle-deferred operator edges, thinner
  diagnostics). Dropped the out-of-scope claim that RBAC is unimplemented
  (both servers enforce it).
- `docs/index.md`: added the new page to the toctree and quick links, and
  included the previously-orphaned psycopg validation report in the toctree
  (it was failing the `-W` docs build as `toc.not_included`).

### `$getField` on an absent field now resolves to missing, not null

Reading a field that doesn't exist with `$getField` used to hand back an
explicit `null`. Real MongoDB treats an absent field as the *missing* value —
and a `$project` or `$addFields` computed field that resolves to missing is
omitted from the output document entirely, rather than emitted as `null`. Both
SecantusDB servers now match that: `{$project: {r: {$getField: {field: "k",
input: "$sub"}}}}` over `[{sub: {k: 1}}, {sub: {j: 2}}, {}]` yields `[{r: 1},
{}, {}]` — the documents with no `sub.k` carry no `r` field at all. A field that
is present with an explicit `null` still returns `null` and is emitted, so the
missing-vs-null distinction is preserved.

The same change makes `$$REMOVE` behave correctly as a `$project` / `$addFields`
computed value: the field is dropped instead of leaking the internal removal
sentinel.

#### Fixed

- `expressions.py` / `secantus-core`: `$getField` returns the missing/`$$REMOVE`
  marker (not `null`) for a field absent from its input; on the Rust side that
  case defers to the pure-Python engine, keeping the parity harness green.
- `aggregate.py`: `$project` and `$addFields` computed fields that evaluate to
  the missing marker are omitted from the output (an existing `$addFields`
  target set to the marker is removed), matching mongod.

### `$inc` / `$mul` on an explicit-null field now errors like mongod

Applying `$inc` or `$mul` to a field that is present with an explicit `null`
value now raises a `TypeMismatch` (error code 14), exactly as real MongoDB
does — "Cannot apply $inc to a value of non-numeric type … of non-numeric type
null". Previously both servers silently coerced the null to `0` and applied the
delta, so `{$inc: {n: 5}}` against `{n: null}` returned `{n: 5}` instead of
failing. A *missing* (absent) field is still treated as `0` and the operation
applied — that has always matched mongod and is unchanged.

The fix distinguishes an absent field from a present-but-null one: the pure-Python
engine raises the coded error directly, and the Rust core defers the null case to
the Python oracle so the exact error code is preserved (the Rust server surfaces
a generic `BadValue`, the documented error-code gap).

#### Fixed

- `update.py` / `secantus-core`: `$inc` / `$mul` on a field present with an
  explicit `null` now errors with code 14 (`TypeMismatch`) instead of coercing
  the null to `0`. A missing field is still treated as `0` and the operation
  applied.

### Docs: the Rust-server Java gauge report joins the site

`invoke validate-java --server rust` has been writing
`docs/validation-report-java-rust-server.md` — the mongo-java-driver suite
pointed at the standalone Rust server — but the report had never been
committed or added to the docs toctree. It now ships alongside the other
validation reports (445/2 passed, 99.6%; the two failures are the
`mapReduce` tests, consistent with the Rust server not implementing
`mapReduce`).

#### Fixed

- `java_validation/generate_report.py`: the generator emitted the
  Python-server title and refresh command for both servers; a
  `-rust-server` output now gets a `(Rust server)` title, the
  `--server rust` refresh command, and a note that the two-phase spawn
  boots `secantusd-rs`.

### `$jsonSchema` `uniqueItems` bridges cross-type numerics recursively (both servers)

`{$jsonSchema: {properties: {arr: {uniqueItems: true}}}}` now detects duplicate
array elements using MongoDB value equality, which treats int / long / double /
Decimal128 as equal when their values match — and does so recursively inside
sub-documents and sub-arrays. So an array like `[{a: 1}, {a: 1.0}]` is correctly
rejected (the two documents are equal), matching real `mongod` 6.0.

Previously only *top-level* scalar arrays collapsed cross-type numerics (`[1, 1.0]`
was already a duplicate); a cross-type-equal numeric nested inside a document or
array element (`[{a: 1}, {a: 1.0}]`) was wrongly treated as distinct on both
servers, because duplicate detection keyed off a raw BSON encoding that differs for
int `1` versus double `1.0`.

#### Fixed

- `query.py` / `secantus-core`: `uniqueItems` duplicate detection uses a recursive
  canonical key (`_unique_items_key` / `unique_items_key`) that normalises numerics
  to a common value form at every nesting level and recurses into sub-documents and
  sub-arrays, instead of Python structural `==` or a raw sort-key/BSON encoding.

### `$mergeObjects` as a `$group` / `$setWindowFields` accumulator

MongoDB's `$mergeObjects` was already available as a `$project` expression, but
not as an accumulator inside `$group` or `$setWindowFields`. It now is: SecantusDB
merges each group member's operand document into a single accumulated document,
with later documents' keys overriding earlier ones. A null or missing operand is
skipped, a group whose operands are all missing/null yields an empty document
`{}`, and a non-null, non-document operand raises the same `Location40400` error
mongod returns — so `{$group: {_id: "$g", merged: {$mergeObjects: "$sub"}}}` now
behaves exactly like a real server.

The accumulator ships on both the Python server and the Rust server, pinned
byte-for-byte by the aggregation parity harness.

#### Added

- `aggregate.py` / `secantus-core` (`group.rs`): `$mergeObjects` accumulator for
  `$group` and `$setWindowFields` — merge operand documents across the group
  (later keys win), skip null/missing, empty group → `{}`, non-document operand →
  `Location40400`.

### `$meta` projection faithful error codes (both servers)

`find()`'s `{field: {$meta: <arg>}}` projection now returns the same errors real
`mongod` does. A `{$meta: "textScore"}` projection without a `$text` predicate in
the query fails with `Location40218` (`query requires text score metadata, but it
is not available`), and any unrecognized `$meta` argument fails with
`Location17308` (`Unsupported argument to $meta: <arg>`). Both errors are raised
at parse time — before matching — so they fire even against an empty collection,
matching mongod. Verified against real mongod 6.0.

For a recognized-but-unsupported `$meta` keyword (`indexKey`, `recordId`,
`sortKey`, and the search/geo/vector variants) SecantusDB degrades gracefully:
rather than emitting a wrong metadata value, it omits the projected field
entirely, leaving the rest of the projection intact. Previously the Python server
mis-handled the `$meta` value as a truthy inclusion flag and the Rust server
errored generically on it.

#### Fixed

- `projection.py` / `secantus-core` / `secantus-commands`: `{$meta: "textScore"}`
  without a `$text` query raises `Location40218`, and an unknown `$meta` argument
  raises `Location17308`, on both servers with mongod's exact codes and wording.
  A recognized-but-unsupported `$meta` arg is validated clean and the field is
  omitted from the result (partial — SecantusDB doesn't compute the metadata).

### `$min` / `$max` compare by BSON order — no more traceback leak (both servers)

The `$min` and `$max` update operators now compare the incoming value against the
current field value by MongoDB's BSON canonical-type order, instead of Python's
native `<` / `>`. This fixes two bugs found by a three-way update differential
against real `mongod` 6.0:

- **A leaked traceback.** A cross-type compare — e.g. `{$max: {a: "str"}}` on a
  numeric `a` — raised a raw `TypeError` (`'>' not supported between 'str' and
  'int'`) that surfaced to the client. Now it orders like mongod: a string
  out-ranks a number, so `$max` sets `"str"`; `$max` of an ObjectId, a date, or a
  bool over a number likewise picks the higher-ranked value.
- **Explicit null treated as "no current".** An explicit-null field is a real
  value (BSON rank 2, below numbers), not an absent field. `{$min: {a: 9}}` on
  `{a: null}` now keeps `null` (null < 9); a genuinely *missing* field is still set
  unconditionally.

#### Fixed

- `update.py` / `secantus-core`: `$min`/`$max` use `ordering._bson_lt` (Python) /
  `order::cmp` (Rust) with a missing-vs-present split. The Rust engine handles the
  sortable subset (null / number / string / objectId / date / doc / array)
  natively and defers a bool / Decimal128 / NaN / exotic operand to the Python
  oracle (whose `_bson_lt` covers the full order).

### PG server: connection teardown releases the thread's WiredTiger session

Every PG connection thread that wrote data leaked its cached WT session on
disconnect (the Mongo server's teardown has always released it;
`_handle_client`'s never did). Dead threads' positioned cursors kept cache
pages pinned, and after a few hundred connections WiredTiger's eviction
livelocked — an application thread wedged in `__wt_cache_eviction_worker`
while holding the storage lock, queueing every other connection forever. The
full psycopg gauge's single-daemon run hung at ~test 420 three times out of
three; with the fix it completes in ~125s (faster than the ~550s baseline,
since sessions no longer pile up). Verified by an 8-writer-connection leak
probe (unfixed: 2 → 10 sessions; fixed: flat) pinned as a regression test.
Also: a binary/garbage COPY payload now raises SQLSTATE 22021 (invalid byte
sequence) instead of escaping as an internal error.

#### Fixed

- `pgserver.py`: `_handle_client`'s finally releases the thread's WT session
  and cached cursors via `Storage._reset_thread_session()`, mirroring the
  Mongo server; `_copy_in` guards `decode_text` with a faithful 22021.
- psycopg gauge headline after the day's slices (COPY transactionality,
  CREATE SCHEMA, server-side cursors, this fix), on the standard
  single-daemon protocol: **2554 passed / 61.9%**, up from 2465 / 59.8% —
  report refreshed.

### Positional `$` projection (both servers)

`find()`'s positional projection operator now works on both servers:
`find({"items.k": "b"}, {"items.$": 1})` returns only the **first array element
that matched the query** on that path — `items: [{k: "b", …}]` — instead of the
whole array stripped to empty documents, which is what both servers previously
produced. The matched element is resolved from the query's clause on the array
(a dotted `items.sub` field, a direct value/range on `items`, or an
`items: {$elemMatch: …}`), so it works for arrays of documents and arrays of
scalars alike. Found by a three-way projection differential against real
`mongod` 6.0; all value cases match exactly.

#### Fixed

- `projection.py` / `secantus-core`: the positional `$` projection resolves and
  returns the first query-matched array element. The find command threads the
  filter into the projection engine so the operator has the query context it
  needs. Validation is parse-time (matching mongod), so an invalid positional —
  more than one (`Location31276`), an exclusion form (`Location31395`), or an
  array field the query doesn't reference (`Location51246`) — errors even when the
  query matches nothing. The Python server reproduces mongod's exact Location
  codes; the Rust server surfaces a generic `BadValue` on these error paths (the
  documented cross-cutting error-code gap). (`$meta` projection remains deferred —
  `tasks/backlog.md` §7.5.)

### The psycopg conformance gauge: SecantusDB's SQL server gets its headline number

The SQL server now has what the Mongo server has had for a year: an
external conformance gauge running a real driver's own unmodified test
suite. `invoke validate-psycopg` vendors psycopg 3.3.4 (pinned in lockstep
with the `dev`-extra wheel), spawns a `SecantusPGServer` daemon on an
ephemeral port, verifies it actually is SecantusDB (a stray real Postgres
would inflate the numbers), runs the full sync half of psycopg's suite over
`PSYCOPG_TEST_DSN`, and renders `docs/validation-report-psycopg.md` with
the per-file pass/fail/skip breakdown. It joins the weekly `validate.yml`
matrix as the fourteenth gauge — and the first for the SQL side. The
opening baseline over the full sync suite is 2415 passed of ~4100 run
(58.6%); the six-file subset that drove this month's conformance work
stands at 91%.

#### Added

- `psycopg_validation/` (runner, include list, report generator),
  `invoke validate-psycopg`, a `psycopg` lane in `validate.yml`, and the
  `vendor/psycopg` submodule @ 3.3.4. `psycopg[binary]` is now pinned
  exactly so the vendored suite and the installed wheel stay in lockstep.

### Python 3.10 actually works — and CI actually tests it

The CI test matrix's `python-version` never took effect: `uv sync` honours the
repo's `.python-version` pin (3.12), so every matrix cell — including the
scheduled 3.10–3.13 sweep — was silently testing 3.12. With the interpreter
genuinely pinned per cell (a job-level `UV_PYTHON`, which outranks the pin file
for every `uv` invocation in the job), the first real 3.10 run surfaced three
breakers that the gap had been hiding, all now fixed: the config loader's
module-level `tomllib` import (stdlib only from 3.11) crashed
`secantus.config` / the `secantusd-py` CLI on 3.10; `datetime.UTC` (a 3.11+
alias) in fifteen test call sites; and `datetime.fromisoformat` on 3.10
rejecting Postgres's short UTC offsets (`+00` / `+0000`), which PG text
rendering emits and timestamptz literals carry.

#### Fixed

- `config.py`: fall back to the API-identical `tomli` backport on Python 3.10
  (`tomli>=2.0; python_version < '3.11'` added to the core dependencies).
- `sql/datetimes.py`: new `parse_iso_datetime` — `fromisoformat` fast path
  (a no-op passthrough on 3.11+) that widens a trailing short UTC offset to
  `+HH:MM` only on failure; wired into `scalar._as_datetime`, `intervals`,
  and both `typemap.coerce` timestamp branches.
- `.github/workflows/test.yml`: the three matrix jobs set a job-level
  `UV_PYTHON: ${{ matrix.python-version }}` so `uv sync` and every `uv run`
  agree on the matrix interpreter (a sync-only `--python` flag is not enough —
  a later bare `uv run` re-resolves against `.python-version` and recreates
  the venv without the dev extras).
- Tests: `datetime.UTC` → `datetime.timezone.utc` in
  `test_indexes` / `test_expressions` / `test_crud`.

### Range operators are type-bracketed, matching mongod

MongoDB's range operators (`$gt` / `$gte` / `$lt` / `$lte`) are *type-bracketed*:
a scalar bound only ever matches values in the same BSON type bracket. SecantusDB
now honours that on both the Python and the Rust server, closing two divergences
that a three-way probe against real `mongod` surfaced.

A document-valued (or array-of-documents) field no longer errors on the Rust
server when compared against a scalar bound — `{a: {$gt: 2}}` against a
document-valued `a`, and `{items: {$elemMatch: {$gt: n}}}` over an array of
sub-documents, now cleanly no-match (as they always did on the Python server and
on `mongod`) instead of the Rust server returning a `BadValue`. And **bool is its
own bracket**: a boolean-valued field no longer spuriously matches a numeric bound
(Python's `bool` is an `int` subclass, so `True < 2` used to match on both
engines), while `bool`-vs-`bool` comparisons (`True > False`) still work. Both the
collection-scan and index-scan paths agree with `mongod` on every case.

#### Fixed

- Range operators (`$gt`/`$gte`/`$lt`/`$lte`) are now type-bracketed on both
  servers. A document/array operand against a scalar bound no-matches instead of
  erroring on the Rust server; a boolean field no longer matches a numeric bound
  (bool compares only with bool). Verified against real `mongod` 6.0 with a
  three-way probe (collection-scan and index-scan paths both).

### Rust server compares array-vs-array range bounds lexicographically

A range query whose bound is an array — `{a: {$gt: [1, 2]}}` — now evaluates on
the Rust server instead of erroring. The Rust matcher previously deferred any
array operand to a `Fallback`, which the Rust server surfaced as a `BadValue`;
it now compares the two arrays **whole-array lexicographically**, exactly as the
Python server (via Python's native `list < list`) and real `mongod` do.

The comparison recurses element-by-element: the first decisive element pair wins,
equal leading elements continue to the next pair, and if one array is a prefix of
the other the shorter one sorts first. A cross-type element pair (where Python's
`<` would raise `TypeError`) yields a clean no-match rather than an error, and an
array field compared against a *scalar* bound still rides the multikey element
path (`{a: [1, 3]}` matches `{a: {$gt: 2}}` because `3 > 2`). Only the exotic BSON
types (JS code / symbol / dbpointer / undefined) as a range operand still defer to
the Python engine.

Verified against real `mongod` 6.0 and pinned to the Python oracle by new curated
parity cases and Rust unit tests.

#### Fixed

- Rust server: `$gt` / `$gte` / `$lt` / `$lte` with an **array bound** (e.g.
  `{a: {$gt: [1, 2]}}`) now compares whole-array lexicographically instead of
  returning `BadValue`, matching the Python server and `mongod`. Array-vs-scalar
  bounds continue to match via the multikey element path; a cross-type element
  pair no-matches cleanly.

### `$log10` now evaluates natively on the Rust server

The `$log10` aggregation-expression operator is now computed natively by the Rust
engine, so the Rust server evaluates it instead of rejecting the pipeline with a
`BadValue`. The rest of the transcendental family (`$exp` / `$ln` / `$log`) was
already native; `$log10` had simply been left out. Rust's `f64::log10` and
CPython's `math.log10` share the platform libm, so the two servers agree
bit-for-bit (pinned by the expression parity corpus). Found by a three-way
differential sweep against real mongod 6.0.

#### Fixed

- `$log10` is evaluated by the Rust `secantus-core` expression engine (was a
  Fallback → `BadValue` on the Rust server). Matches the Python server and mongod
  for positive inputs; a non-positive input yields `null` on both servers (see
  `tasks/backlog.md` §7 for the pre-existing log-domain divergence from mongod).

### Tooling: the sqllogictest conformance gauge (invoke validate-slt)

The SQL server gets its correctness gauge (tasks/sql-gauges-plan.md G1): the
SQLite-originated sqllogictest corpus — 622 files, millions of records — is
vendored pristine at `vendor/sqllogictest` and executed by sqllogictest-rs
over real pgwire, one fresh `SecantusPGServer` daemon per file. A
preprocessing pass (never touching the vendored tree) bridges the three
corpus/runner incompatibilities established empirically: trailing comments on
`skipif`/`onlyif` lines, value-per-line expected blocks for
`nosort`/`rowsort` multi-column records, and sqlite's implicit
`hash-threshold 8` default. The curated 30-file include list currently
passes 26/30 end-to-end; the 4 failures are declared
`EXPECTED_DIVERGENCES` (SQLite read-only views, SQLite's
division-by-zero→NULL vs PG's 22012, and the runner's missing `query I`
type coercion), so the gauge is green in its own terms and reports loudly if
a divergence resolves.

#### Added

- `vendor/sqllogictest` (shallow submodule, dev-only, excluded from
  sdist/wheel), the `slt_validation/` gauge package (preprocessor, per-file
  daemon runner with identity verification, report generator, include +
  expected-divergence lists), the `invoke validate-slt` task, and
  `docs/validation-report-slt.md` in the Sphinx toctree. Requires the
  `sqllogictest` binary (`cargo install sqllogictest-bin`).
- `pyproject.toml`: sdist excludes for `vendor/sqllogictest` /
  `slt_validation` — and the previously-missing `vendor/psycopg` /
  `psycopg_validation` entries.

### SQL: COPY runs inside the open transaction block

The COPY sub-protocol handler never entered the session's user transaction:
`COPY` after a same-block `CREATE TABLE` failed with `UndefinedTable`
(psycopg's standard fixture shape, cascading through ~190 of its COPY-backed
tests), `COPY TO STDOUT` couldn't see rows inserted earlier in the block —
and worst, `COPY FROM STDIN` rows were written *outside* the transaction, so
they survived a `ROLLBACK`. Plan resolution, the copy-in insert, and the
copy-out extract now all run under `use_user_transaction` when a block is
open, and a failed COPY marks the block aborted like Postgres does.

#### Fixed

- `pgserver.py`: `_handle_copy` / `_copy_in` / `_copy_out` wrap their engine
  calls in the session's open user transaction (no-op outside a block);
  a COPY error inside a block sets `txn_failed`. The three copy-heavy psycopg
  suites (test_copy / test_range / test_multirange) move 230 → 374 passing.

### SQL server: bare COPY options and computed projections over row sources

Two more gauge-driven fixes. `COPY … TO STDOUT (FORMAT csv)` — the
options spelling psycopg emits, without `WITH` — now parses (sqlglot only
accepts the `WITH (…)` form, so `parse()` inserts it, anchored on the
STDIN/STDOUT target and a known option keyword). And projections that
compute over a set-returning or catalog row source — `SELECT x * 2 FROM
generate_series(1,3) AS t(x)`, `SELECT 1 FROM pg_namespace` — run through
the per-row evaluated plan instead of failing with "expected a column".
The psycopg-gauge subset stands at 685 of 979 (70%), from 42% at the
first run.

#### Fixed

- `planner.py`: `COPY … TO STDOUT/FROM STDIN (options)` normalizes to the
  `WITH (options)` spelling sqlglot parses; both the table and the query
  form take options.
- `engine.py`: SRF and virtual-catalog row sources route computed
  projections (arithmetic, literals, scalar functions) through the
  evaluated-select plan — execution and Describe agree on the shape.

### SQL: CREATE SCHEMA and schema-qualified user types

`CREATE SCHEMA [IF NOT EXISTS]` / `DROP SCHEMA [IF EXISTS] [CASCADE]` land,
with user-declared types (enum / domain / composite) creatable and droppable
under a schema (`CREATE TYPE testschema.testcomp AS (…)`). Qualified names
resolve everywhere psycopg's type machinery needs them: `to_regtype`, the
`'schema.name'::regtype` literal cast (previously an internal error — the
pushdown's cast coercion knew `regclass` but not `regtype`), `oid::regtype`
rendering, and `TypeInfo`/`CompositeInfo` fetches by dotted string or
`sql.Identifier` spelling. `pg_namespace` carries user schemas with minted
oids and `pg_type` reports the bare `typname` under the schema's
`typnamespace`. Dropping a non-empty schema without CASCADE is a 2BP01
dependency error, CASCADE drops the contained types, and `DROP TYPE IF
EXISTS` tolerates a missing schema. This clears the psycopg gauge's entire
"CREATE SCHEMA is not supported" cluster and unblocks the schema-gated
composite/range/typeinfo fixtures. (Schema-qualified *tables* remain 0A000 —
`tasks/backlog.md`; user-defined `CREATE TYPE … AS RANGE` likewise.)

#### Added

- `catalog.py`: schema registry (`create_schema` / `schema_exists` /
  `drop_schema` / `list_schemas`); `engine.py`: `CREATE`/`DROP SCHEMA`
  routing, qualified-name extraction for `CREATE`/`DROP TYPE`;
  `virtual.py`: user-schema `pg_namespace` rows, dotted-name splitting in
  `pg_type`, quote-normalized qualified lookups; `planner.py`: the
  `::regtype` literal cast resolves built-ins and user types (42704 on
  unknown, like PG).

### SQL server: arbitrary WHERE expressions and three-valued logic

The sqllogictest random corpus writes SQL the way a fuzzer does — `WHERE
- col2 + col1 IS NOT NULL`, `WHERE 1 IN (2)`, `SELECT + + 90 * a * - b` —
and the planner used to reject anything its Mongo-filter pushdown couldn't
express. Untranslatable WHERE clauses now route to per-row evaluation
automatically (a dry-run of the lowering decides), computed unary
projections type correctly instead of crashing tag inference, `ORDER BY
<ordinal>` resolves to the output expression on the evaluated path, and the
scalar evaluator's NOT/AND/OR/BETWEEN implement SQL's three-valued logic
(`NOT NULL` is NULL, `NULL AND FALSE` is FALSE — visible under NOT).

#### Fixed

- `planner.py`: `where_needs_per_row` dry-runs the pushdown lowering and
  falls back to per-row evaluation when it raises; the DISTINCT plan path
  consults it too; `_infer_scalar_tag` types `- col` from its operand;
  `ORDER BY 1` resolves the output ordinal (except SRF outputs, which sort
  post-expansion).
- `scalar.py`: three-valued NOT/AND/OR and a decomposed BETWEEN whose
  definitively-false arm dominates a NULL bound.
- A predicate the pushdown can't lower no longer errors 0A000; cross-type
  comparisons under per-row evaluation match nothing instead of raising
  Postgres' 42883 — a documented divergence (`tasks/backlog.md`).

### SQL server: client_encoding, wire-protocol fixes, and binary-format hardening from the psycopg gauge

Running psycopg 3's own unmodified test suite against the SQL server
(`tasks/sql-gauges-plan.md`) surfaced a batch of wire-protocol and
type-handling divergences beyond the type-OID work. The headline is
`client_encoding` support: the server now honours the startup parameter and
`SET client_encoding` (LATIN1/LATIN2/LATIN5/LATIN9, WIN1250-1252,
SQL_ASCII pass-through), converting query text, text and binary parameters,
text and binary results, arrays, COPY data, and error messages at the wire
boundary while the engine stays UTF-8 throughout. Alongside it, a real
protocol-ordering bug: Describe answered NoData for DML with RETURNING while
Execute then emitted DataRows — a violation that crashed psycopg's pipelined
`executemany`. The measured effect on the fixed psycopg-gauge subset
(six files, psycopg 3.3.4): 409 → 637 passed of 979 (42% → 65%) across this
and the preceding type-OID release.

#### Added

- `client_encoding` (startup parameter and `SET`, with canonical
  ParameterStatus reporting and `22023` on unknown encodings); an
  untranslatable result character raises `22P05` like Postgres instead of
  degrading to `?`, and a NUL byte in a text parameter is rejected with
  `22021`.
- Quoted built-in type names in DDL (`CREATE TABLE t (c "cidr")`, the form
  psycopg's fixtures emit via `sql.Identifier`) resolve as built-ins —
  including array spellings — instead of failing as undeclared enums.

#### Fixed

- `pgextended.py`: Describe on INSERT/UPDATE/DELETE/MERGE … RETURNING
  answers with the RETURNING columns' RowDescription (was NoData followed by
  DataRows — a protocol violation).
- `engine.py`: Describe on a set-returning row source (`FROM
  generate_series(…)` / bare `SELECT generate_series(…)`) resolves the
  result shape instead of erroring — this is what failed every
  `cursor.stream()` (libpq single-row mode) call.
- Array round-trips, all six param/result format combinations: a binary
  array parameter's Python list is rendered as a Postgres array literal
  (was the Python `repr`); the array-literal parser strips only Postgres'
  whitespace set (`\x1c`–`\x1f` are `str.isspace()` to Python but data to
  Postgres); the renderer quotes every whitespace character; binary array
  elements coerce to native values (`bytea` hex, `bool` `'t'/'f'`) before
  encoding. chr(1)–chr(255) plus `€` now round-trip byte-exact in text and
  bytea arrays.
- Binary `numeric` handles `±Infinity` in both directions (signs
  `0xD000`/`0xF000`; encoding previously crashed, decoding produced
  garbage).

### SQL server: oid/regtype, declared-parameter typing, and the full binary codec surface

Three parallel work streams off the psycopg gauge, landing together. The
`oid` type (26, arrays 1028) is now first-class — columns, casts, binary
codecs, `pg_type` rows — and `21::regtype` resolves an OID to its type name
the way Postgres does. Parameter typing got the same discipline on every
path: the OID a client declares in Parse now governs the value whether it
arrives in text or binary format, `'19.99'::numeric`-style scalar casts
convert instead of passing strings through, and Execute encodes DataRows
with the same column OIDs Describe reported (the mismatch fed text bytes to
clients parsing binary numerics). And the binary result/parameter codec
surface now covers what psycopg's full-type faker exercises: time, timetz,
interval, uuid, inet, cidr, macaddr, json, and every range and multirange
type — including new tstzrange/tstzmultirange registration, PG-exact
multirange rendering, JSON integers beyond int64, and Decimal128-safe
numeric handling at any width. psycopg's `test_leak` (the full-type
CRUD matrix) went from 72 failures to 72 passes; the six-file gauge subset
stands at 887 of 979 (91%), from 42% at the first external run.

#### Added

- `oid` type end-to-end; `N::regtype` OID resolution (42704 on unknown
  OIDs); `pg_typeof(x)::oid` resolves to the type's OID.
- Binary codecs (both directions) for time/timetz/interval/uuid/inet/cidr/
  macaddr/json and all range/multirange types; tstzrange/tstzmultirange
  types; `oid[]`/`json[]`/multirange array OIDs.

#### Fixed

- Execute now applies the same declared-parameter OID overrides to its
  DataRow encoding that Describe applies to RowDescription — the divergence
  sent int4/text bytes in fields announced as int2/numeric.
- Text-format parameters with a declared scalar OID convert to the native
  type (declared type governs, matching the binary twin; garbage raises
  22P02); scalar casts to int/float/numeric/bool convert with PG rounding
  semantics.
- Binary numeric survives values wider than Python's default 28-digit
  context (wide-context decode, context-free negate/abs); >34-digit
  numerics round into Decimal128 range instead of erroring on INSERT.
- Numeric/bytes/±inf parameters keep their types through statement binding
  (typed cast nodes / hex literals instead of bare string literals).
- Multirange text rendering drops the ", " separator Postgres doesn't
  print; daterange bounds render date-only; bool coercion of 'f'/'false'
  strings; JSON top-level scalars render as JSON.

### SQL: HAVING IS NULL forms, constant JOIN ON, duplicate join group keys

Round four of the sqllogictest corpus tail. `HAVING <operand> IS [NOT] NULL`
now lowers for bare-column, aggregate, and computed-over-group-key operands
(`HAVING (- col2) IS NOT NULL`) on both the single-table and join HAVING
lowerers. A constant JOIN ON condition (`LEFT JOIN tab0 ON 80 = 70`) folds
three-valued — TRUE joins every foreign row, FALSE/unknown joins none (INNER
drops the row, LEFT null-pads). And two join GROUP BY wrong-answer bugs: the
same bare column name grouped from two aliases (`GROUP BY cor1.col1,
cor0.col1`) collapsed to a single group key, and `SELECT DISTINCT` over
grouped join output never deduplicated.

#### Fixed

- `planner.py`: `_having_to_match` / `_join_having_to_match` lower
  `IS [NOT] NULL` over bare columns, aggregates, and computed group-key
  expressions (the last via `_to_agg_expr` over a group-key resolver, correct
  through any NOT nesting); `[NOT] <expr> IN (<exprs over group keys>)`
  lowers three-valued; always-unknown NULL-operand predicates
  (`HAVING NOT NULL IN (- col1)`, `NOT NULL NOT BETWEEN - col0 AND NULL`)
  fold to match-nothing; `_to_agg_expr` learns unary minus over non-literals.
- `planner.py`: an always-unknown JOIN ON (`ON NOT NULL < expr`) folds like a
  constant-false ON instead of raising.
- `planner.py`: `_lookup_stage` folds a constant ON via
  `_constant_predicate_filter` instead of raising "ON must compare columns".
- `planner.py`: duplicate bare column names in a join GROUP BY mint distinct
  grouped fields on both the join-group and join-group-window paths
  (qualified references rewrite/resolve onto the minted key); grouped
  `SELECT DISTINCT` over a join dedups with the same second `$group` the
  single-table planner uses.

### SQL server: join-path aggregate expressions and WHERE residuals

The JOIN planners catch up with the single-table paths from the last two
rounds: aggregate arguments over joins can be expressions
(`MAX(cor0.col0 + 1)`, `SUM(- 83)` over a CROSS JOIN), lowered through the
join resolver with identity decorations stripped, and a join WHERE the
`$match` lowering can't express routes to the per-row residual the join
pipelines already carry (a dry-run probe, the join twin of the
single-table one) instead of erroring.

#### Fixed

- `planner.py`: computed-over-aggregate outputs over a join
  (`COUNT(*) * 32 FROM a CROSS JOIN b`) route to the group-then-evaluate
  builder instead of failing per-row; `_join_accumulator` lowers
  expression arguments for
  sum/avg/min/max; `_agg_key` identifies expression aggregates by SQL text
  instead of crashing the resolver; `_join_where_lowerable` dry-runs
  `_expr_to_filter` and the inner/outer join builders plus both join
  residual sites consult it.

### SQL server: RowDescription reports real Postgres type OIDs for computed columns

A libpq client keys its result decoding off the type OID in each
`RowDescription` column, and SecantusDB's SQL server used to fall back to
`text` (25) for most computed results — `CASE` expressions, `array[...]`
constructors, array casts, integer arithmetic, bound parameters — and widened
`smallint`/`real` to `integer`/`double precision` everywhere. The first
external-gauge run (psycopg 3's own test suite plus the sqllogictest corpus,
see `tasks/sql-gauges-plan.md` §6) flagged this as the single
highest-leverage divergence. Computed and derived columns now describe with
the OID real Postgres would use, so typed loaders in psycopg / pg8000 /
SQLAlchemy decode results without special-casing.

#### Added

- `pg_typeof()` and `'name'::regtype`: the type-introspection pair psycopg's
  type suite leans on (`select pg_typeof(%s::int2) = 'smallint'::regtype`).
  `pg_typeof` resolves at plan time from the same static inference that types
  RowDescription; `::regtype` normalizes any accepted spelling (`int4`,
  `varchar`, `float4`) to the canonical pretty form `pg_typeof` prints.
- `typemap.py`: first-class `int2` (21) and `float4` (700) type tags —
  `smallint` / `real` columns, casts, arrays (`1005` / `1021`), catalog
  `pg_type` rows, and `information_schema` spellings; `SMALLSERIAL` columns
  now describe as `int2` instead of `text`.

#### Fixed

- `planner.py`: type inference for computed SELECT columns — `CASE` types
  from its result branches; `array[...]` and array casts report the array
  OID; integer arithmetic stays integer (`int + int` → `int4`, matching
  `_pg_div`'s truncating division) instead of `numeric`; an unadorned
  decimal constant (`SELECT 1.5`) is `numeric`, matching Postgres;
  `sum(int2/int4)` → `int8`, `sum(int8)` → `numeric`, `avg(integer)` →
  `numeric` per Postgres' aggregate result types; `CAST($1 AS SMALLINT)`
  coerces its text-bound value numerically.
- `pgextended.py`: `SELECT $1` describes with the parameter OID the client
  declared in Parse (psycopg binds a small Python int as `int2`), instead of
  re-inferring from the substituted Python value.
- `pgextended.py`: binary result format and binary parameters now cover
  arrays (the real ndim/hasnull/elemoid wire layout, both directions). The
  correct array OIDs engage a libpq client's binary array parser, which the
  text-bytes fallback would have fed garbage.

### SQL: server-side cursors over the wire, pg_cursors, pg_prepared_statements

psycopg's `ServerCursor` works end-to-end. A `DECLARE`d cursor is a portal in
the v3 protocol, and psycopg's first move after the DECLARE is a wire
`Describe('P', name)` — which our extended-protocol session answered with
`34000 portal does not exist`. The portal Describe (and Close) now fall back
to the session's DECLAREd cursors, parameterized declarations substitute
their `$N` placeholders inside the raw `DECLARE … FOR SELECT $1` command
text, and the session's cursors and prepared statements surface in new
`pg_cursors` / `pg_prepared_statements` catalog tables. psycopg's
test_cursor_server + test_prepared move 26 → 102 passing.

#### Added

- `pg_catalog.pg_cursors` (name / statement / is_holdable / is_binary /
  is_scrollable / creation_time, from the session's open cursors) and
  `pg_catalog.pg_prepared_statements` (SQL-level `PREPARE`d plus the
  connection's wire-Parse statements, exposed via `Session.wire_prepared`).

#### Fixed

- `pgextended.py`: `Describe('P', name)` on a DECLAREd cursor returns its
  RowDescription; `Close('P', name)` destroys the cursor.
- `planner.py`: `substitute_parameters` also substitutes `$N` textually
  inside a raw `exp.Command` tail (DECLARE bodies aren't parsed trees).

### SQL: three-valued NULL semantics on the pushdown, and the aggregate long tail

The SQL server's Mongo-filter pushdown now honours SQL's three-valued logic: `<>`,
`NOT (...)`, `NOT BETWEEN`, and `NOT IN` no longer match rows whose operand column
is NULL (Mongo's `$ne`/`$nor`/`$nin` are two-valued and matched them), a NULL
candidate in an `IN` list can no longer match a NULL row, and `x NOT IN (…, NULL)`
correctly matches nothing. `SUM` over zero non-null inputs returns NULL instead of
Mongo's 0, on every plan path. Alongside, a round of sqllogictest-corpus aggregate
and planner shapes: FROM-less aggregates (`SELECT COUNT(*)` is 1), `COUNT(<expr>)`
counting non-null evaluations, expression `DISTINCT` aggregate arguments
(`SUM(DISTINCT 77)`), computed and constant projections under GROUP BY, `SELECT *`
grouped by every column, `SELECT DISTINCT` over grouped output, parenthesized join
sources (`FROM (a CROSS JOIN b)`), constant-LHS `IN` (list and subquery forms),
division by zero raising SQLSTATE 22012, and Postgres-exact `float8` wire text
(`12`, not `12.0`; `NaN`/`Infinity` spellings). Three files of the corpus's
`random/` suites now pass end-to-end that previously failed on their first record.

#### Fixed

- `planner.py`: `_negated_filter` lowers `NOT` by pushing the negation into the
  tree (De Morgan, comparison-operator flips, null-guarded single-field fallback)
  instead of Mongo's two-valued `$nor`; `<>` is null-guarded; `$in` lists drop
  NULL candidates; constant-LHS `IN`/`NOT IN` fold three-valued (list + subquery);
  a NULL comparison operand folds to match-nothing even when wrapped
  (`51 <> (NULL)`, `- CAST(NULL AS INT) <> x`); computed comparisons lowered to
  `$expr` guard both sides non-null (BSON total order is two-valued —
  `NULL <> 19` matched every row).
- `planner.py`: a join WHERE the `$match` lowering can't express routes to the
  per-row evaluated join / the pre-group residual instead of being silently
  dropped, on both the plain-join and the join-group-window paths
  (`WHERE (NULL) BETWEEN NULL AND NULL` returned every row).
- `planner.py`: two *different* expression aggregates of the same function
  (`MAX(3)` and `MAX(-94 - -16)`) no longer collide on the `(func, None)`
  accumulator-dedup key and share one value; integer `/` inside aggregate
  arguments and `$expr` lowers with PG's truncate-toward-zero semantics
  (`MIN(col1 / -99)` was computed with real division).
- `planner.py` / `executor.py`: `SUM` over only-NULL inputs is NULL on the plain
  group, group-window, join, join-window, and DISTINCT paths; the evaluated group
  path synthesizes the one implicit-aggregate row over empty input like the
  pipeline path already did.
- `planner.py`: FROM-less SELECTs fold aggregates over their one implicit row;
  `COUNT(<literal>)` no longer misroutes to the lone-`COUNT(*)` fast path;
  `COUNT(<expr>)` counts non-null evaluations (`COUNT(NULL)` is 0).
- `planner.py`: expression `DISTINCT` aggregate arguments push the lowered
  expression into the distinct set (single-table, group-window, join, join-window
  registrars); computed-over-aggregate outputs over a JOIN route to the
  group-then-evaluate builder (`COUNT(*) * COUNT(*)`).
- `planner.py`: grouped SELECTs with computed/constant projections route to the
  evaluated group path; `SELECT *` under GROUP BY expands when every column is a
  group key; `SELECT DISTINCT` over grouped output dedups; `ORDER BY <ordinal>`
  resolves on the group-then-evaluate path.
- `planner.py`: `FROM (a CROSS JOIN b)` unwraps grouping parens instead of
  erroring "a derived table requires an alias".
- `scalar.py`: division / modulo by zero raise SQLSTATE 22012 instead of leaking
  an internal error; `COALESCE` evaluates lazily like Postgres, so a
  division-by-zero in a never-reached argument no longer raises; operand-form
  `CASE x WHEN v` uses SQL equality (a NULL operand or WHEN value never
  matches, where Python `==` matched NULL to NULL).
- `planner.py` / `executor.py`: a constant `HAVING` (``HAVING NOT NULL IS
  NULL``) folds three-valued to match-all / match-nothing; DISTINCT aggregates
  over zero input rows synthesize their NULL row instead of crashing on the
  ``$addToSet`` reduction ("$size requires an array").
- `typemap.py`: `float8` text output uses Postgres' shortest form (`12`, `-0`,
  `1e+20`, `NaN`, `Infinity`).
- `planner.py`: `_infer_scalar_tag` is memoized per statement — deep arithmetic
  chains were exponential (a 20-term sqllogictest expression took ~0.5s; whole
  corpus files timed out).

### SQL: psycopg TypeInfo catalog fidelity (typarray, pg_range, to_regtype)

psycopg's type-registration machinery works end-to-end: `TypeInfo.fetch`,
`RangeInfo.fetch`, `MultirangeInfo.fetch`, `EnumInfo.fetch` (with labels),
and `CompositeInfo.fetch` (with field names) all resolve against the virtual
catalog. `pg_type` gains `typarray` / `typdelim`, a `pg_range` table maps
range oids to their declared subtype and multirange oids, `to_regtype()` is
implemented (built-ins and user-declared enum/domain/composite types,
returning NULL for unknown names), and `oid::regtype::text` renders
user-declared type names. Catalog-table WHEREs that can't lower now evaluate
per-row with the real catalog in scope, and a context-dependent function call
(`to_regtype('mood')`) is no longer folded as if it were a NULL literal.

#### Added

- `pg_type.typarray` / `typdelim` columns; the `pg_catalog.pg_range` virtual
  table (`rngtypid` / `rngsubtype` / `rngmultitypid`, declared subtypes —
  `tsrange` advertises `timestamp`, `daterange` advertises `date`);
  `to_regtype(name)` (scalar + FROM-less + pushdown-constant paths).

#### Fixed

- `oid::regtype` on a user-declared type's oid resolves its name through the
  catalog instead of raising 42704.
- The catalog-table fast path publishes the planning subquery context and
  routes non-lowerable WHEREs through per-row evaluation with the real
  catalog (a synthetic catalog over the row backend knew no user types).
- The NULL-operand comparison folds no longer treat an `Anonymous` function
  call as a NULL literal (`WHERE t.oid = to_regtype('mood')` matched nothing).

### Docs: compatibility / authentication / index pages caught up with shipped features

Three docs pages still described the server as it was several releases ago.
`compatibility.md`'s stub table claimed `getLog` returns an empty array,
`hostInfo` / `whatsmyuri` / `buildInfo` are hardcoded, sessions are untracked,
and `serverStatus` is all zeros — all of those return real data now, so the
table shrinks to the honest remainder (`top`'s zero counters, `buildInfo`'s
deliberate `7.0.0` compatibility identity, `connectionStatus`'s empty
privileges expansion, `serverStatus`'s zeroed fallback for bare
`CommandContext` embedders). The `$lookup` stopgap section described the
pre-index-join hash-only implementation; the date-format section listed
ISO-week tokens as missing; the TTL-index row said there was no background
sweeper. All rewritten to match the code.

`authentication.md` and `index.md` both still said authorization (RBAC) is
not implemented and that an authenticated principal is fully privileged —
RBAC has been enforced for a while (built-in and custom roles, checked on
every command when `--auth` is on). `authentication.md` gains an
Authorization section documenting the enforcement model, the built-in role
list, and the custom-role / grant-revoke command set; both scope lists now
credit SCRAM-SHA-1 and MONGODB-X509 correctly.

#### Changed

- `docs/compatibility.md`: stub table rewritten to current behaviour;
  `$lookup` section describes the index-driven join (IXSCAN on a matching
  foreign-field index, hash-join fallback); date-format token list updated
  (`%G %V %j %U %u %w` all supported); TTL row documents the 60-second
  background sweeper; out-of-scope auth bullet updated (SCRAM-SHA-1
  implemented, RBAC enforced); Rust-server note updated to conformance
  parity with a pointer to the feature comparison.
- `docs/authentication.md`: RBAC documented as enforced (new Authorization
  section: built-in roles, custom roles, grant/revoke quartet, code-13
  behaviour); `createUser` example uses a real role binding.
- `docs/index.md`: in-scope and out-of-scope auth bullets updated to
  SCRAM (SHA-1/SHA-256) + MONGODB-X509 + enforced RBAC.

### `$toDate` conversion expression on both servers

The `$toDate` aggregation expression now works on the pure-Python and Rust
servers. `$toDate: <expr>` is the shorthand for `$convert: {input: <expr>, to:
"date"}`, and SecantusDB implements it as exactly that — a date is returned
unchanged, an int/long/double is read as milliseconds since the Unix epoch, and
an ISO-8601 string is parsed, while `null` or a missing field yields `null`.

Because `$toDate` delegates straight to the existing `$convert`-to-date path, it
inherits precisely the same supported inputs and errors: whatever `$convert` can
turn into a date, so can `$toDate`, with no separate conversion code to drift.
The Rust engine's `$convert`-to-date was also widened to convert an int / long /
double (epoch milliseconds) to a date natively, so both `$convert` and `$toDate`
now compute the numeric case on the Rust server rather than deferring; ISO-string
and ObjectId inputs still defer to the Python oracle (matching `$dateFromString`'s
partial Rust support). The two engines stay byte-for-byte in step (pinned by the
expression parity harness).

#### Added

- `expressions.py` / `secantus-core`: `$toDate` aggregation expression operator,
  delegating to the existing `$convert`-to-date conversion; the Rust
  `$convert`-to-date path gains native int/long/double → epoch-millis conversion.

### Unrecognized aggregation-expression operators report mongod's error codes

When a query or pipeline references an aggregation-expression operator that
doesn't exist (e.g. a typo like `$notreal`, or an operator MongoDB itself hasn't
shipped), SecantusDB now rejects it with the same context-specific error code and
message that real `mongod` returns, instead of a generic one.

An unknown operator inside a query `$expr` — `find({$expr: {$notreal: [...]}})` —
now surfaces `168 InvalidPipelineOperator` with the message
`Unrecognized expression '$notreal'` on both the Python and the Rust server
(previously the Python server returned `14 TypeMismatch` and the Rust server a
generic `2 BadValue`). An unknown operator inside an aggregation `$project` —
`aggregate([{$project: {y: {$notreal: [...]}}}])` — returns
`Location31325` `Invalid $project :: caused by :: Unknown expression $notreal` on
the Python server. mongod emits these same "unknown expression" errors even for
operators it recognises by name but hasn't implemented, so SecantusDB simply
matches that behaviour for any operator it doesn't recognise.

#### Fixed

- Query `$expr` with an unrecognized expression operator returns
  `168 InvalidPipelineOperator "Unrecognized expression '$op'"` on both servers
  (was `14 TypeMismatch` on Python, `2 BadValue` on Rust).
- Aggregation `$project` with an unrecognized expression operator returns
  `Location31325 "Invalid $project :: caused by :: Unknown expression $op"` on the
  Python server (was `14 TypeMismatch`). The Rust server still returns a generic
  `BadValue` here — faithful `$project` detection needs to distinguish the
  projection-only operators (`$slice` / `$elemMatch` / `$meta`) from expressions,
  tracked in `tasks/backlog.md` §7.

### CI: the Java-vs-Rust-server gauge joins the weekly validate run

`docs/validation-report-java-rust-server.md` was only refreshable by hand —
the weekly `validate.yml` run regenerated every other committed report but
not this one, so it would have gone stale. A `java-rust-server` matrix entry
now runs `invoke validate-java --server rust` weekly alongside the other
gauges: it reuses the java gauge's JVM/Gradle toolchain plus the
storage-engine sync, and points `gauge_common.rust_binary` at the
venv-staged `secantusd-rs` via `SECANTUSDB_BIN` (the default search only
covers the cargo target dir).

### CI: the cross-driver summary regenerates with the weekly validate run

`docs/validation-summary.md` had been frozen since 2026-06-20 ("the 11
gauges") while the per-driver reports refreshed weekly. Each gauge job now
uploads its raw output (`.validation/`) as an artifact alongside its
report, and the aggregate job reassembles them and regenerates the summary
in the same refresh PR — no WiredTiger build needed there, because the
generator now reads the package version straight from `src/` and resolves
vendored-driver SHAs from the superproject's gitlinks (`git ls-tree`)
instead of requiring checked-out submodules.

#### Added

- `validation_summary.generate`: collectors for the **mongo-kotlin-driver**
  gauge (JUnit XML from `:driver-kotlin-sync:integrationTest`) and the
  **pymongo (async)** gauge (`AsyncMongoClient` suite), bringing the
  summary to 13 gauges; the gauge count in the prose is computed, not
  hand-written.

### `$push` / `$addToSet` skip missing field values (both servers)

A three-way aggregate differential against real `mongod` 6.0 found that the
`$push` and `$addToSet` group accumulators were adding `null` for a document
whose accumulated field is **absent**, where mongod skips it entirely. They now
match mongod: a missing field is not accumulated, while an explicit `null` still
is — so `{$push: "$s"}` over `["x", <missing>, null, "x"]` yields
`["x", null, "x"]`, and an all-missing field still produces `[]` (not a list of
nulls). The distinction is drawn by a new missing-aware evaluate helper
(`evaluate_or_missing` / `eval_or_missing`) that surfaces an absent field path as
a distinct sentinel rather than `null`.

#### Fixed

- `aggregate.py` / `secantus-core`: `$push` / `$addToSet` (in both `$group` and
  `$setWindowFields`) skip a missing accumulator value. (The differential also
  surfaced three items deferred to `tasks/backlog.md` §7.5: `$mergeObjects` as a
  `$group` accumulator, `$getField` returning `null` instead of missing for an
  absent field, and a last-ULP `$stdDevPop` difference vs mongod.)

### PG server shutdown drains client handlers before storage close

`SecantusPGServer.stop()` now tracks its per-connection handler threads and
joins them (bounded, 5 s) after closing their sockets. Previously `stop()`
returned while a handler could still be mid-request on its per-thread
WiredTiger session, so an embedder's natural `stop()` → `storage.close()`
sequence raced the handler and corrupted the WT session handle (a logged
`WT session close failed during close` / `Session__freecb` TypeError during
teardown — visible whenever a client was abandoned mid-transaction, e.g. a
failing test). A handler that outlives the drain window is logged by name.

#### Fixed

- `sql/pgserver.py`: `stop()` joins live handler threads before returning, so
  `storage.close()` immediately after `stop()` can no longer close a WT session
  a handler thread is still using. Regression test: abandon a client
  mid-transaction, `stop()`, assert every handler exited and `close()` logs no
  session-close error.
- Test docstrings in `test_pgserver.py` / `test_pgserver_copy.py` /
  `test_sql_aggregate.py` still described the deleted `FakeStorage` in-memory
  mock; they now state the real WT-backed `Storage` these suites run on.

### `$in`/`$nin` regex candidates and `$all` + `$elemMatch` (both servers)

A three-way query differential against real `mongod` 6.0 turned up two
match-operator gaps present on both servers, now fixed and three-way verified.

A regex inside `$in` (or `$nin`) now matches string values **by pattern**, as
mongod does — `{s: {$in: [/^h/i]}}` matches `"hello"` and `"HELLO"`. Previously
both servers treated the regex as a literal value to compare by equality, so it
silently matched nothing (and on the Rust server it errored). And `$all` now
accepts `$elemMatch` clauses (`{a: {$all: [{$elemMatch: {$gt: 1, $lt: 3}}]}}`) —
each clause requires *some* array element to satisfy its sub-query, so an array
is matched against several independent element predicates at once.

#### Fixed

- `query.py` / `secantus-core`: `$in` / `$nin` route a regex candidate through the
  regex matcher (`_in_candidate_matches` / `in_candidate_matches`); `$all` handles
  a `{$elemMatch: …}` entry by delegating to the `$elemMatch` matcher over the
  whole array. (A related Rust-only gap — `$gt`/`$lt` against a cross-type operand
  such as a document-valued array element deferring instead of comparing by BSON
  type order — is tracked in `tasks/backlog.md` §7.5.)

### `$pull` query semantics, `$pullAll`, and `$push $sort` on the Rust server

A three-way differential against real `mongod` 6.0 turned up two array-update
**correctness** bugs present on both servers, plus a Rust-server feature gap —
all now fixed and three-way verified.

`$pull` previously removed only elements *literally equal* to the criterion, so a
predicate like `{$pull: {a: {$gte: 10}}}` silently removed nothing and a
sub-document criterion like `{$pull: {a: {x: 5}}}` never matched. `$pull` now
applies the criterion under full query semantics — an operator-only criterion
(`{$gte: 10}`, `{$in: […]}`) is an element-value predicate; any other document
criterion is a sub-document match against each element; a scalar is BSON-aware
equality (so `1` matches `1.0` but not `true`, exactly as mongod does — the old
literal-`==` path wrongly conflated `1` and `true`). `$pullAll`, which removes
every element equal to any value in a list, was **entirely unimplemented** (both
servers rejected it as an unknown modifier) and now works. On the Rust server the
`$push` `$sort` modifier (`1` / `-1` whole-element or `{field: dir}`, in BSON
order) now computes natively instead of deferring.

#### Fixed

- `update.py` / `secantus-core`: `$pull` now matches via the query engine
  (`query.matches` / `query::matches`) instead of literal equality; `$pullAll`
  added to both engines and to the Rust `KNOWN_UPDATE_OPS` validator.

#### Added

- `secantus-core`: `$push` `$sort` (whole-element and `{field: dir}` forms) on the
  Rust server, via the shared `order::cmp` / `is_sortable` contract; an element
  outside the sortable subset still defers. `$inc` / `$mul` with a Decimal128
  operand remains a Rust-side defer (decimal arithmetic parity is out of scope —
  `tasks/backlog.md` §7.5).

### Trigonometric expression operators (both servers)

The full trigonometric family lands on both servers — circular, inverse, and
hyperbolic — matched to real `mongod` 6.0 via a three-way probe (mongod vs Rust
vs Python server) with zero value divergences on the numeric path:

`$sin` · `$cos` · `$tan` · `$asin` · `$acos` · `$atan` · `$atan2` ·
`$sinh` · `$cosh` · `$tanh` · `$asinh` · `$acosh` · `$atanh`

Inputs are int / long / double (result: double); `null` / missing propagate to
null. Domain violations raise exactly as mongod does (`Location50989`): `$asin` /
`$acos` / `$atanh` need `[-1, 1]`, `$acosh` needs `[1, ∞)`, and `$sin` / `$cos` /
`$tan` reject `±Infinity` / `NaN`. `$atanh(±1)` returns `±Infinity` (not a domain
error). A non-numeric argument raises `Location28765` (`Location51044` for
`$atan2`).

#### Added

- `expressions.py` / `secantus-core`: the operators above. Both servers compute
  through the platform libm — Rust `f64::sin` and CPython `math.sin` share it, so
  they agree bit-for-bit (the same basis as the already-shipped `$exp` / `$ln`).
  Decimal128 inputs are float-cast on the Python server (SecantusDB does not
  reproduce mongod's decimal-precise transcendental result) and defer to the
  Python oracle on the Rust side — the documented generic-code gap
  (`tasks/backlog.md` §7.5).

### Set-expression and utility operators (both servers)

The aggregation set-expression family lands on both servers, plus a handful of
comparison / size / angle utilities — all matched to real `mongod` 6.0 via a
three-way probe (mongod vs Rust vs Python server) with zero value divergences:

- **`$setUnion` / `$setIntersection` / `$setDifference`** — set algebra over
  arrays. Union and intersection return their result in BSON sort order (matching
  mongod); difference preserves first-array order. All three dedup by BSON-order
  equality, so `1` and `1.0` collapse but `1` and `true` do not.
- **`$setEquals` / `$setIsSubset`** — set membership predicates over two-or-more /
  exactly-two arrays.
- **`$allElementsTrue` / `$anyElementTrue`** — truthiness reductions over an array.
- **`$cmp`** — three-way comparison (-1/0/1) using the full BSON cross-type order.
- **`$binarySize`** (UTF-8 byte length of a string / length of Binary; null → null)
  and **`$bsonSize`** (encoded BSON byte size of a document; null → null).
- **`$degreesToRadians` / `$radiansToDegrees`** — angle conversions.

#### Added

- `expressions.py` / `secantus-core`: the operators above. Set ops share a
  `_set_dedup_sorted` / `set_dedup_sorted` helper that sorts by BSON order and dedups
  adjacent equal values; a non-array argument (or an element the Rust core can't
  cross-type-order) raises on the Python server with mongod's code and defers to the
  Python oracle on the Rust side (documented generic-code gap, `tasks/backlog.md`
  §7.5).

### Batch of aggregation expression operators (both servers)

Nine more aggregation expression operators land on both servers, all matched to
real `mongod` 6.0 via a three-way probe (mongod vs Rust vs Python server):

- **`$tsSecond` / `$tsIncrement`** — the seconds / increment fields of a BSON
  Timestamp (as longs); null/missing → null, non-timestamp → error.
- **`$dateFromParts` ISO-week form** — `{isoWeekYear, isoWeek, isoDayOfWeek}` (the
  calendar form shipped earlier); starts at the Monday of ISO week 1 and rolls over
  (`isoWeek` 53 → next ISO year).
- **`$type`** — the BSON type string of a value, with `"missing"` for an absent
  field (distinct from `"null"`).
- **`$isNumber`** (int/long/double/decimal, not bool) and **`$isArray`**.
- **`$strcasecmp`** — case-insensitive string comparison (-1/0/1; null → empty
  string).
- **`$replaceOne` / `$replaceAll`** — substring replacement; any null
  input/find/replacement → null, a non-string one → error.

#### Added

- `expressions.py` / `secantus-core`: the operators above. `$strcasecmp` and
  `$replaceOne`/`$replaceAll` follow the existing string-op contract (Rust computes
  ASCII and defers non-ASCII case mapping to the Python oracle); the ISO-week form
  uses `chrono`'s ISO calendar. The Python server reproduces mongod's error codes
  exactly (`5687301`/`5687302`, `40515`/`40516`/`40523`, `51745`); the Rust server
  errors on the same inputs but with a generic code (its core defers error-raising —
  documented gap, `tasks/backlog.md` §7.5).

### `$dateFromParts` expression (both servers)

Both servers now build dates from calendar components with `$dateFromParts`:
`{$dateFromParts: {year, month, day, hour, minute, second, millisecond, timezone}}`.
Components default to month/day = 1 and time = 0, and out-of-range values **roll
over** exactly as mongod does — month 13 → next January, month 0 → previous
December, day 0 → last day of the previous month, hour 25 → next day, millisecond
1500 → +1.5 s. `year` is required and must be 1–9999. A `timezone` interprets the
components as local time in that zone (local→instant). Any null component yields
null. Matched to real `mongod` 6.0 via a three-way probe (mongod vs Rust vs Python
server): all values, rollovers, and the validation error codes (`40515` non-integral
component, `40516` missing year, `40523` year out of range) confirmed.

#### Added

- `expressions.py` / `secantus-core`: `$dateFromParts` — month-carry + day/time
  `timedelta` arithmetic for the rollover. The Python server also resolves *named*
  IANA timezones (via `zoneinfo`); the Rust server computes fixed-offset zones
  natively and defers named zones to the Python oracle (the local→instant direction
  is DST-ambiguous, as with `$dateFromString`). The ISO-week form
  (`isoWeekYear` / `isoWeek` / `isoDayOfWeek`) is not yet supported.

### `$top` / `$bottom` / `$topN` / `$bottomN` accumulators (both servers)

Both servers now support MongoDB 5.2's sort-key `$group` (and `$setWindowFields`)
accumulators: `{$topN: {n, sortBy, output}}` sorts the group's documents by
`sortBy` and returns the top `n` documents' `output`, `$bottomN` the bottom `n`;
`$top` / `$bottom` are the single-value forms and take no `n`. The `sortBy` is a
multi-key spec with per-field directions, matching `$sort`'s cross-type BSON order.
Matched to real `mongod` 6.0 via a three-way probe (mongod vs Rust vs Python
server) — values, multi-key sort, array `output`, integral-double `n`, and the
validation error codes (`5788002`-`5788005`, `5787908`, `10065`) all confirmed.

#### Added

- `aggregate.py` / `secantus-core` (`group.rs`): `$top` / `$bottom` / `$topN` /
  `$bottomN` accumulators — collect `(sortBy-values, output)` per doc, stable-sort
  by the `sortBy` directions at finalize (via the same `_SortKey` / `order::cmp`
  contract as `$sort`, deferring an unsortable sort key to the Python oracle), and
  take the top/bottom output(s). Usable in `$group` and `$setWindowFields`.

### Server stop names the connection thread when a shutdown drain wedges

`SecantusDBServer.stop()` already drains its per-connection handler threads to
zero before closing WiredTiger (polling the active-connection count, re-closing
sockets each poll), and storage's per-op `_closed` fences make that teardown
safe even if the drain times out. What it lacked was *observability*: on a drain
timeout it logged only a count ("N connection thread(s) still active"), which is
exactly what made the intermittent xdist-worker-death race in this area hard to
pin down. Connection threads are now named `secantus-conn-<host>:<port>`, and the
timeout warning dumps the live stack of each still-active one — so a genuine
shutdown wedge names its own culprit instead of surfacing as an opaque number.

#### Changed

- `server.py`: per-connection handler threads are named `secantus-conn-<addr>`;
  the stop-drain timeout warning now includes each stuck thread's stack
  (`_format_stuck_conn_stacks`).
### `$top` / `$bottom` / `$topN` / `$bottomN` accumulators (both servers)

Both servers now support MongoDB 5.2's sort-key `$group` (and `$setWindowFields`)
accumulators: `{$topN: {n, sortBy, output}}` sorts the group's documents by
`sortBy` and returns the top `n` documents' `output`, `$bottomN` the bottom `n`;
`$top` / `$bottom` are the single-value forms and take no `n`. The `sortBy` is a
multi-key spec with per-field directions, matching `$sort`'s cross-type BSON order.
Matched to real `mongod` 6.0 via a three-way probe (mongod vs Rust vs Python
server) — values, multi-key sort, array `output`, integral-double `n`, and the
validation error codes (`5788002`-`5788005`, `5787908`, `10065`) all confirmed.

#### Added

- `aggregate.py` / `secantus-core` (`group.rs`): `$top` / `$bottom` / `$topN` /
  `$bottomN` accumulators — collect `(sortBy-values, output)` per doc, stable-sort
  by the `sortBy` directions at finalize (via the same `_SortKey` / `order::cmp`
  contract as `$sort`, deferring an unsortable sort key to the Python oracle), and
  take the top/bottom output(s). Usable in `$group` and `$setWindowFields`.

### `$firstN` / `$lastN` / `$maxN` / `$minN` as `$group` accumulators (both servers)

Both servers now support the N-element operators as `$group` (and
`$setWindowFields`) accumulators, completing the family whose expression forms
shipped earlier: `{$firstN: {n, input}}` collects the first `n` per-doc `input`
values across the group, `$lastN` the last `n`, and `$maxN` / `$minN` the `n`
largest / smallest by BSON order. Matched to real `mongod` 6.0 via a three-way
probe (mongod vs Rust server vs Python server): **`$firstN` / `$lastN` keep null
values** (they're the first/last values seen), while **`$maxN` / `$minN` drop
them**; `{n, input}` validation (integral-double `n` accepted, mongod error codes)
is shared with the expression forms.

#### Added

- `aggregate.py` / `secantus-core` (`group.rs`): `$firstN` / `$lastN` / `$maxN` /
  `$minN` accumulators (shared `nelem_parse_n` validator; `$maxN`/`$minN` sort via
  the `order::cmp`/`is_sortable` contract, deferring bool/Decimal128 elements to
  Python's `_SortKey`). Usable in `$group` and `$setWindowFields`.

### Storage close-path race fixed; opt-in fast test storage

The WiredTiger-backed storage close path carried a latent use-after-free race.
A connection thread opening or resetting its per-thread session
(`_session` / `_reset_thread_session`) and the cross-thread oplog readers
touched the WiredTiger connection *outside* the storage lock, so under rapid
concurrent teardown they could race `close()`'s own session/connection teardown
— a double-close or an open-on-a-closed-connection that segfaults. Every one of
those paths is now fenced against the closed flag under the storage lock. The
shipped server always checkpoints on close, whose timing masked the race in
practice, and the pure-Rust server already drains its connection threads to
zero before closing the connection — so neither production server was exposed —
but the race was real and is now closed.

Separately, `Storage` and `SecantusDBServer` gained a `durable` parameter so the
test suite can run against a faster non-durable storage mode (journal on,
close-checkpoint skipped — every table is still created on disk, so schema,
persistence and within-session behaviour stay real). It is **opt-in and defaults
to durable**, so the shipped server is unchanged. `SECANTUS_FORCE_DURABLE=1`
forces full journal + checkpoint durability everywhere and overrides the fast
default; a CI lane runs the whole suite that way on every push, so the
checkpoint-durability path (schema, close-and-reopen, PITR / backup) stays
continuously exercised even though the default local suite now runs fast.

#### Added

- `Storage` / `SecantusDBServer`: a `durable` parameter (defaults to durable),
  with `SECANTUS_FORCE_DURABLE` (force durable, wins over everything) and the
  conftest-set `SECANTUS_TEST_FAST_STORAGE` (fast default for the test suite)
  environment overrides.
- CI: a `SECANTUS_FORCE_DURABLE=1` full-suite lane so durability paths run every push.

#### Fixed

- Storage close-path use-after-free / double-close race: `_session`,
  `_reset_thread_session`, and the oplog readers (`read_oplog`, `read_preimage`,
  `oplog_floor_seq`, `find_seq_for_ts`, scan helpers) now open/close WiredTiger
  sessions only under the storage lock and only while the store is open.
### `$firstN` / `$lastN` / `$maxN` / `$minN` as `$group` accumulators (both servers)

Both servers now support the N-element operators as `$group` (and
`$setWindowFields`) accumulators, completing the family whose expression forms
shipped earlier: `{$firstN: {n, input}}` collects the first `n` per-doc `input`
values across the group, `$lastN` the last `n`, and `$maxN` / `$minN` the `n`
largest / smallest by BSON order. Matched to real `mongod` 6.0 via a three-way
probe (mongod vs Rust server vs Python server): **`$firstN` / `$lastN` keep null
values** (they're the first/last values seen), while **`$maxN` / `$minN` drop
them**; `{n, input}` validation (integral-double `n` accepted, mongod error codes)
is shared with the expression forms.

#### Added

- `aggregate.py` / `secantus-core` (`group.rs`): `$firstN` / `$lastN` / `$maxN` /
  `$minN` accumulators (shared `nelem_parse_n` validator; `$maxN`/`$minN` sort via
  the `order::cmp`/`is_sortable` contract, deferring bool/Decimal128 elements to
  Python's `_SortKey`). Usable in `$group` and `$setWindowFields`.

### Date-operator timezone errors now report mongod's exact codes (Python server)

A three-way conformance probe (real `mongod` 6.0 vs the Rust server vs the Python
server) found that the date operators' `timezone` errors used a generic code. The
Python server now reports mongod's exact codes: an **unrecognized time zone**
(`{$dateToString: {…, timezone: "Not/AZone"}}`, and likewise for the `$hour`/…
extractors and `$dateToParts`) is `Location40485` "unrecognized time zone
identifier: \"…\"", and a **non-string timezone** is `Location40517` "timezone must
evaluate to a string, found …". (The Rust server raises on the same inputs but with
a generic code — its core defers error-raising to Python.)

#### Fixed

- `expressions.py` (`_resolve_timezone`): pin the unknown-zone / non-string-timezone
  errors to mongod's `40485` / `40517`, shared by every timezone-aware date operator.

### N-element array expressions `$firstN` / `$lastN` / `$maxN` / `$minN` (both servers)

Both servers now support MongoDB 5.2's N-element array aggregation expressions:
`$firstN` / `$lastN` return the first / last `n` elements of an array, and `$maxN`
/ `$minN` the `n` largest / smallest by MongoDB's cross-type BSON sort order
(descending for `$maxN`, ascending for `$minN`, with null elements ignored). When
the array has fewer than `n` elements all are returned. Neither server recognised
these before.

The `{n, input}` validation is **matched to real mongod 6.0** (via a three-way
probe against `mongod`): `n` may be any positive integral number — an integral
double like `2.0` is accepted — and a missing `n` / `input`, a non-integral or
non-positive `n`, or a **null / missing / non-array `input`** each raises mongod's
exact error code (`Location5787902`-`5787908` / `Location5788200`). In particular a
null or missing `input` is an *error*, not null — an earlier draft (and the
`$firstN`/`$lastN` that first shipped under this Unreleased section) returned null
there, which diverged from mongod; this is now corrected.

#### Added

- `expressions.py` / `secantus-core`: `$firstN` / `$lastN` / `$maxN` / `$minN`
  expression operators over a shared, mongod-faithful `{n, input}` validator
  (`_nelem_n_and_input` / `nelem_n_and_input`). `$maxN` / `$minN` sort via the same
  `order::cmp` / `is_sortable` contract `$sortArray` uses (an element outside the
  sortable subset — bool, Decimal128, … — defers to the Python `_SortKey` oracle),
  so the two engines agree on cross-type order. The Python server reproduces
  mongod's error codes exactly; the Rust server raises on the same inputs but with
  a generic code (its core defers error-raising to Python). The `$group`
  accumulator forms remain a follow-on (`tasks/backlog.md` §7.5).

### Distinguishable daemon names: `secantusd-py`, `secantusd-rs`, `secantusd-py-pg`

The two servers used to collide on the command name `secantusdb`, and the Rust
binary went by the confusable `secantusdb-rs`. Each daemon now has a clear name
under a shared `secantusd-<engine>[-<protocol>]` scheme: the Python MongoDB
server is `secantusd-py`, the Rust MongoDB server is `secantusd-rs`, and the
PostgreSQL-wire server gets its first console script, `secantusd-py-pg`. This is
a clean break with no backwards-compatibility shim: the old `secantusdb` and
`secantus` console scripts are gone, and the two utility commands are renamed to
the bare-`secantus` import-name prefix (they aren't daemons, so the `secantusd-`
prefix doesn't fit them) — `secantusdb-admin` → `secantus-admin` and
`secantusdb-restore-archive` → `secantus-restore-archive`. The PyPI project name
(`SecantusDB` / `pip install secantusdb`) and the `secantus` import package are
unchanged.

The shared configuration file is renamed to match: both Mongo daemons read
`secantusd.toml` (auto-discovered in the cwd, `~/.secantus/`, and
`/etc/secantus/`). The legacy `secantusdb.toml` name is still discovered at each
location — the new name wins on a tie — so an existing config file keeps working.

#### Added

- `secantusd-py` / `secantusd-py-pg` console scripts; `secantusd-py-pg` is a new
  CLI entry point (`main()` / `build_parser()`) for `SecantusPGServer`.
- Config auto-discovery now probes `secantusd.toml` ahead of the legacy
  `secantusdb.toml` at every location, in both the Python and Rust loaders.

#### Changed

- The standalone Rust binary is now emitted as `secantusd-rs` (was `secantusdb`);
  its `--version` / `--help` / startup banner and the `secantusd-rs restore`
  usage text follow suit.
- Utility console scripts renamed: `secantusdb-admin` → `secantus-admin`,
  `secantusdb-restore-archive` → `secantus-restore-archive` (the argparse
  `prog=` / help text follow).
- `secantusdb.toml.example` renamed to `secantusd.toml.example`.

#### Removed

- The `secantusdb` and `secantus` console-script aliases of the Python server.
  Use `secantusd-py` (or `python -m secantus`).

### Bitwise aggregation operators `$bitAnd` / `$bitOr` / `$bitXor` / `$bitNot` (both servers)

Both servers now support MongoDB 6.3's bitwise aggregation expressions.
`$bitAnd` / `$bitOr` / `$bitXor` fold a list of int/long operands with the
corresponding bitwise operator; `$bitNot` complements a single operand. The
result is a long when any operand is a long and an int otherwise; a null or
missing operand makes the whole result null; an empty operand list yields the
operator's identity (all-ones for `$bitAnd`, `0` for `$bitOr` / `$bitXor`). A
non-integer operand (double, bool, decimal, …) raises, matching mongod. Neither
server recognised these before.

#### Added

- `expressions.py` / `secantus-core`: `$bitAnd` / `$bitOr` / `$bitXor` / `$bitNot`
  aggregation expressions, with int32/int64 result-width tracking. (The `$group`
  accumulator forms remain a follow-on — see `tasks/backlog.md` §7.5.)

### Rust server: `$stdDevPop` / `$stdDevSamp` group accumulators

The Rust server now supports the `$stdDevPop` and `$stdDevSamp` accumulators in
`$group` (and `$setWindowFields`), matching the Python server — population
standard deviation (÷n, `0` for a single value) and sample standard deviation
(÷n-1, `null` for fewer than two values). Previously the Rust server rejected
these accumulators; the Python server already had them.

To keep the two engines bit-for-bit identical, both now compute the deviation with
the same fixed sequence of correctly-rounded IEEE operations — a naive left-fold
float sum, multiply-based squaring, and hardware `sqrt`. CPython 3.12's `sum()`
builtin switched to Neumaier *compensated* summation for floats, which is more
accurate but would diverge from the Rust engine's naive fold by a last ULP, so the
Python `_std_dev` now sums with an explicit loop instead of `sum()`. (mongod uses
an online Welford-style algorithm, so neither server matches it to the last ULP;
aligning the two SecantusDB engines is the goal.) A parity fuzz seed caught the
divergence before it shipped.

#### Added

- `secantus-core` (`group.rs`): `$stdDevPop` / `$stdDevSamp` accumulators (numeric
  values folded to `f64`, non-numeric defers to Python), shared by `$group` and
  `$setWindowFields`.

#### Changed

- `aggregate.py` (`_std_dev`): compute with a naive float fold + multiply + `sqrt`
  (no `sum()` / `** 2` / `** 0.5`) so the Python and Rust engines agree bit-for-bit.

### Stable `hello` topologyVersion — no more spurious connection-pool churn (both servers)

`hello`'s `topologyVersion.processId` was minted fresh on every call
(`ObjectId` from `now()` on the Python server, `ObjectId::new()` on the Rust
server). The SDAM spec treats a *changed* `processId` as "the server restarted",
so drivers reacted to nearly every monitoring heartbeat by invalidating and
clearing the connection pool — closing the live connection and reconnecting.
Both servers now pin `processId` once per process, so a driver sees a stable
topology and keeps its connections. This was surfaced by the Java driver's
connection-pool-logging and client-metadata event-count tests (which observed
the extra `connectionClosed` / "Connection pool cleared" events), but the churn
affected every driver's SDAM.

#### Fixed

- **Python server** (`commands.py`) and **Rust server** (`secantus-commands`
  `handshake.rs`): `hello` returns a process-stable `topologyVersion.processId`
  instead of a fresh ObjectId per call. Regressions:
  `tests/test_hello_topology.py` (pymongo, both processId stability and
  cross-connection identity) and a `secantus-commands` unit test.

### `$dateToParts` honours a `timezone` (both servers)

`$dateToParts` now accepts a `timezone` and returns the year/month/day/hour/…
parts read off the wall clock in that zone, instead of always in UTC.
`{$dateToParts: {date: "$d", timezone: "America/New_York"}}` on a `16:30Z` instant
returns hour `11` in winter (EST); `Asia/Tokyo` rolls the day forward. Fixed-offset
zones (`+05:30`, `UTC`) and named IANA zones both resolve; no `timezone` still
reads UTC. Like the date extractors, this was a gap in **both** servers — each
previously ignored `timezone` on `$dateToParts`.

The Rust server resolves named zones via the shared `timezone_offset_ms` helper
(the unambiguous instant→wall-clock direction, matching Python `zoneinfo`); an
unknown zone name defers to the Python oracle. `$dateTrunc` / `$dateDiff` timezone
remain deferred — `$dateTrunc` truncates to a *local* boundary that must convert
back to an instant (local→instant, DST-ambiguous), and `$dateDiff`'s `day`/`week`
already count elapsed duration rather than local-calendar boundaries, so timezone
there would compound an existing divergence (see `tasks/backlog.md` §7.5).

#### Added

- `expressions.py` / `secantus-core`: `$dateToParts` reads `timezone` (fixed-offset
  + named IANA), shifting the instant into the zone before splitting into parts.
  The Rust side factors the fixed-offset/named-zone resolution into a shared
  `timezone_offset_ms` helper now used by `$dateToString`, the `{date, timezone}`
  extractors, and `$dateToParts`.

### Date component extractors honour a `timezone` (both servers)

The date component operators — `$year`, `$month`, `$dayOfMonth`, `$dayOfWeek`,
`$hour`, `$minute`, `$second` — now accept mongod's `{date: <expr>, timezone:
<expr>}` object form, reading the component off the wall clock in the requested
zone rather than always in UTC. `{$hour: {date: "$d", timezone:
"America/New_York"}}` on a `16:30Z` instant returns `11` in winter (EST) and `12`
in summer (EDT); a shift that crosses midnight moves `$dayOfMonth` too. Both
fixed-offset zones (`+05:30`, `UTC`) and named IANA zones resolve, and a bare date
expression still reads UTC as before.

This closes a conformance gap that was present in **both** servers — previously
each ignored `timezone` on these operators and a `{date, timezone}` argument
silently returned null. The Rust server resolves named zones via the same
`chrono-tz` path as `$dateToString` (the unambiguous instant→wall-clock direction,
matching Python `zoneinfo`); an unknown zone name defers to the Python oracle.

#### Added

- `expressions.py` / `secantus-core`: a shared date-operand resolver
  (`_date_operand` / `date_operand_millis`) that both the bare-date and
  `{date, timezone}` object forms of the seven date component extractors route
  through, shifting the instant into the requested zone before the component is
  read. Fixed-offset and named IANA zones both supported on both servers.

### Rust server: `$dateToString` now formats dates in named IANA timezones

The Rust server can now render `$dateToString` in a named IANA timezone —
`{$dateToString: {date: "$d", timezone: "America/New_York"}}` — with the correct
daylight-saving offset for the instant being formatted, exactly as the Python
server (and real mongod) do. A summer date shifts by `-04:00` (EDT), a winter one
by `-05:00` (EST); `Europe/Dublin`, `Asia/Tokyo`, and every other zone resolve the
same way. Previously the Rust server accepted only fixed-offset zones (`+05:30`,
`UTC`) and errored on a named zone, since it has no Python `zoneinfo` to defer to.

This is the unambiguous instant-to-wall-clock direction: a UTC instant maps to
exactly one local time in any zone, so the bundled `chrono-tz` database and Python
`zoneinfo` agree. (`$dateFromString`'s named-zone form — naive-local-to-instant,
which is ambiguous across a DST gap/overlap — still defers to the Python oracle.)

#### Added

- `secantus-core`: `chrono-tz` dependency and a named-zone offset resolver
  (`named_tz_offset_ms`) wired into `$dateToString`. A named zone resolves its
  DST-correct UTC offset at the rendered instant; an unknown zone name still
  defers. Both servers now agree on named-timezone `$dateToString`.

### Rust server: connection-auth mutex locks are poison-tolerant

The three production sites that lock the per-connection auth mutex in
`secantus-commands` used `.lock().expect("conn auth mutex poisoned")`, which
would panic if the mutex had been poisoned by an earlier panic while the lock
was held. They now use the same `.lock().unwrap_or_else(|e| e.into_inner())`
pattern as the rest of the crate (`logbuf`, `transactions`), recovering the
guard instead of panicking — so a poisoned auth mutex degrades to a caught
error at the `catch_unwind` dispatch boundary rather than compounding into a
second panic. No behaviour change on the healthy path. Found by the nightly
security review (2026-07-04 §I14).

#### Changed

- `secantus-commands` (`lib.rs` ×2, `auth.rs` ×1): connection-auth mutex locks
  are poison-tolerant. The two remaining `.lock().unwrap()` sites the review
  flagged are in `#[cfg(test)]` mock stores and are intentionally left panicking
  (a test that poisons its own mock lock should fail loudly).
### Rust server: `$dateToString` now formats dates in named IANA timezones

The Rust server can now render `$dateToString` in a named IANA timezone —
`{$dateToString: {date: "$d", timezone: "America/New_York"}}` — with the correct
daylight-saving offset for the instant being formatted, exactly as the Python
server (and real mongod) do. A summer date shifts by `-04:00` (EDT), a winter one
by `-05:00` (EST); `Europe/Dublin`, `Asia/Tokyo`, and every other zone resolve the
same way. Previously the Rust server accepted only fixed-offset zones (`+05:30`,
`UTC`) and errored on a named zone, since it has no Python `zoneinfo` to defer to.

This is the unambiguous instant-to-wall-clock direction: a UTC instant maps to
exactly one local time in any zone, so the bundled `chrono-tz` database and Python
`zoneinfo` agree. (`$dateFromString`'s named-zone form — naive-local-to-instant,
which is ambiguous across a DST gap/overlap — still defers to the Python oracle.)

#### Added

- `secantus-core`: `chrono-tz` dependency and a named-zone offset resolver
  (`named_tz_offset_ms`) wired into `$dateToString`. A named zone resolves its
  DST-correct UTC offset at the rendered instant; an unknown zone name still
  defers. Both servers now agree on named-timezone `$dateToString`.

### Projection inclusion/exclusion mix reports mongod's specific error code

Mixing an inclusion and an exclusion in the same `find()` projection (e.g.
`{a: 1, b: 0}`) now fails with mongod's *specific* per-field error instead of a
generic `TypeMismatch`. An exclusion inside an inclusion projection returns
`Location31254` ("Cannot do exclusion on field b in inclusion projection"); an
inclusion inside an exclusion projection returns `Location31253` ("Cannot do
inclusion on field b in exclusion projection"). The offending field is named in
the message, matching real mongod and the Rust server (which already emitted
these codes). Drivers' projection-error tests assert both the code and the exact
wording, so this closes a pymongo-gauge fidelity gap.

#### Fixed

- `projection.py`: `_detect_inclusion` validates field-by-field in order and
  raises `ProjectionError` with `code`/`code_name` pinned to `31254`/`31253`
  (the offending field named); `ProjectionError` gained a `code`/`code_name`
  constructor so the dispatch layer surfaces the specific code instead of `14`.
### The Rust server prunes its oplog from write volume alone

The standalone Rust `secantusd-rs` server used to prune its oplog only from
the noop-heartbeat thread — and that thread is off by default
(`--noop-heartbeat-seconds 0`). A long-lived, busy server with the
default configuration could therefore grow its oplog past the retention
window and entry cap without bound, since nothing was trimming it. The
Rust storage engine now self-prunes on the write path, exactly as the
Python server has always done: every 1000 emitted oplog entries it runs
an opportunistic `prune_oplog`, so the oplog stays bounded from writes
alone, with or without a heartbeat. Document data is never touched by a
prune.

#### Fixed
- `secantus_storage::emit_oplog` bumps an in-memory emit counter and runs
  an opportunistic `prune_oplog` every `OPLOG_PRUNE_INTERVAL` (1000)
  entries — mirroring the Python server's `_emit_oplog`. The prune reuses
  the write path's already-held storage lock (`prune_oplog_inner(...,
  take_lock: false)`) and is best-effort, so it never fails an otherwise
  successful write.

### `Storage` is now a context manager

`Storage` gained the `__enter__` / `__exit__` protocol, so you can write
`with Storage(path) as store:` and have WiredTiger torn down (background threads
joined, oplog meta persisted, connection closed) on block exit — even if the
body raises — instead of relying on the embedder to remember `close()`.
`close()` remains idempotent, so an explicit close inside the block is still
safe. Found by the nightly security review (2026-07-04 §I12).

#### Added

- `Storage.__enter__` / `Storage.__exit__` — context-manager support that calls
  `close()` on exit. Tests in `tests/test_storage_ctxmgr.py`.

### The standalone Rust server now honours every Python-server CLI flag

The pure-Rust `secantusd-rs` binary — the standalone server you run with
no Python interpreter in the process — used to accept only a subset of
the daemon flags the Python server understands. That gap meant a
`secantusd.toml` or flag set tuned for one server wouldn't drive the
other. It's now closed: the Rust binary accepts the same eight
configuration flags the Python launcher does, with real behaviour behind
each, so a single configuration drives either server identically.

`--config` loads a `secantusd.toml` with the same discovery order
(`./`, `~/.secantus/`, `/etc/secantus/`), the same `defaults < file <
CLI flag` precedence, table renames, and strict unknown-key/table
rejection as the Python loader. `--log-level` initialises a logger to
the requested level; `--cache-size`, `--session-max`, and
`--sync-on-commit` tune the WiredTiger connection; and
`--oplog-retention-seconds`, `--oplog-max-entries`, and
`--noop-heartbeat-seconds` control oplog retention plus a background
heartbeat that keeps quiet change-stream cursors' resume tokens inside
the retention window. The heartbeat thread and a TTL sweeper also give
the standalone binary the periodic oplog-prune and expired-document
maintenance the Python daemon has always run.

One behavioural alignment worth calling out: the standalone daemon's
default WiredTiger cache is now `1G`, matching `python -m secantus`
(the embedded Rust handle's default is unchanged).

#### Added
- The `--config` / `--log-level` / `--cache-size` / `--session-max` /
  `--sync-on-commit` / `--noop-heartbeat-seconds` /
  `--oplog-retention-seconds` / `--oplog-max-entries` flags on the
  standalone `secantusd-rs` binary, with a faithful TOML config loader
  (`secantus-server`'s new `config` module) mirroring
  `src/secantus/config.py`'s precedence and validation.
- Background noop-heartbeat (with opportunistic `prune_oplog`) and
  TTL-sweep threads in the `secantusd-rs` binary, sharing one
  `Arc<Storage>` and joining on a shutdown flag before WiredTiger closes.
- `secantus_storage::wt_config` builds the WiredTiger connection string
  from the resolved cache-size / session-max / sync-on-commit knobs.

#### Changed
- The standalone `secantusd-rs` daemon defaults its WiredTiger cache to
  `1G` (matching the Python server) instead of the engine's `256M`
  default.
### Projection inclusion/exclusion mix reports mongod's specific error code

Mixing an inclusion and an exclusion in the same `find()` projection (e.g.
`{a: 1, b: 0}`) now fails with mongod's *specific* per-field error instead of a
generic `TypeMismatch`. An exclusion inside an inclusion projection returns
`Location31254` ("Cannot do exclusion on field b in inclusion projection"); an
inclusion inside an exclusion projection returns `Location31253` ("Cannot do
inclusion on field b in exclusion projection"). The offending field is named in
the message, matching real mongod and the Rust server (which already emitted
these codes). Drivers' projection-error tests assert both the code and the exact
wording, so this closes a pymongo-gauge fidelity gap.

#### Fixed

- `projection.py`: `_detect_inclusion` validates field-by-field in order and
  raises `ProjectionError` with `code`/`code_name` pinned to `31254`/`31253`
  (the offending field named); `ProjectionError` gained a `code`/`code_name`
  constructor so the dispatch layer surfaces the specific code instead of `14`.

### Admin UI: stop passing the raw (credential-bearing) MongoDB URI into the server page

The admin server page's render context included `current_uri_raw`, the raw
`mongo_uri` (which can carry a username/password), even though no template ever
referenced it — a latent credential-exposure surface with no live leak. Removed
the dead context variable; the page continues to show the scrubbed
`current_uri_display`. Found by the nightly security review (2026-07-04 §I13).

#### Security

- `admin/routers/server.py`: drop the unused `current_uri_raw` template context
  variable so the unscrubbed connection URI is no longer handed to the renderer.

### PostgreSQL/SQL server: malformed messages get an error reply instead of a dropped connection

The PG/SQL server's simple-query loop now answers a malformed message with a
proper `ErrorResponse` rather than letting the exception escape to the outer
handler and silently drop the connection. Most notably, a **SQL syntax error**
over the simple-query protocol previously escaped (the parse ran outside the
handler's `try`) and dropped the connection with no reply; it now returns
`42601` and the connection stays alive. Invalid UTF-8 in a query message returns
`08P01` and the connection survives (the message was length-framed, so the byte
stream is still in sync), and a genuine framing error (an implausible length
prefix) sends a FATAL `08P01` before closing instead of dropping silently.

Found by the nightly security review (2026-07-04 §I16).

#### Fixed

- `sql/pgserver.py`: `planner.parse` moved inside `_handle_query`'s try so a
  syntax error returns `42601` instead of dropping the connection; `_query_loop`
  now catches `PGProtocolError` (framing) and `UnicodeDecodeError` (query text)
  and replies with `08P01`. Regression: `tests/test_pgserver_framing.py`.

### PostgreSQL/SQL server: a malformed SCRAM client-first is a typed auth error

A truncated SCRAM gs2 header (e.g. a client-first message of just `"n,"`, with no
bare message after the header) made `ScramExchange.server_first` raise a bare
`ValueError` from an unpack, caught only by the connection's outer generic
handler. It now raises the typed `PGAuthError`, consistent with the rest of the
PG-auth path — the connection still fails cleanly with no leak, just via the
right exception type. Found by the nightly security review (2026-07-04 §I21).

#### Fixed

- `sql/pgauth.py`: `ScramExchange.server_first` guards the gs2-header split and
  raises `PGAuthError` on a truncated header instead of an unpack `ValueError`.
  New unit tests in `tests/test_pgauth.py`.

### `$push` / `$addToSet` honour the `$each` modifier

`$push` and `$addToSet` now unwrap the `$each` modifier and append every element,
instead of storing the `{$each: […]}` document as a single array element. So
`{$push: {scores: {$each: [90, 85, 82]}}}` appends three scores, and
`{$addToSet: {tags: {$each: ["a", "b", "a"]}}}` adds each not-already-present tag —
matching MongoDB. `$push` also honours the companion modifiers `$position` (insert
at an index, negative from the end), `$slice` (keep the first N / last |N| / none),
and `$sort` (order the array — whole elements by `1`/`-1`, or documents by a
`{field: dir}` spec, in BSON order).

Previously both operators appended the modifier document verbatim — a silent
data-shape bug for one of the most common update forms. It ships on both the
Python and Rust servers, pinned by the update parity suite; the Rust engine
computes `$each` / `$position` / `$slice` natively and defers `$sort` (BSON-order
array sort) to the Python oracle.

#### Fixed

- `$push` / `$addToSet` `$each` is now unwrapped (multi-element append / add),
  with `$push` `$position` / `$slice` / `$sort` modifiers, instead of the `$each`
  document being stored as a single element. Ships on both servers
  (`update.apply_update` + `secantus-core::update`).

### Rust server: correct error codes for unrecognized / Atlas aggregation stages

The Rust server now validates aggregation stage names up-front, matching the
Python server (and mongod): an unrecognized stage (`{$badStage: …}`) is rejected
with `Location40324` ("Unrecognized pipeline stage name"), and an Atlas-only stage
(`$search` / `$vectorSearch` / `$searchMeta` / `$listSearchIndexes`) with
`CommandNotSupported` (115) and the Atlas configuration message. Previously both
surfaced as a generic `BadValue` (2), so drivers couldn't tell an unknown stage
from any other unsupported construct.

#### Fixed

- **Rust server:** an unrecognized aggregation stage now returns code 40324 (was
  2), and an Atlas-only stage returns 115 with the Atlas message
  (`aggregate::validate_stage_names`, recognized-stage set kept in sync with the
  Python `aggregate._STAGES` registry). Regression:
  `tests/test_rust_server_smoke.py::test_aggregate_stage_name_validation_against_rust_server`.

### PostgreSQL/SQL server: an unexpected internal error no longer leaks its Python text to the client

When a statement hit an *unexpected* exception (not a curated `SQLError`), the
PostgreSQL/SQL server sent the raw `str(exc)` back to the client as
`internal error: <text>`. That text could disclose internal file paths, type
names, or document values. The server now logs the full exception server-side
(as it already did) but answers the client with a generic `XX000 internal error`,
matching the Mongo dispatch's discipline of never leaking a Python traceback to
the wire. Curated `SQLError`s are unaffected — they still surface their real
SQLSTATE and user-facing message.

Found by the nightly security review (2026-07-04 §I17).

#### Security

- `sql/pgserver.py` (simple-query + COPY paths) and `sql/pgextended.py` (extended
  protocol) no longer interpolate the raw exception into the `ErrorResponse` for
  an unexpected internal error. Regression: `tests/test_pgserver_error_hygiene.py`.

### Reads on a view resolve its pipeline

`find`, `aggregate`, and `count_documents` on a view now run the view's stored
pipeline against its base collection instead of returning nothing. Create a view
with `db.createView("active", "users", [{$match: {status: "active"}}])` and a
`find` / `aggregate` on `active` reads exactly the matching users — with the
caller's filter, sort, skip, limit, and projection applied on top (a `find` is
translated into the equivalent aggregate over the base collection). A view defined
on another view resolves recursively.

Previously only the `count` command resolved views; `find` and `aggregate` (and
therefore `count_documents`, which pymongo implements via `aggregate`) treated the
view as an empty collection.

#### Fixed

- `find` / `aggregate` on a view now resolve the view's `viewOn` + pipeline
  (recursively for a view-on-a-view) via `commands._resolve_view`, applying the
  request's own filter/sort/skip/limit/projection over the result. Fixes
  `count_documents` on a view returning 0.

### Rust server: reads on a view resolve its pipeline

The Rust server now mirrors the Python server: `find`, `aggregate`, and
`count_documents` on a view resolve the view's `viewOn` + stored pipeline against
its base collection (recursively for a view-on-a-view) instead of returning
nothing. The aggregate command's initial fetch resolves the view chain (next to
the leading-`$match` / `$geoNear` lifts, keeping the view name for the reply `ns`);
`find` on a view is translated into the equivalent aggregate. Previously the Rust
server treated a view as an empty collection.

#### Fixed

- **Rust server:** `find` / `aggregate` / `count` on a view resolve
  `viewOn` + `viewPipeline` (`aggregate::resolve_view`), applying the request's own
  filter/sort/skip/limit/projection on top. Regression:
  `tests/test_rust_server_smoke.py::test_view_reads_resolve_against_rust_server`.

### Rust server: refuses direct writes to synthetic read-only views

The Rust server now rejects a direct `insert` / `update` / `delete` on
`local.oplog.rs` or `admin.system.users` with `Unauthorized` (13), matching the
Python server (and mongod's RBAC-denial code). These namespaces are synthetic
read-only views — `local.oplog.rs` projects the oplog WT table (written only via
oplog emission) and `admin.system.users` is fronted by `createUser` /
`updateUser` / `dropUser` — so a direct write would land in the wrong table or
break the view's invariants. Previously the Rust server silently accepted such
writes. A regular collection write is unaffected.

#### Fixed

- **Rust server:** direct `insert` / `update` / `delete` on `local.oplog.rs` /
  `admin.system.users` is now rejected with code 13 (matching the Python server),
  instead of being silently accepted. Regression: `tests/test_rust_server_smoke.py::
  test_synthetic_view_write_rejected_against_rust_server`.

### PostgreSQL/SQL server: per-statement RBAC, reusing the Mongo role model

The PostgreSQL/SQL server gained an authorization layer. Until now an
authenticated SQL client — or, in the documented trust default, any client —
could run any statement against any database it named at connect time, so a SQL
client had broader effective access than an equivalently-authenticated Mongo
client on the same data. The SQL surface now enforces the *same* RBAC engine the
Mongo server uses (`secantus.rbac`): each statement maps to one action on the
connection's database and is checked against the authenticated user's roles.

Authorization is opt-in and backward-compatible. Start the server with
`require_auth=True` and per-user role bindings via the new `user_roles` argument
(`{"analyst": [{"role": "read", "db": "shop"}]}`) to turn it on; without
`user_roles`, and for the embedded `run_sql` API, the surface stays unrestricted
exactly as before. Built-in roles (`read` / `readWrite` / `dbAdmin` / `dbOwner` /
`root`) resolve directly and custom roles resolve through the shared roles table,
so a role defined once governs both protocols on a shared `Storage`. A denied
statement returns SQLSTATE `42501` and the connection survives.

Found by the nightly security review (issue #193).

#### Added

- `SecantusPGServer(user_roles=...)`: per-user RBAC role bindings, enforced
  per-statement when `require_auth` is on. New pure `sql/authz.py` maps each
  statement to an `rbac` action and calls `rbac.check_privilege`; transaction
  control, `SET`/`SHOW`, and cursor navigation need no privilege. New
  `errors.insufficient_privilege` (SQLSTATE 42501).

#### Security

- SQL clients on a `Storage` shared with a Mongo server are now gated by the same
  roles as Mongo clients, closing the "authenticated SQL client has unrestricted
  access" gap. Regressions in `tests/test_sql_authz.py` (engine-level, built-in +
  custom roles) and `tests/test_pgserver_auth.py` (over the wire).

### Rust server: rejects a malformed `writeConcern`

The Rust server now validates a write command's `writeConcern` before running it,
matching the Python server (and mongod). A negative or too-large integer `w` is a
`FailedToParse` (9), a string `w` other than `"majority"` is an
`UnknownReplWriteConcern` (79), and a bool / non-number-or-string `w` — or a
non-bool/int `j` or non-number `wtimeout` — is a `TypeMismatch` (14). A
well-formed writeConcern is accepted as before, and a satisfiable-but-too-wide
`w > 1` still succeeds with the single-node `writeConcernError` attached. The check
runs in `dispatch` for every write command (insert / update / delete /
findAndModify / create / collMod / createIndexes / drop / dropIndexes /
dropDatabase / renameCollection).

#### Fixed

- **Rust server:** malformed `writeConcern` values are now rejected with mongod's
  codes (9 / 79 / 14) before the write runs, instead of being silently accepted.
  Regression: `tests/test_rust_server_smoke.py::
  test_write_concern_validation_against_rust_server`.

### Rust server: `$lookup` drives the foreign index (and fixes its result order)

The Rust server's simple-form `$lookup` now drives a per-outer-doc index probe
when the foreign collection has a leading-field index on `foreignField`
(single-field, compound-prefix, or multikey — all resolve to an IXSCAN via
`Storage::find`), falling back to the full-scan hash-join only when no such index
exists. This mirrors the Python server's `$lookup` path.

Besides the performance win, this **fixes a two-server divergence**: the previous
hash-join returned the joined `as` array in foreign-collection scan order, while
the Python server (and the index probe) return it in index order. For an indexed
`$lookup` the two servers now produce byte-for-byte identical results, including
the order of documents within `as`. A regression test drives the Rust server and
asserts the index-ordered output.

#### Fixed

- **Rust server:** simple-form `$lookup` over an indexed `foreignField` now returns
  the `as` array in index order (matching the Python server) instead of
  foreign-scan order, and rides the index instead of materialising the whole
  foreign collection. Regression: `tests/test_rust_server_smoke.py::
  test_lookup_index_order_against_rust_server`.

### PostgreSQL/SQL server: connection cap, cursor caps, and a parser guard

The PostgreSQL wire server gained the resource limits its MongoDB counterpart
already had, closing a compounding denial-of-service surface (all reachable
pre-auth, since `require_auth` defaults off):

- **Connection cap.** `SecantusPGServer` now enforces `max_connections` (default
  1000, matching the Mongo server's `DEFAULT_MAX_CONNECTIONS`). An over-cap
  accept is closed immediately instead of spawning a handler thread.
- **Per-session cursor caps.** `DECLARE CURSOR` (which eagerly materializes its
  whole result set) now rejects a session's 101st open cursor, and a cursor
  whose result exceeds 1,000,000 rows, with `program_limit_exceeded` (SQLSTATE
  54000). Combined with the connection cap, total cursor memory is bounded.
- **Parser guard.** `planner.parse` rejects a statement longer than 1 MB before
  handing it to sqlglot, and converts the `RecursionError` from a deeply-nested
  statement (e.g. hundreds of parentheses — a ~600-byte trigger) into a clean
  54000 error instead of relying on the connection loop's broad `except`.

Found by the nightly security review (issue #194).

#### Security

- `sql/pgserver.py`: `max_connections` cap (default `DEFAULT_MAX_CONNECTIONS`).
- `sql/engine.py`: `MAX_CURSORS_PER_SESSION` (100) + `MAX_CURSOR_ROWS` (1e6) caps
  on `DECLARE CURSOR`.
- `sql/planner.py`: `MAX_SQL_LENGTH` (1 MB) cap + `RecursionError` guard in
  `parse`. New `errors.program_limit_exceeded` (SQLSTATE 54000). Regressions in
  `tests/test_sql_cursors.py` + `tests/test_pgserver.py`.

### `$geoNear` rides the geo index instead of scanning

A bounded `$geoNear` — one with a `maxDistance` — no longer walks the whole
collection computing a distance for every document. When it's the leading stage
and the collection has a matching geo index on the queried field, SecantusDB now
lifts the search into a conservative `$geoWithin` candidate query and serves it
through the same geo-index path `$near` and `$geoWithin` already use, then
computes exact distances and sorts over just the candidates.

The candidate radius is inflated by a negligible epsilon so the fetched set is a
strict superset of the exact within-`maxDistance` set; the `$geoNear` stage then
re-applies the exact distance filter, so results — the documents, their order, and
the attached `distanceField` — are byte-for-byte identical to the brute-force
path. Only the number of documents fetched shrinks. An unbounded `$geoNear` (no
`maxDistance`) must still return every document in distance order, so it keeps
scanning; the optimization is scoped to the bounded case, and a mismatched index
type falls back to the full scan. A randomized regression test asserts the
optimized output equals the scan output across many queries.

#### Changed

- `$geoNear` with a `maxDistance` and a matching `2dsphere` / `2d` index now
  fetches candidates through the geo index (`aggregate._geo_near_index_filter`,
  lifted into the aggregate command's initial fetch) instead of a full collection
  scan. Output is unchanged. (Rust-server mirror below.)

### Rust server: `$geoNear` rides the geo index for a bounded search

The Rust server now mirrors the Python server's `$geoNear` index optimization. A
leading `$geoNear` with a `maxDistance` and a matching `2d` / `2dsphere` index no
longer scans the whole collection: the search is lifted into a conservative
`$geoWithin` candidate fetch in the aggregate command's initial fetch (next to the
existing leading-`$match` lift), and the `$geoNear` stage then re-applies the exact
distance filter over just the candidates.

The candidate radius is inflated by a negligible epsilon so the fetched set is a
strict superset of the exact within-`maxDistance` set, keeping the output — docs,
order, and attached `distanceField` — byte-for-byte identical to the brute-force
path. An unbounded `$geoNear` still scans, and a mismatched index type falls back
to the full scan. A regression test drives the Rust server directly, asserting the
optimized output equals the scan output across many random queries.

#### Changed

- **Rust server:** `$geoNear` with a `maxDistance` and a matching geo index fetches
  candidates through the index (`aggregate::geo_near_index_filter` in
  `secantus-commands`) instead of a full collection scan. Output is unchanged.
  Regression: `tests/test_rust_server_smoke.py::
  test_geo_near_index_optimization_against_rust_server`.

### Constant-time secret comparison in the PostgreSQL SCRAM + admin-token checks

Two authentication comparisons used a plain `!=` / `==` on secret material,
which CPython does not guarantee to run in constant time — a timing side-channel
that can narrow a secret a byte at a time. The PostgreSQL/SQL server's SCRAM
proof check (`sql/pgauth.py`) compared the recomputed stored-key digest with
`!=`, and the admin console's token middleware (`admin/middleware.py`) compared
the presented token with `!=` / `==` on both the HTTP and WebSocket paths. All
three now use `hmac.compare_digest` (bytes-encoded, so a non-ASCII presented
value is rejected rather than raising), matching the Mongo-side SCRAM check in
`secantus.auth` which was already constant-time. Behaviour is otherwise
unchanged — valid credentials/tokens are accepted, wrong or missing ones
rejected. Found by the nightly security review (issue #195).

#### Security

- `sql/pgauth.py`: SCRAM stored-key comparison uses `hmac.compare_digest`.
- `admin/middleware.py`: HTTP `TokenAuthMiddleware` and `verify_websocket_token`
  compare the admin token with `hmac.compare_digest` (and reject a
  missing/non-ASCII token without raising). Regression:
  `tests/test_admin_skeleton.py::test_verify_websocket_token_is_constant_time_and_robust`.

### Rust server: bool-as-int range comparison in the query matcher

The Rust server's query matcher now compares a boolean field against a numeric
`$gt` / `$lt` / `$gte` / `$lte` bound (and vice versa) natively, instead of
deferring the whole match to a `BadValue`. Following the Python oracle — which
compares with Python's `<`, where `bool` is an `int` subclass — `True` counts as
`1` and `False` as `0`: `{x: {$gt: 0}}` now matches `x: true` on the Rust server,
matching the Python one. A bool compared against a non-numeric type (string,
Decimal128, date, …) is `TypeError` in Python, i.e. no match, and is reproduced
as such. Boolean *equality* is unaffected (a bool stays distinct from `1`).

Pinned by the query parity suite (curated bool-vs-number / bool-vs-non-numeric /
multikey cases, plus the existing randomized fuzz whose scalar corpus already
includes booleans — 0 divergences).

#### Added

- **Rust server:** bool-as-int `$gt` / `$lt` / `$gte` / `$lte` comparison in the
  `secantus-core` query matcher (numeric compare vs int / long / double; no match
  vs any other type), matching the Python oracle's `<` semantics.

### Rust server: `getLog` returns a real in-memory log

The Rust server's `getLog` was a stub returning an empty array; it now surfaces a
real bounded in-memory log ring buffer (`secantus_commands::logbuf::LogBuffer`,
5000-entry cap, the Rust port of `logbuf.py`). The server records a startup line
and a `"connection accepted"` NETWORK line per connection, and `getLog: "global"`
returns them as mongod-shaped pre-formatted strings
(`"<ts> <level> <component> <msg>"`) with `totalLinesWritten`. The admin
console's Logs page now shows activity against a Rust target instead of an empty
table. Slice 5 of the Rust admin-command parity work (issue #163).

This resolves the last genuine gap in #163: with `getLog` real, `killOp` and the
role grant/revoke + native backup/restore/prune commands all landed, and the
`profile` command was already at parity with the Python server (both persist the
level / slowms / sampleRate config; neither captures slow ops — out of scope for
the surrogate). `currentOp` / `serverStatus` remain intentionally minimal.

#### Added

- **Rust server:** `secantus_commands::logbuf::LogBuffer` + a `getLog`-backed log
  ring buffer wired through the server (connection-accept + startup lines) and a
  `logs` handle on `CommandContext`. Regressions: `crates/secantus-commands` unit
  tests (buffer append/tail/capacity; `getLog` formatting; empty without a
  buffer) and `tests/test_rust_server_smoke.py::test_get_log_returns_connection_lines`.

### Rust server: `$min` / `$max` / `$addToSet` / `$pull` update operators

The Rust server now applies four more update operators natively instead of
rejecting them: `$min` and `$max` (keep the smaller / larger of the current and
given value), `$addToSet` (append to an array only if not already present), and
`$pull` (remove the array elements equal to a value). Previously each surfaced as
a `BadValue` error on the Rust server; they now match the Python server.

Fidelity follows the Python oracle exactly. `$min`/`$max` compare with Python's
`<` semantics for numeric / string / date pairs (bool counts as its integer
value; a cross-type comparison Python would raise on defers to Python), and an
absent-or-null field is treated as "no current value". `$addToSet` and `$pull`
use Python `==` element equality — cross-type-equal numerics (`1` == `True`) and
structural document/array equality included — via the shared value-equality
helper. It's pinned by the update parity suite with curated edge cases and a fuzz
corpus (0 divergences). `$bit` was already native.

#### Added

- **Rust server:** `$min` / `$max` / `$addToSet` / `$pull` update operators in the
  `secantus-core` update engine, matching the Python oracle's comparison / `==`
  element semantics.

### Rust server: `$dateFromString` `format` (strptime)

The Rust server now parses a `$dateFromString` `format` string natively for the
numeric-directive subset (`%Y` `%y` `%m` `%d` `%H` `%M` `%S` `%j` `%%`, literals,
and whitespace), instead of the whole expression being unsupported. So
`$dateFromString: {dateString: "15/01/2024", format: "%d/%m/%Y"}` now evaluates on
the Rust server, matching the Python one.

Fidelity is structural: the format is translated into a regex built from
CPython's own `_strptime` per-directive sub-patterns, so field matching — the
2-digit-year pivot (`00`–`68` → 2000s, `69`–`99` → 1900s), the value-range digit
rules, single-digit leniency, day-of-year, and full-input consumption — is
identical to Python's by construction. It's pinned by the expression parity suite
with a 6000-case fuzz corpus (0 divergences). Directives outside the subset
(`%z` / `%Z` / `%a` / `%b` / `%p` / …), a `%j` combined with `%m`/`%d`, a leap
second, or any input Python would reject still defer to the Python oracle.

#### Added

- **Rust server:** `$dateFromString` `format` (strptime) for the numeric-directive
  subset, built from CPython `_strptime`'s exact regex fragments for byte-faithful
  field matching. Combined with the fixed-offset `timezone` support, a naive
  strptime result is interpreted in the given offset zone.

### Rust server: `killOp`

The Rust server now implements `killOp`. Real mongod signals a per-op interrupt
flag; SecantusDB's faithful analogue (in both servers) is "close the socket" —
since we model one in-flight op per connection, the `op` a caller passes is the
connection's `conn_id` (readable off `hello`'s `connectionId`). The server gained
a live-connection registry (`conn_id → socket clone`, populated on accept and
cleared on disconnect); `killOp` shuts down the target socket, so the connection
thread's next read returns 0 and the connection ends. The opid is accepted as
Int32 / Int64 / integral Double / numeric string, and the reply mirrors the
Python handler: `{info: "operation killed" | "no operation with that opid" |
"no connection registry", ok: 1}`, or `TypeMismatch` for a non-integer `op`.
The admin console's Connections → Kill button now works against a Rust target.
Slice 4 of the Rust admin-command parity work (issue #163).

Remaining #163 gaps: fleshing out the `getLog` / `profile` stubs (both need
real capture infrastructure — a log ring buffer and slow-op timing).

#### Added

- **Rust server:** `killOp` (gated by `A_KILLOP` under `--auth`), backed by a new
  server-side connection registry + a `ConnectionKiller` handle on
  `CommandContext`. Regressions: `crates/secantus-commands` unit tests
  (registry-present/absent, found/not-found, numeric-string opid, non-integer →
  `TypeMismatch`) and
  `tests/test_rust_server_smoke.py::test_kill_op_closes_a_connection`.

### Rust server: fixed-offset `timezone` on `$dateToString` / `$dateFromString`

The Rust server now handles a fixed-offset `timezone` on the `$dateToString` and
`$dateFromString` aggregation-expression operators natively, instead of the whole
expression being unsupported. `$dateToString` with `timezone: "+05:30"` shifts the
wall clock before formatting; `$dateFromString` with a `timezone` interprets a
naive date string as being in that zone (and ignores it when the string already
carries its own offset), matching the Python server exactly. The `UTC` / `GMT`
aliases and both the `±HHMM` and `±HH:MM` offset spellings are accepted.

Named IANA zones (`America/New_York`) still defer — they need a bundled tz
database — as does a `$dateFromString` `format` (strptime). This narrows the gap
between the two servers on the date operators; the behaviour is pinned against the
Python oracle by the expression parity suite.

#### Added

- **Rust server:** fixed-offset / `UTC` / `GMT` `timezone` support on
  `$dateToString` and `$dateFromString` (offset arithmetic in the shared
  `secantus-core` expression engine). Named IANA zones and `format` strptime still
  defer.

### `$jsonSchema` gains `uniqueItems`

The `$jsonSchema` query operator now understands `uniqueItems`. With
`uniqueItems: true` on an array schema, a document matches only when every element
of the array is distinct — the way to say "this list has no repeats" in a
validator or a `$jsonSchema` query. Elements are compared by value: cross-type
numerics that are numerically equal (`1` and `1.0`) count as duplicates, and
documents compare field-by-field in order, matching how mongod compares them.

The check ships on both the Python and Rust servers, pinned by the query parity
suite; it reuses the shared byte-sortable value encoding as the equality key, so
both engines agree bit-for-bit. (One documented gap: cross-type-equal numerics
*nested inside* a document element — `[{a: 1}, {a: 1.0}]` — are treated as
distinct on both servers; top-level scalar arrays are fully faithful.)

#### Added

- `$jsonSchema` `uniqueItems: true` — rejects arrays with duplicate elements
  (value equality, cross-type-numeric aware for top-level scalars). `false` is a
  no-op.

### Rust server: `grantRolesToUser` / `revokeRolesFromUser`

The Rust server now implements the two user-role management commands it was
missing: `grantRolesToUser` adds roles to a user's assignment list (deduped by
`(role, db)`), and `revokeRolesFromUser` removes them. Both validate the
requested roles against the built-in + custom role catalogue (`RoleNotFound`),
require the user to exist (`UserNotFound`), and refresh the calling connection's
effective roles so a privilege change takes effect immediately — matching the
Python handlers. Previously the Rust server could only set a user's roles at
`createUser` / `updateUser` time; the admin console's Users → Roles editor can
now grant/revoke against a Rust target. Slice 3 of the Rust admin-command parity
work (issue #163).

Remaining #163 gaps: `killOp`, and fleshing out the `getLog` / `profile` stubs.

#### Added

- **Rust server:** `grantRolesToUser` / `revokeRolesFromUser` commands (gated by
  `A_GRANT_ROLE` / `A_REVOKE_ROLE` under `--auth`). Regressions:
  `crates/secantus-commands` unit tests (grant/revoke, dedup, `UserNotFound`,
  `RoleNotFound`) and
  `tests/test_rust_server_smoke.py::test_secantus_grant_revoke_roles_to_user`.

### Rust server: `secantusAdmin.restoreArchive`

The Rust server now implements `secantusAdmin.restoreArchive`, completing the
proprietary backup/restore + prune command family (`backupArchive` and the
prune commands already shipped). It extracts a backup `.tar.gz` (produced by
`backupArchive`) into a fresh `targetDir` the operator then points a new server
at — the running server's storage is untouched, the same side-channel restore
model as the Python command and real mongod's "stop, swap dbpath, start". It
rejects a non-empty target unless `allowExisting: true`, verifies the archive is
a genuine SecantusDB / WiredTiger backup *before* extracting (so a malformed
archive can't pollute the target), and returns `{targetDir, fileCount, archive,
ok}`, mirroring the Python handler. This is slice 2 of the Rust admin-command
parity work (issue #163); the admin console's Backup page can now drive native
archive restore against a Rust target.

Remaining #163 gaps: the standard admin commands `grantRolesToUser` /
`revokeRolesFromUser` / `killOp`, plus fleshing out the `getLog` / `profile`
stubs.

#### Added

- **Rust server:** `secantusAdmin.restoreArchive` wire command
  (`Storage::restore_archive` on the command trait →
  `secantus_storage::extract_backup_archive_ex`, with the target-emptiness /
  WiredTiger-metadata guards and abs-path + file-count reply). Regressions:
  `tests/test_rust_server_smoke.py::test_secantus_admin_restore_archive_roundtrips`
  and `::test_restore_archive_rejects_nonempty_target`.

### Rust server: `secantusAdmin.pruneOplog` and `pruneTtl` maintenance commands

The Rust server now implements two of the proprietary `secantusAdmin.*`
maintenance commands the Python server already had: `pruneOplog` (force an
immediate oplog-retention sweep) and `pruneTtl` (run TTL pruning across every
collection now). Both return `{pruned: <count>, ok: 1}`, mirroring the Python
handlers. The underlying storage already prunes on its own cadence; these
wire commands let an operator — or the admin console's Maintenance page —
drive a deterministic pass on demand. This closes the first slice of the Rust
admin-command parity gap (issue #163); the admin UI's capability probe will now
enable the "Prune oplog / TTL" buttons against a Rust target.

Remaining #163 gaps (tracked, not in this slice): `secantusAdmin.restoreArchive`
and the standard admin commands `grantRolesToUser` / `revokeRolesFromUser` /
`killOp`, plus fleshing out the `getLog` / `profile` stubs.

#### Added

- **Rust server:** `secantusAdmin.pruneOplog` / `secantusAdmin.pruneTtl` wire
  commands (`Storage::prune_oplog` / `prune_ttl_all` on the command `Storage`
  trait, forwarded by the WT adapter to the real storage engine). Regression:
  `tests/test_rust_server_smoke.py::test_secantus_admin_prune_commands`.

### `$derivative` and `$integral` gain a time `unit`

The `$setWindowFields` rate operators `$derivative` and `$integral` now accept a
time `unit`. Over a date-valued `sortBy`, `$derivative: {input: "$v", unit:
"hour"}` reports the change in `v` *per hour* — the x-axis is the date's epoch
milliseconds scaled into the requested unit — and `$integral` computes the
trapezoidal area with the same unit-scaled x-axis. This is how you express a rate
of change in meaningful units (per second, per hour, per day) instead of the raw
millisecond spacing between timestamps.

As with time-`unit` range windows, the `unit` requires a date `sortBy` (a numeric
sort with a `unit` is rejected) and the fixed-duration units
`week`/`day`/`hour`/`minute`/`second`/`millisecond` are supported; variable-length
`month`/`quarter`/`year` defer. The feature ships on both servers, pinned by the
`$setWindowFields` parity suite — the millisecond-to-unit scaling runs in IEEE
double on both sides so the results match bit-for-bit. This completes the
`$setWindowFields` time-`unit` surface.

#### Added

- `$setWindowFields` `$derivative` / `$integral` accept a fixed-duration time
  `unit` over a date `sortBy`, scaling the x-axis into that unit so the rate /
  area is expressed per unit. `unit` requires a date sortBy; variable-length
  units defer.

### `$setWindowFields` range windows over a date sortBy

A `$setWindowFields` value-`range` window can now span a *time* interval. Give the
window a `unit` — `week`, `day`, `hour`, `minute`, `second`, or `millisecond` —
and the numeric `range: [lower, upper]` bounds are measured in that unit against a
date-valued `sortBy` field. A `range: [-2, 0], unit: "day"` window is the trailing
three-day span ending at each row, so `{$sum: "$v"}` over it is a rolling 3-day
total regardless of how the dates are spaced.

The rule mongod enforces holds both ways: a `unit` requires a date `sortBy` (a
numeric sort with a `unit` is rejected), and a date `sortBy` in a range window
requires a `unit` (there is no implicit millisecond arithmetic on dates).
Variable-length units — `month`, `quarter`, `year` — are still rejected, since
their span depends on the calendar position. The feature ships on both the Python
and Rust servers, pinned by the `$setWindowFields` parity suite; the date x-axis
is carried as epoch milliseconds so both engines compute identical window bounds.

#### Added

- `$setWindowFields` `range` windows accept a fixed-duration time `unit`
  (`week`/`day`/`hour`/`minute`/`second`/`millisecond`) over a date `sortBy`,
  offsetting the bounds in that unit against the date's epoch millis. `unit` and a
  date sortBy are mutually required; variable-length `month`/`quarter`/`year`
  defer with an error.

### `$jsonSchema` gains `patternProperties` and `dependencies`

The `$jsonSchema` query operator now understands `patternProperties` and
`dependencies`. `patternProperties` applies a sub-schema to every field whose
*name* matches a regular expression — the way to say "every `s_*` field must be a
string" without listing them — and it also tells `additionalProperties: false`
which keys are legitimately covered, so pattern-matched fields are no longer
flagged as unexpected. `dependencies` expresses conditional structure: when a
trigger field is present, either a list of other fields must also be present
(property dependency) or the whole document must satisfy a sub-schema (schema
dependency) — e.g. "if `card` is set, `billing` is required."

Both ship on the Python and Rust servers, pinned by the query parity suite;
`patternProperties` reuses the shared regex engine.

#### Added

- `$jsonSchema` `patternProperties` (regex-keyed sub-schemas, also honoured by
  `additionalProperties`) and `dependencies` (property-list and schema forms).

### `$setWindowFields` completes its time-series window operators with `$derivative` and `$integral`

`$setWindowFields` now supports `$derivative` and `$integral`, the last two of
MongoDB's time-series window operators. Over the window, `$derivative` reports the
rate of change — the slope between the first and last points, `(yₙ − y₀) / (xₙ −
x₀)`, with the `sortBy` value as x and `input` as y — and `$integral` reports the
area under the curve by the trapezoidal rule. Together with `$shift`,
`$expMovingAvg`, `$locf`, and `$linearFill`, that rounds out the full set of
time-series window functions: rates, running smoothing, lag/lead, and gap-filling
all now run in-process.

Both operate over any window (`documents` or `range`), require a single ascending
numeric `sortBy` as the x-axis, and run the same IEEE-double arithmetic on the
Python and Rust servers (pinned bit-for-bit by the aggregation parity suite). A
`$derivative` window with fewer than two points is `null`. A time `unit` (for a
date x-axis) is not yet modelled and raises a clear error.

#### Added

- `$setWindowFields` `$derivative` and `$integral` window operators (`{input}`) —
  slope / trapezoidal area over the sortBy x-axis, over any window.

### `$setWindowFields` gains `$locf` and `$linearFill`

`$setWindowFields` now supports the two gap-filling window operators, `$locf` and
`$linearFill`. `{$locf: <expr>}` ("last observation carried forward") replaces a
null with the most recent non-null value seen in sort order — the standard way to
hold a reading steady until the next sample. `{$linearFill: <expr>}` instead draws
a straight line between the surrounding non-null anchors and reads off the missing
values along it, using the `sortBy` value as the x-axis. Leading nulls (for
`$locf`) and leading/trailing nulls (for `$linearFill`, which has nothing to
interpolate between) stay null.

Both run on the Python and Rust servers alike, pinned by the aggregation parity
suite (the interpolation is IEEE-double, so the two agree bit-for-bit). With these
plus `$shift` and `$expMovingAvg`, only `$derivative` and `$integral` remain of
the time-series window operators.

#### Added

- `$setWindowFields` `$locf` and `$linearFill` gap-fill window operators
  (`<expr>`) — prefix/partition-based, require a `sortBy` (`$linearFill` a single
  ascending numeric one).

### Rust server: an interior-NUL db/collection/index name no longer panics the connection

A well-formed BSON command whose database, collection, or index name carried
an embedded NUL byte (BSON strings are length-prefixed and may legally contain
one) reached the Rust server's WiredTiger key encoder (`secantus-wt`'s `cstr`,
`CString::new(..).expect(..)`) and **panicked**. Because the storage layer
serialises WiredTiger operations under a `std::sync::Mutex`, that panic unwound
while the lock was held and **poisoned it for every connection** — turning a
single crafted command into a whole-server denial of service, and dropping the
offending socket with no `{ok: 0, ...}` reply (unlike the Python server, which
catches every error in `dispatch`).

The Rust server now rejects an interior-NUL database / collection / index name
during command validation — before it reaches storage — with the same
`InvalidNamespace` error mongod returns, so the connection survives with a
clean wire reply. As defense-in-depth, the per-connection dispatch call is now
wrapped in `catch_unwind`: any future unguarded panic in a handler produces a
wire-level `InternalError` reply instead of a silent disconnect. This is the
Rust-side analogue of the earlier Python-server OP_QUERY hardening. Found by
the nightly security review (issue #139).

#### Security

- **Rust server:** interior-NUL `db` / `coll` / `index` names are rejected with
  `InvalidNamespace` before reaching the WiredTiger key encoder (which would
  otherwise panic and poison the shared storage mutex — a whole-server DoS).
  Per-connection dispatch is wrapped in `catch_unwind` so any residual panic
  surfaces as an `InternalError` wire reply rather than a dropped socket.
  Regressions in `crates/secantus-commands` unit tests.

### `admin.system.users` no longer leaks SCRAM credentials via find/count/aggregate

A query against `admin.system.users` — reachable through ordinary
`find` / `count` / `aggregate` / `explain` / `distinct` / `mapReduce`,
which need only the standard collection-read action — used to return each
user record *including* its SCRAM `credentials` blob (`storedKey`,
`serverKey`, `salt`, iteration count). That's the sensitive artifact:
even without the plaintext password it enables offline dictionary/brute
attacks (salt + iterations are the `/etc/shadow` equivalent) and, via
`serverKey`, lets an attacker stand up a rogue server that completes the
SASL server side. A principal holding only `read` / `readAnyDatabase` on
`admin` — a routine monitoring/backup grant — could read every user's
credential material, which is exactly what `usersInfo` intentionally
gates behind `A_VIEW_USER` + `showCredentials`.

The generic read path now strips `credentials` unconditionally, *before*
the filter runs, so it is never returned and can't be used as a
match-oracle either. Credentials remain reachable only through
`usersInfo` with `showCredentials` and the `A_VIEW_USER` privilege — the
one intentionally-gated path, unchanged. Found by the nightly security
review (issue #167).

#### Security

- `Storage._find_system_users` / `_count_system_users`: the SCRAM
  `credentials` blob is stripped from the generic `admin.system.users`
  read path (`find` / `count` / `aggregate` / …) before filtering, so a
  low-privilege reader can no longer harvest credential material.
  `usersInfo` (gated by `A_VIEW_USER` + `showCredentials`) is unchanged.
  Regressions in `tests/test_system_users_view.py`.
### `$setWindowFields` gains `$expMovingAvg`

`$setWindowFields` now supports `$expMovingAvg`, the exponential moving average —
the second time-series window operator (after `$shift`).
`{$expMovingAvg: {input: <expr>, N: <n>}}` smooths a series over the sorted
partition, weighting recent rows more heavily: each output is
`input·α + previous·(1−α)`, with `α = 2/(N+1)`. An explicit
`{alpha: <0…1>}` may be given instead of `N`. It's the standard smoothing for
noisy time-series — a trend line that reacts faster than a flat rolling average.

The recurrence runs in IEEE double on both the Python and Rust servers, so the
two agree bit-for-bit (pinned by the aggregation parity suite). The remaining
time-series operators (`$derivative`, `$integral`, `$linearFill`, `$locf`) still
raise a clear error.

#### Added

- `$setWindowFields` `$expMovingAvg` window operator (`{input, N | alpha}`) —
  prefix-accumulated per partition, requires a `sortBy`.

### `$jsonSchema` gains logical combinators and `additionalProperties`

The `$jsonSchema` query operator now understands the JSON-Schema logical
combinators — `allOf`, `anyOf`, `oneOf`, and `not` — plus `additionalProperties`.
`allOf`/`anyOf`/`oneOf` compose sub-schemas (all / at-least-one / exactly-one
must hold), `not` inverts one, and `additionalProperties` controls fields not
named in `properties`: `false` forbids them outright, while a sub-schema
validates each one. Together they cover the structural side of collection
validators that go beyond flat field constraints — "exactly one of these shapes",
"none of that", "no unexpected fields".

These keywords were previously absent from *both* servers; they now ship on the
Python and Rust servers alike, pinned by the query parity suite.
(`patternProperties` is still not modelled.)

#### Added

- `$jsonSchema` `allOf` / `anyOf` / `oneOf` / `not` combinators and
  `additionalProperties` (`true` / `false` / sub-schema).

### `$setWindowFields` gains the `$shift` window operator

`$setWindowFields` now supports `$shift` — the first of MongoDB's time-series
window operators. `$shift` reaches across the sorted partition to read a value
from a neighbouring row: `{$shift: {output: <expr>, by: <n>, default: <expr>}}`
evaluates `output` on the row `n` positions away (negative looks back, positive
looks ahead), falling to `default` (or `null`) when that position is past the
partition edge. It's the idiomatic way to compute a delta from the previous row,
peek at the next value, or lag/lead a series — without a self-join. Shifts never
cross a partition boundary.

The same semantics ship in both the Python and Rust servers, pinned by the
aggregation parity suite. The remaining time-series operators (`$derivative`,
`$integral`, `$expMovingAvg`, …) still raise a clear error.

#### Added

- `$setWindowFields` `$shift` window operator (`{output, by, default?}`) —
  position-based, per-partition, requires a `sortBy`.

### `$setWindowFields` learns value-based (range) windows

The `$setWindowFields` aggregation stage now supports value-based windows —
`window: {range: [lower, upper]}` — alongside the position-based `documents`
windows it already had. Where a `documents` window counts rows relative to the
current one, a `range` window is defined by the sortBy *value*: it includes every
row whose value falls in `[current + lower, current + upper]`. That's the natural
way to express "sum everything within 10 units of this point" or a gap-aware
running total, and it's what analytics pipelines reach for. Bounds may be a
number, `"current"` (this row's value), or `"unbounded"`.

The window resolves against a single ascending numeric sortBy field; a range
window with a time `unit`, or over a descending / multi-field / non-numeric sort,
still raises a clear error rather than guessing. The same semantics ship in both
the Python and Rust servers, pinned together by the aggregation parity suite.

#### Added

- `$setWindowFields` value-based windows (`window: {range: [lo, hi]}`) over a
  single ascending numeric `sortBy`, with `"unbounded"` / `"current"` / numeric
  bounds. Time-unit ranges and non-ascending / multi-field / non-numeric sorts
  remain deferred with a clear `AggregateError`.

### Smaller on-disk footprint: WiredTiger log pre-allocation disabled

Each on-disk SecantusDB instance used to reserve ~30 MB of WiredTiger log
files regardless of how little data it held — the active `WiredTigerLog`
plus two 10 MB `WiredTigerPreplog` files WT pre-allocates ahead of the
active log. That pre-allocation is a write-latency optimisation for
long-running, high-throughput servers; SecantusDB's instances are small,
ephemeral, in-process test databases, so it bought nothing and cost disk —
acutely on CI, where a full ~2000-test on-disk run retained thousands of
instances and exhausted the Windows runner's disk (`No space left on
device` → `WT_PANIC`). Disabling `log=(prealloc=false)` drops each
instance's log footprint from ~30 MB to ~10 MB with no durability change
(recovery replays the same log records; WT just allocates each segment on
demand). `file_max` stays 10 MB so a near-`maxBsonObjectSize` document
still fits in one segment.

#### Changed
- `Storage`: on-disk WiredTiger opens with `log=(...,prealloc=false)` —
  ~3x smaller per-instance log footprint, no durability impact.


### A malformed OP_QUERY frame returns BadValue instead of dropping the connection

A legacy `OP_QUERY` frame whose `fullCollectionName` carried no NUL
terminator made `bytes.index(b"\x00", ...)` raise an uncaught `ValueError`
that escaped the wire layer's `(InvalidBSON, _BodyBoundsError)` handler,
killed the connection handler without sending a reply, and logged a Python
traceback. The same gap existed for several siblings on the same
attacker-controlled path: a non-UTF-8 collection name (`UnicodeDecodeError`),
a frame truncated before the skip/return/query fields (`struct.error`), and a
negative/oversized declared query-doc length (the `OP_MSG` path already
guarded this with `_check_doc_len`, but `OP_QUERY` did not). The OP_MSG
kind-1 section-identifier `.index()` / `.decode()` had the identical
unterminated-cstring gap.

`_parse_op_query` is now hardened the same way `_parse_op_msg` already was —
every read on the network buffer raises `_BodyBoundsError` / `struct.error`,
which `read_message` translates into a `BadValue` (2) wire reply while keeping
the connection alive (matching `mongod`). `read_message` also now catches
`struct.error` as a backstop so no malformed frame can escape as an uncaught
exception. Found by the nightly security review (issue #116).

#### Security
- `wire._parse_op_query` / `_parse_op_msg`: malformed OP_QUERY/OP_MSG frames
  (missing cstring NUL, invalid UTF-8, truncation, bad BSON length) now yield
  a `BadValue` reply and a surviving connection instead of a dropped socket +
  logged traceback. Regression: `tests/test_wire_malformed.py` (5 new
  OP_QUERY cases).

### Admin UI security hardening (two CVEs + a stored-XSS fix)

Three findings from the nightly security review, all confined to the
optional `[admin]` extra (the loopback FastAPI console):

- **CVE-2026-48710 "BadHost" (issue #114):** the admin token middleware
  gated its `/healthz` + `/static/` bypass allowlist on `request.url.path`,
  which pre-`starlette` 1.0.1 is rebuilt from an unvalidated `Host` header —
  a request with `Host: x/healthz?t=` could shift `request.url.path` to a
  bypass prefix and reach protected admin endpoints unauthenticated. Bumped
  the `starlette` floor to `>=1.0.1`, **and** the middleware now reads the
  ASGI `scope["path"]` (immune to `Host` spoofing) as defence-in-depth.
- **CVE-2026-53539 (issue #113):** `python-multipart` `<0.0.30` has a
  quadratic-CPU DoS in its urlencoded-form parser. Bumped the floor to
  `>=0.0.30`.
- **Stored XSS in the geo viewer (issue #115):** the geo page injected
  sampled document data into an inline `<script>` via `json_util.dumps` — a
  document whose string `_id` contained `</script><script>…` closed the
  block and injected arbitrary JS (with access to pywebview's `js_api`).
  Feature JSON is now escaped for the script context (`<`/`>`/`&`/U+2028/
  U+2029 → `\uXXXX`) so a payload can never break out of the block.

#### Security
- `admin/middleware.py`: token-bypass check reads `scope["path"]`, not the
  Host-derived `request.url.path`.
- `admin/routers/extras.py`: `_json_for_script` escapes geo feature JSON for
  safe inline-`<script>` embedding.
- `pyproject.toml` `[admin]`: `starlette>=1.0.1`, `python-multipart>=0.0.30`.
- Regressions: `tests/test_admin_security.py`.

### Capped-collection eviction survives a backup taken mid-stream

A capped collection restored from a `backupArchive` could evict the wrong
document on its next insert — dropping the freshly-inserted row instead of the
oldest one. The cause was a stale recovery hint: the oplog-meta row that records
the next insertion sequence is only refreshed on `hello`, `prune_oplog`, and
`close` (the per-write path stopped re-persisting it because it WT-rollbacks
under concurrent writers), so a checkpoint taken between two refreshes captures a
sequence counter that lags the actual data. On reopen the server *trusted* that
stale value, re-minting an already-used natural-order sequence; the collision
overwrote a live document's entry in the insertion-order index and corrupted
capped FIFO eviction. The symptom was load- and timing-dependent — it surfaced
only when the driver's background topology `hello` happened to fall before the
last insert rather than after it.

Recovery now treats the persisted `next_seq` / `next_nat_seq` as a *hint that can
only be corrected upward*: it clamps each counter to what the oplog and
natural-order tables actually contain, so a lagging meta row can never lower the
sequence and re-mint a used value. The oplog maximum is read with a single
`prev()` (the table is keyed on the bare sequence), keeping reopen cheap.

#### Fixed

- Capped-collection FIFO eviction after restoring a `backupArchive` that was
  taken between oplog-meta refreshes — the recovered insertion-sequence counter
  is now clamped up to the natural-order table's maximum rather than trusting a
  stale persisted value, eliminating the sequence collision that dropped a
  just-inserted document. The same clamp guards the oplog `next_seq` against
  re-minting a used sequence (which would silently overwrite an oplog row).

### Storage close no longer swallows durability errors; admin history/backup never persist credentials

Two security findings from the nightly review are closed. `Storage.close()`
used to wrap its final teardown — the last oplog-meta persist, the shutdown
checkpoint, and every WiredTiger session/connection close — in bare
`contextlib.suppress(Exception)`, discarding a checkpoint or connection-close
failure with no trace at all. In a database that is a durability signal, not
noise: the embedder had no way to know the last on-disk image might be
incomplete. The teardown now logs every caught failure via
`log.exception(...)` (matching the TTL-sweep and noop-heartbeat loops) while
still completing idempotently.

The admin console's query-history store and backup helpers no longer let a
credentialed connection string reach disk or the UI in plaintext. `HistoryStore`
scrubs the URI to its password-free `display_uri()` form at the store boundary
before it becomes a SQLite lookup key, so `~/.secantus/admin.db` can never hold
a `mongodb://user:pass@host` string (and a caller passing the raw URI can't
reintroduce the leak). The mongodump/mongorestore helpers still hand the live
credential to the subprocess — they need it to authenticate — but now redact the
password from any captured stdout/stderr, closing the path where a tool that
echoes the connection string on error surfaces the secret in the rendered
backup result.

#### Fixed

- `Storage.close()` teardown errors (oplog-meta persist, shutdown checkpoint,
  WT session/connection close) are logged instead of silently suppressed
  (security issue #138).
- Admin `HistoryStore` persists the scrubbed `display_uri()` form instead of the
  raw credentialed `mongo_uri`; mongodump/mongorestore captured output has the
  password redacted (security issue #140).

### Admin console detects the target server and gates features it can't do

The admin UI is a plain pymongo client, so it can point at any of the
three MongoDB-wire servers: the SecantusDB Python server, the SecantusDB
Rust server, or a real `mongod`. They differ in which commands they
implement — most visibly the four proprietary `secantusAdmin.*`
backup/maintenance commands (no `mongod` has them) and a handful of
standard admin commands the Rust server hasn't ported yet. Until now the
console advertised every button regardless of target, so clicking
"native checkpoint backup" against a `mongod`, or "prune oplog" against
the Rust server, returned a bare `CommandNotFound`.

The console now probes the target once at connect (and on every target
swap) via `buildInfo` + `serverStatus`, classifies it as Python / Rust /
MongoDB from the server's own self-identification
(`serverStatus.secantus.server` and `buildInfo.secantusVersion`), and
derives a capability set the templates consult. A detected-server pill
appears next to the target badge, and features the target can't honour
are disabled with an explanatory tooltip instead of failing on click:
native backup archive / restore and manual oplog/TTL prune (all
`secantusAdmin.*`, SecantusDB-only), plus role grant/revoke and
connection-kill (`killOp`) where the Rust server hasn't ported them. An
unreachable or not-yet-probed target stays fully permissive, so a
transiently-down server never hides a working button.

#### Added

- `secantus.admin.capabilities`: server-capability probe + classifier
  (`classify` / `probe` / `ServerCapabilities` / `UNKNOWN`), wired into
  the app lifespan startup and `swap_target`, exposed to templates as
  `request.app.state.capabilities`.
- Admin templates gate `secantusAdmin.*` backup/maintenance buttons,
  role grant/revoke, and connection-kill to the detected server's
  capabilities, and show a server-type badge.

### Atlas Search index commands are rejected with an "Atlas" error

Atlas Search index management — the `createSearchIndexes`, `updateSearchIndex`,
and `dropSearchIndex` commands plus the `$listSearchIndexes` aggregation stage
(and its `$search` / `$searchMeta` / `$vectorSearch` siblings) — is an
Atlas-only feature. A real non-Atlas `mongod` registers these but fails them at
execution with a message naming Atlas. SecantusDB now does the same
(`CommandNotSupported`, message mentioning Atlas) instead of returning
`CommandNotFound` / an "unrecognized pipeline stage" error. Closes the
mongo-c-driver `/index-management/{list,drop,update,create}SearchIndex` tests,
which assert the error contains "Atlas".

#### Added
- `createSearchIndexes` / `updateSearchIndex` / `dropSearchIndex` command
  handlers and `$listSearchIndexes` / `$search` / `$searchMeta` /
  `$vectorSearch` aggregation-stage rejection, all returning the shared
  `aggregate.SEARCH_INDEX_ATLAS_MSG` (mirrors mongod's not-on-Atlas error).

### Dropping a collection under a tailable cursor reports "collection dropped"

When a capped collection is dropped while a tailable cursor is open on
it, the next `getMore` now fails with `QueryPlanKilled` (175) and a
"collection dropped" message — exactly what mongod surfaces to a tailing
client. Previously the cursor was simply removed, so the follow-up
`getMore` returned a bare `CursorNotFound` (43). Regular (non-tailable)
cursors are unchanged: dropping their collection still yields
`CursorNotFound`, which the strict wire gauges rely on. Closes
mongo-php-driver's `cursor-tailable_error-001`, which asserts the
dropped-collection error mentions "collection dropped".

#### Fixed
- `CursorRegistry.kill_namespace` tombstones tailable cursors (sets a
  `dropped` flag, keeps the entry) instead of deleting them; non-tailable
  cursors are still deleted. `_get_more` returns the new
  `QueryPlanKilled` "collection dropped" reply for a tombstoned tailable
  cursor, and `_drop` tombstones before the storage drop so a parked
  `awaitData` `getMore` (woken by the drop's oplog write) observes the
  flag.

### A tailable cursor's filter now applies to docs inserted after the find

A tailable + `awaitData` cursor on a capped collection re-polls the
collection for documents inserted after the `find`. That poll was
returning *every* new row, ignoring the cursor's query filter — so a
tailable cursor watching for `{a: 1}` would surface unrelated inserts
(and even pre-existing non-matching docs) the moment it ran a `getMore`.
The producer now re-applies the find filter (with the same `let` vars and
collation) to each scanned row, exactly as the oplog-tailing variant
already did. The watermark still advances past every scanned row, matched
or not, so non-matching docs aren't re-examined on the next poll. Closes
the mongo-c-driver gauge's `/Collection/tailable/timeout/single`.

#### Fixed
- `commands._find_tailable`: the capped-collection tailable producer
  filters follow-up inserts through `query.matches` instead of returning
  them unconditionally.

### A malformed `$and` / `$or` / `$nor` is a clean parse error, not a crash

`$and` / `$or` / `$nor` require a non-empty array of sub-documents. A query
passing a non-array (`{$or: true}`), an empty array, or a non-document element
used to crash the query engine — `for c in condition` raised a Python
`TypeError` that escaped the parse-error handling and surfaced over the wire as
a generic `InternalError` (1) with the traceback logged server-side. It now
matches mongod: a `BadValue` (2) parse error ("$or must be an array" / "must be
a nonempty array" / "entries need to be full objects"). Surfaced while triaging
the mongo-c-driver gauge's malformed-input command-monitoring tests.

#### Fixed
- `query._match_clause`: `$and` / `$or` / `$nor` validate their argument is a
  non-empty list of documents and raise `QueryError` (→ BadValue 2) for any
  malformed shape, instead of letting a `TypeError` leak out as InternalError.

### An unrecognised index-key string is rejected as an unknown plugin

A string value in an index key names a special index type ("plugin") — `2d`,
`2dsphere`, `text`, `hashed`. SecantusDB already accepted the geo plugins and
rejected `text`/`hashed` as out-of-scope, but it let *any other* string through:
`createIndex({abc: "hallo thar"})` silently created a broken index instead of
erroring. It now matches mongod and rejects an unrecognised plugin name with
`CannotCreateIndex` (67) "Unknown index plugin '<value>'".

This closes the mongo-c-driver gauge's `/Collection/index_w_write_concern` test,
which (after the 0.5.4b13 write-concern fix) was failing on its invalid-index
assertion — it creates `{abc: "hallo thar"}` and expects the server to reject
it. The test name is misleading; the failure had nothing to do with write
concern.

#### Fixed
- `storage.create_index`: a string index-key value that isn't a recognised
  plugin (`2d` / `2dsphere` / `2dsphere_bucket` / `geoHaystack`) is rejected
  with `CannotCreateIndex` (67) "Unknown index plugin '<value>'", alongside the
  existing `text` / `hashed` out-of-scope rejection.

### A numeric write-concern `w` above 50 is now a parse error

A `writeConcern` with a numeric `w` greater than 50 (or negative) is now rejected
at parse time with `FailedToParse` (9) and the message "w has to be a non-negative
number and not greater than 50" — matching mongod, which caps `w` at the maximum
number of voting replica-set members (50). Previously SecantusDB treated any
`w` above 1 the same way (a `CannotSatisfyWriteConcern` writeConcernError, code
100, attached to a *successful* reply). That's only correct for `1 < w <= 50` —
satisfiable on a multi-node deployment but not on our single node. Above 50 the
value is simply invalid, and mongod errors the whole command.

This closes the mongo-c-driver gauge's last cluster of failures —
`/Collection/{drop,rename,index}` and `/Database/drop`, which each run a DDL op
with `w: 99` and assert the `assert_wc_oob_error` shape (code 9, the message
above) for a server advertising version >= 4.3.3 (we advertise 7.0). The
"state-ordering" label these carried in the backlog was a misdiagnosis: the
failure is deterministic, not dependent on test order.

#### Fixed
- `commands._validate_write_concern`: a numeric `writeConcern.w` outside `[0, 50]`
  is rejected with `FailedToParse` (9) before the command runs, instead of
  falling through to the satisfiability check (code 100). `_drop_database` and
  `_rename_collection` now run this validation too (they previously skipped it,
  relying only on the dispatch-level `_unsatisfiable_wc_error`).

The `$currentOp` aggregation stage now surfaces the connecting driver's
handshake metadata — the full `clientMetadata` document (driver name/version,
OS, application name) plus a top-level `appName` — on its self-row, matching
real `mongod`. Previously only the `currentOp` *command* echoed this back; the
aggregation form (`db.aggregate([{$currentOp: {}}])`) returned a bare stub, so a
client couldn't find its own operation by `appName` or read back the metadata it
sent on connect.

This was the last real divergence the mongo-cxx-driver gauge's "integration
tests for client metadata handshake feature" exercised — it connects with
`?appName=xyz`, scans `$currentOp` for the matching op, and verifies its
`clientMetadata.{application,driver,os}`. With this and the 0.5.4b11
resume-token fix, the cxx gauge's remaining real failures are closed.

#### Fixed
- `aggregate._stage_current_op`: the `$currentOp` self-row now carries
  `clientMetadata` (the connection's `hello.client` subdoc) and a top-level
  `appName` lifted from `application.name`, threaded in via a new
  `PipelineContext.client_metadata` field that the `aggregate` command handler
  populates from the connection registry. The `currentOp` command already did
  this; the aggregation stage now matches.

### Change-stream resume tokens now advance per event, even at batchSize 1

A change stream's `postBatchResumeToken` now tracks the resume token of the
**last event actually returned in each batch**, not the last event the server
happened to prefetch. The producer reads up to 200 oplog rows ahead while the
cursor hands them back `batchSize` at a time, so with a small batch size the
token reported on each `getMore` was stale — three single-event reads all
carried the same token. Drivers that resume off the per-batch token (the whole
point of `postBatchResumeToken`) would resume from the wrong place. Alongside
it, an empty `getMore` over a quiet collection no longer re-mints the token with
a fresh cluster time when the oplog tail hasn't actually moved, so an exhausted
stream reports the same resume token as its last event rather than drifting.

This was surfaced by the mongo-cxx-driver gauge's spec prose test "ChangeStream
must continuously track the last seen resumeToken" (`batchSize=1`, read three
events, assert each token differs, then assert the post-exhaustion token equals
the last event's). The fix is server-side and driver-agnostic — every change-
stream driver benefits.

#### Fixed
- `commands._change_stream_cursor_doc`: `postBatchResumeToken` is now the `_id`
  (resume token) of the last event in the returned batch, not the producer's
  prefetch-tail `last_token` — so per-batch tokens advance correctly under any
  `batchSize`.
- `commands` change-stream producer: an empty `getMore` only advances /
  re-mints the resume token when the oplog tail has genuinely moved past the
  cursor's position, preserving the go `resume_token_updated_on_empty_batch`
  advance while fixing the mongocxx no-change-equals-last-token case. (On a
  truly quiet collection the token now advances via the oplog's periodic noop
  heartbeats, mirroring mongod's `periodicNoopIntervalSecs` — not a per-getMore
  clock tick.)
- The change-stream batch builder (`_change_stream_cursor_doc`) is shared with
  capped-collection tailable cursors, whose documents carry plain `_id` values
  and no resume token. The new `postBatchResumeToken` = last-event-`_id` logic
  is gated to change-stream cursors only, so a capped tailable getMore no longer
  emits a non-document PBRT (strict drivers — the Java driver — rejected an
  int32 there).

### Two more conformance gauges: the Kotlin driver and pymongo async

SecantusDB now also measures itself against the official MongoDB **Kotlin**
driver and against pymongo's native **async** (`AsyncMongoClient`) suite —
bringing the gauge count to thirteen. Both reuse infrastructure already in the
tree rather than vendoring new submodules: the Kotlin driver ships *inside* the
mongo-java-driver monorepo (`driver-kotlin-sync`), so its gauge runs the
`:driver-kotlin-sync:integrationTest` Gradle task against an embedded SecantusDB
daemon over the same JDK/Gradle toolchain the Java gauge already needs; and the
async gauge points pymongo's `test/asynchronous/` suite at the same
embedded-server plugin the sync gauge uses, run under `pytest-asyncio` with
`asyncio_mode=auto`. The async gauge is the more interesting of the two — it
exercises the async/await wire path that replaced Motor, which drives cursors and
change-stream `getMore` polling through a different event-loop code path than the
synchronous client, catching divergences a sync-only gauge can't see.

Run them with `invoke validate-pymongo-async` and `invoke validate-kotlin`. Both
join the weekly `validate.yml` matrix and `invoke validate-all`. Neither touches
the shipped `secantus` package — the gauge directories are dev-only, excluded
from the wheel and sdist like every other gauge.

Bringing the Kotlin gauge up also surfaced and fixed a latent break in the
shared gauge plumbing: every daemon-subprocess gauge (go / node / java / ruby /
rust / c / cxx / dotnet) passes `--log-level WARNING`, but `gauge_common.
spawn_daemon` learns the daemon's kernel-assigned port by grepping its
`listening on <host>:<port>` line — which the Python server logs at INFO, so
WARNING suppressed it and the spawn waited the full timeout (and, with a
blocking read, could hang for hours — one scheduled CI run was cancelled at 6h).
The spawn now forces the Python daemon to INFO (per-request logging is at DEBUG,
so this adds only the one readiness line, no noise) and reads the daemon's output
under a hard deadline so a missing line times out instead of hanging. This is
why those gauges were red in the weekly run; they go green again with this fix.

#### Added
- `pymongo_async_validation/` gauge package (`include_paths` / `generate_report`)
  and the `invoke validate-pymongo-async` task — pymongo's native
  `AsyncMongoClient` suite against an embedded SecantusDB, reusing
  `pymongo_validation.plugin` and the `vendor/pymongo-tests` submodule. Adds a
  `pytest-asyncio` dev dependency.
- `kotlin_validation/` gauge package (`include_modules` / `runner` /
  `generate_report` / `init.gradle.kts`) and the `invoke validate-kotlin` task —
  the official Kotlin driver's `:driver-kotlin-sync:integrationTest` suite against
  a standalone SecantusDB daemon, sharing the Java gauge's JVM toolchain and
  `vendor/mongo-java-driver` submodule.
- Both gauges wired into `invoke validate-all` / `validate-all-servers` and the
  CI `validate.yml` matrix.

#### Fixed
- `gauge_common.spawn_daemon`: the Python daemon is now forced to `--log-level
  INFO` so its `listening on …` readiness line (logged at INFO) is visible even
  though gauges pass `--log-level WARNING`, and the readiness read is bounded by
  the spawn deadline instead of a blocking `readline()`. Unbreaks the
  daemon-subprocess gauges (go / node / java / ruby / rust / c / cxx / dotnet),
  which were failing/hanging in CI because WARNING suppressed the line.

### Closing the gaps the C, C++, and C# gauges opened

Adding the mongo-c-driver, mongo-cxx-driver, and mongo-csharp-driver gauges
turned up a cluster of small conformance divergences in the Python server, and
this release fixes the actionable ones. None were show-stoppers on their own —
they're the kind of edge that a permissive driver like pymongo glosses over but
a strict C extension or a spec-faithful CRUD suite pins exactly — and together
they tighten how faithfully SecantusDB answers the corners of the wire protocol.

The headline is document-validation error detail: a write that fails a
collection validator now returns mongod's full per-operator `errInfo.details`
(`operatorName` / `specifiedAs` / `reason` / `consideredValue` / `consideredType`)
instead of a bare placeholder, so a driver can tell you *why* a document was
rejected. Alongside it: `$out` / `$merge` are now rejected unless they're the
final pipeline stage, they enforce the destination collection's validator
(honouring `bypassDocumentValidation`), a change stream opened with an invalid
`$match` errors immediately rather than on the first batch, `batchSize` accepts
any BSON number type, over-long database names are rejected, dropping or renaming
a collection invalidates its open cursors, and `collMod` can stage an index
`prepareUnique` and convert it to `unique` — reporting the duplicate `_id`
groups as `violations` when the conversion can't proceed.

#### Added
- `collMod {index: {prepareUnique: true}}` arms an existing index so new
  uniqueness-violating writes are rejected (11000) while pre-existing duplicates
  remain, and `collMod {index: {unique: true}}` converts it — refusing with
  `CannotConvertIndexToUnique` (359) plus a `violations: [{ids: [...]}]` array
  when duplicates exist (`storage.set_index_options` / `find_index_duplicates`).

#### Fixed
- Document validation now synthesises mongod's per-operator `errInfo.details`
  for query-expression validators (`commands._validation_failure_details`),
  used by both the insert path and `findAndModify` upsert simulation.
- `$out` / `$merge` are rejected with `Location40601` (40601) unless they are the
  final pipeline stage, and they enforce the destination collection's `validator`
  unless `bypassDocumentValidation` is set (`DocumentValidationFailure`, 121).
- Change streams validate `$match` filter syntax at open time, so an unknown
  query operator errors at `.begin()` (aggregate) rather than the first `getMore`.
- `find` / `aggregate` accept a `batchSize` encoded as any BSON number, including
  `Decimal128`.
- Commands targeting a database whose name exceeds 63 bytes are rejected with
  `InvalidNamespace` (73).
- Dropping or renaming a collection now kills its open cursors, so a later
  `getMore` fails with `CursorNotFound` (43) instead of serving stale rows.

### createIndexes now rejects conflicting index definitions

`createIndexes` previously accepted a re-creation that collided with an existing
index, silently treating it as a no-op. It now matches `mongod`: re-using an
index name for a **different key spec** is rejected with `IndexKeySpecsConflict`
(code 86), re-using a name with the **same key but different options** is
rejected with `IndexOptionsConflict` (code 85), and an **identical** re-create
returns `note: "all indexes already exist"` so drivers report it as a no-op
rather than a fresh build. This was surfaced by the mongo-cxx-driver gauge
(`create_index tests/fails`, `index_view/fails for same name`, `fails for same
keys and options`), and applies to every driver's `createIndex` / index-view
API.

#### Fixed
- `createIndexes`: same-name-different-key now errors `IndexKeySpecsConflict`
  (86); identical re-creates now carry `note: "all indexes already exist"`
  (`storage.create_index` / `commands._create_indexes`). Same-name-different-
  options continues to error `IndexOptionsConflict` (85).

### An eleventh conformance gauge: the MongoDB C# / .NET driver

SecantusDB now also measures itself against the official MongoDB **C# / .NET**
driver — the one the .NET, Unity, and Xamarin ecosystems build on — bringing the
gauge count to eleven. The gauge runs the driver's own xUnit suite via `dotnet
test` against an embedded SecantusDB daemon, with `MONGODB_URI` pointed at it,
scoped to the CRUD specification conformance tests
(`MongoDB.Driver.Tests.Specifications.crud`). `MongoDB.Driver.Tests` as a whole
is enormous and dominated by non-server unit tests and external-service suites
(client-side encryption, Atlas Search, multi-node transactions), so the CRUD
spec runner is the focused, bounded conformance slice — expandable to more spec
families over time. The driver's `[RequireServer]` attribute self-skips tests
whose server-version or topology requirements a single node doesn't meet.

Run it with `invoke validate-dotnet` (needs the .NET SDK and `gpg` — the driver's
encryption project verifies a downloaded libmongocrypt during build). It joins
the weekly `validate.yml` matrix and the cross-driver summary.

#### Added
- `dotnet_validation/` gauge package (`runner` / `generate_report` /
  `include_paths`) and the `invoke validate-dotnet` task; wired into `invoke
  validate-all`, the cross-driver summary (`validation_summary`), and CI.

### Two new conformance gauges: the MongoDB C and C++ drivers

SecantusDB now measures itself against the official MongoDB **C** (`libmongoc`)
and **C++** (`mongocxx`) drivers — bringing the gauge count to ten. libmongoc is
the lowest-level official client, the one the PHP, Ruby, and PyMongo
C-extensions ultimately wrap; mongocxx is the modern C++ driver built on top of
it. Both gauges build the driver's own test suite from source and run it,
unmodified, against an embedded SecantusDB daemon — `test-libmongoc` (curated
CRUD / cursor / aggregation / command / GridFS / index suites over
`MONGOC_TEST_URI`) for C, and the mongocxx `test_driver` Catch2 suite for C++.
Strict native clients surface type- and wire-shape divergences that more
permissive drivers accept silently.

Two wrinkles were worth solving. First, libmongoc's test fixture probes the
server with `replSetGetStatus` and aborts the whole run on an unexpected error,
so SecantusDB now answers it like a standalone `mongod` —
`NoReplicationEnabled` (code 76), "not running with --replSet" — which driver
harnesses special-case as "standalone, skip the replica-set-only paths". Second,
mongocxx's tests hard-wire `mongodb://localhost:27017` with no environment
override, so the C++ gauge binds its daemon on port 27017 and refuses to run if
something else already holds it (it won't gauge a foreign server).

Run them with `invoke validate-c` / `invoke validate-cxx` (both need `cmake` and
a C/C++ toolchain; the first run builds the drivers, later runs reuse the cached
builds). Both join the weekly `validate.yml` matrix and the cross-driver summary.

#### Added
- `c_validation/` and `cxx_validation/` gauge packages (`runner` /
  `generate_report` / `include_paths` each) and the `invoke validate-c` /
  `invoke validate-cxx` tasks; wired into `invoke validate-all`, the cross-driver
  summary (`validation_summary`), and CI.
- `replSetGetStatus` command: returns the standalone-mongod
  `NoReplicationEnabled` error so single-node-aware driver test fixtures skip
  replica-set-only behaviour instead of aborting.

### The in-process Rust engine selection is fully removed

The transitional in-process engine selection — `SECANTUS_ENGINE=python|rust|auto`,
the `--engine` CLI flag, and the `SecantusDBServer(engine=...)` constructor
parameter — has been removed entirely. It was already inert since 0.5.3b3 (no
operator module delegated to the `_secantus_core` extension any more), so this is
a no-op for behaviour: the Python server has been pure-Python end to end for
several releases. This change deletes the dead surface so there is one obvious
way things work.

SecantusDB's Rust implementation lives in the **separate Rust server** (and the
standalone `secantusdb` binary), not in this package's request path. The
`secantus-core` wheel remains as the engine library and the parity-test oracle
that pins each Rust engine byte-for-byte against its pure-Python counterpart.

#### Removed
- The `--engine` CLI flag and the `engine=` parameter on `SecantusDBServer`.
  Passing `engine=` now raises `TypeError` (the flag had no effect since 0.5.3b3).
- The `secantus.engine` module (the inert compatibility stub) and its
  `tests/test_engine.py`.
- The `SECANTUS_ENGINE=rust` full-suite CI step, which re-ran the test suite
  under a now-dead env var and was a source of flaky worker-crash failures.

### Point-in-time recovery: restore the database as it was at any moment in its oplog

SecantusDB already kept a mongod-shaped oplog and could take consistent
WiredTiger backup archives; this release joins the two into real point-in-time
recovery. Given a backup (or a stopped server's data directory), you can now
rebuild a fresh database as it was at any target timestamp by replaying the
oplog forward — documents, in-place updates, deletes, collection options
(`capped` / `size` / `max` / `validator` / `viewOn` / …), and index / `collMod` /
rename DDL are all reconstructed through the ordinary write paths, so the result
is indistinguishable from the live database at that instant.

Recovery is offline, matching real `mongod`: it writes a fresh data directory
you then start a new server on. Drive it from the CLI (`secantusd-py restore
--source <archive|dir> --target-dir <dir> [--to-time <ISO> | --to-timestamp
<secs,ord>]`) or the `secantusAdmin.restoreToTimestamp` admin command. A
multi-document transaction is always replayed all-or-nothing — its statements
share one commit timestamp, so a recovery point never lands mid-transaction.
Out of the box the recovery window is the oplog retention window (tune
`--oplog-retention-seconds` / `--oplog-max-entries` for the horizon you need). To
reach further back, turn on oplog archiving (`--oplog-archive-dir`) and take
periodic base snapshots: SecantusDB then keeps the dropped oplog on disk and
recovery stitches the newest snapshot before your target together with the
archived oplog, so any moment in the archived history is reachable without keeping
the whole oplog live. See [Backup & point-in-time recovery](recovery.md).

#### Added
- `secantus.diff.apply_update_description` — applies a `$v: 2`
  `updateDescription` back to a document (the inverse of
  `compute_update_description`); the keystone of oplog replay.
- `secantus.oplog_replay` — `replay()` / `restore_to_timestamp()` /
  `restore_archive_to_timestamp()`: replays an oplog source into a fresh store,
  stopping at a target `ts` / wall-clock time.
- `Storage.replay_mode()` — a context manager that suppresses oplog emission so
  replay drives the real write paths without regenerating the oplog.
- `secantusd-py restore` CLI subcommand and the `secantusAdmin.restoreToTimestamp`
  wire command.
- Backup archives now embed a `pitr-manifest.json` describing their recoverable
  oplog range (`Storage._pitr_manifest`).
- Collection options (`capped` / `size` / `max` / `validator` / `viewOn` / …) now
  ride the `create` oplog entry, so PITR replay reconstructs them on the restored
  collection (previously only documents and indexes were). `Storage.create_collection`
  gained an `options=` argument; the same options now also surface in the
  `show_expanded_events` change-stream `create` event's `operationDescription`.
- `--preserve-oplog` (`secantusd-py restore`) / `preserveOplog: true`
  (`secantusAdmin.restoreToTimestamp`) carries the replayed oplog onto the
  restored directory verbatim, so a change stream on the restored server can
  resume from a token minted *before* the restore point. The default still starts
  a fresh oplog timeline, matching `mongorestore`. Backed by
  `Storage.import_oplog_segment`.
- **PITR v2 — arbitrary-window recovery** (`secantus.pitr_archive`). A server
  started with `--oplog-archive-dir <dir>` (`[oplog] archive_dir`) archives the
  oplog rows `prune_oplog` is about to drop into durable segment files first;
  `secantusAdmin.archiveBaseSnapshot` / `Storage.archive_base_snapshot` take base
  snapshots into the same directory. Recovery then accepts that **archive
  directory** as the `restore` source (CLI / wire auto-detect it): it picks the
  newest base snapshot at or before the target time and stitches the archived
  oplog forward onto it. This lifts the v1 genesis-intact restriction — a restore
  can reach a time *before* the live oplog floor, without keeping the entire oplog
  live. Base snapshots are taken on demand (no background scheduler, matching
  `prune_ttl` / `prune_oplog`).

## [0.5.3b13] — 2026-06-16

### `find()` with no sort now returns documents in insertion order

An unsorted `find()` now returns documents in **insertion order**, matching
`mongod`'s natural (storage) order. Previously SecantusDB returned them in `_id`
order — which coincides with insertion order for the default monotonic
`ObjectId` `_id`s, but diverged whenever a collection mixed `_id` types or used
non-monotonic `_id`s (an int `1`, a string `"foo"`, a sub-document — BSON sorts
those very differently from the order you inserted them). Code and drivers that
read back rows in the order they were written — a common, reasonable assumption
that `mongod` honours — now see the same order here.

Internally this adds a small natural-order index (a monotonic insertion sequence
→ document map) that an unsorted scan and the `$natural` hint walk; the document
store itself is unchanged, so every `_id` lookup, secondary index, and
uniqueness check is untouched. Capped-collection eviction and equal-key sort
tie-breaks also follow insertion order now. (Multi=false `updateOne`/`deleteOne`
without a sort still pick the `_id`-order-first match rather than the
insertion-first one — a smaller remaining divergence, tracked in the backlog.)

#### Fixed
- Unsorted `find()` (and the `$natural` hint) return documents in insertion
  order, matching `mongod` — including for collections with mixed-type or
  non-monotonic `_id`s. Clears the PHP `BulkWrite::testInserts` /
  `bulkwrite-insert-004` conformance tests.

## [0.5.3b12] — 2026-06-16

### `count` honours a `hint`, and 2dsphere indexes report their version

Two driver-conformance fixes. `count` now respects a `hint`: pass an index name
or key pattern and the count walks that index — so hinting a **sparse** index
counts only the documents present in it. `count({}, hint: "sparse_idx")` returns
the number of docs that have the indexed field, not the whole collection, exactly
as `mongod` does. Previously `count` ignored the hint and always counted every
document.

Separately, `2dsphere` indexes now carry a `2dsphereIndexVersion` in their
`listIndexes` output (version 3, mongod's format since 3.2), so drivers that
introspect geo indexes — like the PHP library's `IndexInfo::is2dSphere()` — read
the field they expect.

#### Added
- `2dsphereIndexVersion` (3) on `2dsphere` indexes, surfaced via `listIndexes`.

#### Fixed
- `count` now honours `hint` (index name or key pattern), counting via the
  hinted index — including sparse-index semantics (missing-field docs excluded).

## [0.5.3b11] — 2026-06-16

### `serverStatus` reports a live open-cursor count

`serverStatus` now includes `metrics.cursor.open.total` — the number of cursors
currently registered on the server. It rises by one when a batched query leaves
a cursor open for `getMore` and returns to its baseline once the cursor is
exhausted or killed (via `killCursors`, which drivers send when a cursor object
is destroyed). Tools and drivers that watch cursor lifecycle — including the PHP
extension's cursor-destruct test — can now see cursors open and close.

The value is read live from the server's `CursorRegistry`, so it reflects the
true set of not-yet-exhausted, not-killed cursors at the moment of the call.

#### Added
- `serverStatus.metrics.cursor.open.total` (live open-cursor count from the
  `CursorRegistry`).

## [0.5.3b10] — 2026-06-16

### `collMod` can retune a TTL index's expiry

`collMod` now handles its `index` modification form: pass
`{collMod: "<coll>", index: {keyPattern: {...}, expireAfterSeconds: N}}` (or
`{index: {name: "<idx>", expireAfterSeconds: N}}`) and SecantusDB rewrites the
TTL index's expiry in place, returning the `expireAfterSeconds_old` and
`expireAfterSeconds_new` pair that `mongod` echoes. The new value takes effect
immediately — `prune_ttl` reads the expiry from the same index options — so a
retuned TTL window applies on the next prune.

Previously `collMod` accepted the command but ignored the `index` form,
returning a bare `{ok: 1}` with neither the old nor new expiry. The PHP
library's `ModifyCollection` and `Database::modifyCollection` tests assert both
values; both now pass.

#### Added
- `collMod` `index` form for TTL retuning: resolves the target index by
  `keyPattern` or `name`, updates `expireAfterSeconds`, and returns
  `expireAfterSeconds_old` / `expireAfterSeconds_new`. Backed by a new
  `Storage.set_index_expiry`.

## [0.5.3b9] — 2026-06-16

### Duplicate-key errors now read exactly like `mongod`

When a write hits a unique-index collision, the `E11000` error message now
matches `mongod` verbatim: `E11000 duplicate key error collection: <db>.<coll>
index: <indexName> dup key: { <field>: <value> }`. Previously SecantusDB emitted
a terser, non-standard form (`… in index <name>: _id=<value>`) that worked for
permissive drivers but failed the type-strict ones that pin the message text —
the PHP extension's `WriteError::getMessage()` and `WriteResult::getWriteErrors()`
assert the full string, down to the shell-formatted `dup key` fragment.

The structured fields drivers parse — `code`, `index`, `keyPattern`, `keyValue`
— were already correct; this is purely the human-readable message catching up,
across every path that can raise a duplicate key (batch insert, update/upsert,
and unique-index builds). The fix clears the PHP extension's `writeError` and
`writeResult` suites.

#### Fixed
- Duplicate-key (`E11000`) error messages now use `mongod`'s exact wording,
  including the `collection: <ns> index: <name> dup key: { … }` shape with
  shell-formatted key values, consistently across all duplicate-key raise sites.

## [0.5.3b8] — 2026-06-16

### `explain` now speaks `allPlansExecution`, and aggregate's inline explain flag works

`explain` gained the last two pieces the official MongoDB drivers probe for. At
the most verbose level, `verbosity: "allPlansExecution"`, the reply now carries
an `allPlansExecution` array inside `executionStats` — empty, because a
single-node query is always served by a single candidate plan with no rejected
alternatives, which is exactly what real `mongod` reports when there's no
multi-planning to summarise. Drivers that assert the key is present (and absent
at lower verbosities) now see the shape they expect.

The `aggregate` command also learned to honour its legacy inline `explain: true`
flag — the form drivers send when you call `explain()` on an aggregation rather
than wrapping it in the top-level `explain` command. SecantusDB previously ran
the pipeline and returned data; it now returns the explain plan instead, and
critically does **not** execute a trailing `$out` or `$merge` write stage under
explain, matching `mongod`'s dry-run behaviour. Together these clear the PHP
library's entire `ExplainFunctionalTest` and aggregate-explain suite.

#### Fixed
- `explain` with `verbosity: "allPlansExecution"` now includes an
  `executionStats.allPlansExecution` array (empty for single-solution plans),
  on both `find`-style and `aggregate` explains.
- The `aggregate` command's inline `explain: true` flag now returns the explain
  document (`stages` / `queryPlanner`) instead of running the pipeline, and
  suppresses `$out` / `$merge` writes under explain.

## [0.5.3b7] — 2026-06-15

### `$exists: true` rides a sparse index instead of scanning the collection

A query of the form `{field: {$exists: true}}` now uses a sparse single-field
index on `field` when one exists, instead of falling back to a full collection
scan. A sparse index holds an entry for exactly the documents where the field is
present — missing-field documents are omitted, present-but-`null` and array
values keep an entry — so the complete set of index entries *is* the
`$exists: true` match set. The planner walks the whole index (no value bound),
and `explain` reports `IXSCAN` accordingly. A non-sparse index still can't serve
`$exists: true` (it has an entry per document, including the absent ones), and
`$exists: false` never uses a sparse index — both correctly stay on `COLLSCAN`.
Results were always correct; this is the missing fast path.

#### Added

- `{field: {$exists: true}}` uses a sparse single-field index (IXSCAN) when one
  is present, via `Storage._sparse_index_for_exists` + `_all_id_keys_for_index`,
  mirrored in `explain_plan`. Non-sparse indexes and `$exists: false` stay on
  COLLSCAN.

#### Fixed

- The three pymongo DBRef-spec tests (`test_dbref.py::TestDBRefSpec`) are now
  deselected from the gauge. They are pure client-side BSON codec tests that
  never exercise SecantusDB; they pass under plain unittest but crash the
  gauge's `-n1` xdist worker because execnet can't pickle the `ObjectId` in
  their `subTest` params (`DumpError`). Deselecting them keeps the gauge run
  clean and stops three spurious failures from being attributed to the server.

### Fixed a shutdown race that could crash the server process

Stopping a `SecantusDBServer` now drains its in-flight per-connection threads
before tearing down WiredTiger. Previously `stop()` joined only the accept
thread and then closed the storage engine — so a connection handler still
mid-WiredTiger-operation (e.g. a change-stream tailable `getMore` reading the
oplog) had its WT connection freed underneath it: a use-after-free that surfaced
as an intermittent native crash (the pytest-xdist worker death seen near the end
of the full suite under churn). `stop()` now closes every connection socket to
unblock reads, wakes any tailable `getMore` parked on the oplog condition
variable, and waits for the active-connection count to reach zero before calling
`storage.close()`. A 200-iteration stress that reliably tripped the use-after-
close now runs clean.

Waking those parked reads is platform-specific, and the first cut got it wrong
on both ends. On POSIX, `shutdown(SHUT_RDWR)` wakes a `recv` blocked in another
thread while leaving the descriptor valid; calling `close()` from the stopping
thread instead does *not* wake the parked `recv` and frees the fd number for
immediate reuse, leaving the handler blocked forever on a recycled descriptor —
so the drain barrier timed out. On Windows the opposite holds: `shutdown` does
not interrupt an already-blocked `recv`, so `closesocket` is required. The wake
is now `shutdown`-only on POSIX and `shutdown`-then-`close` on Windows. The drain
barrier also re-runs the socket wake on every poll, not just once up front: the
accept thread bumps the active-connection count and spawns the handler *before*
the handler registers its socket, so a connection accepted in the instant before
`stop()` could register after the initial sweep and never be woken — re-sweeping
catches it within milliseconds.

#### Fixed

- `SecantusDBServer.stop()` drains in-flight connection threads before closing
  WiredTiger (via `ConnectionRegistry.close_all` + `Storage.signal_shutdown` +
  an active-connection drain barrier), eliminating a use-after-free / native
  crash on teardown under load.
- The stop-time socket wake is now platform-correct: `shutdown`-only on POSIX
  (closing the fd from another thread left handlers blocked on a recycled
  descriptor and timed out the drain), `shutdown`+`close` on Windows (where
  `shutdown` alone doesn't interrupt a blocked `recv`). The drain barrier
  re-sweeps each poll so a connection that registers its socket just after
  `stop()` begins is still woken.

### Tailable cursors over `local.oplog.rs`

A client can now tail the oplog the way replication does: `local.oplog.rs`
accepts `TAILABLE_AWAIT` find cursors and streams oplog entries as they're
written. Two pieces landed for this — the synthetic oplog view is now reported
as a capped collection by `collection_is_capped` (so a tailable cursor isn't
rejected), and a dedicated oplog tailable producer reads new entries by oplog
seq (oplog documents have no `_id`, so the ordinary capped-collection tail path
doesn't apply). `find().sort("$natural", ...)` is honoured against the view —
the oplog's only meaningful order.

To match mongod — whose oplog is never empty (its first entry is the replica
set's "initiating set" noop) — a freshly-started server now seeds one bootstrap
noop into the oplog, so a client can tail `local.oplog.rs` before any user
write. The seed is an `op: "n"` entry (skipped by change-stream projection, so
it never surfaces as a change event) and only fires on a truly fresh oplog.
Closes the pymongo gauge's `test_cursor.test_to_list_tailable`.

#### Added

- `TAILABLE_AWAIT` find over `local.oplog.rs` (via `_find_tailable_oplog`), and
  `$natural` sort on the oplog view.
- A bootstrap oplog noop seeded at server start (`Storage.ensure_oplog_bootstrap`)
  so `local.oplog.rs` is never empty, matching mongod.

### The Python server is pure Python — no Rust dependency — and preserves numeric types

The `secantus` package no longer imports or calls any Rust component. The
original in-process engine-swap — where each operator module could delegate to
the optional `_secantus_core` extension under `SECANTUS_ENGINE=rust` — has been
retired in favour of the two-separate-servers model: the Python server is the
pure-Python implementation, end to end, and the Rust engines live only in the
standalone Rust server (and in the parity-oracle test suites, which import the
extension directly rather than through this package). `secantus.engine` remains
as an inert compatibility stub so `SecantusDBServer(engine=...)` keeps working.

Decoupling the engines let the Python operator engines adopt MongoDB's numeric
type promotion (int32 < int64 < double < decimal128) without being pinned to a
not-yet-updated Rust port. `$inc`, `$mul`, and the `$sum` accumulator now
preserve the BSON numeric type of their result — `Int64(5)` incremented by `3`
stays `Int64(8)` instead of narrowing to int32 on the wire — so a client codec
that keys on the BSON 64-bit type round-trips correctly. This closes the pymongo
gauge's `test_custom_types` aggregate/findAndModify decoder cases.

#### Changed

- `secantus` is now pure Python with no Rust import in the request path; the
  `SECANTUS_ENGINE` in-process accelerator is retired (the Rust engines moved to
  the standalone Rust server). `secantus.engine.available()` / `enabled()`
  always report Python.

#### Fixed

- `$inc` / `$mul` / `$sum` preserve the BSON numeric type per mongod's promotion
  rules (int32 < int64 < double < decimal128) via the new `secantus.numerics`
  helpers, instead of narrowing 64-bit results to int32.

### `find` honours `returnKey` and `showRecordId`

`find` now supports the `returnKey` and `showRecordId` cursor options. With
`returnKey: true` each result is reduced to just the keys of the index that
serves the query — the index's key-pattern fields plus the sort fields (a sort
by `_id`, served by the document table's natural order, yields `{_id: <value>}`).
With `showRecordId: true` each document is tagged with a `$recordId`; when
`returnKey` is also set, `showRecordId` adds nothing, matching `mongod`. Closes
the pymongo gauge's command-monitoring `find with showRecordId and returnKey`.

#### Added

- `returnKey` (project results down to the serving index's key fields) and
  `showRecordId` (`$recordId` tag) options on the `find` command.

### `createIndexes` accepts and ignores the deprecated `dropDups` option

`dropDups` was removed in MongoDB 3.0, but modern `mongod` still accepts it on
the wire and silently ignores it rather than rejecting the index spec. SecantusDB
now matches that: passing `dropDups` no longer trips the unknown-field guard.
The practical upshot is that building a `unique` index over data that already
contains a duplicate fails on the duplicate with `DuplicateKey` (11000) — a
`DuplicateKeyError` to the driver — exactly as a real server does, instead of an
unrelated "unknown field" error. The collection is left untouched and no index is
created. Closes the pymongo gauge's `test_collection.test_index_dont_drop_dups`.

#### Changed

- `createIndexes` accepts `dropDups` and strips it from the stored index
  options (deprecated, ignored — never drops duplicates).

### Partial indexes serve range-on-indexed-field queries with a residual clause

A query that puts a range on a partial index's indexed field and an extra
clause that the index's partial filter absorbs now uses the index — e.g.
`find({x: {$gt: 1}, a: 1})` against an index on `x` with
`partialFilterExpression: {a: {$lte: 1.5}}`. The `x` range rides the index,
the `a: 1` clause is implied by the partial filter (so the index's existence
already guarantees it) and is rechecked by the exact post-scan matcher, and
`explain` reports `IXSCAN` with `isPartial: true`. Previously any multi-field
filter fell off the single-field index path to a COLLSCAN.

The relaxation is deliberately conservative: only *partial* indexes get this
treatment, and only when every residual field is a partial-filter field, so a
non-partial residual still keeps the query on a collection scan. This closes
the last open assertion in the pymongo gauge's `test_collection.test_index_filter`.

#### Changed

- The single-field index lookup and its `explain` mirror now accept a
  multi-field filter when the non-indexed fields are absorbed by an implied
  partial filter, via a shared `_single_field_partial_residual_match` selector.

### Tailable cursors die on capped-collection rollover

A tailable cursor over a capped collection now dies with `CappedPositionLost`
when the collection rolls over and evicts the document the cursor was anchored
on — exactly as `mongod` does. Before, the cursor would blithely keep
streaming the post-rollover documents instead of recognising it had been
lapped. The server detects this by comparing the cursor's last-returned
position against the collection's current oldest document; if the anchor has
been evicted it returns error 136, which `pymongo` swallows for tailable
cursors (the cursor reports `alive == False` and the in-flight read yields
nothing). Closes the pymongo gauge's `test_cursor.test_tailable`.

#### Fixed

- Tailable cursors on capped collections now surface `CappedPositionLost`
  (code 136) when rollover evicts their anchor document, instead of
  continuing to stream the rolled-over documents.

### Change streams report create, modify, and richer DDL events

Change streams opened with `showExpandedEvents: true` now surface the full
set of expanded DDL events that `mongod` 6.0+ emits. A `createCollection`
(including views) produces a `create` event, a `collMod` produces a
`modify` event, and `rename` events carry an `operationDescription` with
the destination namespace and the dropped target's UUID under
`dropTarget`. CRUD events (insert / update / delete / replace) on an
expanded stream also carry the watched collection's `collectionUUID`, the
way a real server tags them.

Previously only `createIndexes` / `dropIndexes` were emitted as expanded
events; `create` and `modify` had no oplog entry at all, so a stream
waiting for them blocked indefinitely. This completes the
`showExpandedEvents` spec surface that single-node SecantusDB can support
(sharding-only events like `shardCollection` remain out of scope), taking
the pymongo change-stream gauge from 102 to 106 passing — a clean sweep of
`test_change_stream.py`.

#### Added

- `create` (createCollection / views) and `modify` (collMod) change-stream
  events under `showExpandedEvents`, both gated off by default like the
  other expanded events.
- `operationDescription.{to,dropTarget}` on expanded `rename` events, and
  `collectionUUID` on expanded CRUD events.

### Resumed change streams return their backlog on open

Opening a change stream with `resumeAfter`, `startAfter`, or
`startAtOperationTime` now returns the already-committed backlog — the
events between the resume point and now — in the aggregate's `firstBatch`,
exactly as `mongod` does. Previously every change-stream open returned an
empty `firstBatch` and deferred all events to the first `getMore`. That
was invisible to most consumers, but a driver that inspects the cursor
for buffered data *before* issuing any `getMore` (pymongo's
`CommandCursor._has_next()`, which never sends one itself) saw nothing
and reported the stream as empty.

A fresh tail watch has no backlog, so it still opens with an empty
`firstBatch` — the change is scoped to the resuming forms. And because a
non-empty `firstBatch` means pymongo doesn't overwrite its cached resume
token from the open response, an uniterated resumed stream now correctly
reports `resume_token` equal to the token the caller passed in. Closes
the pymongo gauge's `test_resumetoken_uniterated_nonempty_batch_*`
(change-streams prose test #14), lifting the change-stream gauge from
100 to 102 passing.

#### Fixed

- Resumed change-stream opens (`resumeAfter` / `startAfter` /
  `startAtOperationTime`) return their committed backlog in `firstBatch`
  instead of deferring every event to the first `getMore`, so a driver
  that checks for buffered data before any `getMore` sees the events and
  an uniterated resumed stream reports the correct `resume_token`.

### Profiler op-class for `distinct` and `count`

`system.profile` entries for `distinct` and `count` are now recorded
under `op: "command"`, matching `mongod` — where only `find` carries
`op: "query"`. The previous bucketing filed both under `op: "query"`, so
a profile query like `{op: "command", "command.distinct": "<coll>"}`
found nothing. Monitoring tooling that slices the profiler by operation
class now sees the same shape it would against a real server.

This closes the pymongo gauge's `test_cursor.test_comment`. The OP_MSG
exhaust-cursor mid-stream-fault hardening shipped earlier this cycle
also gained a dedicated regression test (a synthetic mid-stream
`getMore` fault must terminate the stream with a `moreToCome`-clear
reply, never drop the connection).

#### Fixed

- `distinct` / `count` profiler entries use `op: "command"` (were
  `op: "query"`), so `system.profile` queries that filter by operation
  class find them.

### OP_MSG exhaust cursors

Exhaust cursors (`CursorType.EXHAUST`) now stream over the wire the way
a real `mongod` does. When a driver sets the OP_MSG `exhaustAllowed`
flag on a `getMore`, SecantusDB streams every remaining batch back over
the same socket using the `moreToCome` flag — one round trip instead of
a `getMore` per batch — and closes the stream with a trailing empty
reply carrying `id: 0`. That trailing empty batch is what makes a real
server keep the cursor alive until the client has drained it; pinning it
faithfully is why pymongo's command monitor sees `find, getMore,
getMore, getMore` for three documents at `batchSize: 1`, and why
exhaust-pinned connections return to the pool at exactly the right
moment.

This closes the last wire-protocol gap behind the pymongo gauge's
`test_exhaust` / `test_exhaust_cursor_db_set` cases. The streaming is
driven entirely in the connection loop (`SecantusDBServer._stream_exhaust_getmore`)
off the existing cursor registry, so no operator engine or storage path
changed; `find` / `aggregate` replies that open a cursor are still sent
as a single message (mongod streams only on `getMore`).

#### Added

- OP_MSG exhaust-cursor streaming: a `getMore` with the `exhaustAllowed`
  flag streams all remaining batches with `moreToCome`, ending in a
  trailing empty `id: 0` reply (mongod parity). Tailable / awaitData
  cursors that yield nothing fall back to ordinary `getMore` rather than
  spin the stream. A mid-stream getMore that raises unexpectedly still
  terminates the stream with a `moreToCome`-clear reply, so the client
  never sees "Server ended moreToCome unexpectedly".

### Parse-time update validation, partial-index range implication

`update` now rejects an unknown modifier (`$thismodifierdoesntexist`) at
parse time with code 9, even against an empty collection — matching
mongod, which validates the update before matching any document (the
per-document apply path would never see an unmatched update).
`createIndexes` rejects a malformed `partialFilterExpression` (a
non-document, an unknown operator, a logical operator with a non-array
argument). And a partial index whose filter uses a range operator
(`{a: {$lte: 1.5}}`) is now used when the query provably implies it (an
equality `a: 1`, or `a: {$lt: 1}`) — a sound, conservative range
implication that errs to a full scan rather than risk missing
documents; `explain` flags such a scan with `isPartial`.

#### Added

- Sound range implication for partial indexes (`$eq`/`$lt`/`$lte`/`$gt`/
  `$gte`), with `isPartial` in the explain IXSCAN stage.

#### Fixed

- `update` rejects an unknown modifier at parse time (code 9), even on
  an empty collection.
- `createIndexes` rejects a malformed `partialFilterExpression`.

### Upsert subdocument _id, and idempotent drop with write concern

Two real correctness fixes. An upsert whose filter pins `_id` to a
subdocument value (`{_id: {f: ..., f2: ...}}`) now seeds that `_id`
into the inserted document instead of generating a fresh ObjectId —
the seed extraction was skipping every dict-valued filter field to
avoid copying operator expressions (`{$gt: 5}`), but a literal
subdocument is a real equality and must be kept. And `drop` of a
non-existent collection now returns `{ok: 1}` (idempotent, as modern
mongod does) rather than `NamespaceNotFound`, which also lets an
unsatisfiable write concern surface its `writeConcernError` on the
reply.

#### Fixed

- Upsert seeds a subdocument `_id` from the filter (operator
  expressions are still correctly excluded).
- `drop` of a non-existent collection is idempotent (`{ok: 1}`) and
  honours an unsatisfiable write concern.

### Cursor min() / max() index bounds

The find command's `min` / `max` cursor options are now honoured: they
bound a hinted index scan, with `max` an exclusive upper bound and
`min` an inclusive lower bound on the index key (mongod semantics).
Bounds and documents are compared with the same direction-aware
byte-sortable key encoder the indexes use, so cross-type ordering and
per-field direction are correct. A bound whose field order doesn't
match the hinted index's key pattern is rejected with mongod's 51174.

#### Added

- Cursor `min` / `max` index-bound options on `find` (oracle-pinned
  against mongod; 51174 on a key-pattern mismatch).
- **Rust server:** change streams (R3b-a) — `aggregate` with a leading
  `$changeStream` now opens a tailable oplog cursor instead of
  rejecting, and tailable `getMore` projects insert / update / replace /
  delete events (with `documentKey`, `updateDescription`,
  `updateLookup` `fullDocument`, pre-images, and a resume token under
  `_id`). The projector runs behind a new WT-free `Storage` trait seam
  (`change_stream_poll` / `wait_for_oplog` / oplog accessors) so the
  command crate stays WiredTiger-free. Measured **+58** on the R8
  rust-server gauge (936 → 994 of 1713, zero regressions; 52 are
  `test_change_stream.py`). `awaitData` blocking, resume tokens, and
  invalidation cursor-close land in R3b-b.

### Clustered collections

The `clusteredIndex` create option is now supported. mongod uses it to
make `_id` the collection's clustering key — which is exactly
SecantusDB's WiredTiger layout already (the document table is keyed by
`_id`), so this is a metadata-and-reporting feature: the option is
validated at `create` (only `{_id: 1}` with `unique: true`, mongod's
two rejection codes), echoed in `listCollections.options.clusteredIndex`
with its `v` and defaulted name, and reported by `listIndexes` as a
single entry carrying `clustered: true` (a clustered collection has no
separate `_id_` index). Secondary indexes coexist normally.

#### Added

- `clusteredIndex` create option (`create` / `listCollections` /
  `listIndexes`), oracle-pinned against mongod.

### Matcher correctness, the validate command, and upsert _id fidelity

Continuing the honest-gauge triage, this slice fixes two genuine
correctness bugs the gauge surfaced. Embedded-document equality is now
field-order-sensitive and exact, recursively — `{size: {h: 14, w: 21}}`
matches a document only when `size` is exactly that, in that key order
(a documented mongod gotcha that Python's order-insensitive `dict ==`
got wrong). And an upsert whose resulting `_id` is `None` now reports
`did_upsert` correctly: `None` was doubling as the "no upsert"
sentinel, so a legitimate `{_id: null}` upsert looked like a no-op to
the driver.

The `validate` command is implemented — a clean, mongod-shaped
consistency report (real record and index counts; SecantusDB's
WiredTiger-backed storage has nothing to repair), including mongod's
rejection of `full` + `background` together.

#### Added

- `validate` command (collection consistency check; `full`/`background`/
  `scandata` options, full+background rejected with InvalidOptions).

#### Fixed

- Embedded-document equality is order-sensitive and exact, recursively,
  with numeric-bridged leaves (matcher correctness; both query engines —
  the Rust core already deferred Document/Array equality to Python).
- Upsert with a `None` `_id` reports `did_upsert` and the upserted `_id`
  correctly (update and findAndModify paths).
- **Rust server:** cluster-time gossip — the Rust server now attaches
  `$clusterTime` (keyless signature) and `operationTime` to every reply
  when the replica-set persona is on, matching mongod and the Python
  server (shipped in 0.5.2b19). Reads observe the clock via the new
  `secantus_storage::Storage::peek_cluster_time` without advancing it;
  standalone mode stays gossip-free. Measured +6 on the R8 rust-server
  gauge (930 → 936 of 1713, zero regressions): the `$clusterTime`-gossip,
  causal-consistency, and transaction-commit tests that read
  `operationTime`. Closes a documented Rust-server gap (backlog §7).


### The honest-gauge triage: projection, size caps, snapshot reads, and change-stream fidelity

The first honest pymongo-gauge run (94.8%) left a 64-failure triage list;
this slice clears the bulk of it. Projection gained mongod's exact
semantics for three long-standing divergences — `{_id: 1}`-only specs
are inclusion projections, dotted paths fan out over arrays (with
`{}`-skeleton preservation), and `$slice` interacts with explicit `_id`
correctly — fixed in both the Python and Rust engines with the parity
corpus extended to pin every oracle-checked case. Writes now enforce
`maxBsonObjectSize` server-side with mongod's codes and wording (10334
on insert and update-growth, 17420 on upsert).

Snapshot sessions work end-to-end: `readConcern: {level: snapshot}` is
accepted on find/aggregate/distinct (and their cursor continuations)
under the replica-set persona, with `atClusterTime` stamped on replies
for session pinning — and still rejected like a real standalone when
the persona is off. The `$$NOW` system variable landed as part of the
same path, seeded per-operation for every command's `let` scope.

Change streams got the biggest batch: events that project out the
resume token now fail with mongod's 280 `ChangeStreamFatalError` and
the `NonResumableChangeStreamError` label instead of being silently
swallowed; `fullDocument: required/whenAvailable` follow post-image
semantics (error/null when `changeStreamPreAndPostImages` is off);
`resumeAfter` rejects invalidate-event tokens (260) while `startAfter`
accepts them; `readConcern: local` on `$changeStream` is rejected;
unknown pipeline stages return mongod's 40324 at aggregate time;
pipeline-form updates emit `update` events (with `truncatedArrays`)
instead of `replace`; and `updateDescription.disambiguatedPaths` is
computed for ambiguous numeric-string field names — in both engines,
parity-pinned.

#### Added

- `$$NOW` aggregation system variable (constant per operation, all
  command `let` scopes).
- `updateDescription.disambiguatedPaths` on change-stream update
  events (Python + Rust diff engines).
- `atClusterTime` on snapshot-read replies (cursor and top-level).

#### Fixed

- Projection: `_id`-only inclusion, dotted-path array fan-out, dict
  skeletons, `$slice`+`_id` interaction (both engines).
- Server-side `maxBsonObjectSize` enforcement (10334 / 17420).
- Change streams: 280 + non-resumable label for projected-out resume
  tokens, post-image semantics for required/whenAvailable, invalidate
  tokens rejected by resumeAfter (260), local readConcern rejected,
  40324 for unknown stages at create time, pipeline updates as diff
  events, disambiguatedPaths.
- `AggregateError` can carry mongod-specific codes (40324).


### Real multi-document transactions

`commitTransaction` and `abortTransaction` were the last true stubs in
the Python server: they returned `{ok: 1}` while every operation
"inside" a driver transaction took effect immediately and could never
roll back. They're real now. Each transaction owns a dedicated
WiredTiger session — not the connection thread's, because pymongo can
legally send a transaction's statements and its retryable commit on
different pooled connections — and every statement runs with that
session swapped into the storage layer, so snapshot isolation,
read-your-own-writes, and rollback all come straight from the same
engine mongod uses. Oplog entries are buffered and flushed at commit
with one shared commit timestamp plus `lsid`/`txnNumber`, so change
streams never see uncommitted writes and transaction events carry
their session identity, exactly as in mongod.

The server-side state machine (`secantus.transactions`) pins the
spec's resolution table: statements against unknown or aborted
transactions get 251 `NoSuchTransaction` with the
`TransientTransactionError` label, committed ones get 256, stale
`txnNumber`s get 225 `TransactionTooOld`, commit is idempotent (driver
commit retries depend on it), and any failed statement aborts the
transaction server-side. Write-write conflicts between transactions
surface as statement-time 112 `WriteConflict` + transient label;
`count` inside a transaction gets mongod's 263
`OperationNotSupportedInTransaction`. Transactions idle past 60s
(`transaction_lifetime_seconds`) are reaped, `endSessions`/
`killSessions` abort their session's transaction, and `readConcern:
"snapshot"` is now accepted inside transactions (every in-transaction
read runs against the pinned WT snapshot anyway).

### The whole MongoDB CLI toolchain now runs against SecantusDB

The MongoDB Database Tools are strict Go-driver clients, and two of
them couldn't talk to SecantusDB at all: `mongostat` crashed with a Go
nil-pointer panic because `serverStatus` had no `mem` section (the
tool dereferences `mem.supported` unguarded), and `mongotop` failed
outright because the `top` command didn't exist. Both work now —
`serverStatus` reports a real resident-set size under `mem`, and `top`
returns mongod's exact per-namespace shape (counters are zero pending
per-namespace instrumentation; mongotop renders it like an idle
server).

Every connectable tool in the toolchain is pinned by an end-to-end
test in the default suite: `mongosh`, `mongodump`/`mongorestore`,
`mongoimport`/`mongoexport` (NDJSON + CSV, plus canonical-extended-JSON
type fidelity for ObjectId / datetime / Decimal128 / Int64 / Binary),
`bsondump`, `mongofiles` (GridFS put/get/list/delete against pymongo's
gridfs), and single-iteration `mongostat` / `mongotop` probes. The
Go tools also exposed two connection-lifecycle nits, now fixed: an
RST-style hang-up (how Go's pool drops connections) no longer dumps a
traceback through the catch-all handler, and a request racing
`stop()`'s socket close no longer raises `OSError` reading the server
address.

Compass gets the same treatment, headlessly: every command the GUI
issues — the connect-time instance probes, `$collStats` storage
figures, `$sample` schema analysis, `$indexStats`, both explain
verbosities, and the performance-tab polls — is pinned by tests. That
sweep caught `explain`'s `executionStats` reporting hardcoded zeroes
(Compass would render "0 documents returned" for any query); the
server now really executes the query at `executionStats` verbosity,
and aggregate-explain lifts a leading `$match` so it reports the same
IXSCAN decision the real pipeline run uses.

#### Added

- Multi-document transactions: real `commitTransaction` /
  `abortTransaction`, per-transaction WiredTiger sessions
  (`Storage.begin/use/commit/abort_user_transaction`), the
  `secantus.transactions.TransactionRegistry` state machine
  (251/256/225/50911/263/112 + `TransientTransactionError` labels,
  idempotent commit, implicit abort on a newer `txnNumber`, 60s
  lifetime reaping via `SecantusDBServer(transaction_lifetime_seconds=…)`),
  oplog buffering with a shared commit timestamp, and `lsid` /
  `txnNumber` on change-stream events for transactional writes.
  Conformance: `tests/test_transactions.py`,
  `tests/test_transaction_registry.py`, `tests/test_storage_user_txn.py`;
  divergence notes in backlog §3.4.
- Cluster-time gossip: every command reply in replica-set mode now
  carries `$clusterTime` (unsigned-cluster placeholder signature, as
  mongod without auth keys) and `operationTime`, via the non-minting
  `Storage.peek_cluster_time()`. Drivers track these per session and
  echo `readConcern.afterClusterTime` on causally consistent reads and
  transaction starts — the wire shape the transactions /
  causal-consistency unified specs assert.
- `top` command — mongod-shaped per-namespace reply (`totals` with
  `total`/`readLock`/`writeLock`/per-op `{time, count}` sections,
  RBAC `top` action granted via `clusterMonitor`); counters are zero
  (no per-namespace timing instrumentation yet, see backlog §2).
- `serverStatus.mem` section (`bits`/`resident`/`virtual`/`supported`)
  — `resident` is real (getrusage max-RSS).
- CLI-tool conformance tests: `tests/test_mongoimport_export.py`,
  `tests/test_mongofiles.py`, `tests/test_mongostat_mongotop.py`, and
  a `bsondump` dump-format test in `tests/test_mongodump_restore.py`.
- Compass headless coverage: `tests/test_compass_commands.py` pins the
  full command surface MongoDB Compass issues (instance probes,
  `$collStats`/`$sample`/`$indexStats`, explain at both verbosities,
  performance-tab polls, `atlasVersion` → CommandNotFound).
- `serverStatus` now carries a `secantus` subdocument
  (`{server: "python"|"rust", version: ...}`) on both servers —
  categorical self-identification that real `mongod` never has. The
  conformance-gauge tripwire checks it over the wire before any test
  runs, so the gauge can never again silently measure a foreign server.
- Cluster-time gossip: every reply (success or error) now carries
  `$clusterTime` (keyless signature) and `operationTime` when the
  replica-set persona is on, exactly like a real replica-set mongod;
  standalone mode stays gossip-free. Reads observe the cluster clock
  via the new `Storage.peek_cluster_time()` without advancing it.
  Clears the `startAtOperationTime` / causal-consistency bucket of the
  honest pymongo gauge (Rust-server port tracked in backlog §7).

#### Changed

- CI: the Linux and macOS test cells install mongosh + MongoDB Database
  Tools, so the CLI-tool conformance tests run continuously instead of
  skipping on runners (Windows omitted — mongosh tests skip on win32 by
  design).
- CI: all `actions/*` workflow actions bumped to their Node-24 majors
  (checkout v5, setup-python v6, upload-artifact v6, download-artifact
  v7, cache v5, setup-go v6, setup-java v5, setup-node v5) ahead of
  GitHub's June 16th 2026 forced Node 20 → 24 switch.

#### Fixed

- Arithmetic expressions (`$add` / `$subtract` / `$multiply` /
  `$divide` / `$mod`) now raise mongod's type errors instead of
  silently producing Python-flavoured results: non-numeric operands
  error with mongod's exact messages and codes (verified against a
  real mongod 8.2 oracle), `$divide`/`$mod` by zero error (codes 2 /
  16610) instead of returning null, bool operands are rejected (BSON
  arithmetic has no bool), `$add`/`$subtract` date semantics follow
  mongod (date ± millis, date − date → long, two dates in `$add` →
  16612), and Decimal128 operands widen the fold to decimal. The Rust
  engine defers all error-shaped cases to Python (parity corpus
  extended first; 536 parity tests green).
- Timeseries collections no longer enforce `_id` uniqueness, matching
  mongod (measurements are bucketed by time; `_id` is not a key there).
  Doc-table keys for timeseries rows carry a uniqueness suffix so equal
  `_id`s coexist; index entries point at the actual row key, updates and
  deletes preserve it, and the `_id` point-lookup fast path falls back
  to a collection scan for timeseries. Closes the last E11000 item from
  the honest-gauge triage.
- Aggregation-pipeline updates (`update_one(filter, [{"$set": ...}])`)
  now project as `update` change-stream events with a computed
  `updateDescription`, matching mongod. The replacement classifier
  iterated the pipeline list (whose elements are stage documents, not
  `$`-prefixed keys) and emitted a full-document oplog entry, so
  pymongo's "Test array truncation" unified spec saw `replace`.
- Stale WT read snapshots made the mutating scanners
  (`drop_collection` / `drop_database` / `rename_collection` /
  `drop_index` / `drop_all_indexes`, plus `index_sizes`) silently miss
  rows committed by other connection threads — a pinned snapshot from
  an earlier positioned cursor turned `drop` into a partial or complete
  no-op, surfacing in the pymongo gauge as drop-then-reinsert E11000
  duplicate-key errors. All six now refresh the session snapshot on
  entry, the same discipline the public read paths already had.
- `mongostat` no longer panics against SecantusDB (missing
  `serverStatus.mem`); `mongotop` no longer fails with
  `CommandNotFound`.
- `explain` with `executionStats` / `allPlansExecution` verbosity now
  really executes the query and reports actual `nReturned` /
  `totalDocsExamined` / `totalKeysExamined` / `executionTimeMillis`
  instead of hardcoded zeroes; aggregate-explain lifts a leading
  `$match` into the reported plan, matching the real pipeline run's
  index decision.
- Abrupt client resets (RST close, routine for Go-driver tools) are
  treated as normal disconnects instead of logging `unhandled error on
  connection N` tracebacks.
- Shutdown race: a request arriving while `stop()` closes the listen
  socket no longer raises `OSError: Bad file descriptor` from the
  address probe.
- **The pymongo conformance gauge was not measuring SecantusDB.**
  pymongo's test helpers freeze `DB_IP`/`DB_PORT` at conftest-import
  time, before the gauge plugin's `pytest_configure` wrote them — so
  local runs silently targeted whatever listened on `localhost:27017`
  (a real `mongod`, which produced the previous "100.0%" headline) and
  CI runs, with nothing on 27017, mass-skipped 1100+ tests. The plugin
  now starts the embedded server in `pytest_load_initial_conftests`
  (before any conftest import), aborts via tripwire if the helpers
  captured the wrong address or the target lacks the `secantus`
  marker, and the regenerated honest report shows the real number.
- The weekly `validate.yml` aggregate never opened its report PR:
  `upload-artifact@v4` strips the `docs/` parent from single-file
  artifacts, so the staging glob matched nothing and untracked new
  reports were invisible to `git diff`. Staging now fails loudly on an
  empty match and `git add --intent-to-add`s new report files.
- The gauge now runs under one xdist worker (`-n1`) with a 120s
  per-test deadline, so a hung test is recorded as a crash and the run
  continues, instead of pytest-timeout killing the whole process and
  losing the JSON report.
- Editable storage-engine rebuilds shipped stale Rust extensions: the
  CMake custom command had no dependency on the crate sources, so once
  the staged `.so` existed cargo never re-ran. The build now always
  invokes cargo (its own dependency tracking decides freshness) and
  stages with `copy_if_different`.

## [0.5.2b15] — 2026-05-22

### WT session leak fix unblocks the rust crud unified runner

SecantusDB cached a WiredTiger session per connection thread in
`threading.local()` but never released it when the thread died.
Aggressive driver pools (mongo-rust-driver's spec runners are
the canonical case) opened thousands of short-lived connections;
once cumulative connections crossed WT's 1024-session pool limit,
`hello` started failing mid-handshake with `WT_ERROR: out of
sessions`, which downstream surfaced as a checkpoint stat-error
on `WiredTigerHS.wt`. This release calls
`Storage._reset_thread_session()` in `SecantusDBServer._handle_client`'s
`finally` block, releasing the session/cursors on disconnect so
the pool stays bounded by the live connection count.

The fix also closes a small `aggregate` validation gap: `$out`
and `$merge` under `readConcern: "linearizable"` now return
`InvalidOptions (72)` to match mongod's invariant (the
`aggregate-out-readConcern` unified spec asserts the rejection).

Together these unblock `test::spec::crud::run_unified` in the
rust gauge — ~80 subtests across find / insert / update / delete
/ aggregate / countDocuments / distinct / findOne\* / replaceOne
/ bypassDocumentValidation / collation / hints / comments / let
bindings / readConcern levels / dots-and-dollars keys, running
end-to-end in ~75s. Rust gauge moves from 100 → 101 filters
passing.

#### Fixed
- WT session pool exhaustion under high connection churn: per-
  connection-thread WT session is now released on disconnect
  instead of leaking until the engine's 1024-session pool fills.
- `aggregate` with `$out` / `$merge` under `readConcern:
  "linearizable"` now errors with `InvalidOptions (72)` instead
  of silently returning an empty array.

#### Changed
- Rust conformance gauge: `test::spec::crud::run_unified` is now
  in the include list. `test::spec::collection_management::run_unified`
  and `test::spec::sessions::run_unified` remain deferred for
  separate gaps (time-series collections, snapshot read concern
  under fake replica-set topology).

## [0.5.2b14] — 2026-05-22

### Change-stream split-event implementation: real `{fragment: N, of: M}`

The `splitLargeChangeStreamEvents` opt-in previously stamped every
event with `{fragment: 1, of: 1}` regardless of size — correct from
the driver's reassembly perspective for events under 16 MB, but
wrong for events that genuinely exceed the BSON wire limit (the
typical case being an `update` with `fullDocumentBeforeChange:
required` where the pre-image plus a large `$set` value together
push the projected event past 16 MB).

This slice ships real splitting. When an event's BSON-encoded size
exceeds 16 MB, `stamp_split_event` distributes any top-level field
larger than 1 MB into its own fragment; light metadata (resume
token, operationType, clusterTime, ns, documentKey, wallTime, …)
is copied verbatim into every fragment so each is a valid change
event the driver can process independently. Fragments share the
same `_id` resume token; drivers reassemble by combining fields
across fragments with matching `_id`. The split is size-based, not
field-name-based: any heavy field qualifies (in practice
`fullDocument`, `fullDocumentBeforeChange`, and
`updateDescription.updatedFields` are the candidates).

Two opt-in paths now both light up the producer flag: the original
`$changeStream: {splitLargeChangeStreamEvents: true}` spec field
plus the pipeline-stage form `[{$changeStreamSplitLargeEvent: {}}]`
that the rust / node / java drivers use from their high-level
`watch()` APIs. Either signals to the producer that fragmentation
should run.

mongo-rust-driver's `test::change_stream::split_large_event` —
which constructs a 10 MB pre-image + 10 MB update value and
asserts `events[0].splitEvent == {fragment: 1, of: 2}` and
`events[1].splitEvent == {fragment: 2, of: 2}` — now passes end-
to-end. The rust gauge moves from 92 → 93 (still 100%).

#### Added

- `src/secantus/aggregate.py`: `$changeStreamSplitLargeEvent`
  registered in `_STAGES` as a pass-through marker. The stage
  itself is a no-op in the pipeline (real splitting happens
  upstream at event-projection time); accepted spec is `{}`.
- `src/secantus/changestreams.py`:
  - `_HEAVY_FIELD_BYTES = 1 MB` and `_SPLIT_THRESHOLD_BYTES = 16 MB`.
  - `stamp_split_event(event) -> list[dict]` rewritten to compute
    the event's BSON size, identify heavy top-level fields by
    per-field encoding, and emit one fragment per heavy field
    with light metadata duplicated. Returns one event (no split)
    when the original is under 16 MB.
- `src/secantus/commands.py`: change-stream aggregate handler
  detects the `$changeStreamSplitLargeEvent` pipeline stage and
  sets `cs_spec.split_large_events = True` so the producer
  fragments on that opt-in path too. Producer call sites
  changed from `events.append(stamp_split_event(ev))` to
  `events.extend(stamp_split_event(ev))`.
- `tests/test_change_stream_split_stage.py` (5 tests):
  pipeline parses cleanly; bad-spec rejected standalone; stage
  works outside change-stream context (no-op pass-through);
  10 MB pre-image + 10 MB `$set` value produces two fragments
  with correct `{fragment: N, of: 2}` envelopes and shared
  resume token, heavy fields distributed one per fragment;
  small event with opt-in still produces single
  `{fragment: 1, of: 1}` fragment.

#### Changed

- `rust_validation/include_paths.py` adds
  `test::change_stream::split_large_event` to `INCLUDE` (rust
  gauge 92 → 93). The previous EXCLUDED entry's rationale is
  removed.

### Point lookups by `_id` stop scanning the whole collection

Every MongoDB collection has an `_id` index, and looking a document up
by its `_id` is the single most common read an application makes. In
SecantusDB that lookup was quietly walking the entire collection: the
`_id_` index is virtual — the documents table is itself keyed by the
encoded `_id`, so there's no separate entries table for it — and the
query planner's index pickers only ever consulted the stored secondary
indexes. With nothing matching `_id`, every `find({_id: …})` fell back
to a COLLSCAN that got linearly slower as the collection grew.

`find`, `findOne`, `updateOne`, and `deleteOne` filtered on `_id` now
take a direct primary-key point lookup on the documents table instead.
On a 5,000-document collection that turns a 45 ms read into a 0.6 ms
read — about 74× faster — and the gap widens with collection size.
`explain` reports the lookup honestly as an `IXSCAN` on the `_id_`
index. Equality (`{_id: x}`), `{_id: {$eq: x}}`, and `{_id: {$in: […]}}`
are all accelerated; range, regex, and multi-field filters keep their
existing routing. The cross-numeric `_id` collision (`1 == 1.0 ==
Decimal128("1")`) is preserved because the fast path encodes the query
value with the same `encode_value` used for the stored key.

#### Fixed

- `find` / `findAndModify` / single-document `update` / `delete`
  filtered on `_id` equality (`{_id: v}`, `{_id: {$eq: v}}`,
  `{_id: {$in: [...]}}`) now do an O(1) primary-key point lookup on the
  documents table instead of a COLLSCAN, and `explain` reports `IXSCAN`
  on the `_id_` index. Discovered with the new `bench/rw_harness.py`
  concurrent read/write validator, whose interleaved `_id` read-backs
  collapsed throughput on growing collections.

## [0.5.2b7] — 2026-05-21

### Rust driver gauge — 6th conformance gauge alongside the rest

mongo-rust-driver is now the 6th driver gauge alongside pymongo / go
/ node / java / ruby. The runner spawns SecantusDB on an ephemeral
port and runs ``cargo test --lib -p mongodb`` against a curated
include set with ``MONGODB_URI`` explicitly overridden in the
subprocess env — the rust driver's fallback chain
(``$MONGODB_URI`` → ``~/.mongodb_uri`` → ``localhost:27017``) is
short-circuited at the first step so a stray ambient URI in the
user's shell can't route the gauge at a real mongod. A
belt-and-braces ``hello.setName == "secantus"`` probe at runner
start adds a second layer of confirmation.

Initial baseline: 12 curated handshake + single-collection CRUD
filters expand to 24 actual test runs (libtest substring matching
fans ``test::coll::find`` out across ``find_allow_disk_use`` etc.).
The first cut surfaced two real conformance gaps; both fixed in the
same release:

* ``listDatabases`` now populates ``sizeOnDisk`` per database (sum
  of bson-encoded doc bytes across the db's collections — same
  accounting ``collStats`` / ``dbStats`` use). ``empty`` is derived
  from the size (``size == 0``). ``totalSize`` reports the actual
  sum across all dbs. Previously every entry carried a placeholder
  ``sizeOnDisk: 0`` and ``empty: false``.
* ``hello.client`` subdoc captured per connection in the registry
  and surfaced back via ``currentOp`` as ``clientMetadata``. Drivers
  use it to identify their own connections in admin tooling — they
  send the subdoc on handshake and expect to read it back. Previously
  we threw the subdoc away on hello and ``currentOp`` emitted no
  ``clientMetadata`` field.

After the fixes the rust gauge runs **24/24 (100%)**.

#### Added

- ``rust_validation/`` package — ``__init__.py`` /
  ``include_paths.py`` / ``runner.py`` / ``generate_report.py``,
  mirrors the ``ruby_validation/`` shape.
- ``vendor/mongo-rust-driver`` submodule (7th vendored driver).
- ``invoke validate-rust`` task; ``validate-all`` GAUGES extended
  with the 6th entry.
- ``.github/workflows/validate.yml`` matrix entry for rust;
  toolchain via ``dtolnay/rust-toolchain@stable``; cargo cache key
  on ``vendor/mongo-rust-driver/Cargo.lock``.
- ``validation_summary`` integration — ``_collect_rust``,
  ``PANEL_PROSE`` entry, stale "pending" marker removed.
- ``docs/validation-report-rust.md`` (new) + toctree entry +
  index.md prose update referencing all six drivers.
- ``tests/test_list_databases_size.py`` (4 tests): populated db
  has non-zero ``sizeOnDisk`` + ``empty: false``; ``totalSize``
  sums per-db sizes; ``nameOnly`` skips the size walk; ``filter``
  scopes against the full descriptor.
- ``tests/test_hello_client_metadata.py`` (2 tests): pymongo's
  driver / OS / appname metadata round-trips through hello →
  currentOp; clientMetadata is a dict shape when present.

#### Changed

- ``commands._list_databases``: computes ``sizeOnDisk`` per db as
  ``sum(collection_data_size(...) for coll in list_collections)``;
  ``empty`` derived from size; ``totalSize`` is real.
- ``commands._hello``: captures ``doc.get("client")`` and stashes
  via ``ctx.connections.set_client_metadata(...)``.
- ``commands._current_op``: emits ``clientMetadata`` on each
  in-progress op when the connection's registry entry has it.
- ``connreg.ConnInfo`` grows ``client_metadata: dict | None``;
  ``ConnectionRegistry.set_client_metadata(conn_id, metadata)``
  added; ``get()`` and ``snapshot()`` thread the new field
  through their fresh-copy semantics.

## [0.5.2b5] — 2026-05-21

### `$setWindowFields` rank functions — `$rank` / `$denseRank` / `$documentNumber`

Closes one of the explicit deferred surfaces from the b35
`$setWindowFields` minimum-viable subset. Driver test suites probe
all three regularly; the previous wire-level response was an
explicit "rank functions and time-series operators are not yet
implemented" `AggregateError`.

The three functions share one linear walk per partition. They sit
in `output: {<field>: {$rank: {}}}` alongside the accumulator
functions but evaluate differently — no window argument (mongod
rejects it), no function argument (the spec is just `{$rank: {}}`),
and the value is computed once per partition slot rather than
rolled up over a windowed subset.

* `$documentNumber` — 1-indexed position within the partition.
  Independent of ties; happy with or without `sortBy`.
* `$rank` — 1-indexed position with **gaps** after ties: tied rows
  share the lower rank, next non-tied row jumps by the number of
  ties (`[10, 20, 20, 30]` → `[1, 2, 2, 4]`). Requires `sortBy`.
* `$denseRank` — 1-indexed position **without gaps**: tied rows
  share, next row is +1 (`[10, 20, 20, 30]` → `[1, 2, 2, 3]`).
  Requires `sortBy`.

Tie detection is sort-key tuple equality: compound `sortBy` specs
work uniformly. Rank counters reset at every partition boundary,
same as the accumulator functions.

#### Added

- `src/secantus/aggregate.py`: `_RANK_FUNCS` frozenset; the
  validation branch in `_stage_set_window_fields` recognises the
  three rank ops, rejects `window` / non-empty arg, and requires
  `sortBy` for `$rank` / `$denseRank`. The per-row loop branches:
  rank functions look up a precomputed array, accumulators take
  the existing windowed path.
- `_compute_rank_state` helper does one linear walk over each
  partition's sort-key tuples and emits per-slot vectors for
  whichever of the three functions are referenced. `_sort_key_values`
  extracts the tuple the tie comparison runs on.
- `tests/test_window_rank_functions.py` (13 new tests) — covers
  `$documentNumber` with and without sort, per-partition reset,
  `$rank` gaps with ties, `$rank == $documentNumber` without ties,
  compound sort tie detection, `$denseRank` no-gap semantics, all
  three together in one stage, partition-resets, plus four
  validation tests (window rejected, sortBy required for `$rank` /
  `$denseRank`, non-empty arg rejected).

#### Changed

- `_stage_set_window_fields` docstring rewritten to document the
  rank-function surface.
- `tests/test_set_window_fields.py`: the b35 placeholder test
  `test_unsupported_rank_function_raises` is replaced by
  `test_unsupported_time_series_function_raises`, which now probes
  with `$derivative` to keep the deferred-surface guard alive.

### `apiStrict: true` rejects `distinct` (narrow command-name gate)

The Stable API v1 contract rejects a list of commands when
`apiStrict: true` is set. SecantusDB already rejected non-v1
aggregation **stages** inside `aggregate` pipelines (lights up
mongo-java-driver's `versioned-api/aggregate on database` test
that probes with `$listLocalSessions`). The matching command-name
gate had been intentionally left off in a previous attempt: a
broader whitelist invert reportedly caused 6 cascade failures via
`MongoConnectionPoolClearedException`.

A focused Java-gauge run with a narrow gate
(`_API_V1_REJECTED_BY_NAME = {"distinct"}`) tells a different
story. Rejecting only `distinct` produces **+1 pass** for the
canary `crud-api-version-1-strict.yml` `distinct appends declared
API version` test and **zero** new failures across the 900-test
mongo-java-driver suite — no pool-clear symptoms anywhere in the
JUnit XML. The cascade the previous attempt observed was not
pool-clear semantics; it was the broader invert also rejecting
`count` (used internally by `estimatedDocumentCount`) and other
handshake-adjacent internal commands. The narrow gate sidesteps
that mechanism entirely.

#### Added

- `src/secantus/commands.py`: `_API_V1_REJECTED_BY_NAME`
  frozenset (one entry: `distinct`); the `dispatch` apiStrict
  block grew a command-name check that runs before the
  aggregation-stage check. The rejection's `errmsg` matches
  mongod's `"Provided command distinct is not in API Version 1"`
  so the unified test runner's `errorContains` assertion fires
  cleanly.
- `tests/test_api_strict.py` (5 new tests): `distinct` rejected
  under `apiStrict: true` with code 323; `distinct` allowed
  without `apiStrict`; `count` still allowed under `apiStrict`
  (the cascade-avoidance check); `find` still allowed; `aggregate`
  with a v1 stage still allowed (gates compose).

#### Changed

- Backlog §5 entry on `apiStrict` pool-clear struck through with
  the empirical resolution path. The previous theory turned out
  to be wrong about the mechanism — narrow rejection works.

### Pymongo gauge: +80 passing tests from five newly-includable files

Cross-gauge audit of currently-excluded test files against the work
shipped in this development cycle (0.5.2b1 + the rank-functions
and apiStrict slices above) identified five pymongo test files
that pass cleanly now and had been excluded purely because the
supporting features hadn't shipped. Adding them to
`pymongo_validation/include_paths.py` bumps the gauge from **959 →
1039 passing** with zero new failures, +25 new skips (genuine
feature gaps the suite self-skips on), overall pass rate stays at
100%.

* `test_collation.py` (16 new tests) — unlocked by per-index
  collation work (single-field, compound, sort acceleration).
* `test_versioned_api.py` (4 tests) + `test_versioned_api_integration.py`
  (36 tests) — unlocked by the apiStrict aggregation-stage gate
  and the new `distinct` command-name gate.
* `test_command_logging.py` (20 tests) + `test_logger.py` (4 tests)
  — command monitoring / logging format conformance; no
  SecantusDB-specific blocker.

The audit also confirmed no flip-worthy candidates in the go /
node / java / ruby gauges — every remaining exclusion in those
gauges is a feature genuinely out of scope (replica sets,
transactions, encryption, text indexes, GridFS, time-series,
etc.).

#### Changed

- `pymongo_validation/include_paths.py` — five test files added
  to `INCLUDE`. Inline comments name the slice that unlocked each.

## [0.5.2b1] — 2026-05-20

### MONGODB-X509 auth — cert subject DN as the username

The natural sequel to the b22 mTLS slice. mTLS gives you a
transport-layer "approved client" gate; MONGODB-X509 turns the
client cert's subject DN into the user identity directly, no SCRAM
step. Same flow MongoDB Atlas X509 deployments use: create the user
on `$external` with `mechanisms: ["MONGODB-X509"]` and the cert DN
as the username, connect with
`?authMechanism=MONGODB-X509&authSource=$external`, the server
matches the DN from the verified cert against the user record. No
password to rotate, no SCRAM round-trip, no shared secret on disk.

Mixed mechanisms work too — a user record can carry both
`SCRAM-SHA-256` and `MONGODB-X509` in `mechanisms` for migration or
to keep a SCRAM fallback. The driver picks per-connection from
`saslSupportedMechs`.

Closes the "transport-layer gate only" caveat the production +
configuration docs called out when mTLS shipped; documentation
updated to point at the worked X509 example as the alternative to
SCRAM-on-top.

#### Added

- `secantus.auth.MONGODB_X509` constant, `X509_CREDENTIAL_MARKER`
  for the user record's `credentials` doc (no password to hash —
  the credential IS the cert), and
  `secantus.auth.subject_dn_from_peercert()` which converts
  Python's `ssl.SSLSocket.getpeercert()` tuple-of-tuples into the
  mongod-style RFC 4514 DN string (short attribute names,
  most-specific-first, special-char escaping).
- `CommandContext.peer_cert_dn` — server captures the verified
  client cert's DN once per connection (right after the TLS
  handshake in `_handle_client`), replays it into every
  `CommandContext` so the auth handlers can read it.
- `_sasl_start_x509` and the legacy `authenticate` command handler
  — pymongo / Java / Go / Node all use the legacy command path for
  X509, not `saslStart`. Both are wired up and refuse cleanly on
  plaintext connections / non-X509 users / payload-DN mismatch.
- `createUser` accepts `mechanisms=["MONGODB-X509"]` with no
  password (cert IS the credential). Mixed
  `["SCRAM-SHA-256", "MONGODB-X509"]` works too — SCRAM creds are
  derived from `pwd`, X509 marker is written alongside.
- `tests/test_x509_auth.py` — 9 tests: DN extraction unit tests
  (reversal, short names, escaping, empty), end-to-end happy path
  via pymongo, refused-with-no-matching-user, refused-for-SCRAM-only
  user, SCRAM still works on mTLS-required server, X509 refused on
  plaintext connection.

#### Changed

- `saslSupportedMechs` now includes `MONGODB-X509` when a user has
  that mechanism in its `credentials` doc. SCRAM is still listed
  first when both are available (drivers pick the strongest).
- `_PRE_AUTH_COMMANDS` includes `authenticate` so the legacy X509
  command path bypasses the require-auth gate (same as
  `saslStart` / `saslContinue` already did for SCRAM).
- `docs/authentication.md` — new MONGODB-X509 section with the
  provisioning + connection examples; the stale "what's not here
  yet" list rewritten (RBAC, updateUser, grantRolesToUser, TLS,
  SCRAM-SHA-1 all shipped slices ago and shouldn't have been
  listed as gaps).
- `docs/production.md` + `docs/configuration.md` — mTLS sections
  now offer two routes (SCRAM-on-top vs MONGODB-X509) instead of
  the "transport-layer only, MONGODB-X509 is a follow-on" caveat.

### Per-index collation — case- and accent-insensitive lookups at IXSCAN

The last entry on the compatibility doc's "Deferred" list is gone.
Before this slice, the per-query collation infrastructure already
honoured `collation` for `find` / `count` / `distinct` /
`findAndModify` via `matches()` — but any query that carried a
`collation` argument fell through to COLLSCAN by design, because
index entries were written in raw BSON codepoint order. The
storage-layer comment said as much: "we don't support per-index
collation yet, so the safe path is always-COLLSCAN-when-collation."

That comment is gone. `createIndexes` with a `collation` option
now writes index entries under collation-normalised bytes —
strings that compare-equal under the collation produce the same
key, so a query carrying a matching `collation` hits the same row
at IXSCAN. Strength 1/2/3 + `caseLevel` are supported;
`numericOrdering` still falls back to COLLSCAN (would need a
length-prefixed digit-run encoding to stay byte-sortable, deferred
until a workload needs it).

Two indexes on the same field with different collations are
allowed — the picker walks every candidate and uses the one whose
collation exactly matches the query's. Useful for collections that
mix case-sensitive and case-insensitive lookups against the same
column. Unique indexes with a collation enforce uniqueness
*under* the collation: two docs differing only by case collide
against a `strength: 2` unique index. Only the single-field
equality / range / `$in` picker threads collation through today;
multi-field filters combined with a collation still fall back to
COLLSCAN. Worth widening case-by-case when a workload needs it.

#### Added

- `sortkey.encode_value(value, *, collation=None)`,
  `encode_value_directed`, `encode_compound`, and the bound
  helpers (`gt_bound` / `gte_bound` / `lt_bound` / `lte_bound`) all
  take an optional `collation` kwarg. When set and the value is a
  string, normalisation runs through
  `secantus.collation.normalize_for_index_bytes` before encoding,
  so equal-under-collation strings produce equal bytes.
- `Collation.supports_index_encoding` — True for strength 1/2/3 +
  `caseLevel`, False for `numericOrdering`. The picker treats
  numericOrdering as "no index available for this collation."
- `secantus.collation.normalize_for_index_bytes(s, collation)` —
  bytes form of the collation-normalised string (strips accents
  for strength 1, casefolds for strength ≤ 2, UTF-8 encodes).
- `_parse_index_collation` helper in `storage.py` — reads an
  index's stored collation option blob into a `Collation`,
  returning `None` for collations that don't support index
  encoding.
- `tests/test_per_index_collation.py` — 11 tests covering routing
  (matching collation → IXSCAN, mismatch → COLLSCAN, no-collation
  query against collation-having index → COLLSCAN), correctness on
  equality / range / `$in` / `update_one`, `numericOrdering`
  fallback, unique-index-under-collation, and two indexes on the
  same field with different collations.

#### Changed

- `_index_key` / `_index_key_variants` (the byte-key builders for
  index writes) accept a `collation` kwarg; the storage writers
  load it from the index's stored options and pass it through.
- `_find_leading_field_index` + `_pick_index_for_filter` +
  `_try_index_lookup` + `_try_index_id_keys` thread a `collation`
  kwarg. Indexes whose stored collation doesn't exactly equal the
  query's are skipped — the caller falls back to COLLSCAN, which
  is the safe semantics. `_pick_compound_eq_index` /
  `_pick_compound_range_index` skip collation-having indexes
  entirely; compound pickers don't yet support collation, and
  picking a collation-having index for a no-collation multi-field
  filter would return wrong rows.
- `explain_plan` takes a `collation` kwarg, and the `explain`
  command extracts it from the wrapped command. Mismatched
  collations report COLLSCAN in `winningPlan`; matched ones
  report `IXSCAN` with the index name.
- `find_matching`'s "if collation present, always COLLSCAN" gate
  has been rewritten — now tries the collation-aware index path
  first, falls back to COLLSCAN only when no matching index
  exists.
- `docs/compatibility.md` field-options table: `collation` is now
  Honoured rather than Accepted-but-ignored. The Deferred list is
  now empty.
- `docs/indexes.md`: new "Per-index collation" section with
  examples and rules; the "What's still missing" list updated to
  call out compound-index collation as the next widening.
- `tasks/backlog.md` §2: the per-index-collation stopgap entry is
  struck through with a one-line summary of what shipped and the
  remaining compound-index limitation.

### Compound-index collation — multi-field filters light up under matching collation

The b25 per-index collation slice closed the single-field path
but left the compound pickers
(`_pick_compound_eq_index` / `_pick_compound_range_index`) skipping
any collation-having index — a multi-field filter combined with a
`collation` argument fell back to COLLSCAN even when a compound
collation index could have served it. This slice closes that gap.

Both compound pickers now thread `collation` through and gate by
exact match against each index's stored collation, the same rule
the single-field path already used. The lookup builders thread
collation into every `encode_value_directed` call (leading-equality
prefix bytes and the trailing operator's bound bytes), so the
lookup hits the same byte rows the index-write path produced.
Strength 1/2/3 + `caseLevel` apply uniformly across single- and
compound-field indexes; `numericOrdering` still falls back to
COLLSCAN at every level. The unique-probe path now reads the
index's stored collation too, so a unique compound index with
`{strength: 2}` correctly rejects a second insert whose values
collide under the collation.

After this slice, every CRUD pattern that the single-field
collation path covers — equality / range / `$in` / `update` /
unique enforcement — covers under compound indexes too.

#### Changed

- `_pick_compound_eq_index` + `_try_compound_eq_id_keys` thread
  `collation` through; the compound-eq lookup builds the prefix
  bytes under the same collation as the index.
- `_pick_compound_range_index` + `_try_compound_range_id_keys`
  thread `collation` through; the trailing operator's `$eq` /
  `$in` / `$gt` / `$gte` / `$lt` / `$lte` bounds are all encoded
  under the collation.
- `_try_index_id_keys` no longer short-circuits compound pickers
  when `collation` is set — they're called with the collation kwarg
  and use the exact-match gate.
- `_pick_index_for_filter` (the explain planner) mirrors the same
  threading, so `explain` reports `IXSCAN` for collation-matching
  multi-field queries.
- `_unique_conflict` reads each index's stored collation via
  `_parse_index_collation` and threads it to `_index_key`, so the
  unique probe collides on byte-equal canonical keys (the bug
  that let `("Alice","Boston")` and `("ALICE","BOSTON")` both land
  in a unique strength-2 compound index).
- `docs/indexes.md` "Per-index collation" section rewritten to
  cover the compound case with examples; "What's still missing"
  drops the compound-collation entry.
- `tests/test_compound_index_collation.py` (10 new tests): compound
  bare-eq IXSCAN under matching collation, leading-prefix-only
  scan, mismatch → COLLSCAN, no-collation-vs-collation index
  selection across two indexes on the same fields, compound
  prefix + trailing-operator (`$gt`, `$in`) under collation,
  update via compound collation index, unique compound collation
  enforcement, `numericOrdering` fallback.

### Sort acceleration with collation — index walk replaces Python sort

The third collation slice closes a quieter gap left by the
preceding two. The b25 + b27 slices wired up filter-side
collation routing — equality / range / `$in` / compound bare-eq /
compound prefix + trailing-operator all light up at IXSCAN when
the query's `collation` matches an index's stored collation. But
the sort path stayed on COLLSCAN + Python `sort_docs`: any query
carrying a `collation` argument fell into a single branch that
never tried sort acceleration, even when an index whose collation
matched the query's would have given the requested order for free
just by walking it.

That branch is gone. The collation and non-collation paths through
`find_matching` are now unified, and every sort-picker call
(`_find_leading_field_index` for single-field sorts,
`_compound_index_for_sort` for multi-field) threads
`collation_obj` through with the same exact-match gate as the
filter side. A `find().sort("name", 1).collation({strength: 2})`
walks a `{name: 1}` strength-2 collation index forward; `-1` walks
it backward; multi-field sorts that exactly match (or fully
invert) a compound collation index's key spec walk it forward or
backward respectively, and no Python sort runs in either case.
The same gate keeps no-collation sorts off collation indexes
(walking would give the wrong order) and vice versa.

After this slice the collation domain is structurally complete:
every CRUD pattern that hits an index without collation — filter
lookup, range, `$in`, multi-field filter, sort, compound sort,
unique enforcement — hits the index when a matching collation is
in play, and falls back to COLLSCAN + `matches()` + `sort_docs`
when no matching index exists.

#### Changed

- `find_matching`'s `elif collation_obj is not None: ...` branch
  removed; the no-collation branch's sort logic now runs for both
  cases, with `collation=collation_obj` (which is `None` when no
  collation set) threaded through every picker call. Single-field
  sort + filter on the sort field, single-field sort with empty
  filter, and multi-field sort (compound key match) all
  collation-gate.
- `_compound_index_for_sort` takes an optional `collation` kwarg
  and gates by exact match against each index's stored collation
  (same rule as `_find_leading_field_index` and the compound
  filter pickers). Multikey indexes are still excluded from
  sort acceleration regardless of collation.
- `explain_plan` mirrors the threading: `_find_leading_field_index`
  and `_compound_index_for_sort` both receive `collation=collation_obj`,
  so `explain` reports IXSCAN with the right direction for
  collation-matching sort queries and COLLSCAN otherwise.
- `docs/indexes.md` "Per-index collation" section grows a "sort
  acceleration honours the same gate" subsection with worked
  forward / backward / mismatch examples.
- `tests/test_sort_with_collation.py` (8 new tests): single-field
  ASC + DESC sort with matching collation walks index forward /
  backward; no-collation sort against collation index → COLLSCAN;
  strength-2 index + strength-3 query → COLLSCAN; filter on sort
  field with matching collation hits index in order; multi-field
  sort that matches a compound collation index walks forward; the
  full-inverse sort walks backward; multi-field mismatch falls
  back to Python sort.

### `$type: "int"` / `"long"` distinguishes by BSON type tag, not value range

A quieter long-standing bug in the `$type` query operator. The
`_TYPE_PREDS` table used a Python value-range check
(`-2**31 <= v <= 2**31 - 1`) to distinguish int32 from int64. A
doc inserted as `Int64(5)` — value fits in int32 numerically, but
its BSON tag is int64 — was matched by `$type: "int"` instead of
`$type: "long"`, contradicting mongod.

pymongo's BSON decoder already preserves the int32/int64
distinction by class: int32 round-trips as plain `int`, int64
round-trips as `bson.Int64` (a subclass of `int`). The fix keys
on `isinstance(v, bson.Int64)` for "long" and
`isinstance(v, int) and not isinstance(v, (bool, Int64))` for
"int" — type-tag-faithful, no value-range arithmetic.

`$convert: {to: "long"}` had a paired bug: it returned a plain
`int` so its output couldn't be matched by `$type: "long"` on a
downstream `$match`. Now wraps the result in `Int64` for code 18
(int64); `to: "int"` (code 16) still returns plain `int`.

#### Changed

- `src/secantus/query.py`: replaced `_is_bson_int(... ranged=...)`
  + `_INT32_RANGE` with three named predicates (`_is_int32`,
  `_is_int64`, `_is_bson_number`). `_TYPE_PREDS` entries for
  `int` / `16` / `long` / `18` / `number` now route through them.
- `src/secantus/expressions.py`: `_convert_value` code 18 path
  wraps its result in `Int64` (codes 16 and 18 share the input
  coercion logic but the wrapper diverges).
- `tests/test_type_int32_int64.py` (8 new tests): `Int64(5)` →
  `$type: "long"` (not `int`); plain `int(5)` → `$type: "int"`;
  large int (`2**40`) round-trips as Int64 → `long`;
  `$type: "number"` accepts both; numeric `$type` codes (16, 18)
  agree with their string aliases; array-form `$type` matches
  either; `$convert: {to: "long"}` output matches `$type: "long"`;
  `$convert: {to: "int"}` output matches `$type: "int"`.

### `$unionWith` aggregation stage

A v1 stable-API stage that wasn't yet wired up. `$unionWith`
concatenates docs from a second collection — optionally filtered
through a sub-pipeline — onto the current pipeline's input. Driver
test suites probe it routinely; the prior wire-level response was
a generic "unsupported aggregation stage" error.

Both spec shapes ship:

* Shorthand: `{$unionWith: "<coll>"}`
* Full form: `{$unionWith: {coll: "<coll>", pipeline: [...]}}`

Outer docs land first, then the union docs in the order the
sub-pipeline produced them. No deduplication — duplicates across
the boundary survive, matching mongod. The sub-pipeline runs in a
fresh :class:`PipelineContext`; outer `$lookup let` variables are
deliberately not visible (mongod doesn't accept a `let` field on
`$unionWith`). Chained `$unionWith` stages accumulate; downstream
`$sort` / `$group` / `$count` / `$limit` see the combined set.

A non-existent target collection is treated as empty (mongod's
behaviour). Bad specs (non-string shorthand, missing `coll`,
non-array `pipeline`) surface as `AggregateError` to the client.

#### Added

- `src/secantus/aggregate.py`: `_stage_union_with` handler;
  wired into `_STAGES` next to `$geoNear`. ~30 LOC + docstring.
- `tests/test_union_with.py` (11 new tests): shorthand form;
  full form with and without sub-pipeline; outer-first ordering;
  no-dedup across boundary; chained `$unionWith`; downstream
  `$group` / `$sort+$limit`; missing collection treated as empty;
  empty outer + non-empty union; bad-spec rejection (numeric
  spec, missing `coll`, non-array `pipeline`).
- `docs/aggregation.md` stages table grows a row.

### `admin.system.users` is a synthetic read-only view onto the user store

Credentials live in a dedicated WT table (`secantus_users`) that
`createUser` / `updateUser` / `dropUser` / `usersInfo` own. But
`find` / `aggregate` / `count` against `admin.system.users` —
mongod's canonical user-storage namespace — searched the empty
regular doc table and returned nothing. Tools and a few driver
tests that introspect the user list via `db.system.users.find()`
saw an empty collection on SecantusDB even after a `createUser`
landed.

This slice mirrors the oplog pattern (`local.oplog.rs` is a
synthetic view onto `secantus_oplog`). `admin.system.users` is now
read-only-surfaced: `find` / `aggregate` / `count` route through
`_find_system_users` / `_count_system_users`, which scan the user
table on a fresh WT session for cross-thread visibility and apply
the standard filter / sort / skip / limit / projection /
collation pipeline against the decoded records.

The stored records already carry the mongod-shaped fields
(`_id` = `<db>.<user>`, `user`, `db`, `credentials`, `roles`,
`mechanisms`), so the view requires no schema synthesis. Users
created against any database all surface under
`admin.system.users` (matching mongod — every user record lives
in `admin.system.users` regardless of its auth db, and the
per-record `db` field names the auth database). Querying any
other db's `system.users` returns empty rows (also mongod's
behaviour).

Writes are rejected with code 13 (`Unauthorized`) and a clear
errmsg pointing users at `createUser` / `updateUser` / `dropUser`.
The existing `_reject_oplog_rs_write` helper grew a clause for
`admin.system.users` — it was already wired into every write
command (`insert` / `update` / `delete` / `findAndModify` / `drop`
/ `create` / `createIndexes`) so the rejection lands everywhere
implicitly. Function name kept (`_reject_oplog_rs_write`) for
churn reasons, with the docstring updated to cover both views.

#### Added

- `storage._is_system_users` / `_scan_user_records` /
  `_find_system_users` / `_count_system_users` — the synthetic
  view helpers, modelled directly on the oplog view's pattern.
- `storage.find_matching` + `count_matching` route through the
  new helpers when `(db, coll) == ("admin", "system.users")`.
- `tests/test_system_users_view.py` (13 new tests): find /
  count / projection / aggregate against the view; users created
  across multiple databases all visible; filter on `db` field;
  other-db `system.users` is empty; write rejection on insert /
  update / delete / drop with code 13; `dropUser` /
  `updateUser` mutations reflected in the view.

#### Changed

- `commands._reject_oplog_rs_write` grew a second case for
  `admin.system.users`. Docstring rewritten to cover both views.
  Existing call sites pick up the new behaviour with no further
  edits.

### `$redact` aggregation stage

The largest v1 stable-API aggregation stage still missing. `$redact`
implements content-based document and sub-document pruning — the
pipeline analogue of mongod's field-level access control. The
stage's expression evaluates against each (sub-)doc and returns one
of three sentinel strings; the result drives include / exclude /
recurse behaviour. Driver test suites probe it routinely.

* `"$$KEEP"` — include the sub-doc as-is, no recursion into nested
  sub-docs. Useful for "trusted" sub-docs whose interior shouldn't
  be re-evaluated.
* `"$$PRUNE"` — drop the sub-doc. At the top level the doc leaves
  the pipeline entirely; in a nested context the sub-doc is removed
  from its parent field, or from its array element slot (with the
  surrounding array preserved).
* `"$$DESCEND"` — recurse into every dict-valued field and every
  dict-valued list element. Non-dict scalars and non-dict list
  elements pass through unchanged.

The three sentinels are wired into the expression evaluator as
system variables (alongside `$$ROOT`, `$$CURRENT`, `$$REMOVE`);
their resolved value is the literal `"$$NAME"` string the stage
handler dispatches on. Returning anything else from the expression
raises `AggregateError` — matches mongod.

The stage uses the standard `$cond` / `$switch` / `$let` /
`$ifNull` plumbing that the rest of the expression engine already
provides, so the typical pipeline shape works straight out:

```python
[{"$redact": {
    "$cond": {
        "if": {"$eq": [{"$ifNull": ["$classified", False]}, True]},
        "then": "$$PRUNE",
        "else": "$$DESCEND",
    },
}}]
```

#### Added

- `src/secantus/aggregate.py`: `_stage_redact` handler + private
  `_redact_subdoc` / `_redact_descend` recursive helpers, wired
  into `_STAGES` next to `$unionWith`. The `_redact_descend` walker
  preserves non-dict scalars and non-dict list elements; pruned
  sub-docs are dropped from their parent field or array.
- `src/secantus/expressions.py`: `_resolve_var` recognises
  `$$KEEP` / `$$PRUNE` / `$$DESCEND` and returns the literal
  `"$$NAME"` string — same pattern as `$$REMOVE` for `$setField`.
- `tests/test_redact.py` (11 new tests): unconditional KEEP and
  PRUNE; conditional KEEP-vs-PRUNE access-control canon; DESCEND
  with nested sub-doc pruning; DESCEND into arrays of sub-docs
  with non-dict elements preserved; multi-level deep recursion;
  KEEP short-circuits descent (nested PRUNE never fires); chained
  with `$match`; non-sentinel return rejected; null / empty
  expression rejected; array-element KEEP preserves nested
  sub-docs unchanged.

### `admin.system.version` returns the auth-schema doc

The companion to the b31 `admin.system.users` view. Some
user-management tools (and a handful of driver tests) read
`admin.system.version.find({_id: "authSchema"})` on startup to gate
which user-management features they offer; pre-slice that namespace
was empty and tools either skipped features or assumed the lowest
schema version.

The view returns one hard-coded doc:

```python
{"_id": "authSchema", "currentVersion": 5}
```

`currentVersion: 5` is the SCRAM-SHA-256 baseline (MongoDB 4.0+),
which is what SecantusDB actually implements — so the answer is
honest, not just placating. Other databases' `system.version` still
returns empty. Writes are rejected with code 13 (`Unauthorized`)
via the same `_reject_oplog_rs_write` helper that gates
`admin.system.users` and `local.oplog.rs`.

#### Added

- `storage._is_system_version` / `_system_version_docs` /
  `_find_system_version` / `_count_system_version` — same pattern
  as the b31 `admin.system.users` view; the doc set is fixed at
  one entry rather than scanned from a table.
- `storage.find_matching` + `count_matching` route through the
  new helpers when `(db, coll) == ("admin", "system.version")`.
- `commands._reject_oplog_rs_write` grew a third case for
  `admin.system.version`; existing call sites pick up the
  rejection with no further edits.
- `tests/test_system_version_view.py` (10 new tests): find /
  find_one / count / aggregate read paths; non-matching filter
  returns empty; other-db `system.version` is empty; write
  rejection on insert / update / delete / drop with code 13.

### `renameCollection` cross-process safety — pinned by `WiredTiger.lock`

A backlog item ("renameCollection: atomic per the storage RLock,
but no protection against concurrent writers across worktrees")
turns out to be structurally addressed by WiredTiger itself.
`wiredtiger_open` takes an exclusive lock on the data directory at
open time; a second open on the same path fails with
``WT_ERROR Resource busy`` before any state is touched, so the
"concurrent writers across processes" scenario can't exist in the
first place.

Within-process atomicity is the storage `RLock`. Cross-process
exclusion is `WiredTiger.lock`. The two layers compose: rename is
safe under both. The backlog entry is struck through.

#### Added

- `tests/test_storage_exclusion.py` (2 new tests) pinning the
  guarantee: a second `Storage(path=...)` on the same on-disk
  directory raises a `WiredTigerError` whose message contains
  `"busy"`; the first instance keeps working unaffected.
  `rename_collection` survives a close + reopen round-trip — the
  renamed namespace is visible to a fresh `Storage` instance.

### `$setWindowFields` aggregation stage — minimum viable subset

The largest v1 stable-API stage that wasn't yet wired up.
`$setWindowFields` is mongod's windowed-analytics surface — running
totals, rolling averages, per-partition rankings — all expressed
as a partition + sort + per-row windowed accumulator over the
input. Driver test suites probe it heavily.

Spec shape::

    {
        partitionBy: <expression>,         # optional; default = single partition
        sortBy: <sort spec>,               # optional; default = input order
        output: {
            <field>: {
                <$accumulator>: <expr>,
                window: {documents: [<lower>, <upper>]},  # optional
            },
        },
    }

For each output field, the accumulator runs over the rows inside
that row's window — within the row's partition, in the partition's
sorted order. Original input order is preserved in the result; the
partition / sort dance is purely internal to compute the new
fields.

#### Shipped (first-cut subset)

* The nine `$group` accumulators: `$sum`, `$avg`, `$min`, `$max`,
  `$first`, `$last`, `$push`, `$addToSet`, `$count`. The dispatch
  reuses `_ACC_DISPATCH` from `$group` — same per-doc accumulator
  semantics, just applied over a per-row windowed subset.
* Position-based windows via `window: {documents: [<lower>, <upper>]}`.
  Bound forms: integer offsets relative to the current row,
  `"current"` (= 0), and `"unbounded"` (partition edge).
* Default window (omit `window`) covers the whole partition.
  `[unbounded, current]` gives running-total semantics;
  `[-1, 1]` gives a 3-doc rolling window; etc.
* Empty-window output values: 0 for `$sum`/`$count`, [] for
  `$push`/`$addToSet`, null for the rest (matches mongod).

#### Deferred (raise `AggregateError` with a clear message)

* Range-based windows (`window: {range: [...]}`, optionally with
  `unit:` for date ranges). Needs value-based bounds + date
  arithmetic; out of scope for the first cut.
* Time-series functions: `$derivative`, `$integral`, `$linearFill`,
  `$locf`, `$shift`, `$expMovingAvg`. Each is its own slice and
  not in the common driver-test surface.
* Rank functions: `$rank`, `$denseRank`, `$documentNumber`. These
  need sort-key equality detection (tied rows get the same rank).
  Worth a dedicated slice when a workload needs them.

#### Added

- `src/secantus/aggregate.py`: `_stage_set_window_fields` handler
  + helpers `_window_bounds` (resolves
  `documents: [<lower>, <upper>]` to inclusive partition indices,
  with clamping to partition edges) and `_empty_window_value`
  (mongod-matching defaults). Wired into `_STAGES`. Reuses
  `_ACC_DISPATCH` + `_finalize` from `$group` so the accumulator
  semantics stay aligned across the two stages.
- `tests/test_set_window_fields.py` (15 new tests): no-partition
  totals; partitionBy splits totals correctly; rolling 3-doc sum
  with edge clamping; `[unbounded, current]` running total;
  `[unbounded, unbounded]` per-partition total; `$avg` / `$min` /
  `$max` / `$first` / `$last` over `[-1, 1]`; `$count` over
  `[-1, 1]`; `$push` / `$addToSet` accumulating across rows;
  sortBy controls running-total order independently of input
  order; original input order preserved on output; rank function
  raises; range window raises; missing output rejected; multiple
  accumulators in one output rejected; empty input → empty out.

## [0.5.1b24] — 2026-05-19

### Geo: legacy `$near` sibling form, 2d quadtree covering, java gauge

Three geo improvements that close the long-standing tail of the
phase 1/2 geo work and lift the mongo-java-driver gauge into the
geo surface for the first time.

Legacy mongod 2d shape — `{geo: {$near: [x, y], $maxDistance: r,
$minDistance: r2}}` with the distance bounds at *sibling* level
rather than nested inside `$near` — now matches end-to-end through
both the operator matcher and the 2d-index picker. This is exactly
what `mongo-java-driver`'s `Filters.near(field, x, y, max, min)`
and `Filters.nearSphere(...)` build. Unit conventions match
mongod: legacy `$near` takes the bound in input units (planar
Pythagoras); legacy `$nearSphere` takes radians on the unit sphere
(picker converts to meters for 2dsphere and to degrees for 2d).

The 2d range scan picks tighter Z-order ranges via a quadtree
decomposition of the bbox: each 2^k × 2^k power-of-2-aligned
quadtree cell that lands fully inside the bbox emits one
contiguous Z-range (the invariant that makes Z-order indexes
work). Partial-overlap cells recurse; pure-outside cells are
skipped. Falls back to the single coarse range if the
decomposition would exceed `max_ranges=32`. Tightens the WT range
scan on wider query polygons; correctness is unchanged
(per-doc verifier filters false positives either way).

`mongo-java-driver`'s `GeoJsonFiltersFunctionalSpecification` and
`GeoFiltersFunctionalSpecification` (driver-core functional)
joined the java gauge include list and both pass 10/10. They
exercise `$geoWithin` / `$geoIntersects` / `$near` / `$nearSphere`
through the driver's `Filters` builder against a real 2d and
2dsphere index — the kind of integration coverage neither the
pymongo conformance gauge nor our in-tree pymongo tests reach.

#### Added

- `secantus.geo_index.planar_2d_covering_ranges()` — quadtree
  Z-order range decomposition for 2d index scans. Returns up to
  32 tight `(lo, hi)` ranges; falls back to a single coarse range
  on cap overflow.
- 6 new tests in `tests/test_geo_query.py` /
  `tests/test_geo.py`: sibling-form `$near` with `$maxDistance`,
  sibling-form annulus (max+min), sibling-form `$nearSphere`
  with radians convention, single-range quadtree for an aligned
  bbox, multi-range quadtree for an off-axis bbox, fallback to
  single range under cap.
- `_DRIVER_CORE_FUNCTIONAL_INCLUDES` in
  `java_validation/include_modules.py`: brings the two upstream
  geo functional specs into the java gauge as
  `:driver-core:test` filtered runs.
- [`docs/geospatial.md`](geospatial.md) — dedicated reference
  page: operator-by-operator, both index types, doc-side shapes
  accepted, the legacy / GeoJSON / spherical distance-unit
  conventions, a worked deployment example, validation surface
  summary. Linked from the Highlights list and added to the
  Sphinx toctree.
- [`docs/indexes.md`](indexes.md) — new geospatial section
  pointing at the dedicated page; the "Acceleration summary
  across index types" table now covers `2d`, `2dsphere`, and
  compound geo + scalar.

#### Changed

- `_parse_near_spec` now returns a 5-tuple
  `(center, max_d, min_d, spherical, legacy_form)`; consumers use
  the new `legacy_form` flag to pick the right unit conversion
  (legacy+spherical → radians; legacy+planar → input units;
  GeoJSON → meters).
- 2d-index picker uses the multi-range coverer; existing single-
  range `planar_2d_covering` kept as the coarse fallback.
- [`docs/indexes.md`](indexes.md) — "What's still missing" list
  rewritten. Multi-field sort acceleration, multikey indexing,
  and basic collation all shipped long ago and shouldn't have
  been on the gap list; the actual remaining gaps (per-index
  collation, TTL background sweeper, text / hashed indexes)
  replace the stale entries.
- [`docs/production.md`](production.md) — added a paragraph on
  per-write `writeConcern: {j: true}` routing as the
  finer-grained alternative to the daemon-wide
  `sync_on_commit = true` knob.

#### Fixed

- Legacy mongod `{geo: {$near: [x, y], $maxDistance: r}}`
  previously raised `unsupported query operator: $maxDistance`
  because the dispatcher treated the sibling bound as a
  standalone operator. The matcher now skips the sibling keys
  when iterating and passes them into `_op_geo_near`.
- 2d-index picker no longer over-filters on `$nearSphere` legacy
  form: the radians bound is converted to degrees before
  building the planar disk, matching mongod's behaviour against
  a 2d index.

## [0.5.1b23] — 2026-05-19

### Native TLS + mTLS + per-write `j:true` — production gaps closed

Three slices land together against the production-readiness gaps
called out in the `docs/production.md` page.

`[tls] cert_file` + `[tls] key_file` (in `secantusdb.toml`) or
`--tls-cert-file` / `--tls-key-file` (CLI) makes the daemon wrap
every accepted socket in TLS before the wire protocol starts.
Clients connect with `mongodb://host:port/?tls=true&tlsCAFile=<ca>`
and SecantusDB negotiates the TLS handshake itself; the
connection thread then sees an encrypted socket-like object and
serves mongo wire frames over it unchanged. This closes one of
the biggest production-deployment gaps the `docs/production.md`
page called out — operators no longer need to terminate TLS at an
nginx / HAProxy / stunnel reverse proxy that becomes part of the
trust boundary.

mTLS lands as a layer on top: set `[tls] ca_file` and the daemon
asks connecting clients for their own X.509 cert during the TLS
handshake, verifying it against the configured CA bundle. Set
`[tls] require_client_cert = true` to reject clients that don't
present a cert; the default (`false`, `CERT_OPTIONAL`) verifies a
cert if presented and accepts clients without one — useful for
staged rollouts. mTLS is a coarse-grained "you're someone we
approved of" gate; SCRAM-SHA-256 still identifies the specific
user on top. mongod's `MONGODB-X509` auth mechanism
(cert-subject-DN as the username, no SCRAM step) is a separate
follow-on slice.

Python's `PROTOCOL_TLS_SERVER` (TLS 1.2+, no SSLv2/3 fallback,
default cipher list) is the only protocol mode. The `SSLContext`
is built once at startup and cached — hot cert rotation requires
a daemon restart. `certbot renew --post-hook 'systemctl reload
secantusdb'` is the standard pattern. Without the cert / key
kwargs the daemon stays plaintext exactly as before — no
regression risk for the 1300+ existing tests.

The b20 `sync_on_commit` knob enabled per-commit fsync at the
*connection* level — every write on the daemon shared the same
durability mode. The third slice finishes the story: the per-write
`writeConcern.j` flag now threads from the wire layer through
`Storage.insert` / `update_matching` / `delete_matching` (and all
four `findAndModify` paths) into
`_batch_transaction(sync=True)`, which calls
`session.commit_transaction("sync=on")`. A client can now mix
`j: true` and `j: false` writes against one daemon: the j:true
subset pays the per-commit fsync cost (closes the durability gap),
the rest stays fast.

#### Added

- `[tls]` table in `secantusdb.toml` (`cert_file`, `key_file`,
  `ca_file`, `require_client_cert`). Half-configured TLS (only one
  of cert/key set) raises `ValueError` at startup so deployment
  mistakes can't silently fall back to plaintext.
- `--tls-cert-file` / `--tls-key-file` / `--tls-ca-file` /
  `--tls-require-client-cert` CLI flags. Standard precedence:
  SecantusConfig defaults < TOML < explicit CLI.
- `SecantusDBServer(tls_cert_file=..., tls_key_file=...,
  tls_ca_file=..., tls_require_client_cert=...)` kwargs. When
  cert/key are set an `ssl.SSLContext` is built in `__init__` and
  used to wrap accepted sockets in `_serve_forever`. When ca_file
  is also set, the context asks clients for an X.509 cert during
  the handshake and verifies it against that CA.
- `tests/test_tls.py`: 12 tests via `trustme` for ephemeral CA +
  client cert fixtures. Covers TLS round-trip, non-TLS-client
  rejection, no-args plaintext path (no regression),
  half-configured raises, missing-cert startup error,
  active_conns leak guard, and the four mTLS modes (required +
  valid cert / required + no cert / required + foreign-CA cert /
  optional + both modes).
- `journal: bool = False` kwarg on `Storage.insert` /
  `update_matching` / `delete_matching`. When True, the WT
  transaction commits with `session.commit_transaction("sync=on")`
  — forces a per-commit fsync of the log regardless of the
  connection's `transaction_sync` config.
- `_batch_transaction(*, sync: bool = False)` context-manager
  kwarg. The per-commit-fsync escape hatch the new `journal` write
  kwargs route through.
- `tests/test_write_concern_journal.py`: 10 tests covering the
  storage-layer kwarg threading (`_batch_transaction` is invoked
  with `sync=True/False` appropriately), wire-level happy paths
  on insert / update / delete / findAndModify, and the positive +
  negative routing assertions.

#### Changed

- TLS / mTLS handshake errors are logged + the socket closed +
  the active-connection slot released; the daemon keeps serving
  everyone else.
- `writeConcern: {j: true}` is now honoured per-write: the wire
  layer extracts the flag and threads it through to
  `_batch_transaction(sync=True)`. Previously the flag was
  accepted on the wire but had no effect — only the daemon-wide
  `sync_on_commit` knob (b20) could enable per-commit fsync.
- `docs/production.md` updated: "Native TLS" is no longer in the
  gaps list; the dedicated TLS section now shows the in-process
  config plus the mTLS opt-in instead of an nginx-stream-module
  example.
- `docs/configuration.md` documents the full `[tls]` schema
  (cert / key / ca / require_client_cert), the hot-rotation
  caveat, and the cipher-suite "out of scope for v1" note.

#### Dependencies

- `trustme>=1.2` added to the `dev` extra for the test CA
  fixture (transitively pulls `cryptography`).

## [0.5.1b20] — 2026-05-19

### `secantusdb.toml` config file, native checkpoint restore, j:true durability knob

Two production-shaping slices land together. A new
`secantusdb.toml` configuration file exposes every CLI flag plus
the WT and oplog knobs that were previously hard-coded — including
`cache_size` (so you can size the engine for your dataset instead
of running with the 1 GB test default) and a `sync_on_commit`
switch that closes the long-standing `writeConcern: {j: true}`
durability gap by enabling WT's per-commit fsync. The loader
auto-discovers `./secantusdb.toml`, `~/.secantus/secantusdb.toml`,
and `/etc/secantus/secantusdb.toml`; an explicit `--config PATH`
overrides the search. CLI flags still win over file values, so the
file is a deployment baseline rather than a lock-in.

A new `secantusAdmin.restoreArchive` wire command and matching
`secantusdb-restore-archive` offline CLI close out the backup
story started in b18 — extract a backup `.tar.gz` into a target
directory the operator then points a fresh SecantusDB process at.
The admin UI's per-row Restore button now adapts to backup type:
mongodump directories still call `mongorestore`; native `.tar.gz`
archives surface an inline target-dir field and an Extract action
that hits the new endpoint. Restore intentionally doesn't try to
swap the WT home under a running server (the connection-thread
session-caching layer would need a wholesale rework first), and
matches how real mongod restore tooling already trains operators.

Drive-by fix: the admin UI's "Existing backups" list now also
includes `.tar.gz` files. The native archives created by the b18
backup button were previously invisible because `list_backups`
only enumerated directories.

The new [Running in production](production.md) doc page ties the
config-file, native-backup, and restore work together — honest
comparison vs single-node Postgres (the more useful framing than
"SecantusDB vs mongod"), the gaps you have to accept, and a
concrete `systemd` / TLS / backup / monitoring deployment shape.

#### Added

- [Running in production](production.md) docs page — honest
  comparison vs single-node Postgres (the more useful framing than
  "SecantusDB vs mongod-for-prod"), the gaps you must accept (no
  native TLS, no PITR, no replication, beta maturity), and a
  concrete deployment shape: `systemd` unit, `secantusdb.toml`
  with `sync_on_commit = true`, SCRAM auth provisioning, nginx
  stream TLS termination, hourly native checkpoint backups with
  off-host sync, the restore drill, `serverStatus` scraping for
  Prometheus / Datadog, and capacity sizing notes for
  `cache_size`.
- `secantusdb.toml` configuration file (see
  [Configuration](configuration.md) for the full schema). Auto-
  discovered from `./secantusdb.toml`,
  `~/.secantus/secantusdb.toml`, `/etc/secantus/secantusdb.toml`;
  `--config PATH` disables discovery and loads a specific file.
  Unknown keys / unknown top-level tables fail loudly at startup
  so typos can't silently leave the engine running on the
  hard-coded default.
- `secantus.config.SecantusConfig` dataclass + `load_config()` /
  `apply_overrides()` helpers. CLI flags' argparse defaults are
  now `None` (the "user did not pass this" sentinel) so the
  precedence chain is `SecantusConfig defaults < secantusdb.toml
  < explicit CLI flag` — file is a per-deployment baseline, the
  CLI overrides for one-off runs.
- New CLI flags exposing previously-hard-coded knobs:
  `--cache-size`, `--session-max`, `--sync-on-commit`,
  `--oplog-retention-seconds`, `--oplog-max-entries`. Each has a
  matching `[storage]` / `[oplog]` key in the config file.
- `Storage.__init__` accepts `cache_size`, `session_max`,
  `sync_on_commit` kwargs. The WT engine config string is built
  from these instead of being a hard-coded literal.
- `secantusAdmin.restoreArchive` wire command. Accepts
  `archivePath` (server-side path to `.tar.gz`), `targetDir`
  (extraction destination), and optional `allowExisting` (overlay
  into a non-empty dir). Returns `{targetDir, fileCount, archive,
  ok: 1}`. RBAC: `fsync` action, cluster scope.
- `secantus.storage.extract_backup_archive(archive_path,
  target_dir, *, allow_existing=False)` — module-level helper
  shared by the wire command, the admin route, and the CLI.
  Validates that the archive contains a `WiredTiger` metadata
  file before unpacking, so a malformed tarball can't pollute the
  target.
- `secantusdb-restore-archive` console script (new `[project.scripts]`
  entry). Same validation as the wire command, no server needed.
- Admin UI per-row **Extract** action on `.tar.gz` rows, posting
  to `POST /backup/restore-archive` with editable target-dir form
  field; the existing `Restore` button still handles mongodump
  directories.

#### Changed

- `writeConcern: {j: true}` is now honourable end-to-end via
  `[storage] sync_on_commit = true` (or `--sync-on-commit`),
  which sets WT's `transaction_sync=(enabled=true,method=fsync)`.
  Closes the long-standing durability gap previously documented
  in the backlog. Off by default (matches mongod's default
  `{w:1, j:false}`) since the throughput cost is significant.
- `secantus.admin.backup.list_backups()` now includes
  `*.tar.gz` files alongside directories. Native-archive backups
  produced by b18's backup button were previously invisible in
  the admin UI's "Existing backups" list.
- `MongoFacade.restore_archive(archive_path, target_dir, *,
  allow_existing=False)` — new admin client facade method.

#### Fixed

- "Existing backups" table on `/backup` was silently dropping
  every `.tar.gz` produced by the native checkpoint backup path
  introduced in v0.5.1b18 (only dump *directories* were listed).
  Both kinds now render with the correct per-row restore action.

## [0.5.1b18] — 2026-05-18

### Native WT-checkpoint backups, admin UI /oplog page, and change-stream fidelity wins

The natural follow-on to v0.5.1b17's `local.oplog.rs` synthetic
collection lands as the admin UI `/oplog` page: a paged entry
browser with a window selector (last 50 / 500 / 5000), `op`-checkbox
filter (`i` / `u` / `d` / `c` / `n`), `ns` substring filter, and a
per-row expandable JSON body. Auto-refreshes every 5 s. The data
source is just `client.local.oplog_rs.find()` — no new server-side
surface needed, only the page chrome and an `_rows` partial that
follows the same pattern as `/connections` + `/cursors`.

`showExpandedEvents` on change streams now matches mongod: the flag
defaults to `false`, and DDL "expanded" events (`createIndexes`,
`dropIndexes`) are suppressed unless the user opts in via
`coll.watch(show_expanded_events=True)`. Previously these surfaced
unconditionally — more permissive than mongod, and broke the
conformance contract for tests that assume the stable v1 event set.

`killOp` lands as a real wire command that closes the target
connection's socket via `shutdown(SHUT_RDWR)`. Any in-flight command
finishes, the per-connection thread's next `recv` returns 0, the
loop exits, and the connection unregisters cleanly. Real mongod uses
a per-op interrupt flag, which would need cancellation infrastructure
SecantusDB doesn't carry — but "close the socket" is the visible
end-state users care about, and the kill-and-reap admin button on
`/connections` is now functional.

`$sample` becomes deterministic when `SECANTUS_SAMPLE_SEED=<n>` is
set in the environment. Builds a dedicated `random.Random(seed)`
instance at module load instead of mutating the global `random`
state, so other code sharing the process keeps its own entropy.
Closes the long-standing test-flake source where `$sample` results
varied run-to-run.

#### Added
- Admin UI `/oplog` page (`routers/oplog.py` +
  `templates/pages/oplog.html` + `templates/partials/oplog_rows.html`):
  window / op / ns filters, expandable per-row JSON, 5 s
  auto-refresh, sidebar entry between Profiler and Maintenance.
- `killOp` wire command + `kill(conn_id)` on
  `ConnectionRegistry` (shuts down the socket via
  `shutdown(SHUT_RDWR)`). Per-connection sockets are now stashed on
  the registry at `_handle_client` time.
- `A_KILLOP` privilege action in `secantus.rbac`; granted by
  `clusterAdmin` and `root`.
- Admin UI `/connections` Kill button (was a placeholder),
  typed-confirm modal (`partials/connection_kill_modal.html`),
  facade `kill_connection(conn_id)` method.
- `ChangeStreamSpec.show_expanded_events` parsed from
  `$changeStream.showExpandedEvents`; threaded into
  `changestreams.project`.
- `SECANTUS_SAMPLE_SEED` env var (read at `aggregate` module
  import) — `$sample` uses a dedicated `random.Random(seed)`
  when set.
- `secantusAdmin.backupArchive` wire command + `Storage.create_archive`
  + admin UI "Run native checkpoint backup" button: forces a WT
  checkpoint then tars the storage directory into a single
  `.tar.gz`. Faster + atomic vs `mongodump`; restore is "extract
  + start a new SecantusDB pointing at it". Rigorous round-trip
  test coverage in `tests/test_backup_restore.py` (doc identity at
  scale, every non-default index shape, oplog tail continuity,
  capped collection options + FIFO state, SCRAM users / roles,
  concurrent-writes consistency, archive portability, repeated-
  backup idempotency).
- `$densify` month / quarter / year units via
  `dateutil.relativedelta`. `quarter` is canonically 3 months.
  Adds `python-dateutil>=2.8` to the runtime dependencies (pure
  Python, available almost everywhere as a transitive dep).

#### Changed
- `changestreams.project` suppresses `createIndexes` / `dropIndexes`
  events unless the caller passed `show_expanded_events=True`
  (mongod-faithful default-off). The three existing tests +
  cross-driver DDL smokes (mongosh / node / go / java) all set the
  opt-in.

#### Fixed
- Closes backlog entry `$sample uses random.sample without a fixed
  seed` — deterministic via env var.
- Closes backlog entry `killOp / connection-close command` — admin
  UI Kill button is functional.
- Closes backlog entry `showExpandedEvents — accepted, ignored`.
- Closes backlog entry `Admin UI /oplog page`.
- `updateDescription.truncatedArrays` now emits for any array
  shrink (not just strict head-prefix), with indexed ``updatedFields``
  for kept-prefix changes — matches mongod's $v:2 in-place diff
  rather than wholesale-replacing on any reshape. Same-length-with-
  changes arrays also produce indexed ``arr.<i>`` updates now
  (previously wholesale). Closes the §3.2 backlog entry.

## [0.5.1b17] — 2026-05-17

### `local.oplog.rs` queryable from pymongo, `$merge` pipeline form + `$fill` stage + `$$var.path` resolution

Real mongod exposes the oplog as a queryable collection at
`local.oplog.rs` — pymongo clients can `db.oplog.rs.find()` against
it the same way they would against any collection. Until this release,
SecantusDB's oplog was internal only: `Storage.read_oplog` /
`oplog_floor_seq` / `oplog_tail_seq` were Python methods but had no
wire surface. Now `local.oplog.rs` is a synthetic read-only view —
`list_collections("local")` surfaces it, `find` / `count` /
`listCollections.options` route to a reader that walks the oplog WT
table directly, and write attempts (`insert`, `update`, `delete`,
`findAndModify`, `drop`, `create`, `createIndexes`) refuse with code
13 (Unauthorized) like mongod does. The deferred admin UI `/oplog`
page is unblocked as a follow-up; for now, debugging an in-flight
change-stream pipeline is as simple as
`client.local.oplog_rs.find({"op": "u"}).sort("ts", -1).limit(20)`.

The aggregation expression library picks up two of the three remaining
stages on most "more stages" wishlists. `$merge` was partly
implemented; this batch fills in the rest: `whenMatched: [<pipeline>]`
runs a sub-pipeline against the matched target doc with `$$new` bound
to the source doc and any user `let` vars threaded through;
`whenMatched: "delete"` (MongoDB 5.0+) removes the matched doc; a
unique-index guard refuses non-`_id` `on` fields without a `unique:
true` index covering them, matching mongod's rule against silent
on-field collapse.

`$fill` lands fresh — the 5.3+ stage for filling missing/null fields.
Three modes per output field: `{value: <expr>}` replaces with an
evaluated expression; `{method: "locf"}` carries the last observation
forward within the partition's sortBy order; `{method: "linear"}`
interpolates between bracketing non-null anchors along the sortBy field
(works for numbers and datetimes — timedelta arithmetic divides cleanly
to float and multiplies back to timedelta). Partitioning via
`partitionByFields` or `partitionBy`; sortBy required when any output
uses `method`.

The `$merge` pipeline form was the first thing in the repo to exercise
`$$var.path` (e.g. `$$new.delta`), and surfaced that the expression
evaluator only did exact-name var lookup. Fixed in the same batch:
`$$var.field.path` now walks the dotted path into the resolved value
across `$$ROOT.f` / `$$CURRENT.f` / user-let vars.

#### Added
- `local.oplog.rs` synthetic collection: queryable via `find` /
  `count` / `listCollections`. Walks the existing oplog WT table via
  a private session for cross-thread visibility. `list_databases`
  surfaces `local` whenever the oplog is enabled.
- `$merge whenMatched: [<pipeline>]` with `$$new` binding + `let` clause
  for user-defined vars (`aggregate._stage_merge`).
- `$merge whenMatched: "delete"` (MongoDB 5.0+).
- `$merge` unique-index guard on non-`_id` `on` fields.
- `$fill` stage with `value`, `locf`, and `linear` modes
  (`aggregate._stage_fill`).
- `$$var.field.path` dotted-path resolution in
  `expressions._resolve_var`.
- `docs/changelog.md` as the system of record (see the
  [changelog](changelog) itself and the `changelog/` Python package
  that generates blog posts from it).

#### Changed
- Writes to `local.oplog.rs` (insert / update / delete / findAndModify
  / drop / create / createIndexes) refuse with code 13 (Unauthorized).
- `$merge` validates `whenMatched` / `whenNotMatched` against the
  allowed string sets — typos surface as `AggregateError` instead of
  silently falling through to the default merge.

## [0.5.1b16] — 2026-05-16

### Sidebar grouping, auto-refreshing connections and cursors, Roles in the nav

The `/connections` and `/cursors` admin pages have always been live-data
views — they read `currentOp` and render the connection / cursor list
each time the page is requested — but they didn't refresh. The dashboard
polls 1 Hz over a WebSocket; these two felt stale next to it. v0.5.1b16
extracts each table's tbody into an HTMX partial and lets the tbody
itself swap every 5 s via `hx-trigger="every 5s"
hx-get="/connections/_rows"`. The page chrome and column headers stay
fixed; only the rows refresh. `/connections` also gains a (disabled)
Actions column with a tooltip explaining that connection-kill is
deferred until SecantusDB grows `killOp` — purely a layout-symmetry
fix so the page mirrors the shape of `/cursors`.

The sidebar gets two structural fixes. A `Roles` entry now lives
directly under `Users` with a sub-nav indent (it was reachable only via
the breadcrumb on `/users`, and `roles.html` was setting `active="users"`
so the wrong sidebar item highlighted while you were on the page). A
second visual separator above `Change stream` marks the boundary between
per-target data pages and operational-state pages, mirroring the
existing separator below `Server`.

A separate fix: 15 `*_via_mongosh` cross-driver smoke tests are now
grouped into a single xdist worker. Mongosh launches a full Node-based
shell, and under heavy parallel load the PBKDF2 work inside SCRAM-SHA-256
auth could blow past mongosh's connect timeout. Tagged with
`@pytest.mark.xdist_group(name="mongosh_smokes")` so they serialize.

#### Added
- Sidebar `Roles` entry under `Users` with sub-nav indent + correct
  active highlight on `/roles`.
- Sidebar visual separator above the operational-state group
  (`nav-ops-start` CSS class).
- `/connections` + `/cursors` auto-refresh tbody (`hx-trigger="every 5s"`)
  with new `_rows` partial endpoints.
- `/connections` disabled Actions column for layout symmetry with
  `/cursors`; tooltip explains `killOp` is deferred.

#### Fixed
- 15 `*_via_mongosh` cross-driver smoke tests serialized via
  `xdist_group="mongosh_smokes"` to dodge PBKDF2-handshake timeouts under
  parallel-test CPU contention.

## [0.5.1b15] — 2026-05-16

### One scaffold for every confirmation modal — escape, focus-trap, restored focus

The `secantus-admin` UI has nine confirmation / edit modals
(drop-database, drop-collection, drop-index, drop-user, change-password,
manage-roles, edit-document, delete-document, kill-cursor). They were
assembled at slightly different times and drifted in five different ways
— different destructive-button copy, different typed-confirm targets
(the delete-document modal asked the user to type the collection name
shared by every row; the kill-cursor modal asked for the giant int
cursor id), no Escape-to-close, no focus restoration to the trigger
element, no focus trap so Tab leaked back into the page behind, and
`aria-label="Close"` only on two of nine close buttons.

v0.5.1b15 consolidates all nine on a shared scaffold: a new
`modal-shell.js` exposes `openModal(url)` / `closeModal()` /
`setupModal(el)` plus a global htmx hook that captures the trigger
element so `closeModal()` can restore focus. Each modal partial has the
same overlay shape — `x-init="setupModal($el)"`,
`@click.self="closeModal()"`, `@keydown.escape.window="closeModal()"`,
`role="dialog"`, `aria-modal`, `aria-labelledby` — and Tab / Shift+Tab
cycle within the modal's focusable children rather than escaping into
the page behind.

Three substantive fixes ride along with the scaffolding: destructive
button copy now always restates action+noun (Kill cursor / Delete
document / Drop index / Drop user / Drop database / Drop collection);
the delete-document typed-confirm asks for the doc's `_id` value rather
than the collection name; the kill-cursor typed-confirm asks for the
collection `ns` rather than the unguessable cursor id. None of these
change SecantusDB's wire-protocol behaviour.

#### Added
- `static/js/modal-shell.js`: `openModal(url)`, `closeModal()`,
  `setupModal(el)`, htmx hook for trigger-element capture.
- `[x-cloak]` CSS helper to prevent Alpine flash on first paint.

#### Changed
- All 9 confirmation / edit modal partials use the shared overlay
  shape with `role="dialog"` / `aria-modal` / `aria-labelledby`.
- Destructive button copy restates action+noun across the board.
- `delete-document` typed-confirm uses the doc's `_id` value (was the
  collection name).
- `kill-cursor` typed-confirm uses the collection `ns` (was the cursor
  id).

#### Fixed
- Escape now closes every modal.
- Focus restored to the triggering element after modal close.
- Tab focus-trap inside modals.
- `aria-label="Close"` on all 9 close buttons (was on 2).

## [0.5.1b14] — 2026-05-15

### Admin UI punch list — five silent-failure modes fixed

The May 2026 end-to-end review of the `secantus-admin` web UI catalogued
five P0s — bugs that didn't crash anything but presented wrong
information to the user. v0.5.1b14 fixes all five. None require any
database-level change; this is purely admin-UI plumbing, but each one
was either lying to the user or hiding a real error behind cheerful
copy.

The biggest was the **profiler page swallowing every exception** while
reading `system.profile`. A bare `except Exception:` rendered "no
entries yet — run an operation to see one appear here" no matter what
the underlying error was, including the target server being completely
unreachable. The clause is now narrowed to `PyMongoError` and the
friendly error message gets funnelled into the page's normal error
banner. The same page also had a **`flash` keyword argument that the
template never rendered** — every settings change returned `HX-Redirect`
and the user saw zero confirmation that anything had happened. The POST
handler now re-renders the page inline with a flash banner that names
the new level / slowms / sampleRate values.

The other three are dead-code cleanups: the **doc tour** in
`docs/admin.md` walked the user through a `/console` page that was
renamed to `/query` two refactors ago; the **Maintenance "Drop
collection" form** had an `hx-get` pointing at a route that never
existed; and the **dashboard router** still exposed a `GET
/_partials/dashboard-tiles` endpoint from before the WebSocket dashboard
landed.

#### Fixed
- Profiler page: narrowed bare `except Exception:` to `PyMongoError` so
  server-down errors surface (`routers/profiler.py`).
- Profiler page: added flash banner block to template + POST handler
  re-renders inline instead of `HX-Redirect`.
- Maintenance "Drop collection" form: dropped dead
  `hx-get="/maintenance/drop-collection-redirect"` attribute.
- Dashboard router: deleted unused
  `GET /_partials/dashboard-tiles` endpoint, partial template, and the
  two tests that exercised them.
- `docs/admin.md`: replaced stale `### Console` section with
  `### Query (/query)` + `### Insert (/insert)` + new `### Server
  (/server)` subsection.

## [0.5.1b13] — 2026-05-15

### Zero actionable failures — every driver gauge classified, every gap explained

Over the past few releases the cross-driver gauge pass rate has been
climbing — 99.5% at v0.5.1b4, 99.9% by last week's refresh. The last
0.1% was a handful of failures that either could not be fixed in
SecantusDB (a Java-driver SDAM cascade triggered by a server-side
`APIStrictError`), reproduced only under heavy parallel load (two
`mongo-go-driver` flakes), or assumed a multi-node replica-set
deployment SecantusDB deliberately doesn't simulate (Ruby's `w: 2`
write-concern test). Reporting them as plain "failures" overstated the
gap — but silently dropping them would let real regressions hide in the
same column.

v0.5.1b13 introduces **`validation_summary/expected_failures.py`** — a
small per-gauge registry of `(pattern, rationale)` entries. The
cross-driver summary now separates "Failed" (unexpected, a real bug we
need to fix) from "Expected" (a documented gap with a one-line reason
that ships in the report). A new **Adjusted** column reports the rate
excluding expected failures from the denominator — "how much of the
conformable surface actually conforms." Current numbers: **7,186 tests,
6,254 passed, 0 unexpected failures, 5 expected failures, 927 skipped —
100.0% adjusted across every driver.**

This release also bundles the gauge improvements that landed since
v0.5.1b4: `mapReduce` returns a graceful empty result for non-canonical
bodies, `$changeStream` against a standalone topology is rejected with
code `40573`, Node CSOT explain-plus-`timeoutMS` tests pass via a new
`block_connection` / `block_time_ms` failpoint pair, `getParameter`
advertises `authenticationMechanisms: ["SCRAM-SHA-256"]`, and
`createIndexes` / `create` reject unknown options up-front.

#### Added
- `validation_summary/expected_failures.py`: per-gauge registry of
  documented-known failures with rationales.
- Cross-driver summary "Expected" + "Adjusted pass rate" columns.
- `block_connection` / `block_time_ms` failpoint fields
  (`failpoints._FailCommand`).

#### Changed
- `mapReduce` returns a graceful empty result for non-canonical
  map/reduce bodies (wire-shape probes pass).
- `$changeStream` on a standalone topology is rejected with code 40573.
- `getParameter` advertises `authenticationMechanisms: ["SCRAM-SHA-256"]`.
- `createIndexes` rejects unknown per-index options
  (`_INDEX_SPEC_KNOWN_OPTIONS` whitelist).
- `create` rejects unknown collection options
  (`_CREATE_KNOWN_OPTIONS` whitelist).
- `validate-all` serialized (`max_workers=1`) to dodge load-induced
  inter-gauge flakes.

## [0.5.1b4] — 2026-05-12

### Cross-driver conformance summary — 99.5% across 7,186 tests on one page

Until this release, comparing SecantusDB's conformance across the five
driver gauges (pymongo / mongo-java-driver / mongo-go-driver /
mongo-node-driver / mongo-ruby-driver) required opening five different
reports and squinting at five different per-category breakdowns whose
denominators came from incompatible units of count — JUnit `<testcase>`
versus Mocha test versus RSpec example versus `go test` event versus
pytest item.

v0.5.1b4 ships **`docs/validation-summary.md`** — a single table that
normalises on test count, one row per gauge, the same five columns
across the board: tests run, passed, failed, skipped, pass rate. A new
`validation_summary` Python module reads each gauge's raw artifact
under `.validation/` directly and renders the table; a new
`invoke validate-summary` task refreshes it.

Current numbers: **7,186 tests, 6,232 passed, 33 failed, 921 skipped —
99.5% pass rate** across all five drivers. Java is biggest by raw count
(4,710 tests, 4,242 passed); Node smallest (364).

This release also rolls up two driver-gauge fixes that landed since
v0.5.1b1: a Java widening to 21 of 112 driver-sync functional classes
(+34 passes), and a snapshot-read-concern rejection that turned three
`SessionsTest` snapshot-error scenarios from "expected error, got
success" into "expected error, got `SnapshotUnavailable` (code 246)".

#### Added
- `docs/validation-summary.md` cross-driver normalized table.
- `validation_summary/` Python module (raw-artifact reader + renderer).
- `invoke validate-summary` task.
- `snapshot` readConcern rejected with code 246
  (`SnapshotUnavailable`).
- Java gauge: `ChangeStreamsTest`, `UnifiedWriteConcernTest`,
  `VersionedApiTest` unified-spec runners (21 of 112 driver-sync
  functional classes total).

#### Fixed
- RTD build for v0.5.1b3 failed on a missing toctree entry for the new
  summary file; b4 is the first release where the docs match what's on
  PyPI.

## [0.5.1b1] — 2026-05-12

### Java gauge scope made honest — 18 of 112 driver-sync classes, five named follow-ups

The Java gauge passing rate had been reported at "100%" — but only
across the 13 driver-sync functional classes the gauge was running.
v0.5.1b1 widens the include set to 18 of 112 and adds an explicit
**Scope** section to `docs/validation-report-java.md` that surfaces the
"X of 112 driver-sync functional classes" denominator so the headline
number isn't misleading.

The widened set surfaced five real failures, all named and tracked in
`tasks/backlog.md` §5: Java apiStrict pool-clear cascade, mapReduce
non-canonical bodies, snapshot reads on standalone, distinct
apiStrict — none are SecantusDB bugs, but they're now documented
expected-fail entries.

#### Added
- Java gauge include set widened to 18 of 112 driver-sync functional
  classes (`java_validation/include_modules.py` waves 1 + 2).
- "Scope" section in Java validation report exposing the include-set
  denominator (`java_validation/generate_report.py`).

## [0.5.0b18] — 2026-05-12

### Ruby gauge climbs to 99%, completing the cross-driver 99–100% band

The Ruby gauge had been the weakest of the five at ~95% — a handful of
real SecantusDB gaps the Ruby driver exercises but the others don't.
v0.5.0b18 closes the high-value ones: `writeConcernError` is now
attached on `w > 1` (CannotSatisfyWriteConcern code 100), invalid
`wildcardProjection` is rejected on `createIndexes`, `commitQuorum` is
validated at the top level, `listIndexes` rejects negative batchSize
(code 51024), and `$collStats` surfaces capped-collection bounds
(`storageStats.{capped, max, maxSize}`).

Net: Ruby gauge from 94.6% → 99.7%, 13 net passes. All five driver
gauges now sit in the 99–100% band.

#### Added
- `writeConcernError` attached on `w > 1` (`CannotSatisfyWriteConcern`
  code 100).
- `createIndexes` validates `wildcardProjection` shape.
- `commitQuorum` validated at top-level.
- `$collStats` surfaces capped bounds (`storageStats.{capped, max,
  maxSize}`).

#### Changed
- `listIndexes` rejects negative `batchSize` with code 51024.

## Older releases

Releases before v0.5.0b18 (the `v0.3.0aN` and `v0.4.0bN` lines, and
v0.5.0b1 through v0.5.0b3) shipped before this changelog was the system
of record. See the [GitHub
Releases](https://github.com/jdrumgoole/SecantusDB/releases) page for
the auto-generated commit-list notes from those tags.

[Unreleased]: https://github.com/jdrumgoole/SecantusDB/compare/v0.5.1b18...HEAD
[0.5.1b24]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b24
[0.5.1b23]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b23
[0.5.1b20]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b20
[0.5.1b18]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b18
[0.5.1b17]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b17
[0.5.1b16]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b16
[0.5.1b15]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b15
[0.5.1b14]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b14
[0.5.1b13]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b13
[0.5.1b4]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b4
[0.5.1b1]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.1b1
[0.5.0b18]: https://github.com/jdrumgoole/SecantusDB/releases/tag/v0.5.0b18
