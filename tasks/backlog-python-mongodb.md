# Backlog: the Python MongoDB server

A living list of where this server does not yet behave like the real
product it stands in for: stubs, stopgaps, deferred work and known bugs.
Update it when you stub something, defer a slice, or find a limitation, and
delete an item's lines when you fix it -- finished work lives in git
history and `changelog.d/`, not here. Don't add work that already has a fix
in flight; that lives on its branch and PR.

The backlog is four files, one per server:

| file | server |
| --- | --- |
| `tasks/backlog-rust-mongodb.md` | the Rust MongoDB server, and the Rust storage both Rust servers use; also CI, build and release items no one server owns |
| `tasks/backlog-rust-postgres.md` | the Rust PostgreSQL server |
| `tasks/backlog-python-mongodb.md` | the Python MongoDB server (and the admin UI) |
| `tasks/backlog-python-postgres.md` | the Python PostgreSQL / SQL server |

An item about more than one server is in each file it concerns. Section
numbers (`7.077`, `3.1`, ...) are the ones the single `tasks/backlog.md`
used, kept so the code comments that cite them still resolve.

**`- [ ]` is open work, and every item here is open.** An open item's bold
headline must not read as finished: if completed context belongs in it,
lead with `**OPEN —` (`tests/test_backlog_markers.py`).

**Reproduce an item before working it** -- a probe, the failing test, the
query -- and size it from that, not from the text. Entries here have
described work as missing that was done, and understated bugs by naming one
symptom of several. Correct a superseded entry rather than adding a newer
one beside it.

## Open from the 2026-08-26 triage

- **Scope decision — `$where` and `$function`** need a JavaScript engine.
  Neither server has one, and none is planned (WONTFIX).

## 2. Stopgaps (functional but with significant limitations)

These work end-to-end but cut corners.

- [ ] **`$bucketAuto` `granularity: "POWERSOF2"` answers doubles on the Python
  server** where mongod answers an INT for a whole power (`$pow(2, <int>)`).
  The Rust server was fixed 2026-09-30 (`tools/probes/bucket_auto_granularity.py`
  measures it); `secantus.aggregate._round_up_pow2` / `_round_down_pow2` still
  return `2.0 ** n`.
- **Scope decision — `top` classifies `collMod` as a write.** mongod bumps
  `total` for `collMod` but neither `readLock` nor `writeLock`; this server
  counts it under `writeLock`. The shape matters to mongotop and this
  asymmetry does not, so it is not reproduced.
- [ ] **Index collation `numericOrdering`** falls back to COLLSCAN at every level:
  `sortkey.encode_value_directed` cannot encode it byte-sortably (it would
  need a length-prefixed digit-run encoding). Strength 1/2/3 and `caseLevel`
  work uniformly across single- and compound-field indexes. See
  `docs/indexes.md` "Per-index collation".

## 3. Deferred work (skipped from a slice, ready to come back)

- [ ] **DEFERRED, both servers (re-confirmed 2026-10-07: the only arg-type
  message differences left) -- An unknown-index `hint` reports our message,
  not mongod's plan dump** (6 cases, on `find` / `count` / `aggregate` /
  `update` / `delete` / `findAndModify`). The CODE matches (2) on all six;
  mongod's text is a rendering of the whole canonical query
  (`error processing query: ns=<db>.<coll>Tree: $and\nSort: {}\nProj: {}\n
  planner returned error :: caused by :: hint provided does not correspond
  to an existing index`). Reproducing it means porting
  `MatchExpression::debugString` (`Tree: a $eq 1`), a different rendering
  from the `parsedQuery` normaliser (`secantus/explain.py`), and a half-right
  tree renderer would look authoritative while being wrong on anything
  nested — deliberately deferred, same call as the aggregation wrapper
  prefix in `tasks/remaining-work-plan.md` §1b. Probe:
  `tools/probes/arg_types_messages.py`.

- **BLOCKED ON A PREREQUISITE, not on effort: the Admin UI
      saved-connections / settings page.** The planned `/settings` page
      (saved Mongo URIs and a manual dark/light toggle) was skipped from
      slice 11 of the admin UI. Its whole value is switching targets, and the
      CLI takes a single `--uri` per launch, so a saved list would be
      bookmarks that cannot be opened. It waits on launcher hot-swap
      support; when that lands, it is likely a small SQLite-backed list
      reusing the existing `~/.secantus/admin.db` store.

### 3.1 Authentication

- **OUT OF SCOPE (recorded): LDAP / Kerberos / GSSAPI / MONGODB-AWS /
      MONGODB-OIDC.** Each is its own slice with its own external-system
      dependency, and none of them is something an ephemeral in-process test
      server can satisfy without that system present. SCRAM-SHA-256 and
      MONGODB-X509 -- the two the in-scope drivers exercise -- are shipped.
- **OUT OF SCOPE (recorded): internal cluster auth (keyfile / x509).**
      Only meaningful with replica sets / sharding, and `CLAUDE.md` scopes both
      out: there are no other members to authenticate to.

### 3.2 Change-stream limitations

Single-node change streams are implemented and conformant for typical pymongo `watch()` flows, but the following are deferred or intentionally diverge from real `mongod`:

- **DECIDED (single-node): read concern / write concern semantics.**
      Read concern is accepted and ignored. Write concern is NOT merely
      ignored: an unsatisfiable `w` (e.g. `w: 2` against the one-member set)
      is rejected as mongod rejects it, `{code: 100, codeName:
      "CannotSatisfyWriteConcern", errmsg: "Not enough data-bearing nodes"}`,
      on both servers. What remains accept-and-ignore is `j` / `wtimeout` /
      `w: "majority"` durability semantics, which a single node satisfies
      trivially, so there is nothing left that a one-member deployment could
      get wrong.
- **INHERENT (recorded): the C-driver gauge tests a single-node
      surrogate cannot pass.** `/change_stream/live/read_prefs`,
      `/Client/command_secondary`, `/Collection/aggregate/secondary` and the
      four `/Client/select_server/*` tests need a real SECONDARY (skipped,
      with the rationale, in `c_validation/include_paths.py::SKIP_TESTS`);
      `/Client/ipv6/single` hardcodes `mongodb://[::1]/` on the default port
      and never reads `MONGOC_TEST_URI`, so no server-side change reaches it
      (it still shows as the gauge's failures). Multi-node is out of scope.

- **OUT OF SCOPE (recorded): resume-token cross-server identity.**
      Tokens are opaque to drivers and round-trip fine, but the inner layout is
      `{s, t, n, k}` (BSON-encoded, hex-stringed) rather than mongod's
      keystring format, so a SecantusDB token cannot be presented to a real
      `mongod` or vice versa. Reproducing the keystring layout is a MIGRATION
      property, not a conformance one -- no driver reads inside the token.
      `tools/probes/change_streams.py` is at 0 of 41 with the current layout.

### 3.3 MongoDB CLI / tool conformance tests

Every connectable MongoDB tool (mongosh, mongodump / mongorestore / bsondump,
mongoimport / mongoexport, mongostat / mongotop, mongofiles) has a test in the
default suite that drives the real binary against an embedded
`SecantusDBServer`, and Compass's command surface is pinned headlessly in
`tests/test_compass_commands.py`. Scope decisions:

- **Windows is intentionally uncovered**: the mongosh tests skip on win32 by
  design (PowerShell `--eval` quoting), and a choco database-tools install is
  slow and flake-prone for the marginal remainder.
- **Driving the actual Compass GUI stays out of scope.**

### 3.4 Multi-document transaction limitations

Known divergences, all deliberate:

- **OUT OF SCOPE (recorded): no `recoveryToken` / mongos pinning, no
      prepared transactions, no `maxCommitTimeMS`, no
      `serverStatus.transactions` metrics, no `afterClusterTime`
      enforcement.** Each is sharded- or multi-node-only machinery, which
      `CLAUDE.md` scopes out. (`$clusterTime` / `operationTime` ARE gossiped on
      every reply so drivers can send `afterClusterTime`; the value is
      accepted and ignored.)
- **EXPECTED-RED, not a defect: the three secondary-readPreference
      transaction tests** (`TestUnifiedRunCommand::test_run_command_fails_with_*secondary*`,
      `TestUnifiedReadPref::test_secondary_readPreference`). They fail with a
      client-side 30s `ServerSelectionTimeoutError`: the single-node replica
      set has no secondary to select, and the asserted server error can only
      come from a server that received the command. They fail on mongod
      configured the same way.
- **DECIDED (single-node): readConcern levels inside transactions are
      accept-and-ignore.** Every in-transaction read runs against the
      transaction's pinned WT snapshot regardless of level (`snapshot` is
      exactly that; `local` / `majority` / `linearizable` are
      indistinguishable on one node).

### 3.6 Test-cycle performance (deferred levers)

- **Python-server raw insert throughput (WT pure-Python packing).**
  `test_bulk::test_numerous_inserts` (200k inserts) is ~14.9s and bottlenecked
  on the storage insert itself: profiling 100k inserts (6.5s) shows
  `wiredtiger/packing.py` + `intpacking.py` — the pure-Python `SSu`/`SSq`
  cursor-key packing, run several times per insert — dominating at ~2.9s
  cumulative. This is the known "use the Rust server for throughput" gap;
  shaving it means touching the per-op cursor-key path for every write, which
  is high-risk for a modest win on a server whose throughput story is the Rust
  server. Not worth it now.

## 4. Out of scope (intentional, with reasoning)

These are explicit non-goals. Don't add them without a reason.

- **Real replica sets / sharding** — depend on cluster topology and cross-node consistency. SecantusDB advertises `setName: "secantus"` to satisfy pymongo's change-stream topology check, but the topology is fictional — there are no other members, no elections, no cross-node oplog. Change streams are still in scope (single-node, oplog-backed); see `## 3. Deferred work / Change-stream limitations`.

    **Reading from a secondary is part of that non-goal** (decided 2026-09-27). There is no secondary to read from, so anything whose behaviour is defined by one is out of scope, not deferred. Three driver-gauge tests fail permanently on this and are accepted rather than tracked: `test_transactions_unified.py::TestUnifiedReadPref::test_secondary_readPreference` and the two `TestUnifiedRunCommand` read-preference cases (`test_run_command_fails_with_explicit_secondary_read_preference`, `test_run_command_fails_with_secondary_read_preference`). A permanently-red gauge test with a reason written down beats one quietly deselected — the pymongo pass rate carries the cost honestly.
- **`OP_COMPRESSED`** — compression negotiation. Clients can be told the server doesn't support compression; nothing to do.
- **Text search** (`$text`, `$meta: "textScore"`, text indexes) — would need a full-text index implementation.
- **Geo: exact mongod error-string matching** — chase work without a clear payoff unless a driver test pins exact wording.
- **`$where`** — runs JavaScript. We don't ship a JS runtime.
- **`$function`, `$accumulator`** (aggregation expressions) — same reason: both evaluate user-supplied JavaScript and need an embedded JS engine + sandbox + BSON↔JS shim layer. Would also require a `--javascriptEnabled` gate (mongod gates JS behind this; many prod deployments disable it). Adding the runtime would let us implement these three operators + `mapReduce` together for ~600–1000 LOC + a heavyweight binary dep (PythonMonkey / QuickJS / V8 — each with maintenance trade-offs). Most pipelines that reach for `$function` can be expressed in the existing aggregation expression library. **`lang: "python"` as a SecantusDB extension was considered and rejected (May 2026)**: would break the conformance contract (pipelines that work locally would explode in production with `Unrecognized lang value: 'python'`), and CPython's sandboxing story is worse than embedded JS engines (no per-context isolation primitives, no reliable cross-platform CPU-time interrupt, `RestrictedPython` is an AST rewriter not a true sandbox). If a real user need ever surfaces, the right escape hatch is a server-side trusted-plugin registry — `{$secantusFunction: {name: "<pre-registered>", args: [...]}}` — not user code at query time.
- **`mapReduce`** — same JS-runtime dependency as `$where` / `$function` / `$accumulator`, and deprecated by MongoDB (removed from the Stable API in 5.0). **WILL NOT BUILD** (decided 2026-09-27). `commands._map_reduce` recognises the canonical `emit(this.<field>, 1)` + `values.length` "count by field" pattern and translates it to an equivalent `$group` aggregation; non-canonical map / reduce bodies return `{results: [], ok: 1}`, and `out: "<coll>"` (non-inline) is rejected with FailedToParse 9 (`mapReduce on this server only supports {out: {inline: 1}}`). mongo-java-driver's `testMapReduceWithGenerics` writes to a NAMED output collection and so fails permanently; that failure is accepted rather than tracked.
- **Profiling: `planSummary` / `keysExamined` / `docsExamined` / `nreturned` in `system.profile` entries** — out of scope today (would need post-handler stats plumbing).

## 5. Known bugs and edge cases to watch

- [ ] **OPEN — the Go gauge's remaining failures, triaged (2026-09-29; 6
      leaves as of the 2026-10-09 run).** The gauge completes 659 of 659 and
      reads 598 / 12 / 49 = 98.0% (`docs/validation-report-go.md`, measured
      2026-10-09). The 12 fail events are 6 distinct leaves:

      | leaf | needs |
      | --- | --- |
      | `TestConnectionPoolBackpressure` | an ingress rate limiter AND `$where` |
      | `TestSDAMErrorHandling/.../pool_not_cleared_on_timeout_network_error` | `$where` |
      | `TestSDAMErrorHandling/.../pool_not_cleared_on_context_cancellation` | `$where` |
      | `TestRetryableReadsProse/.../overload_errors_retried_on_a_different_replicaset_server` | a second replica-set member (non-goal) |
      | `TestSessionsMongocryptdProse/18.` | a `mongocryptd` binary on PATH |
      | `TestSessionsMongocryptdProse/19.` | a `mongocryptd` binary on PATH |

      * **`TestConnectionPoolBackpressure`** sets four
        `ingressConnectionEstablishment*` parameters — a real mongod
        connection rate limiter we have no equivalent of — then fires 100
        concurrent finds with `$where: sleep(2000)` and asserts ≥10 checkout
        FAILURES. It needs (a) that rate limiter and (b) server-side
        JavaScript. `setParameter` refuses those parameters deliberately
        rather than accepting names we do not honour.
      * **The two `TestSDAMErrorHandling` cases are not pool-clearing bugs.**
        Both issue a `find` with
        `{$where: "function() { sleep(1000); return false; }"}` and abandon it
        client-side after 100ms; the pool assertion is never reached. A real
        `mongod --noscripting` fails them with byte-identical messages. They
        stay red and un-skipped deliberately: excluding them would hide the
        `$where` gap, and they will pass by themselves if a script engine
        ever lands. The sibling `pool_cleared_on_non-timeout_network_error`
        passes.
      * **The two mongocryptd cases** need a server that reports NO
        `logicalSessionTimeoutMinutes` (the mongocryptd shape). Likely out of
        scope with CSFLE, but the mechanism — how we advertise session
        support — is ours, so confirm rather than assume.

      Reproduce any of them directly rather than through the gauge:
      `cd vendor/mongo-go-driver && MONGODB_URI=mongodb://127.0.0.1:<port> go test
      ./internal/integration/ -run '<TestName>' -timeout 60s -count=1 -v`
      against a daemon WITHOUT `--standalone` (several are replica-set-gated).

- [ ] **OPEN — `$where` / server-side JavaScript is not implemented; the
      refusal is faithful (2026-09-29).** SecantusDB embeds no script engine
      and there is no plan to. Both servers answer as `mongod --noscripting`
      8.2.11 does: 6108304 / `Location6108304` / "no globalScriptEngine in
      $where parsing" in a query, and code 2 `$where is not allowed in this
      context` in an aggregation `$match` (checked at aggregate parse time,
      ahead of the leading-`$match` lift). Pinned by
      `tests/test_mongod_differential.py::test_where_refusal_matches_a_scriptless_mongod`
      / `::test_where_in_aggregate_match_is_a_context_error` and
      `tests/test_crud.py` for a box with no mongod.

      What stays open: the Go tests above that need the JavaScript to actually
      run. Implementing `$where` means embedding a JS engine — a large
      dependency for one operator, and worth a deliberate decision rather than
      drifting into.

- **Driver unified-spec coverage audit (2026-09-25, Python server) — what is
  left.**
  - **Stale Go gauge skips.** `go_validation/include_packages.py` still skips
    `TestConvenientTransactions` (13/14 passed when measured; the skip comment
    says abort does not roll back, and it does), `TestBackpressureProse` (3/3
    passed; the comment says failpoints are not implemented) and
    `TestCausalConsistency_Supported` (29/29 passed). Re-include them.
  - `test_sessions_unified` snapshot tests failed 5 (event-count mismatches,
    not yet diagnosed). This runner is outside the gauge's `INCLUDE`.

### 2026-09-01: two PLANNER differences, measured and dispositioned

Both return CORRECT results and differ only in how much they scan, and the
Python server is explicitly not a throughput target, so they are recorded
rather than scheduled.

- **A multi-field filter falls back to COLLSCAN when only SINGLE-field
      indexes exist.** ``find({a: 1, b: "1"})`` with an index on ``a`` and one
      on ``b`` scans the collection; mongod picks one, scans it, and re-checks
      the other field in the FETCH. Our pickers require a COMPOUND index for a
      multi-field filter (the one exception being the partial-residual path).
- **A sort cannot use an index while the filter is non-empty and
      unindexed.** ``find({nope: 1}).sort({a: -1})`` with an index on ``a``
      scans and sorts in Python; mongod walks ``a_1`` BACKWARD and filters in
      the FETCH. Our sort-acceleration branches gate on an empty filter.

- [ ] **Locale-specific collation ORDERING is not reproduced; it needs
  ICU.** `collation.sort_levels` reproduces the ordering RULES (three levels,
  `caseFirst`, `numericOrdering`, `backwards`) without a locale table, so
  Swedish `ä` after `z` and Danish `å` last differ from mongod (the two
  `KNOWN_LOCALE_GAPS` in `tools/probes/collation_order.py`, which is the
  standing sweep), and `alternate` / `maxVariable` are unimplemented and
  unprobed (`collation.py` lists them out of scope). The Rust server uses
  `icu_collator` and does not have this gap.
- **DECIDED: an invalid collation `locale` stays accepted on the Python
  server**, because rejecting it correctly needs ICU's locale list
  (`commands.py:4559`). Enumerating ICU's locales without ICU means guessing,
  and wrongly rejecting a locale mongod ACCEPTS is worse than accepting one
  it rejects. Reopen only alongside an ICU dependency.
- **AUTHORISED DIVERGENCE (Joe): Decimal128 transcendentals keep the
  correctly rounded last digit.** `$sin`, `$tan`, `$ln`, `$log10`, `$acosh`,
  `$cosh` and `$sinh` of a Decimal128 operand differ from mongod in the 34th
  digit (7 values in the 2026-10-07 `agg_expressions.py` run, at 2.5). Ours
  is the correctly rounded value; mongod's is 1-2 ulp low (Intel's decimal
  library). Do not "fix" it to match.
- **ACCEPTED, not fixable: `$graphLookup`'s result array order.** The SET
  matches mongod exactly; only the order inside the `as` array differs, and
  mongod's own order is not stable: the same graph queried twice in one
  mongod 8.2.11 process came back `[5, 4, 2, 3]` and `[3, 2, 4, 5]`
  (measured 2026-09-01). We return breadth-first, depth ascending, which is
  at least deterministic.
- **DECIDED, not a defect to fix: `getMore` on a namespace that isn't the
  cursor's answers 43, not mongod's 13.** `getMore` is in
  `_NO_PRIVILEGE_COMMANDS`, so a distinguishable error would confirm or deny
  which cursor ids exist on other connections. Reverting it trades a
  hardening property for message fidelity nobody's driver reads.
- **DECIDED, not scheduled: a `find` on a MISSING collection orders the
  cursor reply's keys differently.** mongod answers `{id, ns, firstBatch}`
  there and `{firstBatch, id, ns}` for every other find. Ours is consistent,
  so it matches on the common path. A one-version observation about BSON key
  order is not enough to act on: two version splits on key order were found
  in the same campaign.
- [ ] **UNVERIFIED: the order of an upserted document's query-seeded
  fields.** mongod 6.0.16 sorts the fields seeded from the query's equalities
  (`{b: 1, a: 2}` upserts `{_id, a, b, ...}`); a newer mongod (the Windows CI
  runner's, 2026-08-29) keeps the query's order (`{_id, b, a, ...}`). We
  shipped 6.0's form, and the differential gate deliberately does not assert
  that group's order (`_upsert_key_shape` in
  `tests/test_mongod_differential.py`), so nothing says which form we send now
  or whether it matches 8.2.11. Reproduce: `findAndModify` with
  `query: {b: 1, a: 2}, update: {$set: {z: 1}}, upsert: true, new: true`
  against mongod 8.2.11 and this server, compare key order.
- **DECIDED, not scheduled: a hint that names no index answers a shorter
  message than mongod's.** Code (2) and `codeName` match and the causal
  sentence is the same; mongod's multi-line planner dump prefix
  (`error processing query: ns=<db>.<coll>Tree: _id $eq 1\nSort: {}\nProj:
  {}\nCollation: ...`) is absent, on `find`, `count` and `findAndModify`.
  Reproducing it means porting `MatchExpression::debugString`, a different
  rendering from the `parsedQuery` normaliser, and a half-right renderer
  would look authoritative while wrong on anything nested. Message-only.
- **DECIDED, not scheduled: a `$`-prefixed unknown command field is accepted
  where mongod rejects it** (`Location40415`), the same carve-out `create`
  makes. `$`-keys are the wire envelope (`$db`, `$clusterTime`,
  `$readPreference`, `$audit`, ...) and an allowlist that missed one would
  break a driver over a message nobody reads. Revisit only with a
  driver-verified envelope list.

- [ ] **Retryable writes: no per-statement-id record.** mongod records
  each statement of a retryable batch, so a partially-applied `insert` batch
  retries only its missing documents. The Python server records only
  fully-successful whole-command outcomes (`transactions.py`
  `record_retryable`), so a partially-failed batch re-runs entirely. The
  Rust server records statement ids (`crates/secantus-commands/src/transactions.rs`).
  Deliberate limits that stay: a failed write is not recorded (its retry
  must re-execute), and a different command under the same
  `(lsid, txnNumber)` executes rather than replays.
- [ ] **UNVERIFIED: memory growth in the admin-UI test fixtures.** Measured
  2026-07-21/26 at +206 MB per 113 tests of `tests/test_admin_skeleton.py`,
  halved in #597 (1.82 -> 0.80 MB/test) and still unbounded; fds were flat.
  Refuted as the cause of the ws-changes worker crash, but recorded as worth
  fixing on its own. Reproduce by sampling RSS per test across that file.

- **DECIDED, not a bug: `$group` bucket order.** `$group` output order is
  unspecified in MongoDB; mongod's is an artifact of its internal hash table
  (on `z, a, m, a, z` it returned `['m', 'z', 'a']`). We emit first-seen
  order, which is deterministic. Do not "fix" ours to chase mongod's.
- **TRIAGED, not server divergences: the Java gauge's 5 remaining
  failures** (`ClientMetadataTest` metadata append, `VersionedApiTest` find
  and getMore API version, `CommandMonitoringTest` deleteMany and find with a
  getMore, `ConnectionPoolLoggingTest`). Driving the same operations via
  pymongo produced the spec-expected wire replies, and the pymongo gauge
  passes the identical upstream spec files. Recorded in
  `validation_summary/expected_failures.py` (`JAVA`).
- **NOT SERVER-FIXABLE, mitigated at the runner: the Go gauge's
  `TestIndexView/drop_one` + `drop_all` server-selection timeouts.**
  Cross-gauge CPU/socket contention under `validate-all`; a clean 12-thread
  DDL+CRUD stress ruled out a server fault. `go_validation/runner.py` sets
  `serverSelectionTimeoutMS=60000`.
- **ACCEPTED, proven not server-fixable: the Go gauge's
  `TestChangeStream_ReplicaSet/try_next/one_getMore_sent` flake.** A property
  of the shared-daemon mongo-go-driver mtest harness: `dbName` is one shared
  `TestDB` and long collection names are truncated to a colliding suffix
  (`mongotest.go` `sanitizeCollectionName`), so a concurrent test's write can
  legitimately wake the stream. A collection-scoped stream saw 0 foreign
  events across 400 polls under 2074 concurrent writes. Fixing it means
  editing the vendored tests, which is forbidden.
- **STRUCTURALLY UNREACHABLE, accepted: the Ruby gauge's
  `Index::View#create_one ... with session` test.** The driver's
  `Options::Mapper.transform` strips `invalid: true` before the command is
  built, so the server-side guard (`commands.py:_INDEX_SPEC_KNOWN_OPTIONS`,
  `Location40415`) cannot fire. Real mongod has the same problem.
- **EXPECTED-RED under a single-node topology: the Ruby gauge's `w: 2`
  create test** (`spec/mongo/collection_ddl_spec.rb:211`). Rejecting an
  unsatisfiable write concern (`commands.py:_unsatisfiable_wc_error`,
  `CannotSatisfyWriteConcern` 100) is the correct emulation of a one-member
  set and gains seven other tests; this one would pass only if the
  advertised roster grew.

- [ ] **C gauge: `/BulkOperation/OP_MSG/max_msg_size` ("2 == 1") fails only
  inside the full ~800-test run, on both servers.** It passes standalone and
  beside its `/BulkOperation/OP_MSG/*` siblings; all three advertised limits
  are byte-identical to mongod, and the driver decides the split itself. That
  profile is test interference or accumulated state, not a wire-limit
  divergence. Pinning it down means bisecting a ~10-minute full-gauge run.

## 6. Admin UI review punch list

Severity tiers: P0 broken/silently-wrong, P1 inconsistency or significant
usability gap, P2 polish. File refs are under `src/secantus/admin/` unless
noted.

### P1 — significant inconsistency / usability

- **DEFERRED BY DESIGN: the `/backup/dump` and `/backup/restore` long-task
  UX.** Both call `backup_lib.run_mongodump` / `run_mongorestore`
  synchronously; a spinner and disabled button cover a normal-sized dump. A
  real background-task wrapper with poll status (so the user can navigate
  away) is only worth it for a multi-minute dump, which nobody has hit. Defer
  until someone does.

### P2 — polish

- **EXPLICIT SCOPING DECISION, not a gap: the SQL / PostgreSQL-wire server
  has no admin UI.** `client.check_supported_uri` rejects a `postgresql://`
  target with a message saying so. Every admin page is pymongo-driven; a SQL
  admin surface would need its own console.

## 7. Python → Rust rewrite (in progress)

### 7.07 Open, unexplained, from the 0.7 gauge and benchmark refresh -- 2026-10-09

- [ ] **PYTHON MongoDB server: `profile`'s `slowms` and `sampleRate` are kept
      per database.** `mongod` 8.2.11 keeps them server-wide (set through
      database `a`, `profile: -1` on `b` reads them back). The Rust server
      was moved 2026-10-09; the Python server was not touched. It also
      writes no `Slow query` log line.

### 7.083 User and role commands -- 2026-10-14

- [ ] **PYTHON server parity.** The Rust server's user record's field order
      changed (`userId` added, `customData` and
      `authenticationRestrictions` stored, `credentials: {external: true}`
      for a `$external` user). The Python server reading such a store was
      not measured.

### 7.081 Geo queries -- 2026-10-13

- [ ] **PYTHON MongoDB server:** not measured by `tools/probes/geo_queries.py`.
      It tests `$geometry` in the plane (Shapely), where mongod tests it on
      the sphere (an edge is the great-circle arc between its ends), and a
      flat shape (`$box`, `$polygon`, `$center`) holds points only on mongod,
      so the Rust server's 726 original differences are expected there.
      `find(...).hint(<a 2d index>)` is an internal error
      (`int('2d')` in `_candidates_from_hint`), found 2026-10-14.

### 7.080 The order `find` returns documents in -- 2026-10-13

- [ ] **PYTHON MongoDB server:** not measured by `tools/probes/find_order.py`.
      Two wrong-DOCUMENT defects fixed in the Rust router are not checked
      here: `{a: {$gt: 1, $lt: 9}}` over `{a: 1}` must return `{a: [1, 9]}`
      (two range bounds over a multikey index drop it unless one bound is
      kept); and `{"p.q": null}` over an index on `p.q` must return a
      document whose array `p` has an element without `q`. The index
      entries hold nothing for that element, where mongod indexes it as
      null, so the same hole is there on the Python server.

### 7.079 `$out` and `$merge` -- 2026-10-13

- [ ] **PYTHON MongoDB server: `$merge` with `whenMatched: "merge"` merges
      sub-documents field by field** (`aggregate._deep_merge_docs`). mongod
      sets each top-level field whole: `{o: {p: 1}}` over `{o: {q: 2}}`
      stores `{o: {p: 1}}`, not `{o: {q: 2, p: 1}}`. Not measured by
      `tools/probes/out_and_merge.py` against this server.

### 7.078 `insert`, `delete`, `count` and `distinct` -- 2026-10-12

- [ ] **PYTHON MongoDB server: two `_id` documents equal by value are both
      stored.** After `{_id: {a: 1}}`, `{_id: {a: 1.0}}` is accepted; mongod
      refuses it. The Rust server takes a claim by value in `UNIQ_TABLE`; the
      Python server takes none, so as the server holding a store it accepts
      a second document equal by value to a stored one. It removes the
      record that the claim walk is done when it opens a store, so the Rust
      server claims again after it, keeping both and logging them.

### 7.077 `update` and `findAndModify` -- 2026-10-12

- [ ] **PYTHON MongoDB server:** not measured by
      `tools/probes/update_commands.py`. It decodes with `bson.decode`, which
      does not read extended JSON shapes, so the serde-decode defect is not
      expected there. The engine parity suites now differ from the Rust
      engine where mongod sides with Rust.

### 7.076 DDL and admin commands -- 2026-10-12

- [ ] **PYTHON MongoDB server:** not measured by
      `tools/probes/ddl_admin_commands.py`. Whether it accepts invalid
      database names (`a.b`, `a b`, the empty name, `a/b`) or a
      `dropDatabase: 0` was not checked.

### 7.075 `explain`, command by command -- 2026-10-12

- [ ] **PYTHON MongoDB server:** not measured by
      `tools/probes/explain_commands.py`. Its `explain` has the older,
      smaller shape (no `indexBounds`, no `EXPRESS_IXSCAN`, no `count` /
      `distinct` / write stages), it echoes `{$exists: false}` as sent where
      mongod parses it as `{$not: {$exists: true}}`, and two defects fixed on
      the Rust server were not checked on it: an unknown query operator
      (`{a: {$nope: 1}}`) accepted by `count`, `distinct`, `aggregate`,
      `update`, `delete` and `findAndModify` over an EMPTY or MISSING
      collection, and `count` / `delete` by `_id` on a collection that was
      never written answering an internal storage error.

### 7.074 Change streams, command by command -- 2026-10-11

- [ ] **PYTHON MongoDB server: not measured on this probe**, which takes a
      server URI and was run against the Rust server only. Known from the
      code: it stamps `splitEvent: {fragment: 1, of: 1}` on every event
      under `$changeStreamSplitLargeEvent` (mongod does not), and its
      `$changeStream` spec checks, resume rules and cursor field order
      were not compared.

### 7.073 The cursor commands -- 2026-10-11

- [ ] **PYTHON MongoDB server: not measured on this probe** (it takes a
      server URI and was run against the Rust server only).

### 7.072 Sessions, transactions and retryable writes -- 2026-10-11

- [ ] **PYTHON MongoDB server: not measured on this probe**, which needs a
      server URI and was run against the Rust server only. Its registry
      (`src/secantus/transactions.py`) has the rules the Rust one had before:
      50911 for a restart, whole-command replay, `endSessions` aborting the
      session's transaction.

### 7.071 Time-series collections -- 2026-10-11

- [ ] **PYTHON MongoDB server: 140 of 187 time-series results differ from
      mongod** (`tools/probes/time_series.py`). Not changed, and not read
      through. Whether its `$out` keeps the target's validator and indexes
      was not measured.

### 7.070 Collation -- 2026-10-10

- [ ] **PYTHON MongoDB server: 434 of 764 collation results differ from
      mongod** (`tools/probes/collation.py`). Not changed, and not read
      through. It also has the tie rule the wrong way round in `$min` /
      `$max` (`$group` over int 1, double 1, long 1 keeps the first; mongod
      keeps the last), lists equal values in `$maxN` in input order, leaves
      `q` out where `$fill` `locf` has nothing to carry, sorts an array as a
      whole in `$top` and `$setWindowFields`, and accepts
      `{$group: {_id: {n: 1}}}`. The engine parity suites leave these cases
      out, with the reason beside each.
- [ ] **PYTHON MongoDB server: collated `_id` uniqueness is not enforced.**
      Under a collection collation, a second spelling of a stored `_id`
      (equal under the collation) is accepted; the Rust server, which walks
      the documents again after it, keeps both and logs them.

### 7.068 The `bulkWrite` command and TTL indexes -- 2026-10-10

- [ ] **PYTHON MongoDB server: whether TTL expiries reach the oplog was not
      measured.** The probe's Python server runs without a replica set name,
      so the change stream there is refused.
- [ ] **PYTHON MongoDB server: 46 of 50 TTL results and 63 of 141 `bulkWrite`
      results differ from `mongod`** (`tools/probes/ttl_indexes.py`,
      `tools/probes/bulk_write_command.py`). Not changed and not read
      through beyond the entry above. Whether it stores an array or regex
      `_id` (the Rust server did, on `insert` and on upsert) was not probed.

### 7.067 Index management -- 2026-10-10

- [ ] **PYTHON MongoDB server: `$geoWithin` leaves out points on the
      boundary** of a `$box`, `$polygon` or `$geometry` polygon (Shapely
      `within`); `mongod` 8.2.11 includes them. The parity corpus has no
      boundary point, so the suite did not notice. Hidden indexes on the
      Python server were not probed.
- [ ] **PYTHON MongoDB server: 69 of 87 index-management results differ from
      `mongod`** (`tools/probes/index_admin.py`). Not changed, and not read
      through: check whether `dropIndexes` with the key `{_id: 1}` drops the
      `_id` index there too.

### 7.066 Views and `$count` -- 2026-10-09

- [ ] **PYTHON MongoDB server: 52 of 70 view results differ from `mongod`**
      (`tools/probes/views.py`). Not changed.
- [ ] **PYTHON MongoDB server: `$count` over no documents emits `{n: 0}`.**
      `mongod` 8.2.11 emits nothing (`[{$match: {_id: -1}}, {$count: "n"}]`
      is `[]`, and a `$lookup` / `$unionWith` / `$facet` sub-pipeline that
      counts no match adds no document). The Rust engine was moved
      2026-10-09. `tests/test_rust_aggregate_parity.py::test_pipeline_fuzz`
      now steps over any disagreement in a pipeline that holds a `$count`;
      remove that exclusion when the Python engine is fixed.

### 7.064 Document validation -- 2026-10-09

- [ ] **PYTHON MongoDB server: 16 of 68 document-validation results differ
      from `mongod` 8.2.11** (`tools/probes/validators.py`). Not changed, and
      not read through one by one: it includes an ordered `insert` dropping
      the documents before a validation failure, and `$merge` into a
      validated collection succeeding.

### 7.065 Capped collections -- 2026-10-09

- [ ] **PYTHON MongoDB server: 51 of 97 capped-collection results differ from
      `mongod` 8.2.11** (`tools/probes/capped_collections.py`, 2026-10-09).
      The fixes of that day were made on the Rust server only: a batch and an
      upsert held to the cap, `max <= 0` as no limit, the `size` / `max`
      ranges and integer storage, `collMod` `cappedSize` / `cappedMax`,
      `collStats` `max` / `maxSize`. The Python server does refuse a write
      inside a transaction, with its own wording and reply shape.

### 7.06 Release tooling gaps found cutting 0.6.0b18 -- 2026-10-07

- [ ] **The PyPI project is at its 10 GB quota.** It held 10.72 GB when
  `0.6.0b18` was refused. The wheel is now Python-only, and Joe is deleting
  the `0.5.x` releases (2.12 GB). Watch the total each release; the JSON API
  gives the per-file sizes.

### 7.05 MongoDB-server backlog close-out -- 2026-10-07

**Left open on purpose, each with its reason:**

- **Unknown-index `hint` plan dump** (both servers, 6 message-only cases): needs
  mongod's planner `debugString` renderer for text on an already-correct code.
- **`$where` / server-side JavaScript**: a scope decision.
- **A `w` tag set with more than one key**: accepted divergence (mongod's
  hash-map order).
- **Decimal128 transcendental last digit**: authorised (correctly rounded).
- **CI Python matrix (3.10 only on push)**: a cost decision.
- **Out of scope**: macOS x86_64 wheels.

### 7.00 Rust MongoDB server batch — 2026-09-30, measured against mongod 8.2.11

- **Scope decision: the six message-only bad-hint divergences in
  `tools/probes/arg_types_messages.py` are DELIBERATE, and BOTH servers have
  them.** mongod answers a bad hint with a multi-line planner diagnostic
  (`error processing query: ns=<db>.c / Tree: $and / Sort: {} / Proj: {} /
  planner returned error :: caused by :: hint provided does not correspond to an
  existing index`) where both servers name the hint. The CODE matches.
  `commands.py` records the decision: "assert the rejection rather than the
  text". Reproducing it would mean rendering mongod's `MatchExpression` DEBUG
  string, a third rendering vocabulary distinct from `parsedQuery`'s JSON and
  from the value form. Recorded so it is not re-investigated.

- **Scope decision: mongod's per-position timelib DIAGNOSTIC for a `$toDate` of
  an unparseable string is not reproduced, on either server.** Measured across
  25 strings, there is no small rule -- `'a1'` errors at position 1 but `'1a'`
  at position 0; `'123'` emits one error PER position; `'junk here'` emits two
  including "Double timezone specification"; and `'2020-01-01X'` PARSES because
  `X` is the military timezone UTC+11. It needs timelib's lexer, its timezone
  abbreviation tables and its per-position error accumulation; inventing a
  position would look authoritative and be wrong.

- **Scope decision: a mongod BUG, deliberately not reproduced.**
  `{$round: [-2147483648, -1]}` answers **positive** `2147483646` on mongod --
  an exact int32 wrap of the correct `-2147483650`. mongod handles the same
  overflow three ways (widened for a positive int32, wrapped for a negative
  one, detected at int64 width with `51080`); `$trunc` on the same input is
  correct. Both our servers answer `-2147483650`.

- **Scope decision: the upsert seeded field ORDER is mongod's HASH-TABLE order
  and is deliberately NOT reproduced.** Twelve key sets measured: `{a, b}` ->
  `b, a`; `{aa, ab}` -> `aa, ab`; `{z, a}` -> `a, z`; `{one, two, three}` ->
  `three, one, two`; `{b, a, d, c}` -> `c, a, b, d`. It ignores the query's own
  order and is neither sorted nor reversed, so it is an implementation detail,
  not a contract. Both servers keep the sorted order and the tests compare
  field/value pairs. Reproducing it would mean reimplementing mongod's string
  hash and table growth.

### 7.1 Rust server performance and security review (2026-06-16)

- [ ] **Go gauge: CI runs ~1/5 of the local set** — CI weekly artifacts have
  always reported ~450 tests (e.g. 401/453 on 2026-06-08, 447/900 on
  2026-06-12) while local `invoke validate-go` runs ~4700 (the numbers the
  pre-2026-06-12 committed report carried). Suspects: the 30-minute
  `go test -timeout`, cold caches on runners (GitHub cache service 400s), or
  package discovery under `./internal/integration/...` differing on CI.
  Diagnose before trusting week-over-week go comparisons.
- [ ] **Regex patterns over 1000 characters are capped on both servers** (from
  the removed "Query `$regex`" record). `src/secantus/query.py`
  `_MAX_REGEX_PATTERN_LEN = 1000` is a ReDoS mitigation (Python's `re` has no
  match timeout); a longer pattern is refused. mongod's own limit has not been
  measured against it. Reproduce: `find({x: {$regex: "a" * 1001}})` on each
  server and on mongod 8.2.11.

### 7.5 Remaining Rust-server feature gaps (defer audit, 2026-06-26)

- [ ] **`$toHashedIndexKey` and the `$bitAnd` / `$bitOr` / `$bitXor` `$group`
  ACCUMULATORS are absent on both servers.** (The bitwise *expression* forms
  exist.) Not implementable blind: the mongod builds probed (7.0.12, and the
  builds available when this was last checked) reject the accumulator form with
  `15952 unknown group operator`, so there is nothing to probe against.
  Implement only once a mongod that accepts them is installed — check
  8.2.11 first.
- [ ] **A missing sort key may tie with an explicit `null` where mongod orders
  them** (both servers, recorded 2026-07-17). The record said SecantusDB's sort
  treats a missing field as equal to explicit `null` while mongod sorts missing
  just above null, so `$sort` and `$top` / `$bottom` / `$topN` / `$bottomN`
  can order equal-keyed docs differently when explicit-null and missing-field
  docs tie. Re-probe against mongod 8.2.11 before working it.
- **Out of scope — `$group` accumulator error RE-CODING is not reproduced.**
  mongod substitutes its own code for an error raised inside a `$group`
  accumulator (`4848401` for a division by zero, `7157706` for `$ln`), where
  `$match` / `$sort` keep the original 2 / 28766. There is no rule derivable
  short of probing every accumulator. The other 11 of 12 aggregation
  runtime-error wrapper shapes match mongod 8.2.11.
- [ ] **`$stdDevPop` last-ULP vs mongod** (both servers): both agree with each
  other but differ from mongod in the final ULP (e.g. `2.357022603955158` vs
  mongod's `2.3570226039551585`); mongod uses a different summation order.
  Precision-only; likely a permanent minor divergence. Re-probe on 8.2.11.
- **Out of scope — `$meta` inside an AGGREGATION stage.** 21 shapes probed
  on 8.2.11: mongod's behaviour there is its metadata dependency analysis
  (`DepsTracker`) leaking through (`$project` returns `recordId` but
  `$addFields` omits it; `recordId` survives `$match` but not `$limit` /
  `$skip` / `$group`; `sortKey` without `$sort` errors in `$project` and
  returns `[]` in `$addFields`). Our aggregate `$meta` answers
  `168 Unrecognized expression '$meta'`; revisit only with the dependency
  analysis, not operator by operator. (Find-projection `$meta` is done.)
- **Deliberate divergence — `$meta: "recordId"` values are store-wide.** Our
  RecordId is a store-wide insertion counter and mongod's restarts per
  collection, so a second collection in the same store starts at 3 where mongod
  starts at 1. Matching it means a per-collection counter, and that counter IS
  the document table's key — a storage-format change shared byte-for-byte by
  both servers. Uniqueness and insertion-order ascent hold
  (`tests/test_meta_projection.py`).

## Concurrent-writer contention (found by bench.concurrency, 2026-07-16)

- **Residual**: under sustained multi-writer saturation WiredTiger still
  occasionally marks concurrent batch transactions rollback-only
  (observed as commit-time EINVAL before the mapping fix; cause not fully
  attributed — cache/eviction pressure suspected, key-level conflicts
  ruled out for per-collection writers). Retries absorb it today. If it
  resurfaces as a throughput cliff, instrument `get_rollback_reason()`
  in the retry wrapper and check WT eviction statistics. Multi-writer
  THROUGHPUT scaling itself remains capped by the WT-binding/GIL ceiling
  documented in docs/concurrency.md; lifting it is the
  tasks/wt-concurrency-plan.md end-game.

## pymongo gauges: an intermittent xdist worker crash (2026-07-20)

**An intermittent xdist worker crash.** With `-p no:randomly` in
place the sync gauge still crashed once on
`test_bulk.py::TestBulk::test_bulk_max_message_size` (6 failed / 1225 passed),
then ran clean on the very next invocation (5 failed / 1226 passed) — so it is
intermittent even at fixed ordering.

Evidence pointing at memory/resource pressure rather than a logic bug:
- Every test that has crashed a worker is one of the heavy ones —
  `test_numerous_inserts`, `test_bulk_max_message_size`,
  `test_overflow_int_w_custom_decoder`.
- Each passes in isolation in ~2s (`invoke validate-one`), so nothing is wrong
  with the operation itself.
- The gauge runs the whole suite in ONE xdist worker (`-n1`); sampled worker RSS
  peaks around **1.3 GB** by late in the run.
- The first observation came from a `validate-all --jobs 4` run, i.e. under
  maximum machine contention.

Next step for whoever picks this up: confirm or refute OOM. The runner still
does not surface worker stderr, so start there — capture the worker's exit
signal (a SIGKILL from the OS points at memory; a SIGSEGV points at the
extension). If it is memory, the fix is likely periodic worker recycling
(`--max-worker-restart` already tolerates restarts; xdist has no built-in
max-tests-per-worker, so this may mean splitting the heavy bulk files into their
own gauge invocation). Leaked `Storage` fixtures were suspected and ruled out.
