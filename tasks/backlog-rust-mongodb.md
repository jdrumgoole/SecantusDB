# Backlog: the Rust MongoDB server

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

**Every item carries `Backlog-ID:` and `Verify:` lines.** `Verify:` names a
gap test under `tests/backlog/` that asserts the real product's answer and so
fails while the item is open, or says why none can (`none -- ...`).
`python tools/backlog.py verify` runs them all; the `backlog` skill
(`.claude/skills/backlog/SKILL.md`) is the protocol for filing, working and
closing an item.

**Reproduce an item before working it** -- a probe, the failing test, the
query -- and size it from that, not from the text. Entries here have
described work as missing that was done, and understated bugs by naming one
symptom of several. Correct a superseded entry rather than adding a newer
one beside it.

## Open from the 2026-08-26 triage

- [ ] **Rust MongoDB `explain`: the index shown for each `$or` branch** is
  mongod's cost-model choice and is not reproduced when a branch has several
  usable indexes (noted 2026-10-02, batch 45). Reproduce: two indexes that can
  both serve one `$or` branch, then compare `explain` against mongod 8.2.11.
  Backlog-ID: RM-001
  Verify: tests/backlog/test_rm_001_028.py::test_or_branch_index_choice_matches_mongod
- **Scope decision — `$where` and `$function`** need a JavaScript engine.
  Neither server has one, and none is planned (WONTFIX).

## 2. Stopgaps (functional but with significant limitations)

These work end-to-end but cut corners.

- [ ] **`$bucketAuto` `granularity` value validation surfaces as BadValue on the
  Rust server.** `secantus-core` `group::granularity_coerce` returns
  `Fallback::Defer` for a non-numeric, NaN or negative groupBy value, which on
  the Rust server is BadValue (2); mongod answers 40258 / 40259 / 40260 (the
  Python server reproduces them). One instance of the standing class: a
  construct the Rust engine refuses surfaces as generic BadValue rather than
  mongod's typed code (`update::arith_type_error` is the worked template).
  Backlog-ID: RM-002
  Verify: tests/backlog/test_rm_001_028.py::test_bucket_auto_granularity_value_errors
- **Scope decision — `top` classifies `collMod` as a write.** mongod bumps
  `total` for `collMod` but neither `readLock` nor `writeLock`; this server
  counts it under `writeLock`. The shape matters to mongotop and this
  asymmetry does not, so it is not reproduced.
- [ ] **Index collation `numericOrdering`** falls back to COLLSCAN: index keys
  cannot encode it (it would need a length-prefixed digit-run encoding to stay
  byte-sortable; `secantus-core/src/sortkey.rs` says so). Strength 1/2/3 and
  `caseLevel` work across single-field and compound indexes. See
  `docs/indexes.md` "Per-index collation".
  Backlog-ID: RM-003
  Verify: none -- storage-internal COLLSCAN (performance); results, order and explain executionStats under numericOrdering already match mongod

## 3. Deferred work (skipped from a slice, ready to come back)

- [ ] **OPEN — Rust MongoDB server on Linux under `--sync-on-commit`: not
      measured.** It runs the same patched WiredTiger as the Rust PostgreSQL
      server (`cmake/patch_wt_fsync_group.py`, one `fsync` credited with
      every commit already written under `method=fsync`), which was measured
      and hard-reboot-tested only through `secantusd-pg` on a DigitalOcean
      `c-16`. The PostgreSQL server's zero-filled log
      (`server::zero_filled_log`) is set by that server only. See the Rust
      PostgreSQL backlog's §3 Linux durable-writes entry for method.
      Backlog-ID: RM-004
      Verify: none -- Linux fsync durability measurement needing a hard reboot

- [ ] **Rust MongoDB server is 0.72x-0.76x mongod's throughput on
  incompressible data; p99.9 1.3x-1.8x (2026-10-08)**: the three-droplet
  comparison on DigitalOcean (`c-4` server, 2 x `c-2` clients, 16 workers
  each), **8 KiB incompressible documents**, 70/20/10 insert/find/update mix,
  4G WT cache both, three interleaved passes, against mongod 8.0.32: three
  runs gave 0.72x / 0.72x / 0.76x throughput, p50 1.23x-1.32x, p99
  1.39x-1.49x, p99.9 1.26x-1.77x. Both engines server-CPU-bound with clients
  idle. Published in `docs/benchmark.md` (the source of truth for the
  numbers); reproduce with `invoke do-bench --repeat 3 --payload random`.
  The 72x p99.9 of 2026-08-21 was zlib (`deflate` was 65% of server CPU)
  and is gone since the default block compressor became lz4. What remains is
  a throughput gap no longer dominated by any single cause; the p99.9 ratio
  swings droplet to droplet, so read only the throughput ratio as stable.
  Backlog-ID: RM-005
  Verify: none -- throughput benchmark on DigitalOcean droplets


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

Full analysis + the measured scaling curve live in
`tasks/test-performance-plan.md`. **Won't-do:** I4 (xdist balancing) —
measured: the suite is I/O-bound with a ~177 s serial floor (fsync-bound) that
no rebalancing can touch.

**Constraint on CI disk headroom.** The `test.yml` durable-storage lane runs
the whole suite under `SECANTUS_FORCE_DURABLE=1`, and `tests/conftest.py` pins
`tmp_path_retention_policy="all"` (deleting a passed test's WT home
mid-session races WT's background threads → `WT_PANIC`). If the durable lane
keeps growing past the runner's disk, the next lever is a smaller
per-instance footprint — not a mid-session delete, which reintroduces the
panic.

Deferred, low value / risk — revisit only if inner-loop wall becomes a real
pain point again:

- **I2b — module-scoped server fixtures.** Convert the ~30 function-scoped
  `server(tmp_path)` fixtures to module scope (unique db/collection per test) to
  cut the *number* of WT open/close cycles. Modest upside (scaling curve shows
  the suite near its floor) and real risk: shared oplog / cluster-time across a
  module, so oplog / change-stream / reopen / capped / TTL-clock tests must stay
  function-scoped. Not worth it now.
- **V3 — `run-tests.php -j N` for the PHP-ext gauge.** Parallelise the ~712
  process-spawn `.phpt` runs. Blocked on flake risk: all `.phpt` hit one shared
  SecantusDB daemon — the contention CLAUDE.md flags at high concurrency. Needs
  a local PHP toolchain + a flake-diff-vs-serial run before adopting.
- **V2 — toolchain-daemon reuse in `validate-all`** (shared Gradle daemon across
  Java+Kotlin; dotnet incremental build; skip the gpg libmongocrypt re-verify
  when unchanged). Modest, and only affects the weekly CI wall.
- **Rewrite the test harness in Rust — INVESTIGATED, REJECTED (2026-08-29).**
  A clean full suite was 712 s wall but only 436 s CPU on 8 cores, so it is
  idle-bound, not CPU-bound; of the 281 ms per-test floor, 234 ms is inside
  WiredTiger's C library, ~37 ms is pymongo, and only ~10 ms is Python the
  harness owns. Optimistic ceiling for a total rewrite: ~2 % of wall. Full
  analysis: `tasks/rust-test-harness-investigation.md`.
- [ ] **L1 (WiredTiger-home clone) — remaining headroom.** `tests/wt_template.py`
  clones a pristine home per test for the files converted so far; the
  original count was ~48 more files that build a server by shapes other than
  `storage_path=str(tmp_path)` and still create a home per test. Compounds
  with making more of the 12 eager tables lazy (9.7 ms each, and it speeds
  both shipped servers' startup) and with I2b above. **Trap:** the
  `wt_template` import in `conftest.py` must stay *lazy* — see §11 of
  `tasks/rust-test-harness-investigation.md`.
  Backlog-ID: RM-007
  Verify: none -- test-harness speed lever (WT home clone)

## 4. Out of scope (intentional, with reasoning)

These are explicit non-goals. Don't add them without a reason.

- **Real replica sets / sharding** — depend on cluster topology and cross-node consistency. SecantusDB advertises `setName: "secantus"` to satisfy pymongo's change-stream topology check, but the topology is fictional — there are no other members, no elections, no cross-node oplog. Change streams are still in scope (single-node, oplog-backed); see `## 3. Deferred work / Change-stream limitations`.

    **Reading from a secondary is part of that non-goal** (decided 2026-09-27). There is no secondary to read from, so anything whose behaviour is defined by one is out of scope, not deferred. Three driver-gauge tests fail permanently on this and are accepted rather than tracked: `test_transactions_unified.py::TestUnifiedReadPref::test_secondary_readPreference` and the two `TestUnifiedRunCommand` read-preference cases (`test_run_command_fails_with_explicit_secondary_read_preference`, `test_run_command_fails_with_secondary_read_preference`). A permanently-red gauge test with a reason written down beats one quietly deselected — the pymongo pass rate carries the cost honestly.
- **`OP_COMPRESSED`** — compression negotiation. Clients can be told the server doesn't support compression; nothing to do.
- **Text search** (`$text`, `$meta: "textScore"`, text indexes) — would need a full-text index implementation.
- **Geo: exact mongod error-string matching** — chase work without a clear payoff unless a driver test pins exact wording.
- **`$where`** — runs JavaScript. We don't ship a JS runtime.
- **`$function`, `$accumulator`** (aggregation expressions) — same reason: both evaluate user-supplied JavaScript and need an embedded JS engine + sandbox + BSON↔JS shim layer. Would also require a `--javascriptEnabled` gate (mongod gates JS behind this; many prod deployments disable it). Adding the runtime would let us implement these three operators + `mapReduce` together for ~600–1000 LOC + a heavyweight binary dep (PythonMonkey / QuickJS / V8 — each with maintenance trade-offs). Most pipelines that reach for `$function` can be expressed in the existing aggregation expression library. **`lang: "python"` as a SecantusDB extension was considered and rejected (May 2026)**: would break the conformance contract (pipelines that work locally would explode in production with `Unrecognized lang value: 'python'`), and CPython's sandboxing story is worse than embedded JS engines. If a real user need ever surfaces, the right escape hatch is a server-side trusted-plugin registry — `{$secantusFunction: {name: "<pre-registered>", args: [...]}}` — not user code at query time.
- **`mapReduce`** — same JS-runtime dependency as `$where` / `$function` / `$accumulator`, and deprecated by MongoDB (removed from the Stable API in 5.0). **WILL NOT BUILD** (decided 2026-09-27). The canonical `emit(this.<field>, 1)` + `values.length` "count by field" pattern is translated to an equivalent `$group` aggregation; non-canonical map / reduce bodies return `{results: [], ok: 1}`, and `out: "<coll>"` (non-inline) is rejected with FailedToParse 9 (`mapReduce on this server only supports {out: {inline: 1}}`). mongo-java-driver's `testMapReduceWithGenerics` writes to a NAMED output collection and so fails permanently; that failure is accepted rather than tracked.
- **Profiling: `planSummary` / `keysExamined` / `docsExamined` / `execStats` in `system.profile` entries** — out of scope today: the profiler (2026-10-15) writes each entry from what the command did, and these are per-batch execution statistics this server does not keep (`explain` derives totals for the whole query). Also absent for the same reason: `locks`, `flowControl`, `storage`, the query hashes, `cpuNanos`, and a `getmore` entry's `originatingCommand` (a cursor does not keep its command). A `$jsonSchema` profile filter reads back as given, where mongod expands it.

## 5. Known bugs and edge cases to watch

- [ ] **`$dateFromParts` accepts an out-of-range `month` and answers a
  wrong date.** `{$dateFromParts: {year: 2017, month: 40000}}` returns
  5350-04-01 here; mongod 8.2.11 refuses it with 31034 (`'month' must
  evaluate to a value in the range [-32768, 32767]`). The other fields'
  ranges have not been checked. Found 2026-10-14.
  Backlog-ID: RM-139
  Verify: tests/backlog/test_rm_found_2026_10_14.py::test_date_from_parts_month_out_of_range_is_refused
- [ ] **`saslStart` for a user that does not exist continues the
  conversation.** mongod 8.2.11 fails the first step with 18
  `Authentication failed.`; this server answers `ok: 1, done: false` and
  fails later. Whether the later failure is complete was not checked.
  Found 2026-10-14.
  Backlog-ID: RM-141
  Verify: tests/backlog/test_rm_found_2026_10_14.py::test_sasl_start_for_an_unknown_user_fails

- [ ] **OPEN — the Go gauge's remaining failures, triaged (2026-09-29; 6
      leaves as of the 2026-10-09 run).** The gauge completes 659 of 659 and
      reads 598 / 12 / 49 = 98.0% (`docs/validation-report-go-rust-server.md`,
      measured 2026-10-09). The 12 fail events are 6 distinct leaves:

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
      Backlog-ID: RM-008
      Verify: tests/backlog/test_rm_001_028.py::test_ingress_connection_rate_limiter_parameters

- [ ] **OPEN — `$where` / server-side JavaScript is not implemented; the
      refusal is faithful (2026-09-29).** SecantusDB embeds no script engine
      and there is no plan to. Both servers answer as `mongod --noscripting`
      8.2.11 does: 6108304 / `Location6108304` / "no globalScriptEngine in
      $where parsing" in a query, and code 2 `$where is not allowed in this
      context` in an aggregation `$match` (checked at aggregate parse time,
      ahead of the leading-`$match` lift). Pinned by
      `tests/test_mongod_differential.py::test_where_refusal_matches_a_scriptless_mongod`
      / `::test_where_in_aggregate_match_is_a_context_error` and
      `tests/test_rust_server_smoke.py` for the Rust server.

      What stays open: the Go tests above that need the JavaScript to actually
      run. Implementing `$where` means embedding a JS engine — a large
      dependency for one operator, and worth a deliberate decision rather than
      drifting into.
      Backlog-ID: RM-009
      Verify: tests/backlog/test_rm_001_028.py::test_where_runs_javascript

- [ ] **OPEN — `$geoIntersects` with no sort may return a different order
      from mongod over a 2dsphere index (measured 2026-09-28).** Probing the
      Java `GeoJsonFilters` fixture, mongod returned `[1, 2, 4, 3]` and the Rust
      server `[1, 2, 3, 4]`. Unsorted order carries no guarantee, so this may
      be nothing; but since 2026-10-13 the Rust server orders documents by the
      plan mongod would run (`explain/order.rs`), so re-run the fixture
      against mongod 8.2.11 and either close this or file it under
      `tools/probes/find_order.py`. (The sort-BY-the-2dsphere-field divergence
      found alongside it was a document-comparator bug, fixed 2026-10-06.)
      Backlog-ID: RM-010
      Verify: tests/backlog/test_rm_001_028.py::test_geo_intersects_unsorted_order

- **Not a defect: the C gauge's two `/Client/ipv6/single` failures.** The
      test hard-codes `[::1]:27017` (documented as inherent); they are the gauge's only failures (790 / 2 / 46 of 838 with both
      topologies merged, `tests/test_c_gauge_topologies.py`).

- [ ] **OPEN — the .NET gauge spends ~90% of its wall clock after its last log
      line (2026-09-28).** Observed: TRX and report both written at 14:53, the
      process exited at 15:12 with `rc=0` and nothing logged in between — 19 of
      21 minutes. The temp WiredTiger store was cleaned up and no process was
      left behind, so it is a teardown stall rather than lost data.

      Worth chasing because it makes `validate-all` look far slower than the
      work justifies, and because an unexplained multi-minute gap is exactly the
      kind of thing later mis-attributed to the server. Start by timing the
      daemon shutdown path — the gauge's own test phase was 86s.
      Backlog-ID: RM-011
      Verify: none -- .NET gauge teardown stall (tooling)

- [ ] **OPEN — the Go cross-driver smoke tests download modules at test time.**
      `tests/test_cross_driver_features.py`'s `go run ./cluster_roles`,
      `./sessions` and `./batchsize-zero` fetch their modules from
      proxy.golang.org inside the test, and no CI job caches or pre-downloads
      them. On 2026-10-06 one `test-durable` lane failed all three with
      `stream error: stream ID 23; INTERNAL_ERROR` from the proxy before any
      program reached the server. The rerun passed, but the failure measured
      the network, not the server. Fix: `go mod download` in a setup step,
      with `actions/setup-go`'s module cache, so the test itself never goes to
      the network.
      Backlog-ID: RM-012
      Verify: none -- CI module download/caching

- [ ] **OPEN — `test_stop_ends_a_running_command` on Windows: the fix is not
      yet observed under the original race, and `test_opsboard.py`'s half was
      never touched.** Cause found 2026-10-08: a supervisor killed by `stop`
      could be killed while writing its child's exit code, leaving an empty
      exit file that made every later `status` raise. `scripts/detached_run.py`
      now writes files by rename, reads a damaged exit file as rc=-1, and has
      `stop` record the end itself. The Windows race is rare; if
      `tests/test_detached_run.py::test_stop_ends_a_running_command` fails
      again, its message now carries `status`'s stderr. The companion failure,
      `tests/test_opsboard.py::test_job_log_tail_captures_child_output`
      (seen together under `-n auto` on Windows, passes serially), has not
      recurred and was not diagnosed.
      Backlog-ID: RM-013
      Verify: none -- Windows-only test-harness race

- [ ] **OPEN — a push/PR CI run tests only Python 3.10 on Linux, so a 3.11+
      Linux bug can ONLY surface in the weekly cron (found 2026-09-28).**
      `test.yml`'s push/PR matrix is 3.10 on ubuntu; `workflow_dispatch` and
      `schedule` get the full 3.10–3.13 sweep (plus macOS). So a defect that
      needs 3.11+ on glibc is invisible to every PR, and the place it shows up
      is a cron run nobody is watching — the 2026-09-28 06:44 cron was already
      red on `test-durable (3.11/3.12/3.13, ubuntu-latest, 1)` and
      `test (3.11/3.12, ubuntu-latest, 1)` hours before anyone noticed.

      **Needs a decision rather than a default.** Adding 3.13-on-Linux to
      push/PR would catch this class at the cost of runner time on an already
      congested queue. Cheaper middle grounds: one extra Linux Python on PRs,
      or make the cron's failure loud (it currently notifies nobody).
      Backlog-ID: RM-014
      Verify: none -- CI matrix policy decision

- [ ] **OPEN — the published writer-scaling chart can only be measured on
      SHARED CPU, and a DigitalOcean tier ticket is in flight (2026-09-28).**
      `invoke do-perf` needs >= 8 vCPU (the sweep runs eight writer processes
      plus the server; on four vCPU it measures core starvation instead — mongod,
      the control, scales 4.96x on 8 vCPU against 1.78x on a `c-4`). Two separate
      limits block a dedicated plan:

      * **Account tier.** `c-8` is ABSENT from `GET /v2/sizes` for this
        account, along with the whole `g-` (General Purpose) and `m-`
        (Memory-Optimized) families. Visible CPU-Optimized stops at `c-4`.
        Asking for `c-8` returns
        `422 "This size is currently restricted, please open a ticket to increase
        your account tier."` Joe raised that ticket on 2026-09-28.
      * **Region.** Independently, in `lon1` EVERY plan at >= 8 vCPU is `Basic`
        (shared). The only 8-vCPU non-Basic plans visible are `s5-*`, described
        as "v5 Shared" — still shared — and only in atl1 / mem1 / mkc1 / ric1.
        So the tier alone may not be enough.

      When the ticket lands, re-run the check rather than assuming:

          # does a dedicated >=8 vCPU plan now exist, and where?
          curl -s -H "Authorization: Bearer $DO_TOKEN" \
            "https://api.digitalocean.com/v2/sizes?per_page=250" \
          | python3 -c 'import json,sys; [print(s["slug"], s["description"], sorted(s["regions"])) \
              for s in json.load(sys.stdin)["sizes"] \
              if s["vcpus"]>=8 and s["description"]!="Basic"]'

      `invoke do-perf --size <plan> --region <slug>` can then use it.
      **Re-measure mongod as the control before comparing across a region or
      plan change** — a different machine class moves every column at once,
      which is how one run of this sweep already had to be discarded.

      Why it matters: the concurrency chart's CLAIM is its shape ("monotonic,
      no cliff"), and a noisy neighbour is exactly what could fake a cliff or
      hide one.
      Backlog-ID: RM-015
      Verify: none -- cloud account tier for a benchmark

- **Driver unified-spec coverage audit (2026-09-25) — what is left.**
  - **Stale Go gauge skips.** `go_validation/include_packages.py` still skips
    `TestConvenientTransactions` (13/14 passed when measured; the skip comment
    says abort does not roll back, and it does), `TestBackpressureProse` (3/3
    passed; the comment says failpoints are not implemented) and
    `TestCausalConsistency_Supported` (29/29 passed). Re-include them.
  - Rust's `diagnostics::ok_transaction` ("no-op commit/abort") is dead code
    that nothing dispatches to.

### 2026-09-01: two PLANNER differences, measured and dispositioned

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
  cursor's answers 43, not mongod's 13.** `getMore` needs no privilege, so a
  distinguishable error would confirm or deny which cursor ids exist on other
  connections. Reverting it trades a hardening property for message fidelity
  nobody's driver reads.
- **DECIDED, not scheduled: a `find` on a MISSING collection orders the
  cursor reply's keys differently.** mongod answers `{id, ns, firstBatch}`
  there and `{firstBatch, id, ns}` for every other find. Ours is consistent,
  so it matches on the common path. A one-version observation about BSON key
  order is not enough to act on: two version splits on key order were found
  in the same campaign.
- **DECIDED, not scheduled: a hint that names no index answers a shorter
  message than mongod's.** Code (2) and `codeName` match and the causal
  sentence is the same; mongod's multi-line planner dump prefix
  (`error processing query: ns=<db>.<coll>Tree: _id $eq 1\nSort: {}\nProj:
  {}\nCollation: ...`) is absent. Reproducing it means porting
  `MatchExpression::debugString`, a different rendering from the
  `parsedQuery` normaliser, and a half-right renderer would look
  authoritative while wrong on anything nested. Message-only.
- **DECIDED, not scheduled: a `$`-prefixed unknown command field is accepted
  where mongod rejects it** (`Location40415`). `$`-keys are the wire envelope
  (`$db`, `$clusterTime`, `$readPreference`, `$audit`, ...) and an allowlist
  that missed one would break a driver over a message nobody reads. Revisit
  only with a driver-verified envelope list.

- [ ] **`invoke concurrency-refresh` needs a genuinely quiet box AND a spread
  audit before its output is trusted.** Two consecutive refresh runs
  (2026-08-02) were silently poisoned by a parallel session's gauge load —
  medians-of-3 didn't protect (whole interleaved passes degraded 10-30x,
  mongod itself read 10.5k vs its healthy 104k). A load<3 gate at START is
  insufficient; contention arriving mid-run wrecks all arms. Before accepting
  a refresh: check every `runs_docs_per_sec` cell for >2.5x intra-cell spread
  and re-run (or keep the previously committed clean numbers) when flagged.
  Worth upstreaming the audit into the task itself.
  Backlog-ID: RM-017
  Verify: none -- benchmark refresh needs a quiet box

- [ ] **Intermittent failure of the two `crash_watchdog_nested` tests under the
  full parallel suite (2026-08-29, `5372fdfa`).**
  The culprits are `tests/test_crash_stall_watchdog.py::
  test_faulthandler_dir_captures_a_worker_crash@crash_watchdog_nested` and
  `::test_faulthandler_dir_arms_a_file_per_worker@crash_watchdog_nested`. They
  fail only in the full parallel suite (~1 run in 3: three full-suite runs at
  one commit gave 5032 passed / 2 failed / 5032 passed) and pass 5/5
  standalone. Pre-existing: the file uses no WiredTiger storage, and the
  2-failure run was on an unmodified tree. Isolation passing does not
  exonerate a parallel-only flake. **To chase:** loop
  `uv run python -m pytest -q -rf` with full output retained (no filtering
  `grep`) until it reproduces, then diagnose the shared state / timing issue.
  Backlog-ID: RM-018
  Verify: none -- parallel-suite test flake

- [ ] **UNVERIFIED: a rare nested-pytest wedge BEFORE the summary line.**
  One captured nested run of `tests/test_crash_stall_watchdog.py` ended
  stdout at `[100%]` with no summary, i.e. a teardown wedge before xdist's
  `teardown_nodes` released the summary (distinct from the shared-temp-root
  `atexit` wedge fixed 2026-08-17). `_run_tolerating_teardown_wedge`
  tolerates the shape; the cause is unexplained. May be related to the
  `crash_watchdog_nested` item above.
  Backlog-ID: RM-019
  Verify: none -- nested-pytest teardown wedge

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
  `validation_summary/expected_failures.py` (`JAVA`). Triaged 2026-06-30
  against the Python server.
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
  built, so the server-side guard it wants to exercise cannot fire. Real
  mongod has the same problem.
- **EXPECTED-RED under a single-node topology: the Ruby gauge's `w: 2`
  create test** (`spec/mongo/collection_ddl_spec.rb:211`). Rejecting an
  unsatisfiable write concern (`CannotSatisfyWriteConcern`, 100) is the
  correct emulation of a one-member set and gains seven other tests; this one
  would pass only if the advertised roster grew.

- [ ] **C gauge: `/BulkOperation/OP_MSG/max_msg_size` ("2 == 1") fails only
  inside the full ~800-test run, on both servers.** It passes standalone and
  beside its `/BulkOperation/OP_MSG/*` siblings; all three advertised limits
  are byte-identical to mongod, and the driver decides the split itself. That
  profile is test interference or accumulated state, not a wire-limit
  divergence. Pinning it down means bisecting a ~10-minute full-gauge run.
  Backlog-ID: RM-020
  Verify: none -- C gauge failure only inside full gauge run
- [ ] **A wrong comment in `.github/workflows/test.yml`'s "Rust-server
  concurrency invariants" step.** It says the `[python]` params are skipped
  because they "need the SWIG `wiredtiger` binding this storage-flavoured
  wheel doesn't ship". The wheel does ship it (`CMakeLists.txt` builds it
  with `ENABLE_PYTHON=ON`), and the other gated files import and run there.
  Correct the comment, and decide whether `-k rust` is still needed.
  Backlog-ID: RM-021
  Verify: none -- wrong comment in a CI workflow

## 7. Python → Rust rewrite (in progress)

### 7.07 Open, unexplained, from the 0.7 gauge and benchmark refresh -- 2026-10-09

- [ ] **RUST MongoDB server: one writer made no progress for over 10 s in the
      droplet concurrency sweep.** `invoke do-perf`, 2026-10-09, run 3 of 3,
      server `rust`, 1 writer: the writer process did not stop within 10 s of
      SIGTERM, which means one `insert_many` of 100 x 8 KiB documents had not
      returned, and the harness refused the row (`writer 0 produced no
      summary`). Runs 1 and 2 of the same sweep were normal. Repeating that
      row 12 times on the same droplet afterwards gave 12 normal runs, at
      4,193 to 7,657 docs/s -- low and uneven beside the ~10,000 of earlier
      droplets. The droplet was an unusually bad one: `mongod` itself took 8x
      longer than its median in one latency pass, steal time was 2.8% of all
      CPU since boot, and `vmstat` showed 17-27% I/O wait with 46 MB/s of
      writeback on an otherwise idle machine. No OOM kill, no server log
      survived (the harness removes its temp stores). The evidence points at
      the machine, and does not exclude a stall in the server. Two later
      sweeps the same day, on other droplets, completed all rows.

      Followed up 2026-10-09 on a Mac (`mongod` 8.2.11, 256M cache, six
      processes writing and `F_FULLFSYNC`ing 512 MiB files beside the store,
      one client inserting 100 x 8 KiB): BOTH servers stall single inserts
      for seconds. Two 60 s runs each: `mongod` max 1.5 s and 4.1 s, the Rust
      server 1.3 s and 3.3 s; without the disk load neither exceeds 0.5 s.
      `mongod`'s `Slow query` line puts its 4.1 s in
      `storage.timeWaitingMicros.cacheMicros` with 4 MB written by the
      operation's own thread, i.e. application-thread eviction blocked on the
      disk. So seconds-long stalls under disk contention are shared with
      `mongod`; a TEN-second one was not reproduced and stays unexplained.
      The server now logs `Slow query` lines and the harness keeps a failed
      row's server log and store, so a recurrence says whether the server
      held the insert.
      Backlog-ID: RM-022
      Verify: none -- unreproduced 10 s stall on a bad droplet
- [ ] **RUST MongoDB server: the `Slow query` line does not say where the time
      went.** `mongod`'s carries `storage.timeWaitingMicros` (cache, schema
      lock) and bytes written by the operation; ours has only the duration.
      WiredTiger keeps these per session (`statistics:session`), but the
      storage layer opens a session per operation and exposes none of it.
      Also not on the line: `planSummary`, `keysExamined`, `docsExamined`,
      `nreturned`, `reslen`, and the command document.
      Backlog-ID: RM-023
      Verify: tests/backlog/test_rm_001_028.py::test_slow_query_line_says_where_the_time_went

### 7.084 What an authenticated user may run -- 2026-10-14

`tools/probes/auth_enforcement.py` starts mongod 8.2.11 and the Rust server
with `--auth` on empty stores, gives 66 users one role each (every built-in
role, and stored roles naming one collection, one database, a collection
name on every database, the cluster, another role, any resource), and has
each run about 180 commands. It compares whether the command ran and, if
not, the code. Pinned by
`crates/secantus-storage-adapter/tests/auth_enforcement_wt.rs`.

What is open:

Re-measured 2026-10-14 (`tools/probes/admin_commands.py`, 279 of 281
replies match mongod): **61 of 12,053 differ**, none of them about who
may run a command.

- [ ] **The probe's mongod is a standalone** and the Rust server says it is
      a replica-set member, so `$changeStream`, `replSetGetStatus`,
      `replSetGetConfig`, `getDefaultRWConcern` and `setDefaultRWConcern`
      (mongod: 51300) differ for the users both allow (32). Running the
      probe against a one-member replica set needs the set initiated before
      the localhost exception is used.
      Backlog-ID: RM-024
      Verify: none -- the probe's mongod topology (standalone vs replica set), not a server behaviour
- [ ] **`setFeatureCompatibilityVersion` records the version and gates
      nothing.** mongod at FCV 8.0 refuses the features 8.1 and 8.2 added;
      this server has no such gates. `getParameter` reports the stored
      version.
      Backlog-ID: RM-025
      Verify: tests/backlog/test_rm_001_028.py::test_fcv_80_refuses_a_feature_82_added
- [ ] **`dbHash` leaves out a time-series collection** and hashes none of
      its buckets; mongod hashes `system.buckets.<name>`. Not measured.
      Backlog-ID: RM-026
      Verify: tests/backlog/test_rm_001_028.py::test_dbhash_includes_timeseries_buckets
- [ ] **`mapReduce` with `out`** (18) and **`setParameter notablescan`**
      (3) answer an error here where mongod runs them; the users allowed are
      the same. (`compact` and `reIndex` refuse as mongod does on a
      replica-set primary, which this server reports itself as; compared
      against a standalone mongod they look like a difference and are not.)
      Backlog-ID: RM-027
      Verify: tests/backlog/test_rm_001_028.py::test_commands_mongod_runs
- [ ] **The server's own commands have no mongod rule.**
      `secantusAdmin.backupArchive` and `archiveBaseSnapshot` need cluster
      `readBackupFile` (the `backup` role), `restoreArchive` cluster
      `applyOps` (`restore`), `pruneOplog` cluster `replSetResizeOplog`, and
      `pruneTtl` cluster `compact`. These were chosen, not measured.
      Backlog-ID: RM-028
      Verify: none -- privilege rules for SecantusDB-only commands have no mongod reference

### 7.083 User and role commands -- 2026-10-14

`tools/probes/user_role_commands.py`, mongod 8.2.11: 16 scenarios, 399
replies over a raw connection, covering the sixteen user and role
management commands and the two that list them. The Rust server differs on
7. `tools/probes/scram_auth.py` is at none of 26.
Pinned by `crates/secantus-storage-adapter/tests/user_role_commands_wt.rs`
and the unit tests of `auth.rs` and `roles.rs`. The built-in roles and the
170 action names are a table generated from mongod
(`tools/probes/gen_builtin_roles.py` writes `builtin_roles.rs`).

What is open:

- [ ] **`rolesInfo: 1` lists stored roles by name** (2): mongod lists them
      in the order they were created. The role table is keyed by database
      and name and holds no sequence.
      Backlog-ID: RM-029
      Verify: tests/backlog/test_rm_029_056.py::test_roles_info_lists_roles_in_creation_order
- [ ] **The order of a set of roles** (5): a user's `roles` after
      `grantRolesToUser`, a role's `roles` after `grantRolesToRole`, and
      the built-in roles under `showBuiltinRoles`. mongod keeps each in a
      hash set, and the order differs between two databases of one
      process. This server keeps the order they were given in.
      Backlog-ID: RM-030
      Verify: none -- mongod's order is a hash set's: three different orders for one grant in one process, so no answer to assert
- [ ] **A user record written before this change has no `userId`**, and
      `usersInfo` lists none for it. Nothing rewrites the record. Such a
      store otherwise reads and authenticates: users and a role created by
      the released 0.5.3-beta.192 binary were read, and logged in over
      both SCRAM mechanisms, by this build (2026-10-14).
      Backlog-ID: RM-031
      Verify: none -- needs a store written by an older release (0.5.3-beta.192); mongod has no such state to compare
- [ ] **PYTHON server parity.** The user record's field order changed
      (`userId` added, `customData` and `authenticationRestrictions`
      stored, `credentials: {external: true}` for a `$external` user).
      The Python server reading such a store was not measured.
      Backlog-ID: RM-032
      Verify: none -- parity with the deprecated Python server, not with mongod

### 7.082 Handshake, diagnostic and session commands -- 2026-10-13

`tools/probes/diagnostic_commands.py`, mongod 8.2.11 (a replica-set member):
57 scenarios, 1,148 replies over a raw connection. The commands about the
server and the connection: `hello` and its older names, `ping`, `buildInfo`,
`connectionStatus`, `getParameter`, `serverStatus`, the statistics commands,
the session commands, the fields every command takes, and the commands this
version no longer has. Most replies hold values that differ from one machine
to the next, so a step is compared whole with the volatile keys reduced to
their BSON type, or by its keys and types to a given depth. mongod answered
the same on three runs against itself. The Rust server differs on 156
replies. Pinned by
`crates/secantus-storage-adapter/tests/diagnostic_commands_wt.rs` and the
unit tests of `generic.rs`, `serverinfo.rs` and `handshake.rs`.

Still different:

- [ ] **`validate` with `background: true` reads the live collection.**
      mongod validates the last checkpoint, so a collection created since
      then is 26 ("does not exist to validate") and one written since is
      validated as it was. Here it validates what is there now. Modelling
      it means reading WiredTiger's checkpoint, not the live table.
      (`validate` itself reads the store: `Storage::check_collection`
      compares every document with each index's entries, both ways, and
      with the `_id` index. Its key counts match mongod over seven index
      shapes. It repairs nothing, and it does not check a time-series
      collection's buckets or a unique index's claims.)
      Backlog-ID: RM-033
      Verify: tests/backlog/test_rm_029_056.py::test_background_validate_of_an_uncheckpointed_collection
- [ ] **`serverStatus.locks` is absent**, and with it the gap test's last
      section. mongod counts its lock manager's acquisitions per resource
      and mode, and this server has no such lock manager. The other
      sections are reported from real counters (`asserts`, `mem`,
      `extra_info`, `globalLock`, `logicalSessionRecordCache`,
      `opLatencies`, `security`, `transactions`, `wiredTiger`,
      `metrics.{commands, document, operation, query}`); those that need a
      mechanism this server lacks stay absent rather than zero: `repl`, the
      plan-cache and sort metrics, `queues`, `transportSecurity`.
      Counted differently from mongod: `asserts.user` moves by 1 where
      mongod raises an error twice internally (an invalid sort, an index
      build, a duplicate in an unordered insert); an `aggregate` whose stage
      does not parse counts as a run; `$out` / `$merge` writes are not in
      `metrics.document`; a `bulkWrite` multi-update counts its documents
      in `metrics.query`.
      Backlog-ID: RM-034
      Verify: tests/backlog/test_rm_029_056.py::test_server_status_sections
- [ ] **Server parameters** (about 30). `getParameter` reports the
      thirteen parameters that are true of this server
      (`serverinfo::parameters`) and mongod has several hundred.
      `setParameter` changes six (`cursorTimeoutMillis` and
      `transactionLifetimeLimitSeconds` among them, each applied to its
      registry, and `notablescan`); the others mongod changes at runtime are
      20 here: of those this server reports, `scramIterationCount` and
      `scramSHA256IterationCount` (mongod sets both at runtime; a fix must
      make the value govern the credentials `createUser` / `updateUser`
      write afterwards, not just store it). `notablescan`'s refusal has mongod's code and closing words
      (`... running with 'notablescan' 2`) but not its query-tree text.
      Backlog-ID: RM-035
      Verify: tests/backlog/test_rm_found_2026_10_15.py::test_scram_iteration_count_is_settable_at_runtime
- [ ] **Commands answered only as far as this server honours them.**
      `features` has no `js` block (there is no script engine) and refuses
      `oidReset`; `getClusterParameter` has six of mongod's fifteen
      parameters, the ones whose default is a fact here (no shards, no
      default `maxTimeMS`, pre-images that go with the oplog), and answers 4
      for the rest; `planCacheSetFilter` is not a command, because no filter
      would be applied; `replSetResizeOplog` refuses a `minRetentionHours`
      above 0, because this oplog drops an entry older than its retention
      window whatever its size; `replSetReconfig` is not a command. Each
      is a feature to build, not a reply to change.
      Backlog-ID: RM-143
      Verify: tests/backlog/test_rm_029_056.py::test_features_reports_the_script_engine,tests/backlog/test_rm_029_056.py::test_features_oid_reset,tests/backlog/test_rm_029_056.py::test_plan_cache_set_filter,tests/backlog/test_rm_029_056.py::test_resize_oplog_min_retention_hours,tests/backlog/test_rm_029_056.py::test_every_cluster_parameter
- [ ] **A frame with an opcode the server does not serve closes the
      connection.** mongod 8.2.11 sends nothing back for opcode 9999, or 1
      (`OP_REPLY`) sent as a request, and answers the next request on the
      same socket; this server resets the connection, losing anything else
      in flight on it. Found 2026-10-15.
      Backlog-ID: RM-144
      Verify: tests/backlog/test_rm_found_2026_10_15.py::test_unknown_opcode_is_ignored_and_the_connection_kept
- [ ] **A legacy `OP_GET_MORE` closes the connection.** mongod 8.2.11
      answers an `OP_REPLY` (flags 2, cursor 0) holding `{$err: "OP_GET_MORE
      is no longer supported. ...", code: 5739101, ok: 0}` and keeps the
      connection. Found 2026-10-15.
      Backlog-ID: RM-145
      Verify: tests/backlog/test_rm_found_2026_10_15.py::test_legacy_op_get_more_is_refused_with_an_op_reply
- [ ] **An exhaust `getMore` whose last batch is short sends one frame too
      many.** With 10 documents and `batchSize: 3`, mongod 8.2.11 sends
      `[3,4,5]` and `[6,7,8]` with `moreToCome`, then `[9]` without it and
      cursor id 0; this server sends `[9]` with `moreToCome` and the live
      id, then an empty final frame. Found 2026-10-15.
      Backlog-ID: RM-146
      Verify: tests/backlog/test_rm_found_2026_10_15.py::test_exhaust_get_more_ends_with_the_short_batch
- [ ] **`serverStatus` has no `network.compression`.** mongod 8.2.11
      reports `bytesIn` / `bytesOut` for each compressor and decompressor
      (snappy, zstd, zlib; int64, zero on a fresh server). This server
      speaks all three since 2026-10-15 and counts none of them.
      Backlog-ID: RM-147
      Verify: tests/backlog/test_rm_found_2026_10_15.py::test_server_status_reports_compression_counters
- **Scope decision — `buildInfo.secantusVersion` and `serverStatus.secantus`
  stay** (Joe, 2026-10-15). Each names this build beside mongod's own
  fields. They are additive, documented (`CLAUDE.md`, `docs/compatibility.md`,
  `docs/servers.md`), and how the admin UI tells this server from mongod;
  no driver compares a whole `buildInfo` reply.
- [ ] `buildInfo` has no per-OS block; `collStats` has no `wiredTiger` or
      `indexDetails` (they are WiredTiger statistics of the collection's
      and each index's own table, and here collections share a documents
      shard table and every index one entries table, so no per-collection
      statistics exist; the shard table's own would count other
      collections too); `latencyStats` is zeros; `listDatabases` lists no
      `config` (mongod writes its sessions to `config.system.sessions`;
      here they are kept in memory), so `dbHash` on `config` differs, as on
      `admin` does (`admin.system.keys` holds random signing keys there, a
      digest no server could match); `connectionStatus` lists no
      privileges for a user who has some; `hostInfo.extra` is empty on
      Linux. `$indexStats` counts the plan the `explain` planner picks:
      where several indexes could serve, mongod's trial may pick another
      (measured: `find({a: 1, b: 6})` counts `b_1` on mongod, `a_1` here).
      `$listCatalog` (decided 2026-10-15) names this store's own tables and
      has no `multikeyPaths`; it has no `ident` for `local.oplog.rs` (the
      oplog spans several tables), lists by database and then name rather
      than creation order, gives an index `spec` in `listIndexes`' key
      order, and reads a geo index's `multikey` from the documents now.
      Backlog-ID: RM-040
      Verify: tests/backlog/test_rm_029_056.py::test_coll_stats_wiredtiger_and_index_details,tests/backlog/test_rm_029_056.py::test_list_databases_lists_admin_and_config

### 7.081 Geo queries -- 2026-10-13

`tools/probes/geo_queries.py`, mongod 8.2.11: 105 scenarios, 1,278 replies
over a raw connection (`$geoWithin`, `$geoIntersects`, `$near`,
`$nearSphere`, the `$geoNear` stage, and what a `2d` or `2dsphere` index
holds and refuses). A read is compared by its whole reply, a double to nine
digits; an index build or a write by its reply and by what the collection
and its indexes hold afterwards. mongod answered the same on three runs
against itself. The Rust server differs on 137 replies. Pinned by
`crates/secantus-storage-adapter/tests/geo_queries_wt.rs`.

mongod tests `$geometry` on the sphere (an edge is the great-circle arc
between its ends), and a flat shape (`$box`, `$polygon`, `$center`) holds
points only. Where the code is now: `secantus_core::geoshape` (shapes, the
sphere, `$near`, what an index holds), `secantus-commands/src/near.rs`
(where `$near` may stand, its index, nearest first),
`aggregate::apply_geo_near`.

Still different, all 137:

- [ ] **The order under a geo index** (about 100). mongod returns a
      `$geoWithin` or `$geoIntersects` in the order of its index scan: S2
      cell order for `2dsphere`, geohash order for `2d`. Here it is storage
      order. Documents at one distance from a `$near` come in that scan's
      order too; here points first, then insertion order.
      Backlog-ID: RM-041
      Verify: tests/backlog/test_rm_029_056.py::test_geo_within_in_2dsphere_scan_order,tests/backlog/test_rm_029_056.py::test_geo_within_in_2d_scan_order,tests/backlog/test_rm_029_056.py::test_near_ties_in_scan_order
- [ ] **`2d` index options** (11): `bits` outside 1 to 32 and `min` not
      below `max` are refused by mongod (72) and built here; `bits`, `min`
      and `max` are not listed as given.
      Backlog-ID: RM-042
      Verify: tests/backlog/test_rm_029_056.py::test_2d_index_options_refused,tests/backlog/test_rm_029_056.py::test_2d_index_options_listed
- [ ] **A `2d` index with its own range** (7): a `$near` centre outside
      the range is mongod's 16433, and the two ends of the range.
      Backlog-ID: RM-043
      Verify: tests/backlog/test_rm_029_056.py::test_near_centre_outside_2d_range
- [ ] **A hinted geo index** (10): `find({}, hint: <2dsphere index>)`
      returns what the index holds in its order; a `$near` with a hint to
      another index is refused.
      Backlog-ID: RM-044
      Verify: tests/backlog/test_rm_029_056.py::test_hinted_2dsphere_index_order,tests/backlog/test_rm_029_056.py::test_near_hinted_to_another_index
- [ ] `2dsphereIndexVersion: 2.5`, two indexes with one key in one
      `createIndexes` (85), `$geoNear` inside `$lookup` and `$unionWith`
      (mongod's wrapper on the 291).
      Backlog-ID: RM-045
      Verify: tests/backlog/test_rm_029_056.py::test_2dsphere_index_version_not_an_integer,tests/backlog/test_rm_029_056.py::test_two_indexes_one_key_in_one_create_indexes,tests/backlog/test_rm_029_056.py::test_geo_near_in_lookup_wrapper
- [ ] **Approximations in the sphere tests.** A point is on an edge when it
      is within 1.5e-9 radians of it, where mongod asks whether the polygon
      may meet the point's leaf cell. A polygon is taken to be the side its
      vertices' centre is on, so one larger than a hemisphere is read as its
      complement, as mongod reads it without a `crs`; the `strictwinding`
      `crs` is not read. `$centerSphere` holds a line or polygon when every
      vertex is inside.
      Backlog-ID: RM-047
      Verify: tests/backlog/test_rm_029_056.py::test_center_sphere_and_a_line_bulging_out,tests/backlog/test_rm_029_056.py::test_strictwinding_crs
- [ ] **A bounded `$geoNear` reads every document.** Its `$centerSphere`
      candidate fetch left out a line or polygon whose nearest point is
      inside the distance and a vertex outside (`geo_near_index_filter`,
      switched off).
      Backlog-ID: RM-048
      Verify: none -- performance: the bounded $geoNear answer is right, it reads every document

### 7.080 The order `find` returns documents in -- 2026-10-13

`tools/probes/find_order.py`, mongod 8.2.11: 47 scenarios, 6,531 replies
over a raw connection. A read is compared by the `_id`s it returns, in
order; a write of one document (`delete` with `limit: 1`, `update` without
`multi`, `findAndModify`) by its reply and by what the collection holds
afterwards. mongod answered the same on three runs against itself. The
Rust server differs on 97 replies. Pinned by
`crates/secantus-storage-adapter/tests/find_order_wt.rs`.

The store says only WHICH documents match (`Storage::matching_rows`, in
insertion order) and the order is the plan mongod runs, walked over those
documents (`crates/secantus-commands/src/explain/order.rs`, on the planner
`explain` already had). `find`, the front of `aggregate`, `distinct`,
`findAndModify` and the two write commands read through it. Collections that
read in an order of their own are left as they were: time-series, clustered,
the oplog view.

What the order follows, each measured:

- An index scan returns its entries by key and then by insertion; a
  backward scan returns equal keys last first.
- `$ne`, `$nin`, `$exists`, `$type`, `$not`, `$mod` and a regex read the
  index in mongod, and come back in its order. The router scans the
  collection for them.
- A hinted `_id` index reads in `_id` order.
- `$or` branches that are one predicate on an index's leading field share
  one scan of it; the scans come index by index, not in the order written.
- A sort on a field that holds arrays is read from the index only when the
  bounds leave no key out. Otherwise it is a sort of the scan.
- A sort the predicate's index does not return: mongod also tries a walk
  of an index that does and keeps the plan that finishes first. That is
  counted here, in mongod's own steps, for a collection of up to 2,000
  documents (`plan::walk_finishes_first`). Past that the predicate's index
  is kept.
- An index of another collation does not return a sort, and a regex is
  still bounded by an index of plain strings under a query collation.
- An empty array is indexed as `undefined`, between MinKey and null, and an
  array element without the rest of the path as null.

Still different, all 97:

- [ ] **Ties under a sort with a limit** (62). mongod keeps the first
      `limit` documents of a blocking sort in a heap, and the order the heap
      leaves equal documents in is its standard library's (`make_heap` and
      `pop_heap`; working libc++'s by hand gives mongod's answer for
      `{a: 5}, sort {a: -1}, skip 1, limit 2`). This server keeps them in
      the order they arrived. A Linux mongod is built on another library
      and was not measured.
      Backlog-ID: RM-049
      Verify: tests/backlog/test_rm_029_056.py::test_ties_under_a_sort_with_a_limit,tests/backlog/test_rm_029_056.py::test_ties_under_an_aggregate_sort_with_a_limit
- [ ] **Ties between the scans a sort merges** (about 15): `{a: {$in: [5,
      3]}}, sort {b: 1}` over `{a: 1, b: 1}`. mongod merges one scan per
      value through a priority queue; equal documents come out in the
      queue's order. Here they come out scan by scan.
      Backlog-ID: RM-050
      Verify: tests/backlog/test_rm_029_056.py::test_ties_between_merged_scans
- [ ] **An unknown hint's wording** on `aggregate` (15), as under 7.075.
  Backlog-ID: RM-051
  Verify: tests/backlog/test_rm_029_056.py::test_unknown_hint_on_aggregate
- [ ] **Which of several predicate indexes is read** (1): `{a: {$gt: 0},
      b: 1}` with `a_1`, `b_1` and `a_1_b_1`. mongod reads `b_1`; the fixed
      preference here takes `a_1_b_1`.
      Backlog-ID: RM-052
      Verify: tests/backlog/test_rm_029_056.py::test_which_predicate_index_is_read
- [ ] **A `$match` after `$addFields`** (1): mongod moves it ahead of the
      stage when it reads no field the stage writes, and then reads its
      index.
      Backlog-ID: RM-053
      Verify: tests/backlog/test_rm_029_056.py::test_match_after_add_fields_reads_its_index
- [ ] One regex under a collation with two indexes on the field, and two
      `$in` sorts after writes that turn on the same merge.
      Backlog-ID: RM-054
      Verify: tests/backlog/test_rm_029_056.py::test_regex_under_a_collation_with_two_indexes,tests/backlog/test_rm_029_056.py::test_in_sort_after_writes
- [ ] **The multikey paths are not stored.** They are read from the
      collection the first time a query needs them after the server starts
      or an index is dropped, and kept current by writes after that
      (`Storage::index_multikey_paths`). mongod's are in its catalog and
      never shrink; after a restart these are what the documents hold.
      Backlog-ID: RM-055
      Verify: tests/backlog/test_rm_029_056.py::test_multikey_paths_survive_a_restart
- [ ] **The cost.** Timed against the build before on 20,000 documents
      (`find` by `_id`, by an indexed value, an indexed range, a multikey
      value, a full scan, sorts, `update_one`): within 8% where the router
      read the index the plan names, since its rows are already in order
      (`already_in_order`), and for a scan. `{a: {$ne: 5}}` over `a_1`
      costs 3.2 times as much (19,980 rows to put in index order where the
      router scans), a sort with ties over a 500-document collection with
      an index on each field 1.6 times (the trial reads the collection),
      and `update_one` by an indexed value 1.2 times (the rows are read to
      be ordered, then again to be written). An index scan in the store
      that reads bounds in order would remove the first.
      Backlog-ID: RM-056
      Verify: none -- performance budget (timings against an earlier build)

### 7.079 `$out` and `$merge` -- 2026-10-13

`tools/probes/out_and_merge.py`, mongod 8.2.11: 16 scenarios, 376 replies
over a raw connection. Each case compares the reply AND what the source
and the target hold afterwards: documents in order, indexes, options. The
Rust server differs on 4 of 376. Pinned by
`crates/secantus-storage-adapter/tests/out_and_merge_wt.rs`.

`$merge` is written with the `update` command, one statement a document,
as mongod writes it (`aggregate::apply_merge`): `merge` is `{$set: doc}`,
`replace` a replacement, `keepExisting` `{$setOnInsert: doc}`, a pipeline a
pipeline update with `c: {new: doc}`, `fail` an insert.

Still different, all 4:

- `cursor: {batchSize: 0}` (2): mongod does not run the stage until the
  first `getMore`. The lazy-cursor decision.
- A `$merge` whose first write fails to parse its query (1): mongod has
  already created the target.
- The collation key in a duplicate-key message under a collation (1):
  ICU's bytes.

A fifth comes and goes: with two `on` fields, one of them missing from
the source document, mongod inserts `{_id, a: null, k}` on one run and
`{_id, k, a: null}` on another. This server always stores them sorted.
It is the upsert seed order of 7.077.

Not measured: `$merge` into another database; `$out` and `$merge` in a
transaction.

### 7.078 `insert`, `delete`, `count` and `distinct` -- 2026-10-12

`tools/probes/insert_delete_count_distinct.py`, mongod 8.2.11: 38
scenarios, 905 replies over a raw connection. Each case runs against a
collection of its own and compares the reply AND the documents left
behind, in the order the collection holds them. The Rust server differs on
32. Pinned by
`crates/secantus-storage-adapter/tests/insert_delete_count_distinct_wt.rs`.

A document `_id` takes a claim by value in `UNIQ_TABLE`
(`Storage::document_id_claim`), on top of the `_id` index keyed by BSON
bytes (`encode_id_key`, on disk and shared with the Python server). A
document stored before that is given its claim the first time this version
opens the store (`backfill_document_id_claims`, once for a store; a row in
`UNIQ_TABLE` under an empty namespace records it). Two stored documents
already equal by value both stay, and the older holds the claim.

Still different, all 32:

- **The message for a hint that names no index** (20, with 7.077's 7).
  mongod prints its query tree. Joe's call, not made.
- `$where` in a query (3): not run here.
- `$near` with no geo index (2): mongod answers 291; this server finds
  nothing.
- `includeQueryStatsMetrics` (2): mongod adds a `metrics` document.
- Which unique index a duplicate of two is reported for (1): mongod
  checks them in the order they were built, which the catalog does not
  record (the same gap as `listIndexes` order).
- The collation key printed in a duplicate-key message under a collation
  (1): ICU's bytes.
- `$expr` that fails when folded (1): mongod adds `Failed to optimize
  expression :: caused by ::`.
- `distinct` over a multikey index (1): mongod returns `9` for a value
  nested as `[8, [9]]`, where its collection scan returns `[9]`.
- `distinct` with a collation over an index of that collation (1): mongod
  leaves out `null`.

Not measured: a `readConcern` with `atClusterTime` older than the oldest
timestamp is `239 SnapshotTooOld` on `find`, `aggregate` and `distinct`;
this server runs the read.

### 7.077 `update` and `findAndModify` -- 2026-10-12

`tools/probes/update_commands.py`, mongod 8.2.11: 30 scenarios, 1,295
replies over a raw connection. Each case runs against a collection of its
own and compares the reply AND the documents left behind. The Rust server
differs on 17 of 1,295. Pinned by
`crates/secantus-storage-adapter/tests/update_commands_wt.rs`.

What is left, on the RUST MongoDB server:

- [ ] **`secantus-core-py`, the parity binding, still decodes through
      serde** (36 calls). The PostgreSQL crates' five were replaced on
      2026-10-14 (see the Rust PG server's built-in functions entry).
      Backlog-ID: RM-058
      Verify: none -- the serde decode is in the secantus-core-py parity binding, a test vehicle with no wire behaviour to compare against mongod
- [ ] **The field order of a document upserted from two or more
      equalities.** mongod: `{a: 1, b: 2, c: 3, d: 4}` gives `c, a, b, d`;
      `{d: 1, c: 2, b: 3, a: 4}` gives `d, b, a, c`; an explicit `$and`
      keeps the order written. This server sorts them. No rule was found.
      Backlog-ID: RM-059
      Verify: tests/backlog/test_rm_057_084.py::test_upsert_field_order_from_several_equalities
- [ ] **`$where` in an update's query** is refused (no JavaScript).
  Backlog-ID: RM-060
  Verify: tests/backlog/test_rm_057_084.py::test_where_in_an_update_query
- [ ] **`$text` in an update's query** answers a generic refusal; mongod
      answers 27, `text index required for $text query`.
      Backlog-ID: RM-061
      Verify: tests/backlog/test_rm_057_084.py::test_text_in_an_update_query_without_a_text_index
- [ ] **`$$CLUSTER_TIME`** in a pipeline update is refused.
  Backlog-ID: RM-062
  Verify: tests/backlog/test_rm_057_084.py::test_cluster_time_in_a_pipeline_update
- [ ] **`$replaceWith` of a non-document in an update** names only the
      fields the expression read as the input document; mongod prints the
      whole document there (and only those fields in `aggregate`).
      Backlog-ID: RM-063
      Verify: tests/backlog/test_rm_057_084.py::test_replace_with_a_non_document_in_an_update
- [ ] **A parse error in `$currentDate` against a collection that does not
      exist** is no longer reported, since the statement is skipped.
      Backlog-ID: RM-065
      Verify: tests/backlog/test_rm_057_084.py::test_current_date_parse_error_on_a_missing_collection

### 7.076 DDL and admin commands -- 2026-10-12

`tools/probes/ddl_admin_commands.py`, mongod 8.2.11: 37 scenarios, 809
replies over a raw connection (`create` and every option it takes,
`listCollections`, `collMod`, `renameCollection`, `drop`, `dropDatabase`,
`dbStats`, `collStats`, `dataSize`, `listDatabases`, and invalid database
names across thirteen commands). Byte counts are compared by type only. The
Rust server differs on 2 of 809. Pinned by
`crates/secantus-storage-adapter/tests/ddl_admin_commands_wt.rs`.

What is left, on the RUST MongoDB server:

- [ ] **`listDatabases` lists `admin` and `config` only once something is
      written to them.** mongod always has them. (`local` is listed, as
      empty; mongod's holds the oplog.)
      Backlog-ID: RM-066
      Verify: tests/backlog/test_rm_057_084.py::test_list_databases_always_has_admin_and_config
- [ ] **`listIndexes` after a conversion to unique** lists `unique` after
      `expireAfterSeconds`; mongod lists it before. The same family as the
      index build order under 7.071.
      Backlog-ID: RM-067
      Verify: tests/backlog/test_rm_057_084.py::test_list_indexes_key_order_after_conversion_to_unique
- [ ] **`dbStats` reports `fsUsedSize` and `fsTotalSize` as 0.** The
      filesystem's numbers are not read.
      Backlog-ID: RM-068
      Verify: tests/backlog/test_rm_057_084.py::test_db_stats_reports_the_filesystem

### 7.075 `explain`, command by command -- 2026-10-12

`tools/probes/explain_commands.py`, mongod 8.2.11: 51 scenarios, 791
`explain` replies over a raw connection (`find`, `count`, `distinct`,
`aggregate`, `update`, `delete`, `findAndModify`; index bounds, sort,
paging, projection, `$or`, hints, collation, views, execution counts, what
the command refuses). Nearly every scenario offers the planner ONE usable
index or none, so the plan is a rule and not the outcome of mongod's trial
runs; the scenarios named `choice:` offer two. The Rust server differs on
72 of 791. `explain` (`crates/secantus-commands/src/explain/`) derives the
plan from mongod's planning rules, and `executionStats` from walking that
plan over the collection's documents. Pinned by
`crates/secantus-storage-adapter/tests/explain_commands_wt.rs`.

What is left, all on the RUST MongoDB server:

- [ ] **`explain` describes the plan mongod runs, which is not always how
      this server reads the documents.** Results are the same; the access
      path is not. Measured over 57 filter shapes, `explain` reports an
      index scan and the storage router scans the collection for 19: `$ne`,
      `$nin`, `$not`, `$exists`, `$type`, `$mod`, `$all` of one, both forms
      of `$elemMatch`, an equality to a sub-document, a `$regex` (prefix or
      not), a range on `_id`, and a filter written as an explicit `$and`.
      Each is a place the router could use the index it has. Whether
      `explain` should say what mongod does or what this server does is
      Joe's call; it says what mongod does.
      Backlog-ID: RM-069
      Verify: none -- the router's internal access path is not observable on the wire (results and explain both match mongod); whether explain should describe it is Joe's call
- [ ] **No rejected plans.** `rejectedPlans` is always empty. Where two
      indexes could serve a query, the winner agreed with mongod on all 20
      `choice:` replies, by a fixed order of preference (`plan.rs`) and not
      by trial. 19 replies differ only in the count of rejected plans.
      Backlog-ID: RM-070
      Verify: tests/backlog/test_rm_057_084.py::test_explain_reports_rejected_plans
- [ ] **`aggregate` beyond the stages the planner absorbs** (about 24
      replies). Not reproduced: `$group`, `$count`, `$sortByCount`,
      `$bucket` and `$lookup` pushed into the query (mongod reports a
      slot-based plan, `explainVersion: "2"`); `$group` served by a
      `DISTINCT_SCAN`; the projection mongod adds for the fields later
      stages need; a `$match` moved ahead of an `$unwind`; `$facet`,
      `$documents`, `$project` and `$group` as mongod re-prints them; and
      per-stage `nReturned` under a `$limit`, which here is the count of a
      full run.
      Backlog-ID: RM-071
      Verify: tests/backlog/test_rm_057_084.py::test_explain_of_a_group_pushed_into_the_query
- [ ] **`count` and `distinct` on a view** report the cursor and the
      desugared stages, not mongod's slot-based `GROUP`.
      Backlog-ID: RM-072
      Verify: tests/backlog/test_rm_057_084.py::test_explain_of_count_on_a_view
- [ ] **Smaller:** `$expr` is not rewritten (`{$expr: false}` is not an
      `EOF`, an equality inside `$expr` adds no `$_internalExprEq`); a
      comparison to an ARRAY bounds nothing; `$where` is refused by `find`
      itself; `explain` of `mapReduce` and `bulkWrite` is refused;
      `local.oplog.rs` and `system.views` report `EOF`; several intervals
      on one multikey field take the first predicate's bounds, where mongod
      races the alternatives.
      Backlog-ID: RM-074
      Verify: tests/backlog/test_rm_057_084.py::test_explain_of_expr_false_is_eof

### 7.074 Change streams, command by command -- 2026-10-11

`tools/probes/change_stream_commands.py`, mongod 8.2.11 as a replica-set
member: 115 scenarios over a raw connection (what `$changeStream` accepts,
every event's shape, `getMore`, the invalidating operations, resuming by
`resumeAfter` / `startAfter` / `startAtOperationTime`, the three scopes,
pre-images, transactions), each on a database of its own. The Rust server
differs on 5 results. The harness salts database names per run, because
mongod keeps the oplog of earlier runs and a stream started in the past
would see them. What is left:

- [ ] **RUST MongoDB server: a resume token mongod cannot decode gets this
      server's one error.** mongod parses a token as a KeyString and
      reports where it failed (`Invalid advance (8) past end of buffer`,
      15; `KeyString format error: Unknown type: 0`, 50811) for `_data`
      that is valid hex but not a token; this server answers 40647 for all
      of them. Its tokens are its own encoding (hex of a BSON document),
      not mongod's, so a token from one cannot be given to the other (3 of
      the 5).
      Backlog-ID: RM-075
      Verify: tests/backlog/test_rm_057_084.py::test_undecodable_resume_token_errors
- [ ] **RUST MongoDB server: `explain` of a `$changeStream` is this
      server's plan.** mongod explains the oplog scan it would run, with
      the full match over `local.oplog.rs`; not reproduced, like the rest
      of mongod's planner output (1 of the 5).
      Backlog-ID: RM-076
      Verify: tests/backlog/test_rm_057_084.py::test_explain_of_a_change_stream_scans_the_oplog
- [ ] **RUST MongoDB server: a `delete` with `limit: 0` after a `multi`
      update deletes in natural order.** mongod 8.2.11 deleted the two
      documents in the reverse order; why was not measured (1 of the 5).
      Backlog-ID: RM-077
      Verify: tests/backlog/test_rm_057_084.py::test_delete_order_after_a_multi_update
- [ ] **RUST MongoDB server: smaller change-stream differences.** The
      `lsid` on a transaction's events carries the SHA-256 of the empty
      string as `uid` whoever is authenticated (mongod hashes the user). A
      280 error's message shows the whole token; mongod shows 128
      characters and `...`, and this server's tokens are usually shorter.
      A token
      from before 2026-10-11 is lower-case hex and still resumes.
      Backlog-ID: RM-078
      Verify: tests/backlog/test_rm_057_084.py::test_resume_token_not_found_message_cuts_the_token
- [ ] **RUST MongoDB server: an event over 16 MB is split differently.**
      This server's first fragment carries `fullDocument`, mongod 8.2.11's
      carries `updateDescription`. (Sharded-only event types --
      `shardCollection`, `reshardCollection` -- cannot be probed on one
      node.)
      Backlog-ID: RM-079
      Verify: tests/backlog/test_rm_057_084.py::test_an_event_over_16mb_is_split

### 7.073 The cursor commands -- 2026-10-11

`tools/probes/cursor_commands.py`, mongod 8.2.11: 85 scenarios (`find`'s
cursor options, `getMore`, `killCursors`, the cursors of `aggregate` and the
listing commands, tailable cursors, `count`, `distinct`). The Rust server
differs on 5 scenarios. What is left:

- [ ] **RUST MongoDB server: a cursor is the result as it was when the
      command ran.** `find` and `aggregate` read every matching document and
      keep the rest for `getMore`. mongod reads on as the cursor is read, so
      its `getMore` sees a document inserted, changed or deleted since
      (measured: an insert appears, a delete disappears). Tests that write
      between batches see the difference; so does memory, for a large
      result. One scenario.
      Backlog-ID: RM-080
      Verify: tests/backlog/test_rm_057_084.py::test_get_more_sees_writes_made_since_the_find
- [ ] **RUST MongoDB server: `listIndexes` answers in name order.** mongod
      answers in the order the indexes were built (`_id_` first on both).
      The catalog keeps no build order, so this needs a field on disk, which
      the Python server would then have to carry too. One scenario.
      Backlog-ID: RM-081
      Verify: tests/backlog/test_rm_057_084.py::test_list_indexes_in_build_order
- [ ] **RUST MongoDB server: an `aggregate` cursor outlives the index it
      reads.** A `find` cursor is killed when its index is dropped (175,
      since 2026-10-14); an `aggregate` whose `$match` reads one is not.
      Not measured against mongod.
      Backlog-ID: RM-082
      Verify: tests/backlog/test_rm_057_084.py::test_aggregate_cursor_after_its_index_is_dropped
- [ ] **RUST MongoDB server: smaller cursor differences.** A projection of
      `{$meta: "recordId"}` or `{$meta: "indexKey"}` adds nothing (mongod:
      the record id, the index key). An unknown hint is refused with the code and this server's own text,
      a decision already made (`tests/test_rust_hint_error_message.py`).
      Not probed: `oplogReplay`, `$meta: "textScore"` sorts, an
      `explain: true` aggregate.
      Backlog-ID: RM-083
      Verify: tests/backlog/test_rm_057_084.py::test_meta_record_id_and_index_key_projections

### 7.072 Sessions, transactions and retryable writes -- 2026-10-11

`tools/probes/transactions.py`, mongod 8.2.11 as a replica-set member: 251
scenarios, each a script of commands over raw connections with the session
fields written out (the transaction state rules, what a transaction refuses,
conflicts, collections made in a transaction, cursors, the session commands,
retryable writes). The Rust server differs on 11 scenarios. What is left:

- [ ] **RUST MongoDB server: `drop` removes a collection an open
      transaction holds.** With a transaction holding a collection, mongod
      8.2.11 makes `drop` (and `createIndexes`) wait until `maxTimeMS`,
      answers 50, and keeps the collection. Here the drop answers ok within
      half a second and the collection is gone while the transaction is
      still open -- after a transaction that only READ, and (re-measured by
      the gap tests on 2026-10-14) after one that WROTE, where an earlier
      probe had seen it wait for the commit. What the open transaction then
      sees or commits has not been measured, and is the data-integrity
      question. `tools/probes/transactions.py`, "a collection dropped under
      a transaction ...".
      Backlog-ID: RM-084
      Verify: tests/backlog/test_rm_057_084.py::test_drop_waits_for_a_transaction_that_read,tests/backlog/test_rm_057_084.py::test_drop_under_a_transaction_that_wrote_honours_max_time
- [ ] **RUST MongoDB server: smaller differences.** A `count` with no query
      does not see another session's uncommitted insert (mongod's fast count
      does: it answered 3 for 2 committed documents). A write that waits on a
      transaction's document past `maxTimeMS` answers a plain 50 (mongod adds
      a `writeConcernError` and words it `Plan executor error during
      update`). `$where` has no JavaScript to run, in or out of a
      transaction (2 scenarios). An error that names the request or mongod's
      oplog entry (40606 / 40607 / 40608 / 40612 on a mismatched
      `findAndModify` retry; the two statement-id errors) has the code and a
      shorter text. A write conflict on an insert has an empty cause where
      mongod names its index table. The other session errors are worded for
      a session with no user.
      Backlog-ID: RM-087
      Verify: tests/backlog/test_rm_085_112.py::test_fast_count_sees_another_sessions_uncommitted_insert

### 7.071 Time-series collections -- 2026-10-11

`tools/probes/time_series.py`, mongod 8.2.11: 187 scenarios on a fresh
database each (what `create` accepts, inserts, reads, the restricted writes,
indexes, `collMod`, rename and the rest). The Rust server differs on 6.
mongod keeps the rows as buckets in `system.buckets.<name>` and presents the
collection as a view; this server keeps ordinary documents, and most of what
is left follows from that:

- [ ] **RUST MongoDB server: the buckets are derived, not stored**
      (2026-10-11; 6 of 187 still differ). The server keeps measurements as
      ordinary documents, the format the Python server shares, and works the
      buckets out when asked (`crates/secantus-commands/src/ts_buckets.rs`):
      reads of `system.buckets.<name>` and `system.views`, the listings, the
      scan order, `validate`, `collStats` and `$collStats`. What that leaves:
      - **Placement can differ for a batch that runs backwards in time.**
        mongod sorts an insert batch before placing it; the derivation sees
        only insertion order, so such a batch can make more buckets here.
        A bucket also never closes for size or a schema change here, only
        at 1,000 measurements.
      - **A bucket's `data` columns are valid but not mongod's bytes.** Each
        value is a literal and each gap a skip. mongod 8.2.11 unpacks them to
        the same measurements (checked by inserting one into its
        `system.buckets`), but it writes delta-encoded runs.
      - **A bucket cannot be written.** An insert that passes the bucket
        schema, and any update or delete on `system.buckets.<name>`, answers
        115. `renameCollection` on it answers 166 (mongod renames it).
        An index on it must be one the collection can show.
      - **`collStats` has no write counters** (`numBucketInserts`,
        `numBucketsClosedDueTo...`): a history this server does not keep.
        Its sizes are of the derived buckets, not of compressed ones.
      - **An unsorted read of a time-series collection reads the whole
        collection** to place the measurements, whatever its filter.
      Backlog-ID: RM-088
      Verify: tests/backlog/test_rm_085_112.py::test_bucket_collection_can_be_deleted_from
- [ ] **RUST MongoDB server: smaller time-series differences.** A
      measurement inserted with no `_id` is given one (mongod stores none);
      an index on `{_id: 1}` reports that it already exists (mongod builds
      one); a `hashed` index is refused as on any collection (mongod builds
      it); a hint of `{_id: 1}` is refused with this server's wording for an
      unknown hint. `listCollections` puts each bucket namespace after its
      collection and `system.views` last; mongod's order is not stable
      between two runs of the same commands.
      Backlog-ID: RM-089
      Verify: tests/backlog/test_rm_085_112.py::test_measurement_without_id_is_stored_without_one

### 7.070 Collation -- 2026-10-10

`tools/probes/collation.py`, mongod 8.2.11: 764 scenarios (reads under 29
collations, 39 malformed specs on every command, the expression and query
operators, and a fresh database per scenario for defaults, indexes, views and
writes). The Rust server differs on 4 (`tasks/rust-mongod-remaining-plan.md`,
2026-10-14). It compares through ICU4X
(`crates/secantus-core/src/collation.rs`), which ordered 157 strings as
mongod does under 40 collations with the one exception listed first below.
What is left, there and beside it:

- [ ] **RUST MongoDB server: canonically equivalent strings are always
      equal.** mongod with `normalization: false` (the default) tells
      `"ọ̈"` from `"ọ̈"`; ICU4X normalises always, so the
      Rust server calls them equal under every collation. Also, under
      `alternate: "shifted", maxVariable: "space"` the Rust server calls
      `"a b"` and `"a​b"` equal to their neighbours and mongod
      does not. mongod links ICU 57.1 and ICU4X carries newer CLDR data, so a
      character or tailoring added since can order differently; none was
      found in 24 locales.
      Backlog-ID: RM-091
      Verify: tests/backlog/test_rm_085_112.py::test_canonically_equivalent_strings_differ_without_normalization
- [ ] **RUST MongoDB server: collated `_id` uniqueness, what is left.**
      (The rule: a claim per document in the unique-keys table under the
      collated key; documents stored before it, or by the Python server, are
      claimed when the store opens.) A duplicate whose `_id` is a document
      shows the value as written (mongod shows each string in it as raw
      sort-key bytes). The Python server does not enforce the rule itself:
      as the server holding the store it accepts a second spelling, and the
      Rust server, which walks the documents again after it, keeps both and
      logs them.
      Backlog-ID: RM-092
      Verify: tests/backlog/test_rm_085_112.py::test_collated_duplicate_document_id_shows_sort_key
- [ ] **RUST MongoDB server: a query never uses a collated index.** An index
      with a collation now holds collation sort keys (catalog marker
      `collationKeys`), which is what makes its `unique` right, and the
      planner passes it over: a query with a collation scans. Correct, slow
      on a large collection. A hint naming such an index is accepted and
      scans.
      Backlog-ID: RM-094
      Verify: none -- explain already reports IXSCAN (derived from mongod's rules); the gap is the router scanning, a cost no reply shows
- [ ] **RUST MongoDB server: five collation types are refused.** `search`,
      `searchjl`, `big5han`, `gb2312han` and `phonetic` (as
      `@collation=<type>`) are not in ICU4X's bundled data; the server
      answers 2 naming the type, mongod accepts them.
      Backlog-ID: RM-095
      Verify: tests/backlog/test_rm_085_112.py::test_five_collation_types_are_accepted
- [ ] **RUST MongoDB server: smaller collation differences.** `Did you
      mean` is missing or different for a three-letter code (`eng`, `en_USA`)
      and for `zh@collation=stroke` (mongod: `zh_Hant`); a duplicate-key
      error quotes ICU4X's sort key, whose bytes are not ICU 57's (`2b0105`
      for mongod's `290105`), and for a string inside a document it quotes
      the string where mongod quotes the key; `$lookup` from a view whose
      collation differs from the pipeline's answers (mongod: 167); `$merge`
      onto a collated unique index does not raise the duplicate key mongod
      does; `hashed` and `text` indexes with a collation are refused as all
      such indexes are; a validator on an `update` reads the statement's
      collation, not the collection's.
      Backlog-ID: RM-096
      Verify: tests/backlog/test_rm_085_112.py::test_did_you_mean_for_a_three_letter_code,tests/backlog/test_rm_085_112.py::test_lookup_from_a_view_with_another_collation
- [ ] **RUST MongoDB server: a duplicate `_id` spelled exactly the same in a
      collection with a default collation** is reported from the plain `_id`
      index (`index: _id_ dup key: { _id: "a" }`); mongod reports it under
      the collation (`index: _id_ collation: { ... } dup key: ...`), as it
      does for a second spelling. Message text only (found 2026-10-14).
      Backlog-ID: RM-142
      Verify: tests/backlog/test_rm_found_2026_10_14.py::test_exact_duplicate_id_under_a_default_collation_names_the_collation
- [ ] **RUST MongoDB server: `$densify` with `bounds: "full"` and
      `partitionByFields` fills each partition only to its own ends.**
      mongod fills every partition across the whole collection's range, and
      its output has a quirk: the second document in sort order, when it
      starts a new partition, is emitted as it stands and its partition is
      then filled from the start, repeating that value (`[(a,0), (c,1),
      (a,2), (b,3)]` gives a generated `(c,1)` beside the stored one). Not
      reproduced; the rule needs more than the six shapes measured.
      Backlog-ID: RM-097
      Verify: tests/backlog/test_rm_085_112.py::test_densify_full_partitions_fill_the_whole_range
- [ ] **RUST MongoDB server: `$setWindowFields` emits partitions in the
      order first seen.** `$fill` was measured on 2026-10-10 to emit them in
      partition-key order and now does; an older note in `windowfields.rs`
      records first-seen order as measured for `$setWindowFields`, which was
      not re-measured.
      Backlog-ID: RM-098
      Verify: tests/backlog/test_rm_085_112.py::test_set_window_fields_partition_order

### 7.069 A WiredTiger panic opening a store, in CI on macOS -- 2026-10-10

- [ ] **OPEN — `wiredtiger_open` panicked once in the `test-durable
      (macos-14, 3.12, 3)` lane: a log write failed with `Bad address`.**
      Run [38040336462](https://github.com/jdrumgoole/SecantusDB/actions/runs/38040336462/job/114179135784),
      on PR #1860, whose whole diff is version strings. The PYTHON server's
      storage, in the fixture of
      `tests/test_sql_ddl_name_resolution.py::TestSequenceAsARelation::test_tracks_the_value_actually_handed_out`
      (2,900 passed, 1 error):

      ```
      __posix_file_write, 629: .../popen-gw1/test_tracks_the_value_actually0/./WiredTigerTmplog.0000000001:
          handle-write: pwrite: failed to write 128 bytes at offset 0: Bad address
      __log_fs_write, 218: ./WiredTigerTmplog.0000000001: fatal log failure: Bad address
      __log_fs_write, 218: the process must exit and restart: WT_PANIC: WiredTiger library panic
      ```

      So `pwrite` returned EFAULT for the first 128 bytes of a brand-new log
      file, while the connection was still opening. Nothing had been written
      to that store yet, so no data was at risk in this instance. EFAULT
      means the kernel could not read the buffer it was handed.

      **Looked at again 2026-10-11. The cause is still not known, and it has
      not been reproduced.** What was established:

      - **WiredTiger did not hand `pwrite` a bad pointer.** The write is the
        log file header, from `__log_file_header` (`src/log/log.c`): it
        takes a scratch buffer, zeroes all 128 bytes, fills in the
        descriptor, checksums the 128 bytes, and passes that same pointer
        and length to `__wt_log_fill`, which calls `pwrite`. A bad pointer
        would have faulted in the `memset` or the checksum, in this
        process, before the system call. So the pages were readable in user
        space and unreadable to the kernel a few instructions later.
      - **Neither log patch is on that path.** `patch_wt_dsync_group.py` and
        `patch_wt_fsync_group.py` change `__log_write_internal` and
        `__wt_log_release`; neither touches the header write or its buffer.
      - **It is rare, and so far macOS only.** Every failed job of the last
        400 `Tests` runs (2026-09-28 to 2026-10-11: 56 runs not green, 42
        failed jobs, 11 of them macOS) was read for `Bad address`,
        `WT_PANIC` and `fatal log failure`. This run is the only hit.
      - **A local stress did not produce it.** 10 processes each opening,
        writing and closing 400 durable stores (4,000 opens, macOS arm64,
        load 18), with a thread per process spawning subprocesses and the
        allocator churned: no failure
        (`wtopen_stress.py`, not kept in the repository).

      What is left is a page that became unreadable between two adjacent
      uses: either the runner could not page it back in, or another thread
      in the same process unmapped it (which would be a heap bug in some
      extension loaded into the test worker, not necessarily WiredTiger).
      Nothing here tells those apart. If it happens again, record whether
      it is again the header write at open, which lane, and what else the
      worker had loaded.
      Backlog-ID: RM-099
      Verify: none -- one-off WT panic in CI on macOS, not reproducible by a test

### 7.068 The `bulkWrite` command and TTL indexes -- 2026-10-10

`tools/probes/bulk_write_command.py` and `tools/probes/ttl_indexes.py`, mongod
8.2.11. The Rust server differs on 2 `bulkWrite` results (both the hint
wording filed under 7.067) and 2 TTL results
(`tasks/rust-mongod-remaining-plan.md`, 2026-10-14). What is left:

- [ ] **RUST MongoDB server: a clustered collection reports an `_id` index.**
      `createIndexes` on one answers `numIndexesBefore: 1`; `mongod` answers
      0, because the clustered key is not a separate index.
      Backlog-ID: RM-100
      Verify: tests/backlog/test_rm_085_112.py::test_clustered_collection_has_no_separate_id_index
- [ ] **RUST MongoDB server: a pipeline upsert that yields an array `_id`
      answers 53.** `mongod` answers 54 (`... the (immutable) field '_id' was
      found to be an array or array descendant`), as it does for a
      replacement, which the Rust server matches. The storage path cannot
      tell a pipeline from an operator update.
      Backlog-ID: RM-101
      Verify: tests/backlog/test_rm_085_112.py::test_pipeline_upsert_with_array_id
- [ ] **RUST MongoDB server: `upsertSupplied` is two steps.** The update runs
      without the upsert, and when nothing matched `c.new` is inserted: not
      one atomic statement, so a concurrent insert between the two can make
      it a duplicate-key error. (The validator is applied, and the `_id` is
      the query's, re-measured 2026-10-14.)
      Backlog-ID: RM-102
      Verify: none -- a race between the two steps; no deterministic check
- [ ] **RUST MongoDB server: `getParameter ttlMonitorSleepSecs` reads 60 until
      a client sets it**, whatever cadence the server was started with, and a
      server started with the sweep off (`ttl_sweep_seconds = 0`) has no
      monitor for `ttlMonitorEnabled` to turn on. The embedded Python
      `RustServer` handle runs no monitor at all.
      Backlog-ID: RM-103
      Verify: tests/backlog/test_rm_085_112.py::test_ttl_monitor_sleep_secs_reports_the_started_cadence
- [ ] **RUST MongoDB server: the TTL cutoff is exclusive.** A date exactly
      `expireAfterSeconds` old is kept until the next pass. Which side
      `mongod` puts that one millisecond on was not measured.
      Backlog-ID: RM-104
      Verify: none -- the boundary millisecond depends on when a TTL pass runs; not observable deterministically
- [ ] **RUST MongoDB server: `bulkWrite` ignores a `collectionUUID`
      mismatch.** It inserts where mongod 8.2.11 refuses with 361. Not
      probed: `maxTimeMS`, and the command inside a transaction.
      Backlog-ID: RM-105
      Verify: tests/backlog/test_rm_085_112.py::test_bulk_write_collection_uuid_mismatch

### 7.067 Index management -- 2026-10-10

`tools/probes/index_admin.py`, mongod 8.2.11: the Rust server differs on 5
of 90: `text` and `hashed` are not built (out of scope) and so cannot be
dropped (four), and `listIndexes` lists by name.

- [ ] **RUST MongoDB server: `listIndexes` is in name order after `_id_`.**
      `mongod` lists in creation order. The storage layer keeps no creation
      order for indexes.
      Backlog-ID: RM-107
      Verify: tests/backlog/test_rm_085_112.py::test_list_indexes_in_creation_order

### 7.066 Views and `$count` -- 2026-10-09

`tools/probes/views.py`, mongod 8.2.11: the Rust server is at 0 of 70
(`tasks/rust-mongod-remaining-plan.md`, 2026-10-14). What is left beside it:

- [ ] **RUST MongoDB server: smaller view differences.** A `hint` naming an
      index on a view is ignored (`mongod`: 2, `hint provided does not
      correspond to an existing index`); `$indexStats` on a view answers
      (`mongod`: 40602); `distinct` on a view returns values in scan order,
      `mongod` in hash order.
      Backlog-ID: RM-108
      Verify: tests/backlog/test_rm_085_112.py::test_hint_on_a_view_is_refused, tests/backlog/test_rm_085_112.py::test_index_stats_on_a_view_is_refused
- [ ] **`mongod` 8.2.11 itself crashes on `{collMod: <a view>, cappedSize:
      100}`** (the connection closes and the server is gone). Not ours; the
      Rust server answers 72 `Collection must be capped.` Do not put that
      command in a probe that needs mongod to survive it.
      Backlog-ID: RM-109
      Verify: none -- mongod's own crash, not a Rust server gap

### 7.065 Capped collections -- 2026-10-09

- [ ] **RUST MongoDB server: a `collMod` of `cappedSize` / `cappedMax` is
      written to the oplog as `size` / `max`.** `mongod` records the command
      as sent. Replay applies it correctly; a `showExpandedEvents` change
      stream shows different field names. Not probed.
      Backlog-ID: RM-110
      Verify: tests/backlog/test_rm_085_112.py::test_coll_mod_capped_size_event_names_fields_as_sent
- [ ] **RUST MongoDB server: capped eviction rescans the whole collection on
      every insert chunk.** `enforce_capped_bounds` calls `scan_docs` and sums
      every document's size to learn the collection's size and count, so an
      insert into a capped collection costs time proportional to the
      collection. Not measured. A running size and count would remove it.
      Backlog-ID: RM-111
      Verify: none -- performance (rescan per insert), no answer differs

### 7.06 Release tooling gaps found cutting 0.6.0b18 -- 2026-10-07

- [ ] **The binary smoke test and the stale-artifact check look at DIFFERENT
  binaries.** `tests/test_rust_binary_smoke.py` prefers
  `crates/secantusdb/target/release/secantusd-rs` and falls back to debug,
  while `./inv rust-binary-build` builds debug and the provenance check passed
  on it. A `beta.165` release binary from 29 September, older than the SIGTERM
  fix (#1738), was smoked instead and failed
  `test_stops_on_sigterm_sent_at_the_banner...[_block_stop_signals]`
  deterministically. That looked like a regression in the release suite.
  Fix: make the provenance check cover every binary the smoke test can pick,
  or make the smoke test use exactly the one the check verified.
  Backlog-ID: RM-112
  Verify: none -- release tooling (which binary the smoke test picks)
- [ ] **The `secantusdb-release` skill's finalize loop uses `timeout`, which
  macOS does not have** (`command not found`, exit 127 on all four attempts,
  which reads as four finalize failures). Use a background run, or `gtimeout`
  from coreutils.
  Backlog-ID: RM-113
  Verify: none -- release-skill tooling (macOS lacks timeout)

### 7.05 MongoDB-server backlog close-out -- 2026-10-07

**Left open on purpose, each with its reason** (the entry itself has the detail):

- **Unknown-index `hint` plan dump** (both servers, 6 message-only cases): needs
  mongod's planner `debugString` renderer for text on an already-correct code.
- **`$where` / server-side JavaScript**: a scope decision; it also accounts for
  three of the Go gauge's remaining failures.
- **Go gauge `TestSDAMProse/heartbeats_processed_more_frequently`**: the Python
  cause was the awaitable `hello` (fixed); not re-run on the Rust server this
  batch, so the Go-gauge triage entry stays open until the next Go run.
- **A `w` tag set with more than one key**: accepted divergence (mongod's
  hash-map order).
- **The multikey `$sort` plan artifact**: accepted -- mongod contradicting its
  own `_id` tie-break.
- **Decimal128 transcendental last digit**: authorised (correctly rounded).
- **Make Rust the recommended default**: a product decision for Joe.
- **PyPI trusted publisher for `secantus-core` / lockstep publishing**: needs
  Joe's PyPI account.
- **CI Python matrix (3.10 only on push)** and **vendored WiredTiger rebuilt per
  CI job**: cost decisions.
- **.NET gauge on this box**: a stale `gpg` lock -- environment, not code.
- **Performance items** (droplet comparison, write amplification,
  `--oplog-async` default, cache defaulting, `log_file_max`, the writer-scaling
  chart, `concurrency-refresh` on a quiet box): need droplet runs and
  durability decisions, not code.
- **`test_tmp_retention_guard` timeout**: not reproduced; a watch item.
- **Go smoke tests fetch modules at test time**: needs a vendoring decision.
- **Stale `-rust-server` validation reports**: the stamping is fixed; the
  numbers wait on re-running those gauges.
- **Out of scope**: macOS x86_64 wheels, and the in-process engine-selection
  notes in the porting history.

### 7.03 Rust packages (crates.io) -- open items, 2026-10-01

- [ ] **The embedded Python `RustServer` handle runs no sweepers.**
      `secantus_mdb::Server` (and the daemon) expire TTL-indexed documents
      every 60 seconds (`Builder::ttl_sweep`) and can run the noop heartbeat
      that also prunes the oplog (`Builder::noop_heartbeat`); the Python
      embedded handle runs neither. Check: `git grep -i "ttl_sweep\|noop_heartbeat"
      crates/secantus-server-py/` finds nothing (2026-10-14).
      Backlog-ID: RM-114
      Verify: none -- embedded Python RustServer handle lifecycle; needs the PyO3 extension and a 60s sweep

### 7.01 Rust MongoDB server driver-gauge sweep — 2026-09-30

- [ ] **ENVIRONMENT, not code -- left for Joe. The .NET gauge could not run on this box:** its build step `gpg
  --batch --import` blocks on a stale `~/.gnupg/public-keys.d/pubring.db.lock`
  left by a dead process under an old hostname. An environment problem, not a
  repo one; clear the lock and re-run `invoke validate-dotnet --server rust`.
  (The Ruby gauge hit a similar local problem -- Homebrew's read-only
  `rdoc_plugin.rb` -- and passed with `BUNDLE_PATH` pointed elsewhere.)
  Backlog-ID: RM-115
  Verify: none -- local gpg lock, environment

- [ ] **NOT REPRODUCED -- left open as a watch item. `test_tmp_retention_guard.py::test_default_tmp_retention_policy_is_allowed`
  timed out ONCE (2026-09-30).** It passed in every full-suite run of the
  2026-10-07 batch (14,500+ tests under `-n auto`). Its nested collect-only pytest hit the 300s
  `subprocess.run` timeout in an `-n auto` run of eight files alongside the
  PG-binary tests; it takes ~3s, and passed 3 of 3 alone straight after. A
  300s wall on a 3s job is a hang, not slowness -- the docstring already
  names one such hang (the atexit `pytest-of-<user>` cleanup), which
  `--basetemp` was meant to remove. Not reproduced yet; run the file under
  `-n auto` next to a heavy suite and sample the nested process if it sticks.
  Backlog-ID: RM-116
  Verify: none -- unreproduced test-suite timeout watch item

### 7.00 Rust MongoDB server batch — 2026-09-30, measured against mongod 8.2.11

- [ ] **Two of the 21 write-command divergences found on 2026-09-02 were
      never identified as fixed.** Instrumenting `update_operators`,
      `arg_types_documents`, `update_path_conflicts` and
      `findandmodify_shapes` with a Rust column (`tools/probes/_servers.py`,
      `probe_targets()`) found 21 Rust-vs-mongod divergences; 19 were fixed
      and the record said the other 2 "are recorded below", but no later
      entry names them. Re-run those four probes against `secantusd-rs` and
      mongod 8.2.11 and file whatever still differs.
      Backlog-ID: RM-117
      Verify: none -- probe re-run campaign to find two unnamed divergences

- **Scope decision: the six message-only bad-hint divergences in
  `tools/probes/arg_types_messages.py` are DELIBERATE, and BOTH servers have
  them.** mongod answers a bad hint with a multi-line planner diagnostic
  (`error processing query: ns=<db>.c / Tree: $and / Sort: {} / Proj: {} /
  planner returned error :: caused by :: hint provided does not correspond to an
  existing index`) where both servers name the hint. The CODE matches.
  Reproducing it would mean rendering mongod's `MatchExpression` DEBUG string, a
  third rendering vocabulary distinct from `parsedQuery`'s JSON and from the
  value form, for an error whose code is already right. Recorded so it is not
  re-investigated.

- **Scope decision (Joe, 2026-10-06): keep CORRECT ROUNDING for the Decimal128
  transcendentals.** The last digit of a Decimal128 transcendental (`$ln`,
  `$log10`, `$exp`, `$asinh`, the trig and hyperbolic family) is the correctly
  rounded value, and where mongod's own last digit differs (7 of 6,628 cases in
  `agg_expressions.py`, 1-2 units in the 34th digit; mongod carries Intel RDFP's
  last-digit error) that is an AUTHORISED divergence -- this narrow one only:
  the last digit of a finite decimal transcendental. Do not "fix" it toward
  mongod. Matching mongod exactly would need RDFP linked; that option is
  unchosen.

- **Scope decision: mongod's per-position timelib DIAGNOSTIC for a `$toDate` of
  an unparseable string is not reproduced, on either server.** Measured across
  25 strings, there is no small rule -- `'a1'` errors at position 1 but `'1a'`
  at position 0; `'123'` emits one error PER position; `'junk here'` emits two
  including "Double timezone specification"; and `'2020-01-01X'` PARSES because
  `X` is the military timezone UTC+11. It needs timelib's lexer, its timezone
  abbreviation tables and its per-position error accumulation; inventing a
  position would look authoritative and be wrong. The parse SUCCESS path ships.

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

### 7.0 Cache-pressure rollback — CI exercises it now

`WT_ROLLBACK` is two conditions wearing one code: a write-write conflict
(retryable → `WriteConflict`) and WiredTiger abandoning a transaction whose own
dirty content it cannot evict (NOT retryable → `TransactionTooLargeForCache`).
`Session::rollback_reason()` separates them at all three sites that mint the
error (#981), and the condition can surface at EITHER level (outer `Err` from
the post-statement guard, or the INNER one from an engine-side rollback);
`txn_budget.rs` accepts both.

The branch is exercised, but not deterministically: it fires on a loaded CI
runner and could not be reproduced on a fast machine. A `StorageOptions`
override for the dirty budget would let a test force the engine to lose the
race on demand, which is worth having if this area changes again.

### 7.1 Rust server performance and security review (2026-06-16)

**Deferred / not yet ported:**
- [~] **R7 tail — Windows lane added; PGO on MSVC still open**
  The `x86_64-pc-windows-msvc` lane in the `secantusdb-v*` release-binaries
  matrix builds `no_pgo`. PGO on MSVC needs an instrumented MSVC build plus an
  `llvm-profdata` merge. Two Windows-specific details worth remembering:
    * `+crt-static` — WT's `wiredtiger.lib` uses the static CRT while Rust's
      MSVC target defaults to the dynamic one. That mismatch is what
      `LNK4098: defaultlib 'LIBCMT' conflicts` was reporting, and it means two C
      runtimes with two heaps in one process — a real corruption risk for a
      database, not a cosmetic warning.
    * The binary smoke test can't use SIGTERM on Windows (`send_signal` maps it
      to `TerminateProcess`: immediate kill, exit 1, no handler). It sends
      `CTRL_BREAK_EVENT` to a `CREATE_NEW_PROCESS_GROUP` child instead, which
      the `ctrlc` crate's console handler turns into the same graceful path
      SIGTERM takes on Unix.
- [ ] **Go gauge: CI runs ~1/5 of the local set** — CI weekly artifacts have
  always reported ~450 tests (e.g. 401/453 on 2026-06-08, 447/900 on
  2026-06-12) while local `invoke validate-go` runs ~4700 (the numbers the
  pre-2026-06-12 committed report carried). Suspects: the 30-minute
  `go test -timeout`, cold caches on runners (GitHub cache service 400s), or
  package discovery under `./internal/integration/...` differing on CI.
  Diagnose before trusting week-over-week go comparisons.
  Backlog-ID: RM-118
  Verify: none -- Go gauge CI test-count discrepancy
- [ ] **Per-index-collation IXSCAN on the Rust server** (from the removed
  "Collation server-wide" record). Any command `collation` forces a COLLSCAN
  plus an in-memory collation-aware match / sort:
  `crates/secantus-storage/src/lib.rs` `find_matching_with` sets
  `force_collscan = coll_opt.is_some()` ("Per-index-collation IXSCAN is a later
  optimisation"), even when an index was built with the same collation. Results
  are correct; the cost is a full scan. The Python server already uses a
  collated index whose collation matches exactly (`docs/indexes.md` "Per-index
  collation"; `numericOrdering` still COLLSCANs there). Reproduce: create
  `{name: 1}` with `collation: {locale: "en", strength: 2}`, then
  `find({name: "x"}).collation({locale: "en", strength: 2})` and look at how
  the storage router reads it.
  Backlog-ID: RM-119
  Verify: none -- performance (router COLLSCAN); results correct and explain reports mongod's plan
- [ ] **Regex patterns over 1000 characters are capped on both servers** (from
  the removed "Query `$regex`" record). `crates/secantus-core/src/regexutil.rs`
  `MAX_REGEX_PATTERN_LEN = 1000` (mirroring `secantus.query._MAX_REGEX_PATTERN_LEN`,
  a ReDoS mitigation for Python's `re`) makes a longer pattern fail to compile
  (`Err(())`, which defers, i.e. an error on the Rust server). mongod's own
  limit has not been measured against it. Reproduce:
  `find({x: {$regex: "a" * 1001}})` on each server and on mongod 8.2.11.
  Backlog-ID: RM-120
  Verify: tests/backlog/test_rm_113_138.py::test_regex_over_1000_characters
- [ ] **One-time: configure a PyPI Trusted Publisher for `secantus-core`.**
  On PyPI, add a Trusted Publisher for the (new) `secantus-core` project pointing
  at `.github/workflows/rust-wheels.yml` (environment `pypi`), mirroring the
  `secantus` setup. Until this exists the tag-gated `publish` job fails auth; the
  `build`/`sdist` jobs run on every push/PR and are already CI-validated, so this
  is the only thing blocking the first published `secantus-core` wheel.
  Backlog-ID: RM-121
  Verify: none -- PyPI account configuration
- [ ] **Release-process: bump & publish `secantus-core` in lockstep.** The
  `secantus[rust]` extra pins `secantus-core` to the exact SecantusDB version, so
  every release must (a) bump BOTH versions — root `pyproject.toml` and
  `crates/secantus-core/pyproject.toml` — and the `rust` extra pin in the root
  pyproject, and (b) publish both wheels. Wire this into the `/secantusdb-release`
  skill so the two can never drift (the `secantus[rust]` install breaks if the
  matching `secantus-core` version isn't on PyPI).
  Backlog-ID: RM-122
  Verify: none -- release-process wiring

### 7.4 Verified rust-only gauge tail (2026-06-25)

Rust-only driver-gauge failures that are deliberately not chased:

- **Out of scope (session plumbing, do not chase):** ruby "behaves like a failed
  operation using a session raises an error" (×3, `Collection#create` / `#indexes` /
  `Index::View#create_one`) and php-lib `WatchFunctionalTest::testSessionFreed` (×1,
  `resumeCallable` unset on invalidate via reflection). Same class as the deferred
  change-stream session items — driver-internal session lifecycle, not a wire divergence.
- **go `TestChangeStream_ReplicaSet/try_next/one getMore sent` (3 entries, 1 test) — NOT a server bug; §5 harness race (verdict 2026-06-26).**
  Accepted, same as the Python-server verdict in §5. The Rust change-stream
  behaviour was verified correct directly (a pymongo repro against the standalone
  binary: correct start position, no scope leak; the go test run in isolation
  passes 10/10). The full-suite failure is the shared-daemon gauge's lack of
  per-test namespace isolation: other `t.Parallel()` tests write the shared
  `TestDB` namespace during the `try_next` await window, and that genuinely
  same-namespace write correctly wakes the stream. Runner-side isolation was tried
  twice (2026-06-26) and still flaked, so it is not suppressible at the runner.

### 7.5 Remaining Rust-server feature gaps (defer audit, 2026-06-26)

- [ ] **`$toHashedIndexKey` and the `$bitAnd` / `$bitOr` / `$bitXor` `$group`
  ACCUMULATORS are absent on both servers.** (The bitwise *expression* forms
  exist.) Not implementable blind: the mongod builds probed (7.0.12, and the
  builds available when this was last checked) reject the accumulator form with
  `15952 unknown group operator`, so there is nothing to probe against.
  Implement only once a mongod that accepts them is installed — check
  8.2.11 first.
  Backlog-ID: RM-124
  Verify: tests/backlog/test_rm_113_138.py::test_to_hashed_index_key
- [ ] **A deferred error path answers a generic `BadValue` (2) instead of
  mongod's code on the Rust server.** Where the Rust core returns
  `Fallback::Defer` on an error path, there is no Python behind it on the Rust
  server, so the client gets code 2 where the Python server raises mongod's
  Location code. Remaining examples: `$dateFromParts` component errors in
  `crates/secantus-core/src/expressions.rs` (`dfp_comp` and neighbours — the
  comments name Python's 40515 / 40516 / 40523). The worked template for closing
  a family is a standalone validator beside the operator
  (`update::arith_type_error`, `query::json_schema_keyword_error`) plus one arm
  in the adapter's code table, rather than widening `Fallback`.
  Backlog-ID: RM-126
  Verify: tests/backlog/test_rm_113_138.py::test_date_from_parts_error_codes
- [ ] **A DBPointer operand to the `$min` / `$max` UPDATE operators
  defers on the Rust server** (so it is an error there; mongod treats it as
  a no-op). A DBPointer nested in a range operand already matches.
  (`order.rs`, `bson_lt`.) DBPointer is a deprecated BSON type no supported driver
  emits; low priority.
  Backlog-ID: RM-127
  Verify: tests/backlog/test_rm_113_138.py::test_dbpointer_min_update_operand
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
  Backlog-ID: RM-128
  Verify: tests/backlog/test_rm_113_138.py::test_std_dev_pop_last_ulp
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

### 7.7 Rust storage: no close-time checkpoint / no `durable` mode

- **Intentional WT-config divergences from the Python server** (perf tunings,
  not risks): `file_max` 10MB vs 128MB (#575), Rust-only `eviction` threads
  (#575), Rust-only `block_compressor=zlib` (#578).

### 7.8 RecordId keying (step 1): pre-PR sharded stores are refused at open (no migration)

- **Decision — no in-place migration.** A store whose doc shards are in the
  pre-RecordId `SSu` format is refused at open
  (`reject_pre_recordid_doc_format`; test
  `open_rejects_pre_recordid_doc_shard`), because the Rust servers are pre-1.0
  beta with no upgrading users. If RecordId ever ships to users who must
  upgrade in place, THEN add a shard-generation migration (read each old `SSu`
  shard, mint RecordIds, write framed `SSq` rows into a fresh shard-table
  generation + the `_id` index, swap).

## The tail is CPU-bound, and 65% of the CPU is zlib (2026-08-22)

lz4 is the default block compressor for the document and oplog tables, and
the daemon takes `--block-compressor lz4|zlib|none` / `[storage]
block_compressor`. zlib must stay linked: `block_compressor` is recorded per
table at create time, so older stores have zlib tables
(`zlib_must_remain_available_for_legacy_tables`).

- [ ] **Per-table block-compressor default.** The documents and oplog tables
  have different read/write mixes, but the 2026-08-22 sweep measured them
  together and one setting applies to both. Measure them separately (the
  `SECANTUS_DATA_TABLE_EXTRA` / `SECANTUS_OPLOG_TABLE_EXTRA` hooks make each
  sweepable) and decide whether each wants its own default.
  Backlog-ID: RM-129
  Verify: none -- compressor performance measurement

## WiredTiger write volume against mongod: no like-for-like measurement yet (2026-08-21)

A comparison of WiredTiger's `bytes dirty in the cache cumulative` counter
between SecantusDB and mongod (enable with
`statistics=(fast),statistics_log=(wait=1,json=true)` via
`SECANTUS_WT_CONFIG_EXTRA` here, `--wiredTigerEngineConfigString` on mongod)
was void: that counter does not cover every table's writes (the documents
table wrote ~1x logical data while its cumulative-dirty counter read ~zero),
so it measured counter coverage rather than work. What survives:
`cache: bytes written from cache` shows the oplog and the documents table each
writing ~1x logical, ~2x total, agreeing with the WAL measurement. The
mechanism behind mongod's better tail at the same cache size is still
unexplained.

- [ ] **Re-do the comparison with a counter that covers both engines.**
  `cache: bytes written from cache` per table is symmetric and trustworthy on
  the SecantusDB side; the same counter on mongod's collection and oplog tables
  would give a like-for-like write-volume comparison. That is the measurement
  the 42x claim should have been built on.
  Backlog-ID: RM-130
  Verify: none -- write-volume measurement campaign

## The WAL is uncompressed: 2x logical data, and 22% of the tail (2026-08-21)

The Rust server's WiredTiger connection config sets
`log=(enabled=true,file_max=...,prealloc=false)` and names no compressor, so
the WAL is uncompressed (byte-identical WAL volume for random and repeated
payloads, 2.04x logical data), while mongod defaults to
`--wiredTigerJournalCompressor snappy`. Measured with `do-client --payload
random`, 8 writers, 1G cache, 8 KiB docs, medians of two passes:

| WAL | ops/s | p50 ms | p99.9 ms |
| --- | ---: | ---: | ---: |
| uncompressed (today) | 36,285 | 0.19 | 8.45 |
| `compressor=zlib` | 34,365 | 0.20 | **6.62** |

**-22% p99.9 for -5% throughput**, composing with `--oplog-async` (-24%) since
they attack different parts of the write path. Those numbers were taken under
the zlib block compressor; re-measure under the current lz4 default.

- [ ] **Configure a WAL compressor.** `zlib` is already linked into the
  vendored WiredTiger (`HAVE_BUILTIN_EXTENSION_ZLIB`) and is what the data
  tables use, so this is a connection-string change, not a build change.
  mongod uses snappy; zlib measured well here.
  Backlog-ID: RM-131
  Verify: none -- WAL compressor config/perf decision

## The 2GB WAL default costs 19x the disk — for up to 19% of throughput (2026-08-21, corrected 08-22)

`log_file_max` is 128MB by default since 2026-10-13 (it was 2GB); the
re-measurement on Linux (`bench/results/mdb-droplet/2026-10-13-log-file-max/`)
found no throughput difference between 2GB / 512MB / 128MB / 32MB.

- [ ] **64 writers got less than 16 from `secantusd-rs`** in that run
  (about 16,100 operations a second against 22,400, half inserts and half
  updates of 1 KiB documents), with the clients' CPUs under 5% busy. Not
  looked into.
  Backlog-ID: RM-132
  Verify: none -- writer-scaling throughput on droplets
- [ ] Re-check whether the full-document oplog entry can be trimmed, now that
  the WAL cost of it is visible (2x logical data).
  Backlog-ID: RM-133
  Verify: none -- oplog entry size design re-check

**Methodology note**: a bytes-on-disk measurement is only meaningful with
`--payload random`. `do-client`'s default payload is a repeated character,
which both engines compress to nearly nothing — a bytes-on-disk comparison on
it measures the compressor, not the engine.

## p99.9 write-tail: WiredTiger cache pressure, unthrottled (2026-08-21)

Investigated with `do-client --slow-ms`, which records every operation over a
threshold with its timestamp. Reproduces locally (macOS over loopback), so
every experiment is a free local run. There is no read tail; the write tail is
concurrency-driven (it jumps 7x between 2 and 4 writers while the median barely
moves, and large stalls hit all workers within the same few milliseconds), and
it is governed by cache headroom versus the rate dirty data fills the cache
(cache sweep: p99.9 9.28ms at 512M → 1.29ms at 8G with flat throughput; 256B
docs instead of 8 KiB: -96%). The 2026-08-22 profiling then found 65% of
server CPU in zlib `deflate`; the lz4 default (shipped) is what moved the
published tail.

### Ruled out by experiment

Each a 45-60s local run against an 8.26ms baseline (zlib era):

| suspect | test | p99.9 |
| --- | --- | --- |
| Checkpoints | `--checkpoint-seconds 3600` | no change |
| WT log preallocation | `log=(...,prealloc=true)` | no change |
| WT log file size | `file_max=1GB` vs 128MB | no change |
| **WT logging entirely** | `--data-nonlogged --oplog-nonlogged` | **no change** |
| Eviction thread count | 4 → 8 threads | +2% (none) |
| Eviction dirty thresholds | `dirty_target=5,trigger=10` | **+19% (worse)** |
| Eviction dirty thresholds | `dirty_target=1,trigger=5` | **+16% (worse)**, -19% throughput |
| Eviction updates thresholds | `updates_target=2,trigger=5` | **+16% (worse)** |
| Oplog pruning | effectively disabled | unchanged, but worst stall 126ms → 26ms |

### Admission control: prototyped, measured, and NOT the answer

`--write-tickets N` (`secantus-storage/src/admission.rs`, default 0 = off)
bounds how many writes are inside the storage engine at once. Measured: -22%
throughput to buy -12% p99.9. It relocates the queue rather than removing it.
**Decision: the flag stays off by default**, kept as a diagnostic and a
tail-predictability knob (with tickets the tail becomes perfectly repeatable);
it should not be enabled hoping for a faster tail.

### Where to go next

- [ ] **Measure write amplification** — bytes on disk per insert, SecantusDB
  versus mongod at a fixed document size. The leading hypothesis, and untested.
  Backlog-ID: RM-134
  Verify: none -- write-amplification measurement
- [ ] **`--oplog-async` as a default** — worth 24% of the tail and 42% of the
  stalls on its own, independently of the cache story. Every write currently
  passes through one process-wide `Arc<Mutex<OplogState>>`.
  Backlog-ID: RM-135
  Verify: none -- --oplog-async default decision
- [ ] **Cache defaulting** — the daemon caps at 4G; the sweep shows the tail is
  governed by cache headroom versus write rate, so the default deserves
  revisiting (and documenting) rather than being a fixed number.
  Backlog-ID: RM-136
  Verify: none -- cache-size default decision

Reproduce any of this with `do-client --slow-ms 5` (see `bench/DO_CLUSTER.md`
"Diagnosing a tail"). Relates to `tasks/wt-concurrency-plan.md`.

## Concurrent-writer contention (found by bench.concurrency, 2026-07-16)

- **Residual (Rust DDL statement transactions):** a DDL issued *inside* a user
  (multi-document) transaction bumps the DDL generation at statement end, not
  at the user-txn commit where its rows become visible — irrelevant today
  because the command layer never routes drop/rename through a user
  transaction (mongod prohibits them in transactions too).

## CI build cost

- [ ] **Vendored WiredTiger rebuild in CI and local builds — what remains.**
  The build-dir cache (`test.yml` "Restore the WiredTiger build", keyed on
  runner OS + image version + WT rev + config + interpreter, no
  `restore-keys`) and the `SKBUILD_CMAKE_DEFINE=Python3_EXECUTABLE=$(uv python
  find <ver>)` pin are live. Still open: Windows is unstabilised (venv pythons
  are copies, so the `REALPATH` stamp fix is a no-op), and the workflow's
  critical path is `storage-engine (windows-latest)`, which is excluded from
  the WT cache because `uv build --wheel` leaves no reusable build dir on
  Windows. LOCAL builds do not get the interpreter pin (it lives in the
  workflow), so a developer on a non-pinned Python pays the full ~38s WT
  rebuild per `uv sync`; the workaround is to export the same variable.
  Backlog-ID: RM-137
  Verify: none -- CI/local WiredTiger build cost

## Warning-class RUSTSEC advisories (surfaced by cargo-audit, not gated) (2026-07-20)

The `cargo-audit` CI gate (`.github/workflows/cargo-audit.yml`) fails only on
*vulnerability*-class advisories. These warning-class ones (unmaintained /
unsound) are printed in the gate log every run but do not block. None is a
reachable exploit; clear them opportunistically when the owning dep is next
touched:

- **RUSTSEC-2025-0134** — `rustls-pemfile` 2.2.0 unmaintained. Its API folded
  into `rustls-pki-types`. Still a direct dependency of `secantus-server` (and
  of the PostgreSQL crates).
- **RUSTSEC-2026-0196 / -0197** — `cgmath` 0.18.0 unmaintained + `swap_columns`
  UB. Pulled in transitively via the geo/spatial stack (`s2`). No fixed release;
  revisit if the geo dependency is ever swapped.

## Weekly validate.yml failures (2026-08-03 triage)

- [ ] **Rust server: a `drop` queued behind live writers starves
  indefinitely (lock fairness).** Observed during the 2026-08-11
  concurrency-report refresh. The bench harness now avoids the problem
  (fresh per-row collection names) rather than the server fixing it.
  Reproduce: run a multi-writer churn against one collection and issue `drop`
  on it mid-churn.
  Backlog-ID: RM-138
  Verify: none -- seen only under bench-level write load; a light-load test passes and proves nothing

## The macOS `test` lane wedges for its whole 30-minute cap, rarely (2026-09-18)

`test (macos-14, 3.12, 4)` on PR #1468 ran 35 minutes and was killed by the
job's `timeout-minutes` cap, having normally taken **3-5 minutes** on the six
preceding `main` runs. A re-run of that job on the **identical commit** passed
in 4 minutes, and `test-durable (macos-14, 3.12, 4)` -- the same shard, same
OS, same test selection -- passed first time on the same commit. So it is not
caused by the code under test; it is an intermittent wedge in that lane.

Nothing was diagnosable after the fact: GitHub keeps **no log for a cancelled
job** (`/logs` answers `BlobNotFound`), and the job's own "Upload faulthandler
crash dumps" step never runs because the cap kills the job before it. That is
the actual gap -- the wedge is invisible by construction, so the next
occurrence will be just as opaque.

The shape matches the one `test.yml` already names as the reason for the cap: a
daemon or thread not reaped (historically a rust-server `stop()` or a
change-stream tail), which `tests/conftest.py`'s faulthandler watchdog exists to
dump. Worth checking whether that watchdog's timeout actually fires INSIDE the
30-minute job cap on macOS; if it fires later than the cap, it can never
produce the dump it exists for.
