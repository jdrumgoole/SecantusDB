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
