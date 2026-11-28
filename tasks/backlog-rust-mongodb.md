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

- **Scope decision — `$where` and `$function`** need a JavaScript engine.
  Neither server has one, and none is planned (WONTFIX).

## 2. Stopgaps (functional but with significant limitations)

These work end-to-end but cut corners.

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
- [ ] **`secantusd-mdb service install` has no Windows support.** On macOS
  it writes a launchd LaunchAgent and on Linux a systemd user unit; on
  Windows it refuses ("supports launchd (macOS) and systemd (Linux)"). A
  Windows service needs the Service Control Manager: the binary answering
  its start / stop control requests (the `windows-service` crate) or a
  wrapper, registered with `sc.exe create`.
  Backlog-ID: RM-730
  Verify: none -- needs the Windows Service Control Manager, and no backlog lane runs on Windows

## 3. Deferred work (skipped from a slice, ready to come back)

- [ ] **A PITR archive cannot prove that nothing was truncated between two
  segments.** The oplog's own mark of where it begins is mongod's, the
  `initiating set` entry (`oplog_reaches_genesis`); a segment's `prev` is 0
  for an oldest entry that is it, else the end of the newest segment in the
  archive, else `first - 1`. When the archive was off for a truncation (a
  restart without `--oplog-archive-dir`) and on again, the chain claims an
  abutment it cannot know, and a restore across the gap replays without the
  truncated entries. The marker that said so (`secantus_oplog_meta`'s
  `pruned` row) has no mongod equivalent and went with format 6. Options:
  (a) the archive records, in its own directory, the position its last
  segment ended at and each truncation made while it was off; (b) segments
  overlap by one entry, which proves abutment from the entries alone but
  keeps the newest archived entry in the oplog; (c) restore refuses a base
  older than the first segment unless the oplog reaches genesis. A decision
  for Joe.
  Verify: none -- a design decision about the PITR archive's chain; no mongod behaviour to compare
  Backlog-ID: RM-737
- [ ] **`--oplog-retention-seconds` (default 3600) truncates entries mongod
  would keep.** mongod holds its oplog to its size and a `minRetentionHours`
  floor; it has no age limit. This server also deletes entries older than the
  retention window whatever the size, so an idle store loses its oplog's
  history in an hour. Removing the default (or the knob) is a decision for
  Joe: `docs/recovery.md` and the change-stream pages describe it as the
  recovery window.
  Verify: none -- a design decision; the difference needs an hour of idle time to observe
  Backlog-ID: RM-738

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
      on both servers. `j` and `w: "majority"` are honoured as mongod honours
      them: the Rust MongoDB server syncs the journal before acknowledging
      such a write (the implicit default included), and a journal flusher
      syncs every 100 ms for the rest (fixed 2026-10-20). `wtimeout` remains
      accept-and-ignore: one node has nothing to wait for.
- **INHERENT (recorded): the C-driver gauge tests a single-node
      surrogate cannot pass.** `/change_stream/live/read_prefs`,
      `/Client/command_secondary`, `/Collection/aggregate/secondary` and the
      four `/Client/select_server/*` tests need a real SECONDARY (skipped,
      with the rationale, in `c_validation/include_paths.py::SKIP_TESTS`);
      `/Client/ipv6/single` hardcodes `mongodb://[::1]/` on the default port
      and never reads `MONGOC_TEST_URI`, so no server-side change reaches it
      (it still shows as the gauge's failures). Multi-node is out of scope.

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
- **`OP_COMPRESSED`** — no longer out of scope: the Rust server negotiates snappy, zstd and zlib since 2026-10-15; its `serverStatus` counters are RM-147.
- **Text search** (`$text`, `$meta: "textScore"`, text indexes) — would need a full-text index implementation.
- **Geo: exact mongod error-string matching** — chase work without a clear payoff unless a driver test pins exact wording.
- **`$where`** — runs JavaScript. We don't ship a JS runtime (confirmed by Joe 2026-10-16; MongoDB deprecated it in 8.0).
- **`$function`, `$accumulator`** (aggregation expressions) — same reason: both evaluate user-supplied JavaScript and need an embedded JS engine + sandbox + BSON↔JS shim layer. Would also require a `--javascriptEnabled` gate (mongod gates JS behind this; many prod deployments disable it). Adding the runtime would let us implement these three operators + `mapReduce` together for ~600–1000 LOC + a heavyweight binary dep (PythonMonkey / QuickJS / V8 — each with maintenance trade-offs). Most pipelines that reach for `$function` can be expressed in the existing aggregation expression library. **`lang: "python"` as a SecantusDB extension was considered and rejected (May 2026)**: would break the conformance contract (pipelines that work locally would explode in production with `Unrecognized lang value: 'python'`), and CPython's sandboxing story is worse than embedded JS engines. If a real user need ever surfaces, the right escape hatch is a server-side trusted-plugin registry — `{$secantusFunction: {name: "<pre-registered>", args: [...]}}` — not user code at query time.
- **`mapReduce`** — same JS-runtime dependency as `$where` / `$function` / `$accumulator`, and deprecated by MongoDB (removed from the Stable API in 5.0). **WILL NOT BUILD** (decided 2026-09-27). The canonical `emit(this.<field>, 1)` + `values.length` "count by field" pattern is translated to an equivalent `$group` aggregation; anything else, a named `out: "<coll>"` included, is refused as a `--noscripting` mongod 8.2.11 refuses it (31264, re-measured 2026-10-20). mongo-java-driver's `testMapReduceWithGenerics` writes to a NAMED output collection and so fails permanently; that failure is accepted rather than tracked.
- **Profiling: execution statistics in `system.profile` entries** — no longer out of scope: decided in scope 2026-10-16 (Joe), counted during execution; done in RM-023 (2026-10-27), and a profile entry's per-stage `execStats` in RM-359 (2026-11-07). A `$jsonSchema` profile filter still reads back as given, where mongod expands it.

## 5. Known bugs and edge cases to watch



- [ ] **OPEN — the Go gauge's two mongocryptd leaves: confirm how session
      support is advertised.** The gauge completes 659 of 659 and read 598 /
      12 / 49 = 98.0% on 2026-10-15 (`docs/validation-report-go-rust-server.md`).
      Its six failing leaves, as of 2026-10-20:

      | leaf | needs |
      | --- | --- |
      | `TestConnectionPoolBackpressure` | `$where` (the ingress rate limiter it sets is honoured since 2026-10-20) |
      | `TestSDAMErrorHandling/.../pool_not_cleared_on_timeout_network_error` | `$where` |
      | `TestSDAMErrorHandling/.../pool_not_cleared_on_context_cancellation` | `$where` |
      | `TestRetryableReadsProse/.../overload_errors_retried_on_a_different_replicaset_server` | a second replica-set member (non-goal) |
      | `TestSessionsMongocryptdProse/18.` | a `mongocryptd` binary on PATH |
      | `TestSessionsMongocryptdProse/19.` | a `mongocryptd` binary on PATH |

      The three `$where` leaves fail on a `mongod --noscripting` 8.2.11 the
      same way (measured 2026-10-20 for the backpressure test: 1 checkout
      failure of the 10 it wants) and are recorded as expected failures
      (`validation_summary/expected_failures.py`); no JavaScript engine is
      built (scope decision, Joe 2026-10-16). What is left: the two
      mongocryptd cases need a server that reports NO
      `logicalSessionTimeoutMinutes` (the mongocryptd shape). Likely out of
      scope with CSFLE, but how this server advertises session support is
      its own, so confirm it with a mongocryptd binary rather than assume.
      Reproduce: `cd vendor/mongo-go-driver && MONGODB_URI=mongodb://127.0.0.1:<port>
      go test ./internal/integration/ -run '<TestName>' -timeout 60s -count=1 -v`
      against a daemon WITHOUT `--standalone`.
      Backlog-ID: RM-008
      Verify: none -- needs a mongocryptd binary (MongoDB Enterprise) to compare with

- **Scope decision — no server-side JavaScript** (Joe, 2026-10-16,
  formerly RM-009). `$where`, `$function`, `$accumulator` and `mapReduce`
  are refused as `mongod --noscripting` 8.2.11 refuses them (6108304 in a
  query, 2 in an aggregation `$match`, 31264 for mapReduce, and 31264 for
  `$accumulator` once a document reaches it -- over no documents that mongod
  succeeds, and so does this server, measured 2026-10-22); MongoDB
  deprecated all four in 8.0. Pinned by
  `tests/test_mongod_differential.py::test_where_refusal_matches_a_scriptless_mongod`
  / `::test_where_in_aggregate_match_is_a_context_error`. The three Go
  leaves that need a `$where` to run (the two `TestSDAMErrorHandling` cases
  and `TestConnectionPoolBackpressure`, whose rate limiter is honoured since
  2026-10-20) are expected failures.

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
      **Decided (Joe, 2026-11-16): make the weekly cron's failure loud (it notifies) AND add one extra Linux Python to PR runs; not the full 3.13 matrix.**
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
      **Decided (Joe, 2026-11-16): run the concurrency benchmark on dedicated-CPU droplets, created for the run and destroyed after.**
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
- **Scope decision — the order of a wrong-type error's expected types**
  (this was RM-465, 2026-11-12). mongod's order is not one it keeps: in one
  8.2.11 build it differs per IDL file (`FindCommandRequest.*` and
  `getMore.*` "[decimal, double, long, int]", `create.*` "[int, double,
  decimal, long]", `count.*` "[int, decimal, double, long]",
  `cursor.batchSize` "[double, decimal, long, int]", `aggregate.maxTimeMS`
  "[double, int, long, decimal]"), and it moves between patch builds
  (8.2.1, 8.2.11 and 8.2.12 all differ; CI installs the latest 8.2). The
  SET matched mongod on every numeric slot probed, so the list is compared
  as a set (`tests/mongod_reference.py::sort_type_lists`), as
  `test_wrong_type_lists_mongods_set_of_types` now does.

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
- [ ] **Execution admission control's ticket points and reports are not
      all mongod's.** mongod 8.2.11 admits every operation through its
      ingress and execution admission control. This server has mongod's
      ingress admission control since 2026-11-08
      (`secantus-commands` `admission/ingress.rs`;
      `tests/mongod_live/test_ingress_admission.py`) and its execution
      control's ticket pools since 2026-11-09 (`admission/execution.rs`, a
      port of `TicketHolderManager` and `ThroughputProbing`;
      `tests/mongod_live/test_execution_admission.py`): the read and write
      pools of the gradual resize policy, sized by throughput probing (twice
      the logical cores, split 0.5) or fixed by a concurrency given at
      startup, the sixteen parameters, `serverStatus.queues.execution` with
      its `monitor`, `globalLock.currentQueue` counting the operations
      queued in them, and a ticket taken with each owner's global lock
      (`lock_manager`, mongod's `Locker::_acquireTicket`), returned between
      a transaction's statements and taken again with mongod's
      `reacquireTicket` conflict check. Since 2026-11-09 the `Slow query`
      line prints `queues.execution` and `delinquencyInfo`, and the line
      and profile entry `storage.timeWaitingMicros.storageExecutionMicros`
      (the global IS the line reads its statistics under takes the
      admission mongod's does; compared operation by operation in
      `tests/mongod_live/test_slow_query_lines.py`). Since 2026-11-12
      `serverStatus.queues.ingressSessionEstablishment` and `connections`'
      `queuedForEstablishment` and `establishmentRateLimit` report the
      connection establishment rate limiter (`admission/rate_limiter.rs`, a
      port of mongod's `RateLimiter`), and a `$currentOp` row its
      operation's `queues` and `currentQueue`
      (`tests/mongod_live/test_ingress_admission.py`). Open:
      - where this server's ticket points differ from mongod's: a yield here
        keeps its locks, so it keeps its ticket and takes no second
        admission (mongod returns both and takes them again, RM-360).
        Since 2026-11-17 an `update` or `delete` statement takes its round
        as it runs and lets it go before its line reads its statistics, as
        mongod's does; a retryable one still holds every round to the end
        (RM-357's session row);
      - mongod's first retryable write after startup takes a second,
        exempt, ingress admission (not traced), measured 8.2.11
        2026-11-08;
      - an index build: mongod 8.2.11 (one-member replica set, measured
        2026-11-18 under `hangAfterStartingIndexBuild`) shows TWO rows, the
        `IndexBuildsCoordinatorMongod-0` thread's (`queues.ingress`
        admissions 0, not holding; `queues.execution` holding a ticket,
        admissions counting its yields, 139 for 1,000 documents; locks
        `w` on transition, global, database and collection) and the
        client's `createIndexes` (`ingress` 1, holding; `execution` 4, not
        holding; `locks: {}`). This server builds in the command and has no
        `configureFailPoint`, so neither row can be caught yet; a gap test
        needs that failpoint (or a build slow enough to observe) first.
      **Decided (Joe, 2026-11-08): build mongod's execution admission control in full, the faithful option: the ingress controller, then the read and write ticket pools with `throughputProbing` (replacing `admission.rs`'s opt-in write tickets), then the per-command admission counts, so the concurrency cap is on by default as on mongod. Several PRs, in that order.** The ingress controller landed 2026-11-08, the ticket pools 2026-11-09, and the per-command admission counts on the line 2026-11-09; what is left is above.
      Backlog-ID: RM-355
      Verify: none -- the yield's second admission is RM-360's; the second exempt ingress admission and an index build's `queues` have no gap test yet
- [ ] **mongod's `validate` sometimes warns on `config.image_collection`'s
      `_id_` after this server served a mongod-written store.** In
      `test_secantus_serves_the_phase_0b_store_mongod_wrote` (RM-723's
      regression test) mongod 8.2.11 reopens the store `secantusd-mdb`
      wrote to and `validate({full: true})` on `config.image_collection`
      answered `valid: true` with warnings `["Found warnings in _id_"]`
      once in the RM-650 lane gate (2026-11-23, load ~6 on 12 cores); the
      gate before it and two isolated reruns passed. A warning on an index
      this server maintains (retryable `findAndModify` images) is a storage
      signal: capture the full `validate` reply (its `keysPerIndex`,
      `indexDetails` and the mongod log's index-entry lines) on a failing
      run and find which key is missing or extra. Found 2026-11-23 working
      RM-650, whose change does not reach storage.
      Measured 2026-11-23 (RM-765 lane, mongod 8.2.11, debug `secantusd-mdb`):
      NOT reproduced in 192 further runs of that test (12 serial; 40 and 60
      in 4 and 6 parallel loops at load ~12; 80 in 8 loops beside 4 CPU
      burners), nor in 40 restarts of a store only mongod wrote with
      `validate({full: true})` of `config.image_collection`,
      `config.transactions` and `app.c` three times after each step-up, nor
      in ~130 validates of them beside a loop of `fsync` (checkpoints). What
      is known: the scenario writes no retryable `findAndModify`, so
      `config.image_collection` has `nrecords: 0`, `keysPerIndex: {_id_:
      0}` in every run measured, and this server writes a record there only
      for a retryable `findAndModify` or a replay of one
      (`replay_session_state`), so no key of ours can be missing or extra in
      an empty table. The same warning, with "transient issue", is what
      mongod gives a table it is using as validate runs
      (`_BUSY_WHILE_MONGOD_RUNS`). The test now returns the whole `validate`
      reply with a failing finding, so the next failure shows its
      `warnings`, `keysPerIndex` and `nrecords`: if `nrecords` is 0 and the
      warning is "Found warnings in _id_" alone, it is the busy-table case
      and `config.image_collection` belongs in that set; a non-empty table
      or an `extraIndexEntries` / `missingIndexEntries` is ours.
      Verify: none -- intermittent (1 run in ~200), so no gap test fails
      reliably; RM-723's regression test, named above, shows it when run in
      a loop
      Backlog-ID: RM-765
- [ ] **Some writes' `locks` are not mongod's.** Since 2026-11-07 a write
      counts mongod's flow control tickets (one per global IX taken apart)
      and a retryable write's session checkout and session row, and a
      journal wait (`j`, `w: "majority"`) its transition lock and global
      IS, so an insert, `drop` or `create` reads as mongod 8.2.11's; since
      2026-11-12 a command that took the global lock in IX and logged
      nothing (`create` of a collection there, `drop` or `dropIndexes` of
      one not there) reads the latest optime for its last op as mongod's
      entry point does (`lastop::entry_point_fixes`); since 2026-11-16 a
      `collMod` of a collection there takes mongod's global IS and its
      collection X twice; since 2026-11-17 a `findAndModify` that removes
      takes one round, and a retryable one writes its image under the
      transition, global and image collection IX; since 2026-11-18 a
      profiled update or delete statement's entry counts mongod's
      `Metadata: {W: 1}` and global, database and collection IX in its
      command's locks; since 2026-11-18 a retryable `update` or `delete`
      statement takes its own round as it runs and keeps its session's row
      under it; since 2026-11-18 an `$out` on a replica set's primary takes
      its four global IS under the transition lock, and the transition lock
      alone once for each X round after the first; since 2026-11-18 a
      collectionless `aggregate`'s `$lookup` and `$unionWith` take the
      global IS twice for each read of their collection, under no lock of
      the command's; since 2026-11-27 a failed batch's one-at-a-time
      rounds match, and since 2026-11-28 an `insert` of more than 50
      documents takes mongod's read round for the index count that tunes
      its batch size, and a `collMod` of a collection not there takes the
      same rounds as one of a collection there; since 2026-11-28 a
      transaction takes its reads' locks in IX and its statements' profile
      entries count what the transaction took for each, and a collectionless
      `aggregate`'s entry is compared with mongod's. Still different,
      measured against a one-member replica set:
      - an `insert` whose batch fails counts an interrupt check for each
        lock on mongod; this server's line prints no `numInterruptChecks`
        (RM-360);
      - a profiled `update` or `delete` statement's entry's locks are taken
        without interrupt checks here (the command's `numInterruptChecks`
        is mongod's without them);
      - each global lock taken apart takes an execution ticket, so where
        these locks differ the line's `queues.execution` admissions differ
        too (RM-355);
      - a collectionless `aggregate` whose `$lookup` or `$unionWith` reads
        a collection: its line's execution admissions are not compared yet
        (RM-355).
      Backlog-ID: RM-357
      Verify: none -- the interrupt checks are RM-360's and the execution admissions RM-355's
- [ ] **Some queries have no `queryShapeHash`.** Since 2026-11-07 a
      `find`'s, `distinct`'s and `aggregate`'s line, profile entry and
      `explain` carry mongod's `queryShapeHash` (`secantus-commands`
      `query_shape.rs`, `tests/mongod_live/test_query_shape_hash.py`,
      against mongod 8.2.11), `$jsonSchema` among them since 2026-11-16,
      and a `$lookup` from `local` / `config`, `$out` to a time-series
      collection and every change stream flag since 2026-11-17. A query
      with a `$rankFusion` or `$scoreFusion` input of `$search` or
      `$vectorSearch` prints none, where mongod prints one; those stages
      need mongot, so mongod's serializer for them is unmeasured here.
      Backlog-ID: RM-358
      Verify: none -- the mongot inputs need a mongot
- [ ] **A spilled SORT of index keys compresses to a different size.**
      Each member mongod 8.2.11 spills carries the id of the storage
      snapshot it was read in, drawn from a process-wide counter every
      storage transaction advances; this server writes 0
      (`sorter::SNAPSHOT_ID`), and its `explain` writes each document's
      place in the collection as its RecordId (`exec.rs`'s `key_members`). The uncompressed bytes agree, but
      `spilledDataStorageSize` does not: 29061 against 31229 for 2000 int
      members over `{y: 1, x: 1}` under a 100000-byte budget, and mongod's
      own answer moves between runs of the same data (31229, 31231, 31240
      measured 2026-11-12). A SORT over a covered projection's OWNED_OBJ
      members is the same: 34936 against mongod's 36220 for 2000 `{x}`
      documents (42497 for `{x, y}`; RM-473). Found 2026-11-12 (RM-475).
      **Decided (Joe, 2026-11-13): model the counter.** Keep mongod's
      process-wide snapshot id, advanced by every WiredTiger snapshot this
      server opens since start, and write each spilled member's own
      snapshot's id where `sorter::SNAPSHOT_ID` writes 0. It cannot equal
      a given mongod's, whose background threads advance it too, so the
      gap test asserts what the mechanism gives: members spilled after more
      snapshots carry larger, non-zero ids, and `spilledDataStorageSize`
      falls within mongod's own run-to-run spread of its answer.
      Verify: none -- mongod's own answer varies between runs; no test can pin it
      Backlog-ID: RM-556
- [ ] **Some lines have no `numInterruptChecks`.** Since 2026-11-08 this
      server checks for interrupt where mongod 8.2 does, and counts it
      (`secantus_core::deadline::check_for_interrupt`): at dispatch, at
      each lock it is granted (a re-take by its holder included, as
      mongod's `Locker::_lockBegin` counts one), once per index scan of its
      one plan and once when planning is done, in the express executor
      (none for IDHACK, which mongod builds without its planner), in a
      `getMore` once it has its cursor, as a cursor left open is checked
      back in, at the lock it takes to read the line's storage statistics,
      and at each yield (two: the yield's, and the locks it takes again),
      the per-document polls having become mongod's yield every
      `internalQueryExecYieldPeriodMS` (10 ms, a parameter here too). Since
      2026-11-09 a write checks as mongod's does as it finishes: a
      retryable write's session check-out, the reads of the system's
      latest optime for the client's last op that a write which logged
      nothing makes (`lastop.rs`), and its write concern's waits (the
      oplog's holes, the journal flush, the commit point). Since
      2026-11-12 an `aggregate` mongod's query layer runs whole checks as
      a `find` does, and one through its cursor stage checks for each
      buffer the stage fills (the global IS it takes for it, so a `getMore`
      of such a cursor takes no lock of its own) and ticks the pipeline's
      interrupt checker once per stage `getNext`, the checker going on
      with the cursor from batch to batch. Since 2026-11-16 a `$map`,
      `$filter` or `$reduce` checks after every tenth element of each
      evaluation, as mongod 8.2.11's does, and leaves the pipeline's checker
      alone. Since 2026-11-17 a `$sort` alone after a `$group` SBE runs
      counts the cursor stage's buffers (each a global IS) and its own and
      the cursor stage's `getNext`s, and its `getMore`s go on counting
      with no lock of their own; so, since the same day, do a `$count`,
      `$sortByCount` and `$bucket` SBE runs (the `$group` mongod expands
      each into, with the `$project` or `$sort` after it left to the
      pipeline), and a `$switch` of 50 branches or more (a `$bucket` of 50
      buckets or more) keeps its `$group` classic, as on mongod. The line of a `find`, `count`
      or `distinct` of a collection with one candidate plan, a `getMore` of
      a `find`, an `aggregate` of a collection mongod's query layer runs
      whole, a `$sort` after an SBE `$group`, or (since 2026-11-18 asked for a first batch of none too, the `$sort` after a `$group` included) whose stages after its cursor stage are `$project`,
      `$unset`, `$addFields`, `$set`, `$replaceRoot`, `$replaceWith`,
      `$unwind`, `$skip` and (since 2026-11-18, with a cursor stage over a
      plan with a limit filling one buffer to it) `$limit`, and the `getMore`s of its cursor,
      `listIndexes`, `listCollections`, `collStats`, `ping`, `buildInfo`,
      and an `insert`, `update` or `delete` on a member of a replica set
      prints mongod's count (99 shapes in
      `tests/mongod_live/test_interrupt_checks.py`). Every other line
      prints none, where mongod prints one:
      - a read with more than one candidate plan, a plan from the plan
        cache, or a subplanned `$or`: mongod checks once per index scan of
        every plan its `PlanEnumerator` builds (with indexes on `x` and
        `y`, `{x: {$gt: 1}, y: 1}` checks 8 times on mongod: four leaves,
        its index intersections' included; three indexes 13) and not at
        all for a cached plan, and this planner's candidates are not its
        enumeration;
      - the other aggregates (since 2026-11-19 a `$sort` closing stages
        that pass one document on for each, `$addFields`, `$set`, `$unset`,
        `$project`, `$replaceRoot`, `$replaceWith`, over a classic read is
        counted, since 2026-11-27 with a `$limit` after the `$sort`, and
        since 2026-11-28 a classic `$group` or `$count` closing such
        stages): a `$group` or `$count` with stages after it, a `$sample`
        (4), one with a `$match` or `$redact` left after
        its read, a `$lookup`,
        `$unionWith`, `$facet`, `$geoNear` or `$out` / `$merge`, a `$group`
        run classic over the cursor stage (a `$bucket` of 400 buckets: 16
        and a `getMore` of 4 on mongod), one over a
        view or a time-series collection;
      - `findAndModify`: counted as the other writes are, it matches mongod
        8.2.11 but where its locks do not (RM-357: a retryable one's image,
        19 to this server's 16; one that removes, 7 to 10), and an IDHACK
        upsert counts 10 to mongod's 9; it prints none until all do;
      - an `insert` of several documents one of which fails (RM-357's
        rounds: an unordered pair with a duplicate key first, 27 to 19);
      - a write whose write concern waits in a way not modelled: `fsync`,
        a numbered `w` above 1, a tag, `w: 0`, or `majority` with `j:
        false` (mongod 8.2.11 counts 13 for it, as for `majority`, and
        takes the oplog's read locks, which its source's `NONE` sync mode
        does not explain);
      - DDL (`create` 13, `drop` 17, `createIndexes` 73, measured
        2026-11-07): their write units and index build phases check where
        this server has not been made to; the coarse `deadline::check_now`
        polls of the pipeline, the index build and `findAndModify` are this
        server's own, uncounted; `bulkWrite`;
      - a statement inside a transaction, a lock granted after a wait
        (mongod checks at each wake), and the commands not probed.
      A yield here counts the re-take of the statement's locks without
      releasing them, so a request waiting on them is not let in by the
      yield as mongod's is. `includeQueryStatsMetrics` needs the count too
      (RM-170). The remainder follows the decision recorded on this item in
      e01df2d2 (2026-11-08), each command checked against a probe of its own.
      Backlog-ID: RM-360
      Verify: tests/backlog/test_rm_355_364.py::test_every_line_counts_its_interrupt_checks
- [ ] **Operations this server runs another way than mongod's plan say
      less.** Since RM-023 a read or a write's read that a stream runs as
      mongod's plan counts its keys and documents examined, and since
      RM-359 its profile entry carries its stages (`execStats`); these do
      neither:
      - a read two or more plans could serve, which mongod chooses by
        trial, other than a `find`'s or an `aggregate`'s: an `aggregate`
        that groups what a `$match` and a `$skip` or `$limit` read (in SBE
        the entry's reads are the trial's to that limit, and the counted
        read does not yet bound its trial by it; RM-340's test pins the
        plan), a `count`'s, a
        `distinct`'s, a write's. Its documents come from the store's
        router. mongod counts what the winner examined in its trial, and
        since RM-349 a `find` and an `aggregate` (one that streams, sorts
        or, since 2026-11-17, groups what its `$match` reads) run the
        winner after its trial's buffer and count as mongod's do; the
        other readers still decline a contested plan
        (`order::stream_plan`);
      - a read of a time-series collection made whole: under a
        `$group` (since 2026-11-18 a `$group` on the
        meta field that only counts, which mongod runs over the buckets,
        counts as mongod's does, and since 2026-11-18 one on a constant,
        `count_documents`'s, a `$count` stage, and since 2026-11-27 a `$group`
        after a `$match` on a measurement field, which mongod runs in SBE,
        and since 2026-11-28 one after a `$match` on the meta field, which
        mongod reads through the bounded scan of its index over the buckets,
        keys counted), an inclusion `$project` (RM-456), a hint or a
        collation. mongod
        counts the buckets its cursor stage buffered (five of five for a
        sorted `find`). Since RM-455 a `find` is the aggregation mongod
        converts it to, and counts where a time-series `aggregate` streams,
        and since 2026-11-17 under a `$sort`;
      - a `$near` or `$text` read (since 2026-11-28 a `find`'s
        `$geoWithin` / `$geoIntersects` counts what `explain`'s run of its
        plan examines, as mongod's does; a `$near` limited to 3 over a `2d`
        index: mongod 38 keys, this server 37).
      Measured 2026-10-27, 2026-11-07, 2026-11-10, 2026-11-13 and 2026-11-28.
      Backlog-ID: RM-363
      Verify: tests/backlog/test_rm_355_364.py::test_a_near_read_counts_what_it_examined
- [ ] **WiredTiger 11's interruptible cache wait is not on the line.**
      mongod 8.2.11's WiredTiger splits a cache wait into the share spent
      in interruptible eviction (`timeWaitingMicros.cacheInterruptibleMicros`)
      and the mandatory rest; the one vendored here (mongodb-7.0.33) has no
      interruptible eviction, and this engine counts the whole wait in
      `cacheMicros`. Porting it means WiredTiger 11's `interruptible` path
      through application eviction (`__wt_evict_app_assist_worker`,
      `__evict_check_user_ok_with_eviction`) and mongod's interrupt
      callback, not a counter. Measured 2026-10-27 (eviction under a 256 MB
      cache). (Its other half, `txnNumUpdates`, closed 2026-11-07.)
      Backlog-ID: RM-364
      Verify: none -- no test drives mongod's eviction into an interruptible wait on demand
- [ ] **mongod's startup lines this server does not write.** Since RM-302
  the log is mongod's structured log, and since 2026-11-13 this server
  starts it as mongod 8.2.11 does on `initandlisten`: `MongoDB starting`,
  `Build Info` (23403), `Operating System` (51765), `Options set by command
  line` (21951), the
  lock file's `Detected unclean shutdown` (22271), `Opening WiredTiger`
  (22315), WiredTiger's own messages under it (22430, since 2026-11-16:
  the store opens with mongod's `json_output` / `verbose` configuration),
  `WiredTiger opened` (4795906), `WiredTiger recoveryTimestamp` (23987,
  since 2026-11-17), `WiredTiger session cache max value has been set`
  (9086700, since 2026-11-18), the spill engine's `Opening spill
  WiredTiger` (10158000), its own 22430 messages and `Spill WiredTiger
  opened` (10158001, since 2026-11-18: the engine opens at startup), the
  durable catalog's `Initializing durable catalog` (9529901), `Retrieving
  all idents from storage engine` (9529902) and `Initializing all
  collections in durable catalog` (9529903, since 2026-11-18), the spill
  engine's `Dropping spill idents` (10682200, since 2026-11-28), `Flow
  Control is enabled on this deployment` (20536, since 2026-11-28) and,
  last, `mongod startup complete` (8423403). mongod writes about fifty
  more there that this server does not: the network lines, the spill
  engine's shutdown lines (10158003, 10158006, 10158007), the cluster
  server parameters line (6608200), FCV and FTDC lines (measured 2026-10-24,
  2026-11-07, 2026-11-10, 2026-11-13 and 2026-11-16). Two limits on the
  22430 lines: they are held by the connection and written together once
  the open returns, not as WiredTiger writes them, and a message written
  after the open (a checkpoint's or compaction's progress, the shutdown's
  recovery lines) is held (up to 4096) but never logged, where mongod logs
  each. The vendored WiredTiger 7.0 writes four on a new store where
  mongod's newer engine writes fourteen. Found with RM-302.
  Backlog-ID: RM-305
  Verify: tests/backlog/test_rm_001_028.py::test_startup_reports_the_topology_version_observer_after_flow_control
### 7.091 mongod's slot-based engine (SBE) -- 2026-10-23

mongod 8.2 runs the front of a pipeline in its slot-based engine when it can
push a `$group` or `$lookup` down, and SBE answers its own way. The Rust
server follows it (RM-280, closed 2026-10-23): `secantus-core/src/sbe.rs`
ports the pushdown rule of `sbe_pushdown.cpp` under `trySbeRestricted` and
the stage builder's build-time checks, `expressions/sbe_ops.rs` gives each
operator SBE compiles SBE's checks, codes, messages and values
(`gen_expression.cpp` and the VM builtins), and `group.rs` keeps the
N-accumulators as SBE and the classic engine each keep them (`stl.rs` is
libstdc++'s heap and sort). Probes: `tools/probes/expression_shapes.py --sbe`
(3,360 shapes) and its `expression_shapes_sbe.py` corpus (3,614 more); both
leave only differences listed here or in other items. Regression tests:
`tests/backlog/test_rm_sbe.py`.

- **Scope decision — the order of SBE's set expressions.** `$setUnion`,
  `$setIntersection` and `$setDifference` in SBE return their elements in
  the order of an `absl` hash set, which mongod seeds per process: the same
  expression over the same document gave `[3, 'x', 2, 5, 4, 1]` and
  `['x', 2, 4, 1, 3, 5]` on one mongod 8.2.11 (2026-10-23). No fixed order
  can match it; this server keeps the classic engine's sorted order, which
  is one SBE can give. The same holds for the accumulators (section 7.090).
- **Scope decision — ties in SBE's heaps are libstdc++'s.** SBE keeps
  `$topN`, `$bottomN`, `$maxN` and `$minN` in a `std::push_heap` /
  `std::pop_heap` heap and sorts it with `std::sort`, so which of several
  equal values survive, and their order, is the C++ library's. The Linux
  build (libstdc++) is the exemplar and is what this server follows; the
  macOS build (libc++) breaks those ties its own way, so
  `test_n_accumulator_ties_follow_the_heap` compares with mongod on Linux and
  with a model of libstdc++'s heap elsewhere. A libc++ model reproduced the
  macOS mongod 8.2.11 on 1,500 of 1,500 cases.
- **Scope decision — a `$dateFromString` message that is not UTF-8.** On a
  string with multi-byte characters mongod's "Error parsing date string"
  message splits them byte by byte and sends bytes that are not UTF-8, which
  pymongo cannot decode; this server sends the same text as valid UTF-8.


### 7.090 `$group` code no test had executed, against mongod -- 2026-10-22

The 2026-10-21 coverage run left 455 lines of `secantus-core/src/group.rs`
unexecuted: the accumulators' corners, `$median` / `$percentile`,
`$mergeObjects`, the N-accumulators' arguments, `$bucket` and `$bucketAuto`.
`tools/probes/group_shapes.py` was written against them. On `main`, 43 of
its first 245 cases differed from mongod 8.2.11. The batch that wrote it fixed
five causes (RM-275 to RM-279, regression tests in
`tests/backlog/test_rm_group_shapes.py`), the next closed RM-281, and the one
after that RM-280 (section 7.091). Of the probe's 282 cases, one differs:
`$accumulator`, which a mongod with scripting runs and this server refuses by
design (Scope decision, above).

Not a divergence: the ORDER of `$addToSet` / `$setUnion` elements, and of
equal counts after `$sortByCount`. mongod takes them from a hash table and
does not fix it: the same pipeline on the same mongod gave each of two
orders, 9 and 11 times in 20 runs (8.2.11, 2026-10-23). The order this
server returns is one mongod gives, and the probe compares them as sets.


### 7.088 Expression code no test had executed, against mongod -- 2026-10-21

A line-coverage run (`docs-rust/coverage.md`, `./inv rust-mdb-coverage`)
found ~2,000 lines of `crates/secantus-core/src/expressions.rs` that no test
suite executed; the generic `tools/probes/agg_expressions.py` sweep reached
about half of them, and none of what it reached diverged beyond the
authorised Decimal128 last digit. The rest sits behind STRUCTURED arguments
the generic sweep cannot build (`op_switch` had never run at all), so the
cases in `tools/probes/expression_shapes.py` were written against those
lines and run against mongod 8.2.11 on macOS arm64. Each item below is one
root cause; its gap test in `tests/backlog/test_rm_found_2026_10_21b.py`
holds every shape measured for it and asks mongod live. Four more were
fixed in the same batch, all in the comparison code every query shares:
BinData order (RM-232) and JavaScript-with-scope as its own type (RM-233),
which moved the index-entry format to 5, and MinKey / MaxKey equality
(RM-234) and field order in document equality (RM-235). On 2026-10-22 a
second batch took the "not supported" refusals out of the expression engine
(in the harness, 207 refusals became 3) and closed RM-238, RM-240,
RM-241, RM-242, RM-247 and RM-250, and parts of RM-246 and RM-252. A third
batch the same day closed the silent wrong values RM-236, RM-237, RM-239,
RM-243, RM-244 and RM-248 (in the harness, 98 wrong values became 34).
RM-249 followed: the server now answers time-zone questions from mongod's
own timezonedb (`crates/secantus-core/tzdata/`), and `tools/probes/time_zones.py`
finds no difference on any of 601 names.
A fourth batch closed RM-246 and RM-252 and part of RM-251, and filed and
fixed RM-255 to RM-257 (in the harness, 63 wrong codes became 13); RM-258
was filed open.
RM-251 closed after it: constant sub-expressions are folded bottom-up in
every stage, as mongod folds them.
RM-245 closed after that: the Decimal128 transcendentals now differ from
mongod only in the authorised last digit.
RM-253 closed after that: `$bucket` parses, places and orders as mongod's.
RM-258 closed last: a no-colon `HHMM` time is ten seconds early on mongod,
whose timelib leaves the microseconds unset, and here too.


### 7.087 Window functions and the operator tail against mongod -- 2026-10-21

The first comparison of `$setWindowFields` / `$fill` and of a tail of
operators no CI test had covered with mongod 8.2.11
(`tests/mongod_live/`), change streams among them. It found six
divergences; two were fixed in the
same batch: `$shift` answering null for a MISSING output instead of its
default (RM-227), and a lowercase `collmod` alias mongod does not have
(it answers 59). Each left is a
`Fallback::Defer` in `crates/secantus-core/src/windowfields.rs`: with no
Python behind the Rust server, a deferral answers 2 "aggregation pipeline
uses a stage or operator not supported by the Rust server". The file has
about forty such points; these are the ones measured.

- [ ] **A write of many through some index reads of a clustered
      collection reads every candidate before it writes one.** mongod
      writes each document as its plan returns it, whatever the plan, so an
      updateMany or deleteMany out of `maxTimeMS` part-way has written some
      (mongod 8.2.11, 100,000 documents, `maxTimeMS: 30`). A plan this
      server's write cannot stream -- a clustered or time-series
      collection, a geo plan -- leaves the write to the store, which reads
      a collection scan (since 2026-11-10), a one-field bounded index read
      (since 2026-11-17), and a filter of operator clauses no index covers
      whole through one field's bounded read, a compound index's included
      (since 2026-11-18), and one field's `$in` / `$eq` of plain scalars
      (since 2026-11-18), a page at a time, writing each page first. The
      router's other index reads (`try_index_id_keys`: an equality prefix
      of a compound index, a plain equality `{a: 5}`, an `$in` holding a
      null, document, array or regex, `$and`, `$or`, `$elemMatch`, geo)
      are still read whole before the first page, and a budget spent
      there writes nothing. The work is to stream a clustered collection's
      plan as an ordinary one's (its natural order is `_id`'s, which the
      stream does not yet read), or to page each of those reads into the
      write as `bounded_span_pages` does; none of those shapes is probed
      against mongod yet.
      A `backlog_fixed` RM-390 test regressed once in an RM gate at high
      load (2026-11-23), unrecorded as to which. Not reproduced: 44 loops
      of all nine tests (four in parallel, load up to 54, debug build, mongod
      8.2.11), none failing; mongod alone wrote 5,210-9,064 of 100,000
      (30 ms, 25 repeats, load ~27) and stopped within 68 ms; this server
      always wrote 1,000 (one page, 153-185 ms, deadline checked per
      page), so the one-index shapes cannot write nothing. The suspect is
      the contested pair (a plan trial of about 30 ms before the first
      write, against 150 / 400 ms), already widened twice for CI load. The
      tests now put the plan, budget, written count, elapsed time and
      load average in their assertion message; read that on the next
      failure before changing a budget.
      Backlog-ID: RM-390
      Verify: none -- the compound residual and `$in` reads this item's
      gap tests measured are paged
      (tests/backlog/test_rm_found_2026_11_10_rm390.py, now backlog_fixed); the remaining shapes have no gap test until one
      is probed against mongod 8.2.

### 7.086 The Rust MongoDB server, audited -- 2026-10-20

An audit of the command, expression and wire layers, each finding reproduced
against mongod 8.2.11 (standalone, or a one-member replica set where it
needs sessions) with a `secantusd-mdb` built from tree `af2629ec`. The gap
tests are in `tests/backlog/test_rm_found_2026_10_20b.py` and ask mongod
live. Not filed here: `$rankFusion` / `$score` / `$scoreFusion` (RM-198),
`j: true` durability (RM-186), `storageSize` (RM-040).

- **Scope decision — a snapshot read at a past time needs a store whose
  data tables are not logged** (Joe, 2026-10-25). WiredTiger keeps no
  history for a logged table, so a store created with `--data-logged`, or by
  the PostgreSQL server, refuses `atClusterTime` before the newest time with
  246 SnapshotUnavailable, by name, rather than answer as of now; without
  `atClusterTime` it reads the latest data, which is correct. `secantusd-mdb`
  creates its stores the other way, mongod's (RM-210).
- [ ] **A spill the spill engine writes out takes this server's size on
      disk, not mongod's.** A `$setWindowFields` spills to a table of a
      second WiredTiger instance at `<dbpath>/_tmp/spilldb`, as mongod 8.2's
      does (RM-325's fix), and while the table stays in that engine's cache
      both report the 4096 bytes WiredTiger creates its file with. Past
      about 14 MB mongod writes pages out, the same every run: 73728 bytes
      for 28 MB, 204800 for 60 MB (mongod 8.2.11, 2026-11-13). The tables
      are zstd at level -7 on both sides since 2026-11-27 (the open config
      is mongod's logged one), and this server now stays at 4096 every run.
      The rest is WiredTiger's eviction: on the 28 MB spill both engines
      force-evict pages over `memory_page_max=10m`, but mongod's WiredTiger
      12.0 reconciles 2 of them to the file (`bytes written from cache`
      1885779, `maximum page size seen at eviction` 10493783), while the
      vendored 7.0.33 splits its 6 in memory (12180 bytes written, max page
      size 0). Closing it means 12.0's eviction for the spill engine, a
      vendoring decision for Joe. Found 2026-11-13 working RM-325.
      Verify: tests/backlog/test_rm_found_2026_11_13_rm325.py::test_a_window_spill_the_engine_writes_out_is_mongods_size
      Backlog-ID: RM-611
- [ ] **An upsert's field order sometimes repeats for all twelve
      upserts.** `tests/backlog/test_rm_057_084.py::test_upsert_field_order_from_several_equalities`
      (RM-059's regression test) failed in one RM-625 gate run on
      2026-11-18 and once in about 60 reruns: this server gave
      `('d', 'c', 'a', 'b', 'x')` for all twelve upserts of
      `{d: 1, c: 2, b: 3, a: 4}`, where the test expects both orders of an
      absl pair. The order comes from `secantus_core::absl_hash`'s
      per-thread `NextSeed` (start at a thread-local's address, step
      0xad53), so a run of identical orders depends on where a thread's seed
      starts. Not yet known: whether mongod's own `NextSeed` can give a
      twelve-long run too (then the test asks more than mongod guarantees)
      or the model differs from absl (then the server is wrong). Found
      2026-11-18 (RM-625).
      Verify: none -- intermittent, about 1 run in 30; no deterministic gap test until the cause is known
      Backlog-ID: RM-721
- [ ] **Some explain tests still compare yield counters with a mongod that
      yields on time.** mongod yields once its executor has run
      `internalQueryExecYieldPeriodMS` (10 ms) since its last yield, which
      load decides; each yield moves a stage's `saveState` / `restoreState`
      by one, and this server's explain counts them structurally, so it
      never does. Measured 2026-11-28 on `mongo:8.2.11` (Linux x86-64, the
      exemplar build): the 23 cases of `test_lookup_hash_join.py`'s
      `executionStats` explain gave the same answer in every one of 5
      unloaded runs with yields at the default and with the period at
      86,400,000 ms, and on a container held to 0.05 CPUs 13 of the 23
      differed across 8 runs at the default (`saveState` / `restoreState`
      0 or 1, up to 6 on the `many_documents_and_a_match_after` case)
      against none across 8 with the period off. So the no-yield answer is
      the exemplar's own, and `mongod_rs_no_yield` / `no_timed_yields` is the
      reference for these tests. Moved to it: `test_lookup_hash_join.py`,
      `test_sbe_find_explain.py`, `test_rm_found_2026_11_10_rm418.py`,
      `test_rm_found_2026_11_14_rm434.py`,
      `test_rm_found_2026_11_08_rm310.py` (RM-768's occurrences) and
      RM-683's two loop-join files, then (2026-11-28) `test_change_stream_explain.py`,
      `test_rm_355_364.py`'s collectionless-aggregate and `awaitData`
      tests, `test_rm_found_2026_11_08_rm341.py`'s four plan-cache trial
      tests and `test_rm_found_2026_11_14_rm539.py`'s `$unionWith` test.
      Those were found by forcing yields instead of reading each file: a
      pytest plugin starts every reference mongod that has not turned
      time-based yields off with `internalQueryExecYieldIterations=1` (a
      yield at every plan iteration, so a test that compares a yield
      counter fails every run, not one in thirty) and gives `no_timed_yields`
      the same off switch; every file under `tests/backlog` and
      `tests/mongod_live` that mentions `explain` and a reference mongod
      (117 files; `test_rm_storage_model.py` was stopped part-way, as it
      has none) was run that way, and each file that failed was rerun
      without it. What is left:
      tests that fail without the plugin too on the binary the sweep used
      (prebuilt 2026-11-28 04:38, older than the tests: `test_rm_found_2026_11_15_rm452.py`,
      `_rm418.py`'s sorted-find cases, `test_sbe_filtered_front_explain.py`,
      three tests of `_rm341.py`, four of `test_rm_355_364.py`, the geo-near
      test of `test_rm_geo_wildcard_plans.py` and `test_rm_001_028.py`'s
      flow-control test) cannot be told apart from yield counters until they pass, so
      rerun the sweep on a current binary; and a test that does not say
      `explain` (a slow-query line's `locks` or `numYield`) was not swept.
      Verify: none -- intermittent test quality, no gap in the server
      Backlog-ID: RM-768
- [ ] **A `{$meta: "sortKey"}` with no `$sort` is refused where the query
      layer does not take it.** mongod 8.2.11 gives `[]` in a `$project`
      behind a `$limit` or another `$project`, and in a `$replaceWith`, as
      it does in `$addFields`; this server refuses each with code 2
      "aggregation pipeline uses a stage or operator not supported by the
      Rust server". Found 2026-11-23 (RM-634).
      Verify: tests/backlog/test_rm_found_rm634.py::test_an_unpushed_sort_key_read_with_no_sort_is_empty
      Backlog-ID: RM-766
- **Scope decision — SBE's `$concatArrays` refusal names a count mongod
  never set.** Over its budget mongod 8.2.11 says "Concatentating array of
  2 elements and N bytes with array of 87684434848907256 elements and M
  bytes": the operand's element count is read from memory it never wrote.
  This server gives the code and the byte counts as mongod computes them,
  and the operand's length for that count.
- `$convert` to binData subtype 2 (the deprecated "old binary") is
  written with the old format's inner length, as the bson crate writes
  every subtype-2 value. mongod 8.2.11 writes the bytes alone, which is not
  a valid subtype-2 value: pymongo refuses to decode the reply. Out of
  scope; nothing reads the old subtype as mongod emits it.
- [ ] **`$queryStats`' remainder: entry sizes, `otherNss` order, unmeasured
      key components.** Since 2026-11-14 the store is mongod's partitioned
      LRU cache, sized by `internalQueryStatsCacheSize` and sampled by
      `internalQueryStatsSampleRate`, records an `aggregate` that explains,
      and lists keys under `transformIdentifiers`
      (`tests/mongod_live/test_query_stats.py`,
      `tests/mongod_live/test_query_stats_store.py`). What is left: an
      entry is charged its key's representative BSON plus a per-command
      constant measured from mongod 8.2.11's
      `queryStatsStoreSizeEstimateBytes`, not mongod's `sizeof` of its
      parsed structures, so the point a partition starts evicting differs
      by a few bytes an entry; `otherNss` past one namespace is in the
      order the pipeline names them, where mongod's is a hash set's; and,
      unmeasured, the key's `writeConcern` and `$readPreference` (copied as
      the command gave them) and `fromMultiPlanner` for a rooted `$or` the
      subplanner plans (counted false here). Found 2026-11-10 working
      RM-215; trimmed 2026-11-14.
      Verify: none -- the sizes are C++ object sizes this server does not
      have, mongod's `otherNss` order is its hash set's, and the rest is
      unmeasured: a probe must find a difference before a test can assert it
      Backlog-ID: RM-532
- [ ] **A run-time error names the stored document, not the one the read
      projected.** mongod 8.2.11 reads only the fields a pipeline depends
      on, and an expression's error names that projection:
      `{$replaceRoot: {newRoot: "$x.b"}}` over `{_id: 1, x: {a: 1, b: 1}}`
      is 40228 "... Input document: {x: {b: 1}}", in `aggregate` and in
      `explain`; behind `{$replaceRoot: {newRoot: "$x"}}` it is the whole
      new root. This server names the whole stored document. Found
      2026-11-26 working RM-659.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm536.py::test_a_run_time_error_names_the_document_the_read_projected
      Backlog-ID: RM-809
- [ ] **An `$expr` equality on a `let` variable is not planned on an
      index.** mongod 8.2.11 plans `{$match: {$expr: {$eq: ["$b", "$$x"]}}}`
      with `let: {x: 1}` on an index on `b` (1 document and 1 key examined
      of 2), as it plans the same with a constant; this server scans the
      collection (2 documents, 0 keys), at the top level and in each run of
      a correlated `$lookup` (6 documents and 3 scans where mongod examines
      2 documents and 2 keys of `b_1`). Found 2026-11-26 working RM-682.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm539.py::test_an_expr_equality_on_a_let_variable_uses_an_index
      Backlog-ID: RM-813
- [ ] **A `$lookup` that mongod joins by a dynamic indexed loop is
      explained as a classic stage.** mongod 8.2.11 explains a `$lookup` on
      `localField` / `foreignField` as SBE whatever the strategy; this
      server builds the hash join, the indexed loop join and the nested loop
      join (`explain/sbe/lookup.rs`, `index_join.rs`; RM-683, closed
      2026-11-27) and declines the one left: `DynamicIndexedLoopJoin`, which
      mongod takes when the only index leading with the foreign field has a
      collation the query lacks and no hash join is possible
      (`allowDiskUse: false`, or a foreign collection past 10,000 documents
      or 100 MiB). Its plan decides at run time, for each local key, whether
      to seek the index or scan the collection
      (`buildDynamicIndexedLoopJoinLookupStage`, `gen_lookup.cpp`).
      `lookup_joins` (`explain/mod.rs`) declines it on purpose: a stand-in
      `queryPlan` alone would be wrong. Found 2026-11-27 working RM-683.
      Verify: tests/backlog/test_rm_lookup_sbe.py::test_a_collated_index_without_a_hash_join_is_a_dynamic_indexed_loop_join
      Backlog-ID: RM-840
- [ ] **An indexed loop join over a decimal key is explained as a classic
      stage.** mongod 8.2.11 seeks a `Decimal128` key as it seeks any number
      (a decimal and an int or double that are equal share a KeyString), and
      explains the `$lookup` as an `IndexedLoopJoin`; this server's
      KeyString encoder (`key_string.rs`) refuses a `Decimal128`, so
      `lookup_joins` declines a join whose local keys, or whose foreign
      index keys, hold one. Found 2026-11-27 working RM-683.
      Verify: tests/backlog/test_rm_lookup_sbe.py::test_an_indexed_loop_join_over_decimal_keys_is_explained_as_sbe
      Backlog-ID: RM-838
- [ ] **A `$lookup` under a collation is explained as a classic stage.**
      mongod 8.2.11 explains a `$lookup` under the command's `collation`, or
      over a local collection with a default one, as SBE: an
      `IndexedLoopJoin` over an index whose collation is the query's (the
      seeks made with `collKs`, the key sets with `collArrayToSet`), a hash
      join or a `DynamicIndexedLoopJoin` otherwise; so is a `$lookup` over a
      foreign collection with a default collation. This server explains each
      as a classic `$cursor` and `$lookup` (`lookup_joins` declines a
      foreign collection with a collation, and the query is not planned
      into SBE under one). Found 2026-11-27 working RM-683.
      Verify: tests/backlog/test_rm_lookup_sbe.py::test_a_lookup_under_a_collation_is_explained_as_sbe
      Backlog-ID: RM-839
- [ ] **A `$lookup` beside other pushed stages is explained as a classic
      stage.** mongod 8.2.11 runs a `$lookup` in SBE after a `$sort`,
      `$project` or `$group` in front, and with a `$project`, `$group`,
      `$count`, `$limit`, `$skip` or `$sort` after it (the last three move
      ahead of the `$lookup` into the query; a `$project` after it puts a
      projection of the fields it reads, plus the local field, under it),
      over a query an index serves (`IXSCAN` and `FETCH` under the
      `EQ_LOOKUP`), and with a dotted `as`; this server explains each as a
      classic `$cursor` and `$lookup`. Found 2026-11-26 working RM-683, whose builder takes a
      plain collection scan and stages that hand on whole documents.
      Verify: tests/backlog/test_rm_lookup_sbe.py::test_a_lookup_among_other_pushed_stages_is_explained_as_sbe
      Backlog-ID: RM-827
- [ ] **The operation rows `currentOp` lists lack mongod's detail.** Since
      2026-10-20 a registry of running operations stands behind
      `currentOp`, `$currentOp` (with `idleCursors`) and `killOp`, which
      ends the operation it names with 11601 wherever it next checks: a
      waiting getMore, or any scan's deadline poll. Open:
      - a row has no `currentOpTime`, `lsid`, `planSummary`,
        `queryFramework`, `numYields`, `locks` or `waitingForLock` (`queues`
        and `currentQueue` since RM-355, 2026-11-12);
        a getMore's `cursor` block has only its id; an idle cursor's has no
        `createdDate`, `lastAccessDate`, `nDocsReturned`, `nBatchesReturned`
        or `originatingCommand` (the cursor registry does not keep them; its
        clock is monotonic);
      - `$currentOp`'s `allUsers`, `idleConnections` and `idleSessions`
        options are not applied.
      Backlog-ID: RM-216
      Verify: tests/backlog/test_rm_216_current_op_fields.py::test_a_running_operations_row_has_mongods_fields, tests/backlog/test_rm_216_current_op_fields.py::test_an_idle_cursors_row_has_mongods_fields
- [ ] **`dbCheck` `extraIndexKeysCheck` refuses Decimal128 keys and geo / wildcard indexes; `applyOps` some entries.**
      Since 2026-11-14 the check walks an ascending, descending, compound,
      multikey, sparse, partial or collated index as mongod 8.2.11 does
      (KeyString V1 bytes, `crates/secantus-commands/src/key_string.rs`).
      Open: a key holding a Decimal128 is refused (115; mongod encodes it,
      a decimal equal to a double with the double's bytes, others with
      key_string.cpp's decimal continuations), and so is a `2dsphere`,
      `2d` or wildcard index, whose keys need each kind's key generator.
      mongod also takes `applyOps` `importCollection`,
      `createDatabaseMetadata` and `modifyCollectionShardingIndexCatalog`
      entries (115 here). `dbCheck` also runs before it answers (mongod runs
      it in the background), walks a database's collections in name order
      (mongod's is its catalog's internal order) and does not apply
      `maxBytesPerBatch` / `maxBatchTimeMillis` / `maxCountPerSecond`.
      Carried over from RM-416, 2026-11-14.
      Verify: tests/backlog/test_rm_219_diag_query_settings.py::test_db_check_extra_index_keys_remainder, tests/backlog/test_rm_219_diag_query_settings.py::test_apply_ops_other_entries
      Backlog-ID: RM-688
- [ ] **The FTDC archive writes no periodic metadata.** Since 2026-11-14
      samples go to `diagnostic.data` in mongod's format (RM-417). Open:
      mongod 8.2.11 also writes `type: 2` documents, `{start, getParameter,
      getClusterParameter, end}` in full with `count: 0` after each file's
      metadata and then only what changed; this server writes none.
      `diagnosticDataCollectionEnableLatencyHistograms` and
      `diagnosticDataCollectionVerboseTCMalloc`, which add to the sample,
      are not registered (72 here). In `transportLayerStats` mongod also
      lists its Unix domain socket (`/tmp/mongodb-<port>.sock`), which this
      server does not listen on, and on Linux measures each TCP listener's
      backlog depth and counts the connections it discards for a client
      that left; macOS mongod reports 0 for both, as this server does
      everywhere (not probed on Linux).
      Verify: tests/backlog/test_rm_219_diag_query_settings.py::test_the_archive_records_the_parameters
      Backlog-ID: RM-690
- [ ] **Query settings miss two of mongod's mechanisms.** Since 2026-11-08
      `setQuerySettings` / `removeQuerySettings` / `$querySettings` keep
      mongod's settings, a read of their shape is refused (411) or planned
      over its `indexHints` (RM-219), since 2026-11-10 its
      `queryFramework` chooses the engine, and since 2026-11-14
      `showDebugQueryShape` gives each `debugQueryShape` (RM-418), and
      since 2026-11-16 a `$jsonSchema` query's setting is kept (RM-358).
      Open: a write made to `config.clusterParameters` directly is read
      only at the next start (mongod's op observer reloads it).
      Verify: tests/backlog/test_rm_418_direct_cluster_parameter_write.py::test_a_direct_delete_of_the_query_settings_lifts_them
      Backlog-ID: RM-418
- [ ] **A `find` under `queryFramework: "sbe"` that is index-served, or
      sorted on a path or with a skip, is explained in the classic
      shape.** mongod 8.2.11 runs it in SBE and explains it so (`nlj` over
      an `ixseek` and a `seek` for an index); this server builds the slot
      program only for a forward collection scan, its filter (comparisons
      and `$in`s), skip, limit, a simple projection of top-level fields,
      since 2026-11-26 a sort on one top-level field with or without a
      limit, and since 2026-11-28 on several, with mongod's parallel-arrays
      check, or with a skip, alone or with a limit (`explain/sbe/find.rs`).
      Also not built: a sort on a path (`traverseP` with
      `getNonLeafSortKeyAsc`), or with a projection; an `$in` holding null
      (mongod's `typeMatch` program), a
      projection of a path or a computed field, and a projection under both
      a skip and a limit. For an `aggregate` (RM-542) the `$addFields`
      after its query are built when they set new top-level fields to
      constants or one field path; an `aggregate` whose query is sorted or
      `$project`ed, or whose `$addFields` compute, is explained classic
      (its `queryHash` and `planCacheKey` are mongod's). Found 2026-11-10
      (RM-418).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm418.py::test_sbe_find_shapes_are_explained_as_sbe
      Backlog-ID: RM-543

### 7.085 The shared Rust storage, audited -- 2026-10-20

A read-only audit of `secantus-storage` / `secantus-wt`, the layer both
Rust servers use. The two items with gap tests were reproduced against
mongod 8.2.11 with a `secantusd-mdb` built from tree `af2629ec`; the rest
were confirmed by reading the code path and need a fault no quick test can
inject (a panic, a power loss, a failing checkpoint).

- **Scope decision — `compact` compacts the files a collection shares**
  (2026-10-26, RM-192). mongod compacts a collection's own table and its
  indexes'; here a collection's documents are rows in one of sixteen
  document shards and its indexes rows in shared tables, so `compact`
  compacts those files, gives back the free space in them whoever freed it
  (a dropped neighbour's pages included), and answers `bytesFreed` as how
  much they shrank -- mongod's measure, over the files this store keeps the
  collection in. `freeSpaceTargetMB` is WiredTiger 11.3's check, made from
  the block manager's free-bytes statistic before the vendored 11.2 is
  asked (`crates/secantus-storage/src/compact.rs`).
- [ ] **`compact`'s dry run does not estimate.** `{compact, dryRun: true}`
      answers `estimatedBytesFreed: 0` and compacts nothing. mongod 8.2.11
      runs WiredTiger 11.3's estimation phase and reports the statistic it
      leaves; the vendored WiredTiger (11.2) has no estimation phase. mongod
      answered 0 on every shape measured (2026-10-26: five collections of
      6,000 to 200,000 documents, files of 13 to 155 MB with 12 to 103 MB
      free, the default target and 1 MB), so no answer is known to differ.
      Backlog-ID: RM-316
      Verify: none -- no shape was found where mongod 8.2.11 estimates more than 0
- [ ] **`autoCompact` cannot stop a file part-way, and judges files by the
      bytes given back.** WiredTiger 11.3's background compaction server is
      rebuilt over 11.2 (`crates/secantus-storage/src/auto_compact.rs`).
      11.2 cannot interrupt a compaction, so disabling it lets the file in
      hand finish (11.3 interrupts it and counts `background compact
      interrupted`, never counted here), and closing the store waits for
      that file. 11.2 does not count the bytes a compaction rewrote, so the
      moving average that decides which files to skip for an hour -- and
      `background compact moving average of bytes rewritten` -- is of the
      bytes each pass gave back.
      Backlog-ID: RM-317
      Verify: none -- an interrupt is a timing inside WiredTiger, and the rewritten-bytes count is internal to it
- [ ] **`--repair` leaves a resurrected duplicate where mongod moves it
      aside.** When a salvage brings back a deleted document beside the
      document that replaced it (same `_id`, another RecordId), the repair
      keeps the earlier one in the `_id` index and says how many it left
      out; mongod moves the other into `local.lost_and_found.<uuid>`, which
      this server does not have, so the extra document stays in its
      collection, readable by a scan and not by `_id`.
      Backlog-ID: RM-318
      Verify: none -- needs a salvage that brings back a deleted document beside its replacement, which no quick test can stage

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

- **`tools/probes/auth_enforcement.py` runs mongod as a one-member
  replica set** with `--noscripting` (this was RM-024, 2026-10-20), as the
  Rust server reports itself and as the JavaScript scope decision has it:
  a standalone answered `$changeStream`, `replSetGetStatus` and the default
  read/write concern commands differently whoever ran them. Against it the
  Rust server differs on none of 12,053 checks (66 users).
- **Scope decision — `mapReduce` with a named `out`** (Joe, 2026-10-16,
  formerly RM-027). Refused as a `--noscripting` mongod 8.2.11 refuses it
  (31264, since 2026-10-20; it answered 9 before), like every map/reduce
  but the one inline count shape. A JavaScript engine is not being built.
  The Java and Kotlin gauges' "MapReduce omits default write concern" is
  recorded in `validation_summary/expected_failures.py`. `compact` and
  `setParameter notablescan` match mongod; `reIndex` refuses as mongod does
  on a replica-set primary, which this server reports itself as. Pinned by
  `tests/backlog/test_rm_001_028.py::test_commands_mongod_runs`.
- **Scope decision — the privileges of this server's own commands**
  (Joe, 2026-10-16, formerly RM-028). `secantusAdmin.backupArchive` and
  `archiveBaseSnapshot` need cluster `readBackupFile` (the `backup` role),
  `restoreArchive` cluster `applyOps` (`restore`), `pruneOplog` cluster
  `replSetResizeOplog`, and `pruneTtl` cluster `compact`: each the action
  that guards mongod's nearest real operation (`secantus-commands/src/authz.rs`).

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

- **Scope decision — the order of a set of roles** (this was RM-030,
  2026-10-20). A user's `roles` after `grantRolesToUser`, a role's `roles`
  after `grantRolesToRole`, and the built-in roles under
  `showBuiltinRoles`: mongod keeps each in a hash set, and gave three
  different orders for one grant in one process, so no fixed order is
  mongod's. This server keeps the order they were given in, as with the
  order of `distinct` on a view.

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

- **Not measured — `validate` with `background: true`** (RM-033's
      remainder, 2026-10-16). It reads WiredTiger's last checkpoint now, so
      a collection no checkpoint holds is 26, as on mongod
      (`test_background_validate_of_an_uncheckpointed_collection`). Still
      read live, and not yet compared with mongod: a time-series
      collection's `nrecords` (its buckets), and `system.views`. A store held
      in memory answers 115 with this server's own text, because mongod's
      in-memory engine is Enterprise-only and its message could not be
      measured.
- **Scope decision — `replSetReconfig` takes one member, this server**
  (2026-10-20, under "Explicitly out of scope: real replica sets"). mongod
  8.2.11 accepts a configuration that adds a member it cannot reach and
  goes on to replicate to it; this single-node set refuses any
  configuration of more than one member (103). Every other check and
  setting is mongod's (17 scenarios, no difference), and the configuration
  is kept in `local.system.replset` across restarts.
- **Scope decision — the order of `planCacheListFilters`.** mongod lists
  index filters, and the indexes of each, in its hash set's order (setting
  `["a_1", "zz"]` lists `["zz", "a_1"]`); no fixed order can match it, so
  they are listed in the order they were set (since 2026-10-20, when index
  filters landed; formerly part of RM-143).
- **Scope decision — `buildInfo.secantusVersion` and `serverStatus.secantus`
  stay** (Joe, 2026-10-15). Each names this build beside mongod's own
  fields. They are additive, documented (`CLAUDE.md`, `docs/compatibility.md`,
  `docs/servers.md`), and how the admin UI tells this server from mongod;
  no driver compares a whole `buildInfo` reply.
- [ ] `buildInfo` has no per-OS block; `collStats`' `wiredTiger` and
      `indexDetails` list only each table's `metadata`, `creationString`,
      `type`, `uri` and `version` (RM-796), not the WiredTiger statistics
      categories mongod adds (`btree`, `cache`, `cursor` and the rest: since
      storage-model phase 5 each
      collection and index has its own `collection-<UUID>` /
      `index-<UUID>` table, as on mongod, so the statistics exist but are
      not yet read -- and `storageSize` in `collStats` / `dbStats` is still
      `dataSize` copied rather than the table's bytes on disk, and
      `indexSize` an estimate); `latencyStats` is zeros; `config.system.sessions`
      exists but holds no sessions (mongod writes them there; here they are
      kept in memory), so `dbHash` on `config` differs, as on `admin` does (`admin.system.keys` holds random signing keys there, a
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
      Not workable as one fix (a fix-one session declined it, 2026-11-14): it
      bundles about ten gaps (buildInfo's per-OS block, latencyStats,
      connectionStatus privileges, hostInfo.extra on Linux, $indexStats
      following the trial, $listCatalog fields, sessions in
      config.system.sessions) besides the per-table sizes RM-499 decides.
      Split it into one item per gap, each with its own gap test, before
      batching.
      **Decided (Joe, 2026-11-16): RM-499 goes ahead; once its storage stage lands, split this item's per-table parts and its other parts into separate items with their own gap tests.**
      Backlog-ID: RM-040
      Verify: tests/backlog/test_rm_029_056.py::test_coll_stats_lists_a_tables_statistics

### 7.081 Geo queries -- 2026-10-13

`tools/probes/geo_queries.py`, mongod 8.2.11: 105 scenarios, 1,278 replies
over a raw connection (`$geoWithin`, `$geoIntersects`, `$near`,
`$nearSphere`, the `$geoNear` stage, and what a `2d` or `2dsphere` index
holds and refuses). A read is compared by its whole reply, a double to nine
digits; an index build or a write by its reply and by what the collection
and its indexes hold afterwards. mongod answered the same on three runs
against itself. The Rust server differed on 137 replies; since 2026-10-15
it differs on none. Pinned by
`crates/secantus-storage-adapter/tests/geo_queries_wt.rs`, `geo_edges_wt.rs`
and `geo_order_wt.rs`.

mongod tests `$geometry` on the sphere (an edge is the great-circle arc
between its ends), and a flat shape (`$box`, `$polygon`, `$center`) holds
points only. Where the code is now: `secantus_core::geoshape` (shapes, the
sphere, `$near`, what an index holds), `secantus-commands/src/near.rs`
(where `$near` may stand, its index, nearest first),
`aggregate::apply_geo_near`. The ORDER is mongod's own index scan, replayed:
`secantus_core::s2legacy` (mongod's S2 library, transcribed),
`secantus_core::geohash2d` (the `2d` hash and its coverer),
`secantus_core::geoorder` (what an index keys a document by, a query's
intervals, the `$near` stage's rings), `secantus_core::heap` (libc++'s
priority queue) and `secantus-commands/src/explain/geo.rs`.

What the probe does not reach, each found beside it:

- **Not measured — geo order past the replayed shapes** (RM-151's
  remainder; the `$or` branches and a range on a compound `2dsphere`
  index's trailing field replay mongod's scans since 2026-10-20): a `$near`
  replay past 2,000 documents (nearest first), `2dsphere` index versions 1
  and 2, and a Linux x86-64 mongod (the geo order is the macOS arm64
  reference's: libc++ heap, FMA).
- **Line against line and line against polygon are S2's own** (RM-148,
  closed 2026-10-20): `S2Polyline::Intersects` and mongod's
  `polygonLineIntersection` (`S2Polygon::IntersectWithPolyline`), with S2's
  exact predicates. Line within polygon and polygon within polygon are
  still vertex and crossing tests with a 1.5e-9 tolerance, not S2's
  clipping; they agreed with mongod on everything probed
  (`geo_queries.py`, 1,278 replies, none differ).

### 7.080 The order `find` returns documents in -- 2026-10-13

`tools/probes/find_order.py`, mongod 8.2.11: 47 scenarios, 6,531 replies
over a raw connection. A read is compared by the `_id`s it returns, in
order; a write of one document (`delete` with `limit: 1`, `update` without
`multi`, `findAndModify`) by its reply and by what the collection holds
afterwards. mongod answered the same on three runs against itself. The
Rust server differs on 77 replies (97 before Phase 4, 2026-10-15). Pinned by
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
  index in mongod, and come back in its order. The router reads the index
  for them too (`secantus-storage/src/scan_bounds.rs`).
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
- Where several plans could serve a query, mongod's trial is reproduced
  (`explain/trial.rs`): works counted round-robin, its plan ranker's
  scores and tie-breaking bonuses, AND_SORTED intersections (a multikey
  index's point scan included), and one plan per predicate on an index's
  array field. That trial also picks the read order.
- mongod's plan cache (since 2026-10-27, `explain/plan_cache.rs`, RM-154):
  the trial runs once per query shape and its winner is cached under
  mongod's own key and hashes (`explain/cache_key.rs`); a repeated shape
  runs the cached plan, and so reads in its order, until the plan does
  worse than ten times its works and is replanned. `explain` reports the
  hashes and `isCached`, `$planCacheStats` the entries, `planCacheClear`
  clears them. Pinned by `tests/backlog/test_rm_154_plan_cache.py`.
- Scans a sort merges (an `$in` under a sort) come out through libc++'s
  priority queue (`secantus_core::heap`), as in mongod.
- A `$match` after `$addFields` or `$unwind` that reads no field the stage
  writes moves ahead of it (`explain/hoist.rs`).
- An index of another collation does not return a sort, and a regex is
  still bounded by an index of plain strings under a query collation.
- An empty array is indexed as `undefined`, between MinKey and null, and an
  array element without the rest of the path as null.

Still different, all 77:

- **Scope decision — ties under a sort with a limit keep their arrival
  order** (this was RM-049; decided by Joe 2026-10-16, measured
  2026-10-20). mongod keeps the first `skip + limit` documents of a
  blocking sort in a heap (`TopKSorter`: `make_heap`, then `pop_heap` /
  `push_heap` per better document, `sort_heap` at the end), and the order
  equal documents come out in is the C++ library's. The decision was to
  reproduce libc++'s heap if a Linux (libstdc++) mongod agreed with the
  macOS one, and otherwise to accept the divergence. It does not: over 540
  cases of 3 to 40 documents with ties (`find` and `aggregate`, limits 2 to
  11, skips 0 to 2), mongod 8.2.12 on Ubuntu 24.04 and mongod 8.2.11 on
  macOS returned different orders in 309. No order is mongod's, so this
  server's (equal documents in the order the plan reads them) stands. The
  two gap tests were retired with the item: they asserted the macOS
  answer, which a Linux mongod would fail too.
- **An unknown hint's wording is mongod's since 2026-10-20** (it was an
  accepted difference, Joe 2026-10-16, formerly RM-051). The planner's text,
  `error processing query: ns=<ns>Tree: <query tree>\nSort: ...`, is
  rendered by `explain::debug_string` from the parsed query, and what each
  command adds to it (paging, projection, collation) was measured per
  command. Pinned by `tests/backlog/test_rm_029_056.py::test_unknown_hint_on_aggregate`
  and `tests/backlog/test_rm_planner_text.py`. `$jsonSchema` prints as the
  predicates it parses into (since 2026-10-20); `$near` is refused in
  words of its own before any plan.
- [ ] **The cost of the plan trial.** Release builds against 43463aa6
      (2026-10-15): a query two indexes could serve costs about 5 times as
      much on 2,000 documents (`{a: 5, b: 3}` over `a_1` and `b_1`), 2
      times for a range; the trial reads and decodes the collection and
      encodes its keys. `update_one` by an indexed value is 0.9 times the
      build before and 1.2 times an update by `_id`
      (`order::store_order_is_the_plans`); `{a: {$ne: 5}}` over `a_1` 0.7
      times, a regex prefix 0.01 times, now that the router reads the index.
      The plan cache landed 2026-10-27 (RM-154): a shape's trial runs until
      its entry is active, then only the cached plan is worked, within ten
      times its works. That bought little. Release builds, 2,000
      documents, median of 300 finds, two runs each, against 3b47b41a:
      `{a: 5, b: 3}` over `a_1` and `b_1` 2.0 -> 1.63 ms, a range on both
      2.66 -> 2.28 ms, where one index is 0.49 ms and `_id` 0.10 ms. The
      cost is reading and decoding the collection and keying its index,
      which checking the cached plan's works needs as the trial did. The
      lever left: keep a collection's trial documents and index keys
      between reads, or count the cached plan's works over the index
      itself.
      Backlog-ID: RM-056
      Verify: none -- performance budget (timings against an earlier build)
- [ ] **`$planCacheStats` lists no `estimatedSizeBytes` on Linux.** The
      field is mongod's estimate of the entry's C++ objects, so its figure
      depends on how mongod was built (RM-341): this server reproduces
      macOS arm64's (libc++, `explain/entry_size.rs` `LIBCXX_ARM64`) and
      leaves the field out elsewhere. Linux x86_64 mongod 8.2.11, which CI
      runs, is built against libstdc++: `std::string` is 32 bytes with 15
      characters inline (libc++: 24 and 22), `std::set` 48, so every
      `sizeof` and every capacity term differs. Fit a `LIBSTDCXX_X86_64`
      layout by probing a Linux 8.2.11 (CI or a container; none is on this
      Mac), the way the arm64 one was: the shapes of
      `test_entries_of_many_shapes_report_mongods_size`, and index names
      either side of 15 characters. **Decided (Joe, 2026-11-08, on RM-341):
      a layout table per mongod build, chosen by the target the server is
      built for, each fitted by probing that platform's mongod 8.2.11.**
      Blocked (2026-11-14): fitting the LIBSTDCXX_X86_64 table needs a Linux
      x86_64 mongod 8.2.11, and this Mac has only macOS builds and no
      container runtime. Either install colima or Docker and run the Linux
      image here, or probe CI's Linux mongod from a dedicated session.
      **Decided (Joe, 2026-11-16): install colima on this Mac and run the Linux x86_64 mongod 8.2.11 image locally to fit LIBSTDCXX_X86_64.**
      Backlog-ID: RM-442
      Verify: tests/backlog/test_rm_340_342_plan_cache_gaps.py::test_an_entry_reports_its_estimated_size_on_linux
- [ ] **`estimatedSizeBytes` is still left out of some entries whose
      index tree this server does not build.** It sizes an entry's
      `PlanCacheIndexTree` from the canonical query and the index tags of
      its winner (`plan::index_tags`, `plan::or_tags`,
      `entry_size::tagged_tree`): one index, or an `OR` of single-predicate
      branches beside other clauses, an `$elemMatch` over operators, `$not`
      among them, tagged on the `ELEM_MATCH_VALUE` itself when the winner
      reads it, and the object form of `$elemMatch` over one predicate,
      read from its index or not, and an `$or` branch of several
      predicates (an `AND`, the predicate on the scanned index's leading
      field tagged; 10381 for `{$or: [{a: 3, c: 2}, {b: 5}], c: {$lt: 3}}`
      over `a_1`, `b_1` and `c_1` on macOS arm64, sized 2026-11-28). mongod
      8.2.11 sizes every entry; this server leaves the field out for an
      `$or` whose clauses mongod pushes into the branches (`OrPushdownTag`;
      9729 for `{$or: [{a: 3}, {b: 5}], c: {$lt: 3}}` over `a_1`, `b_1`,
      `a_1_c_1` and `b_1_c_1`), an `$or` branch read through
      `AND_SORTED`, a `$not` over several predicates, `$jsonSchema`, and an
      `AND_SORTED` winner, and mis-sizes an object `$elemMatch` over
      several predicates (3486 for `{a: {$lt: 5}, d: {$elemMatch: {e:
      {$lt: 2}, f: null}}}` over `a_1`, `b_1`, `d.e_1`; this server 3326:
      no measured figure yet says how its inner `AND` is tagged). Found
      2026-11-14 (RM-436); object `$elemMatch` sized 2026-11-28.
      Verify: tests/backlog/test_rm_found_2026_11_08_rm341.py::test_an_object_elem_match_of_two_predicates_reports_its_size
      Backlog-ID: RM-694

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
  first `getMore`. Decided 2026-10-16 (Joe): cursors stream as mongod's
  do, a suspended plan pulled a batch at a time.
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

A document `_id` is compared by value: the `_id_` index is keyed by the
KeyString of `_id` (store format 4), where `{a: 1}` and `{a: 1.0}` are one
key, so there is no claim beside it.

Still different, of those 32 (the other four are the items below):

- **The message for a hint that names no index** (20, with 7.077's 7):
  mongod's since 2026-10-20 (see 7.080); the probe was not re-run then.
- `$where` in a query (3): not run here.
- `$near` with no geo index (2): mongod answers 291; this server finds
  nothing.
- Which unique index a duplicate of two is reported for (1): mongod
  checks them in the order they were built, which the catalog does not
  record (the same gap as `listIndexes` order).
- The collation key printed in a duplicate-key message under a collation
  (1): ICU's bytes.
- `$expr` that fails when folded (1): mongod adds `Failed to optimize
  expression :: caused by ::`.

A `readConcern` with an `atClusterTime` older than the oldest timestamp is
`239 SnapshotTooOld` on `find`, `aggregate` and `distinct`, as on mongod
(measured 2026-10-16).

- [ ] **RUST MongoDB server: `includeQueryStatsMetrics` adds nothing.**
      mongod 8.2.11 answers `includeQueryStatsMetrics: true` on `find`,
      `aggregate` and `distinct` with a `metrics` document: `keysExamined`,
      `docsExamined`, `hasSortStage`, `usedDisk`, `fromMultiPlanner`,
      `fromPlanCache` (which the explain module's plan and executionStats
      could supply), and work and time counters (`cpuNanos: -1`,
      `numInterruptChecks`, `workingTimeMillis`, ...) that no plan derives.
      Measured 2026-10-16: mongod's `metrics` has 14 fields (keysExamined,
      docsExamined, bytesRead, readingTimeMicros, workingTimeMillis,
      hasSortStage, usedDisk, fromMultiPlanner, fromPlanCache, cpuNanos,
      three delinquency counters, numInterruptChecks), per batch, on find,
      aggregate, distinct, count and getMore. This server could compute the
      key and document counts, `hasSortStage`, the working time and
      `cpuNanos`, and since the plan cache landed (2026-10-27, RM-154)
      `fromPlanCache` and `fromMultiPlanner` too (`explain/order.rs`'s
      `consult` knows whether a read ran a cached plan or a trial), and
      since RM-023 (2026-10-27) `bytesRead` / `readingTimeMicros` from
      WiredTiger's per-session statistics; not `numInterruptChecks` (RM-360)
      or the three delinquency counters (mongod's execution admission
      control, RM-355), which since 2026-11-09 each operation's execution
      admission context counts (`AdmissionContext::delinquent_acquisitions`
      and the two delinquency totals).
      **Decided 2026-10-16 (Joe):** leave it open; no stand-in values. Build
      it when those mechanisms exist.
      Blocked (checked 2026-11-14): waits on RM-355 (execution admission
      control) and RM-360 (interrupt checks), per the decision above; both are
      still open in part.
      Backlog-ID: RM-170
      Verify: tests/backlog/test_rm_found_2026_10_16.py::test_include_query_stats_metrics_adds_a_metrics_document

### 7.077 `update` and `findAndModify` -- 2026-10-12

`tools/probes/update_commands.py`, mongod 8.2.11: 30 scenarios, 1,295
replies over a raw connection. Each case runs against a collection of its
own and compares the reply AND the documents left behind. The Rust server
differs on 17 of 1,295. Pinned by
`crates/secantus-storage-adapter/tests/update_commands_wt.rs`.

What is left, on the RUST MongoDB server:

- **`$where` in an update's query** is refused as a `--noscripting`
  mongod refuses it (the no-server-side-JavaScript scope decision, formerly
  RM-060): the statement's `writeErrors` entry, 6108304. Pinned by
  `tests/backlog/test_rm_057_084.py::test_where_in_an_update_query`.

### 7.076 DDL and admin commands -- 2026-10-12

`tools/probes/ddl_admin_commands.py`, mongod 8.2.11: 37 scenarios, 809
replies over a raw connection (`create` and every option it takes,
`listCollections`, `collMod`, `renameCollection`, `drop`, `dropDatabase`,
`dbStats`, `collStats`, `dataSize`, `listDatabases`, and invalid database
names across thirteen commands). Byte counts are compared by type only. The
Rust server differed on 2 of 809, `listDatabases` without `config` and
`dbStats` without the filesystem's sizes; both were fixed on 2026-10-16
(the probe was not re-run then). Pinned by
`crates/secantus-storage-adapter/tests/ddl_admin_commands_wt.rs`.

### 7.075 `explain`, command by command -- 2026-10-12

`tools/probes/explain_commands.py`, mongod 8.2.11: 51 scenarios, 791
`explain` replies over a raw connection (`find`, `count`, `distinct`,
`aggregate`, `update`, `delete`, `findAndModify`; index bounds, sort,
paging, projection, `$or`, hints, collation, views, execution counts, what
the command refuses). Nearly every scenario offers the planner ONE usable
index or none, so the plan is a rule and not the outcome of mongod's trial
runs; the scenarios named `choice:` offer two. The Rust server differs on
35 of 791 (72 before Phase 4, 2026-10-15). `explain` (`crates/secantus-commands/src/explain/`) derives the
plan from mongod's planning rules, and `executionStats` from walking that
plan over the collection's documents. Pinned by
`crates/secantus-storage-adapter/tests/explain_commands_wt.rs`.

What is left, all on the RUST MongoDB server:

- [ ] **`aggregate` stages mongod runs in its slot-based engine, beyond a
      plain `$group`.** mongod 8.2.11 pushes a `$group` not served by a
      DISTINCT_SCAN, and `$count`, `$sortByCount`, `$bucket` and `$lookup`,
      into SBE and explains them with `explainVersion: "2"`, a
      `winningPlan` of `{queryPlan, slotBasedPlan}` and executionStats made
      of SBE stages. Since 2026-11-08 (`explain/sbe/`) this server builds,
      prints and RUNS that plan for a `$group`, or the `$group` a `$count`
      is, over a forward collection scan with no sort, projection, hint or
      collation, whose filter is `$eq` / `$lt` / `$lte` / `$gt` / `$gte` of
      a number, string or boolean on a path of named fields, `$eq: null`
      (on a dotted path too, since 2026-11-28), an `$in` of such values (holding null too on one
      field, since 2026-11-27), the `$not: {$eq}` a `$ne` is
      (since 2026-11-14), and the `$and`s of those (`explain/sbe/filter.rs`), with a skip and a limit where the
      group reads no field (RM-072); whose `_id` is a field path, `$$ROOT`,
      a scalar constant or an object of those, whose arguments are field
      paths or `$$ROOT` (numeric constants for `$sum`), with `$sum`,
      `$count`, `$avg`, `$first`, `$last`, `$min`, `$max`, `$push` and
      `$addToSet`, and at most one distinct dotted path; and the `limit 0`
      / `coscan` plan over a collection that does not exist, and an
      `$exists` (`exists(s)` on one field, a `traverseF` of
      `exists(getField(...))` on a dotted path, since 2026-11-28). Still the
      classic `$cursor` / `stages` shape: any other filter (an `$in` holding
      null on a dotted path, `$or`, ...), a sorted,
      projected or index-served front (`IXSCAN` / `FETCH` under a group
      that reads a field; the single-interval `FETCH` under one that reads
      none is built since 2026-11-27, RM-695; the `COUNT_SCAN` of a single-field
      index under a group that reads no field is built since 2026-11-14,
      RM-072), a skip or limit with a group that reads a field (mongod
      puts its dependency projection among them), `$lookup`,
      `$sortByCount` / `$bucket` (not desugared before
      `secantus_core::sbe::pushdown`, which the executor shares), any other
      accumulator or expression (the expression compiler of
      `gen_expression.cpp`), a collation (its env slot), and two or more
      distinct dotted paths (mongod orders their slots by an
      `absl::flat_hash_map` salted per process, so its own order varies
      from run to run). The SBE stages never spill here (`usedDisk` is
      always false), and the `$cursor` batches are capped by count only,
      not by `internalDocumentSourceCursorBatchSizeBytes`.
      `explain_commands.py` replies (0-based steps, measured 2026-11-08):
      `aggregate: stages after the cursor` 7, 8, 11; `aggregate: $group
      and $lookup` 2, 5, 6, 7; `executionStats: aggregate` 6. Also open in listings: mongod
      constant-folds (`{$abs: -1}` is `{$const: 1}`) and orders the fields
      of object-form operators its own way (`$dateToString`).
      **Decided 2026-10-16 (Joe):** reproduce SBE faithfully: transcribe
      mongod 8.2.11's SBE stage builders for these shapes and EXECUTE the
      slot programs, so `queryPlan`, `slotBasedPlan` and the SBE execution
      stats all come from running the plan. Not a printed stand-in.
      Backlog-ID: RM-071
      Verify: tests/backlog/test_rm_057_084.py::test_explain_of_a_group_pushed_into_the_query, tests/backlog/test_rm_057_084.py::test_explain_of_a_filter_sbe_runs_beyond_comparisons
- [ ] **`explain` prints a non-boolean `$exists` as the command gave it.**
      mongod 8.2.11 canonicalises `{c: {$exists: 1}}` to `$exists: true`
      and a falsy value to `$not: {$exists: true}` in `parsedQuery` and the
      plan's `filter`, a string such as `"x"` being truthy; this server
      echoes `$exists: 1` (so an aggregate over it stays the classic
      `$cursor` shape rather than SBE) and prints `$exists: "x"` as `$not`.
      The rows returned match. Found 2026-11-28 working RM-071.
      Verify: tests/backlog/test_rm_found_2026_11_28_rm071.py::test_explain_canonicalises_a_non_boolean_exists
      Backlog-ID: RM-845
- [ ] **A `FETCH` over an index scan of more than one interval is not
      run in SBE under a group that reads no field.** mongod 8.2.11 runs
      `count` on a view `{$match: {a: {$gt: 1}}}` with `query: {b: {$in:
      [1, 2]}}` over `b_-1` as an SBE plan (`explainVersion: "2"`, a
      `GROUP` over `FETCH` / `IXSCAN`); this server keeps the classic
      cursor. The single-interval shape is built since 2026-11-27 (RM-695:
      the `nlj` of an `ixseek` and a `limit 1` `seek`, `fetch_seek` in
      `explain/mod.rs`); several intervals need the seek over each one
      that `buildIndexScan` generates for them (its union or its
      `ixscan_generic` over a bounds program), still unprobed.
      **Decided 2026-10-16 (Joe):** as RM-071, transcribe and execute.
      Verify: tests/backlog/test_rm_057_084.py::test_explain_of_count_on_a_view_fetching_from_index_intervals
      Backlog-ID: RM-831
- [ ] **A fusion input pipeline's `$unionWith` counts none of the stages
      after its `$cursor`.** At `executionStats`, mongod 8.2.11 gives each
      stage a `$rankFusion` input pipeline runs in its `$unionWith` an
      `nReturned` and `executionTimeMillisEstimate` (4 each under a `$limit: 4`
      in pipeline `b`), the `$_internalSetWindowFields` its memory and spill
      figures, and the top-level `$_internalSetWindowFields` / `$group`
      their `maxFunctionMemoryUsageBytes` / `maxAccumulatorMemoryUsageBytes`;
      this server lists those stages with no counts. The `$cursor` itself
      matches since RM-696. Found 2026-11-26 working RM-696.
      Verify: tests/backlog/test_rm_found_2026_11_15_rm426.py::test_a_fusion_input_unions_stages_count_as_mongod_does
      Backlog-ID: RM-830

### 7.074 Change streams, command by command -- 2026-10-11

`tools/probes/change_stream_commands.py`, mongod 8.2.11 as a replica-set
member: 115 scenarios over a raw connection (what `$changeStream` accepts,
every event's shape, `getMore`, the invalidating operations, resuming by
`resumeAfter` / `startAfter` / `startAtOperationTime`, the three scopes,
pre-images, transactions), each on a database of its own. The Rust server
differs on none (re-measured 2026-11-09 after RM-454; 1 earlier that day, 2 on 2026-10-16, 5 before). The harness salts database names per run, because
mongod keeps the oplog of earlier runs and a stream started in the past
would see them. What is left:

- [ ] **RUST MongoDB server: the Boolean simplifier differs from mongod's
      on about 0.3 percent of random trees, and on nested `$nor`.** The
      simplifier (`crates/secantus-commands/src/explain/simplify.rs`, RM-774)
      is built from what mongod 8.2.11 was measured to do -- a DNF and
      Quine-McCluskey at every node (no Petrick: a term a merge made does
      not absorb its neighbours, except after an `$and`'s product), the
      common predicates of an `$and`'s terms taken out, the result kept
      only if smaller -- not from a port of
      `query/boolean_simplification/`, which is not on this box. A seeded
      run of 4,000 random `$and` / `$or` trees over nine predicates differs
      on 13 (the smallest are the gap tests), and one with nested `$nor`
      on about 1 in 140: mongod keeps a `b` a merge here removes, simplifies
      a contained `$or` in its `$and`'s context where this server does not,
      and does not simplify a tree whose `$nor` holds a contradiction this
      server folds. The residue points at a rule not yet found (how a
      negated node is converted, and what its size counts), not at noise.
      Reproduce with `tests/mongod_live/test_boolean_simplifier.py`'s
      random run at other seeds (2026-11-24, found working RM-774). The
      smallest such case: mongod keeps `{$nor: [{$and: [a, b]}], c: 1}`
      (and `{a: {$not: {$lt: 9, $gt: 3}}, b: 1}`, which parses to that
      shape) as it is, where this server turns the `$nor` into an `$or` of
      `$not`s. Counting a `$not` leaf as two nodes fixes these two and makes
      the seeded fuzz worse (11 -> 52 of 1,500 with `$nor`), so it is not
      the rule (2026-11-26, working RM-660).
      Verify: tests/backlog/test_rm_found_2026_11_24_rm774.py::test_a_random_tree_is_simplified_as_mongod_does
      Backlog-ID: RM-778
- [ ] **Two `$or`s in one `$and` are planned through the first only.**
      mongod 8.2.11 plans `{$and: [{$or: [{a: 1}, {b: 2}]}, {$or: [{c: 3},
      {d: 4}]}]}` over `a_1`, `b_1`, `c_1` and `d_1` through either `$or`
      (the other is a rejected plan, and its trial caches an entry); this
      server offers the first `$or`'s plan alone. Found 2026-11-28 (RM-826).
      Verify: tests/backlog/test_rm_found_2026_11_28_rm826.py::test_two_ors_in_one_and_are_each_planned
      Backlog-ID: RM-851
- [ ] **An `$or` with a compound branch beside a clause is enumerated as the
      product of its branches' indexes; mongod's list is shorter.** mongod
      8.2.11 lists eight plans for `{$or: [{a: 3, b: 1}, {b: 5}], c: {$lt:
      3}}` over `a_1`, `b_1`, `a_1_c_1`, `b_1_c_1` and `c_1_a_1` (an `OR`
      of `FETCH` over `b_1` and `b_1_c_1` wins; no plan reads one index for
      both branches but through the `b_1` / `b_1_c_1` pair, nor `a_1_c_1`
      with `b_1_c_1`); `or_variants` (RM-826) offers the full product up to
      ten, eleven plans. Which pairs the enumerator drops is not modelled.
      Found 2026-11-28 (RM-826).
      Verify: tests/backlog/test_rm_found_2026_11_28_rm826.py::test_an_or_of_a_compound_branch_enumerates_mongods_combinations
      Backlog-ID: RM-852
- [ ] **RUST MongoDB server: a change stream's `docsExamined` leaves out
      mongod's own oplog entries.** mongod 8.2.11's `executionStats` of a
      change stream counts every oplog entry its scan reads from the start
      time, its periodic no-ops among them; this server's oplog keeps no
      no-op entries, so its `docsExamined` counts only the writes (the
      counts of the entries the stream keeps match since RM-453, 2026-11-15).
      Verify: none -- mongod writes its no-ops on a timer, so no test can pin how many a scan reads
      Backlog-ID: RM-700

### 7.073 The cursor commands -- 2026-10-11

`tools/probes/cursor_commands.py`, mongod 8.2.11: 85 scenarios (`find`'s
cursor options, `getMore`, `killCursors`, the cursors of `aggregate` and the
listing commands, tailable cursors, `count`, `distinct`). The Rust server
differs on 5 scenarios. What is left:

- [ ] **RUST MongoDB server: a time-series read beside a TTL or 2dsphere
      index, a partial index of `$type` / `$in`, a `$where` / `$text`
      filter, or an `$expr` it cannot push down, still reads the collection
      at the command.** Since 2026-11-26
      an index of its own (an ascending / descending key, with or without a
      collation, and with or without a partial filter of comparisons or
      `$exists: true`) is kept over the buckets as mongod keeps it
      (`{control.min.v: 1, control.max.v: 1}`, the partial filter as
      `index_bucket_filter` writes it, `ts_store::sync_bucket_indexes` after
      `createIndexes` / `dropIndexes` / `collMod`) and a read plans over it
      with this server's planner (`explain::bucket_access`, the bucket
      filter's `$_internalExpr*` comparisons read as the plain ones when it
      tests a partial filter; an `$or` is planned under a SUBPLAN: one scan
      of the buckets when a branch has no bounds, the index when every
      branch has): measured 8.2.11, `v < 100` and `v = 5` are an IXSCAN
      (`control.min.v` leads), `v >= n` / `v > n` alone a COLLSCAN of the
      buckets in order (only the second key field is bounded), `v < 3000, v
      > 5` beside `partialFilterExpression: {v: {$gt: 5}}` an IXSCAN of the
      partial index, `v < 100` beside `{v: {$exists: true}}` a COLLSCAN (a
      `$_internalExprLt` does not prove `$exists`), a collated index
      served only to a read under the same collation, a hint to any index
      over the buckets its scan (collated or not, under a collation or not,
      with a meta filter or not), and an `$or` of branches an index each
      answers (one index per branch, a bounded scan of each) is the `OR` of
      their scans, read one after the other and each bucket once, under a
      SUBPLAN (`BucketAccess::Union`, each scan's bounds tested on the
      entry's key; measured 8.2.11, 2026-11-28: `v < 100 or w < 100` beside
      `{v: 1}` and `{w: 1}`, `rejectedPlans: []`, 112 after the delete, 3 of
      3 runs). An `$expr` comparison of a field with a
      constant derives the bucket predicates mongod derives (`$eq`; `$gt` /
      `$gte` with the field first, `$lt` / `$lte` with it second; `$lt` of
      the field none), and one that derives none reads the buckets in
      order. What is left: a string filter under a collation (the bucket
      bounds and the index over the buckets compare without the collation:
      this server reads whole, 99 where mongod leaves 53); a TTL or
      2dsphere index
      (`{data.loc: "2dsphere_bucket"}`); a partial index whose filter is
      `$type`, `$in`, ...; `$where` and `$text` filters with an index of its
      own, and an `$expr` that is not a single comparison (`$and`, `$or`,
      computed operands: mongod derived only the first clause of one `$and`
      measured, `$gte` of `v` alone); a read two indexes could answer is mongod's trial between the
      plans, which `explain::bucket_access` runs over the buckets as a
      read of the collection does (the candidates' scans, the clustered
      scan of the bucket `_id`s among them, raced and ranked by
      `explain::trial`, the plan cache consulted and written; RM-810 and
      the `$or` trials, 2026-11-28: with the meta filter beside the `$or`
      of two indexes the default index's scan wins the trial, 144 after the
      delete, and a branch two indexes answer is a trial of its own,
      112). The winner's trial results (up to 101 buckets) are the cursor
      stage's whole first buffer, then it reaches 224 buckets in all, as
      the buffers of 32, 64 and 128 do with no trial. Where
      mongod filters the buckets it buffers (a collection scan under a
      measurement-level filter) its count after the delete varies between
      runs (2,294 to 2,298 for `v > 10`, 1,915 to 1,917 for `v < 3000, v >
      3` beside the partial index, 1,021 to 1,028 for `$expr: {$gte: ["$v",
      "$w"]}`, 62 to 63 for `v > 3900` under a collation) and this server's
      is one number (2,292, 1,915, 1,017), so no test pins those. Found
      while working RM-457, 2026-11-15.
      Measured 2026-11-28 (mongod 8.2.11, the count after a delete): a TTL
      index (mongod needs its partial filter on the meta field), a
      2dsphere index and a `$text` / `$where` filter read as mongod does
      (`$where` and `$text` are refused alike), so they are not left; the
      string filters under a collation and the `$expr` that is no single
      comparison are the tests of `test_rm_701_time_series_remainder.py`.
      Verify: tests/backlog/test_rm_701_time_series_remainder.py::test_a_time_series_string_filter_under_a_collation_buffers_as_mongod, tests/backlog/test_rm_701_time_series_remainder.py::test_a_time_series_expr_that_is_no_single_comparison_buffers_as_mongod
      Backlog-ID: RM-701
- **Not probed (RM-083's remainder, 2026-10-17)** -- `oplogReplay`,
  `$meta: "textScore"` sorts, an `explain: true` aggregate. Every
  `{$meta}` shape measured on 8.2.11 matches since 2026-10-17, except a
  `$project` of `sortKey` with no `$sort` before it: both refuse it with 2,
  in different words (mongod's names its planner). The record-id counter is
  NOT stored: mongod rebuilds it from the largest record id when it reopens
  a collection, and so does this server. A store written before 2026-10-16
  keeps its store-wide record ids on existing documents. Not measured: a
  collection dropped while a transaction still holds uncommitted inserts in
  it (RM-084's situation) could reuse a record id when recreated.

### 7.072 Sessions, transactions and retryable writes -- 2026-10-11

`tools/probes/transactions.py`, mongod 8.2.11 as a replica-set member: 267
scenarios, each a script of commands over raw connections with the session
fields written out (the transaction state rules, what a transaction refuses,
conflicts, collections made in a transaction, cursors, the session commands,
retryable writes). The Rust server differs on 4 (2026-11-09): two `$where`
scenarios (no server-side JavaScript, a scope decision), the collation key
of a duplicate-key message (7.078) and the `createIndexes` below. The probe
masks only the write-conflict cause. What is left:

- [ ] **RUST MongoDB server: smaller differences.** The session errors
      are worded for a session with no user (not measured with one). A `createIndexes` that
      waits on a transaction past `maxTimeMS` (the probe's "an index built
      under a transaction"): mongod 8.2.11 adds a `writeConcernError` (50)
      to its 50 in 2 runs of 2 on 2026-11-09, where this server answers a
      plain 50; 12 runs of 12 had answered a plain 50 before. (The request
      and oplog-entry dumps of the statement-id and `findAndModify` retry
      errors are mongod's since 2026-11-09.)
      Backlog-ID: RM-087
      Verify: none -- the wording is unmeasured with a user, and mongod's writeConcernError on the createIndexes timeout varies between runs (2 of 2 then, 0 of 12 before), so no test can pin one answer

### 7.071 Time-series collections -- 2026-10-11

`tools/probes/time_series.py`, mongod 8.2.11: 191 scenarios on a fresh
database each (what `create` accepts, inserts, reads, where measurements are
placed, the restricted writes, indexes, `collMod`, rename and the rest). The
Rust server differs on 1 (2026-11-11): `collStats`' sizes -- its storage size,
as on every collection (RM-040), and its index sizes, 0 while the indexes are
not built over the buckets (RM-457); a second, `listCollections` order, differs in the runs where
mongod's own order comes out the other way.
Since 2026-11-11 this server keeps a time-series collection as mongod does:
bucket documents in `system.buckets.<name>`, each data field mongod's
compressed column, placed by mongod's bucket catalog
(`crates/secantus-commands/src/bucket_catalog.rs`). What is left:

- [ ] **RUST MongoDB server: `bucketCatalog`'s open and archived counts
      split differently from mongod's.** mongod 8.2.11 reports
      `numOpenBuckets` from its stripes' open buckets and
      `numArchivedBuckets` as the active buckets less those; after a
      series' bucket rolls forward two days and a measurement for that
      series' earlier window arrives (with other series and a count
      rollover beside it), mongod has 5 buckets, 3 open, 2 archived and 4
      managed, where this server has 4 open and 1 archived. Find which
      bucket mongod holds outside its open set (measured 2026-11-25).
      Verify: tests/backlog/test_rm_bucket_catalog.py::test_open_and_archived_buckets_split_as_mongod_counts_them
      Backlog-ID: RM-798
- [ ] **RUST MongoDB server: a dropped collection's buckets are cleared at
      once, not by era.** mongod 8.2.11's bucket state manager clears them
      lazily: a drop opens a new era (`currentEra` 1,
      `trackedClearOperations` 1) and the dropped buckets stay among the
      open and managed ones (`numOpenBuckets` 2 with `numBuckets` 1, so
      `numArchivedBuckets` -1) until they are next touched. This server
      forgets them at once and reports `currentEra` 0 (measured
      2026-11-25).
      Verify: tests/backlog/test_rm_bucket_catalog.py::test_dropping_a_collection_clears_its_buckets_by_era
      Backlog-ID: RM-799
- [ ] **RUST MongoDB server: `bucketCatalog.memoryUsage` releases a cleared
      bucket at once, not when it is next touched.** Found with RM-710
      (2026-11-27). Linux x86-64 mongod 8.2.11 keeps the buckets of a dropped
      collection, and a bucket a direct write (a delete) cleared, in its
      catalog until they are next touched, as RM-799 measures for the counts:
      with two collections written, dropping one takes `memoryUsage` from
      14568 to 14304 (the collection's statistics only), a delete of one
      series' documents leaves 14152 where it was, and the series' next write
      moves it (14224, 14176). This server forgets a cleared bucket at once
      (14568 to 11716; 14152 to 11660). The account itself follows the bytes;
      the missing piece is the registry's era (RM-799), which decides when a
      cleared bucket leaves memory.
      Verify: tests/backlog/test_rm_bucket_catalog.py::test_memory_usage_keeps_cleared_buckets_until_they_are_touched
      Backlog-ID: RM-833
- **Scope decision — `listCollections` order around time-series buckets.**
  This server puts each bucket namespace after its collection and
  `system.views` last; mongod's order is not stable between two runs of the
  same commands, so there is none to match.

### 7.070 Collation -- 2026-10-10

`tools/probes/collation.py`, mongod 8.2.11: 764 scenarios (reads under 29
collations, 39 malformed specs on every command, the expression and query
operators, and a fresh database per scenario for defaults, indexes, views and
writes). The Rust server differs on 1 (2026-11-09): text indexes, which it does not
build (hashed indexes are built since RM-732). It
compares through the ICU
57.1 mongod links, built from mongo's own copy (`crates/secantus-icu`,
`vendor/icu4c-57.1`), so locales, per-locale defaults, order and sort-key
bytes are ICU's. What is left, there and beside it:

- [ ] **RUST MongoDB server: a query never uses a collated index.** An index
      with a collation now holds collation sort keys (catalog marker
      `collationKeys`), which is what makes its `unique` right, and the
      planner passes it over: a query with a collation scans. Correct, slow
      on a large collection. A hint naming such an index is accepted and
      scans.
      **Decided 2026-10-16 (Joe):** `explain` reports mongod's plan, as it
      does; where the router reads differently, as here, the router is
      brought to the plan. Each such place is a performance bug.
      (This was RM-119, from the removed "Collation server-wide" record.) Any
      command `collation` forces a COLLSCAN plus an in-memory collation-aware
      match / sort: `crates/secantus-storage/src/lib.rs` `find_matching_with`
      sets `force_collscan = coll_opt.is_some()` ("Per-index-collation IXSCAN
      is a later optimisation"), even when an index was built with the same
      collation. Results are correct; the cost is a full scan. The Python
      server already uses a collated index whose collation matches exactly
      (`docs/indexes.md` "Per-index collation"; `numericOrdering` still
      COLLSCANs there). Reproduce: create `{name: 1}` with `collation:
      {locale: "en", strength: 2}`, then `find({name: "x"}).collation({locale:
      "en", strength: 2})` and look at how the storage router reads it.
      Backlog-ID: RM-094
      Verify: none -- explain already reports IXSCAN (derived from mongod's rules); the gap is the router scanning, a cost no reply shows


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


### 7.067 Index management -- 2026-10-10

`tools/probes/index_admin.py`, mongod 8.2.11: the Rust server differed on 5
of 90 (2026-10-10): `text` (out of scope) is not built and so cannot be
dropped, `hashed` was not built either (it is, since RM-732), and
`listIndexes` lists by name.

`listIndexes` order is RM-081 (RM-107 was the same item, folded in 2026-10-16).

### 7.066 Views and `$count` -- 2026-10-09

`tools/probes/views.py`, mongod 8.2.11: the Rust server is at 0 of 70
(`tasks/rust-mongod-remaining-plan.md`, 2026-10-14). What is left beside it:

- **Scope decision — `distinct` on a view may answer in any order.**
  mongod 8.2.11 answers it in a hash table's order that changes from call
  to call: 18 calls over six identical databases gave 13 orders
  (2026-10-16). No order is mongod's, so the sorted one this server gives is
  as faithful as any. (This was RM-108, which said the order was stable per
  build; it is not.)
- [ ] **`mongod` 8.2.11 itself crashes on `{collMod: <a view>, cappedSize:
      100}`** (the connection closes and the server is gone). Not ours; the
      Rust server answers 72 `Collection must be capped.` Do not put that
      command in a probe that needs mongod to survive it.
      Backlog-ID: RM-109
      Verify: none -- mongod's own crash, not a Rust server gap

### 7.065 Capped collections -- 2026-10-09


### 7.05 MongoDB-server backlog close-out -- 2026-10-07

**Left open on purpose, each with its reason** (the entry itself has the detail):

- **Unknown-index `hint` plan dump**: reproduced by the Rust server since
  2026-10-20; the Python server still names the hint.
- **`$where` / server-side JavaScript**: a scope decision; it also accounts for
  three of the Go gauge's remaining failures.
- **A `w` tag set with more than one key**: accepted divergence (mongod's
  hash-map order).
- **The multikey `$sort` plan artifact**: accepted -- mongod contradicting its
  own `_id` tie-break.
- **Decimal128 transcendental last digit**: authorised (correctly rounded).
- **Make Rust the recommended default**: decided 2026-10-16 (Joe): Rust first everywhere — README (the PyPI page) and docs lead with the Rust servers, the Python servers move to a marked-deprecated section, and the `secantusdb` alias names `secantusd-mdb`. Removing the Python servers stays a separate decision.
- **PyPI trusted publisher for `secantus-core` / lockstep publishing**: needs
  Joe's PyPI account.
- **CI Python matrix (3.10 only on push)** and **vendored WiredTiger rebuilt per
  CI job**: cost decisions.
- **.NET gauge on this box**: a stale `gpg` lock -- environment, not code.
- **Performance items** (droplet comparison, write amplification,
  cache defaulting, `log_file_max`, the writer-scaling
  chart, `concurrency-refresh` on a quiet box): need droplet runs and
  durability decisions, not code.
- **`test_tmp_retention_guard` timeout**: not reproduced; a watch item.
- **Go smoke tests fetch modules at test time**: needs a vendoring decision.
- **Out of scope**: macOS x86_64 wheels, and the in-process engine-selection
  notes in the porting history.

### 7.03 Rust packages (crates.io) -- open items, 2026-10-01


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


- **The six message-only bad-hint divergences in
  `tools/probes/arg_types_messages.py`** are gone from the Rust server
  (2026-10-20: it renders mongod's planner diagnostic); the Python server,
  deprecated, still names the hint.

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

- **Scope decision — `$densify` with explicit bounds fills each
  partition's tail in first-seen order.** mongod 8.2.11 fills the tails at
  the end in its hash table's order, which varied between runs (2026-10-16),
  so there is no order to match; everything before the tails follows
  mongod's `$_internalDensify` (RM-157).
- **Scope decision — the `$bitAnd` / `$bitOr` / `$bitXor` `$group`
  accumulators** are refused, as mongod 8.2.11 refuses them (`15952 unknown
  group operator`, measured 2026-10-16). (`$toHashedIndexKey`, the rest of
  what was RM-124, now hashes as mongod does.)
- **Out of scope — `$group` accumulator error RE-CODING is not reproduced.**
  mongod substitutes its own code for an error raised inside a `$group`
  accumulator (`4848401` for a division by zero, `7157706` for `$ln`), where
  `$match` / `$sort` keep the original 2 / 28766. There is no rule derivable
  short of probing every accumulator. The other 11 of 12 aggregation
  runtime-error wrapper shapes match mongod 8.2.11.
- **Scope decision — `$meta` inside an AGGREGATION stage follows where
  mongod's query layer hands the document over, not an operator table.**
  Since 2026-10-16 `recordId` and `indexKey` are given there
  (`secantus-commands/src/agg_meta.rs`, measured 8.2.11): the record id
  only in a `$project` the query layer takes, the index key through the
  stages that keep a document's metadata; since 2026-10-17 also `sortKey`
  and the `$geoNear` distance and point, from the stage that sets them
  (`agg_meta::rewrite_sort_metadata`). (Find-projection `$meta` is done.)
- **Decided 2026-10-16 (Joe) — record ids are numbered per collection
  from 1**, as on mongod, so `$meta: "recordId"` and `showRecordId` give
  mongod's numbers. This replaced the earlier deliberate divergence (one
  store-wide counter). The counter is rebuilt from the collection's
  largest record id, not stored: mongod does the same.

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
  (This was RM-134.) Also measure write amplification: bytes on disk per
  insert, SecantusDB versus mongod at a fixed document size. The leading
  hypothesis, and untested.
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

**-22% p99.9 for -5% throughput**, (it composed with `--oplog-async` then; that option is gone)
since they attacked different parts of the write path. Those numbers were taken under
the zlib block compressor; re-measure under the current lz4 default.

- [ ] **Configure a WAL compressor.** `zlib` is already linked into the
  vendored WiredTiger (`HAVE_BUILTIN_EXTENSION_ZLIB`) and is what the data
  tables use, so this is a connection-string change, not a build change.
  mongod uses snappy; zlib measured well here.
  **Decided (Joe, 2026-11-16): benchmark first -- write latency and throughput with the log uncompressed, zlib, and snappy (mongod's choice; add it to the WiredTiger build if needed) -- then choose on the numbers.**
  Backlog-ID: RM-131
  Verify: none -- WAL compressor config/perf decision

## The 2GB WAL default costs 19x the disk — for up to 19% of throughput (2026-08-21, corrected 08-22)

`log_file_max` is 128MB by default since 2026-10-13 (it was 2GB); the
re-measurement on Linux (`bench/results/mdb-droplet/2026-10-13-log-file-max/`)
found no throughput difference between 2GB / 512MB / 128MB / 32MB.

- [ ] **64 writers got less than 16 from `secantusd-mdb`** in that run
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
bounded how many writes were inside the storage engine at once. Measured: -22%
throughput to buy -12% p99.9. It relocates the queue rather than removing it.
Since 2026-11-09 it is gone: mongod's execution admission control replaced it
(RM-355, Joe's decision of 2026-11-08), with read and write pools that
throughput probing sizes by default, and `--write-tickets N` now fixes the
write pool (`storageEngineConcurrentWriteTransactions=N`). The measurement
above was of a fixed limit; the tail under probing is not yet measured.

### Where to go next

- [ ] **Cache defaulting** — the daemon caps at 4G; the sweep shows the tail is
  governed by cache headroom versus write rate, so the default deserves
  revisiting (and documenting) rather than being a fixed number.
  **Decided (Joe, 2026-11-16): adopt mongod's rule -- the larger of 50% of (RAM - 1 GB) and 256 MB -- and document it.**
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
  into `rustls-pki-types`. Still a direct dependency of `secantus-server`
  (`src/lib.rs`'s cert / key loading and `tests/tls.rs`). The PostgreSQL
  crates no longer use it (`secantus-pgwire`'s examples moved to
  `rustls_pki_types::pem::PemObject`, 2026-10-19).
- **RUSTSEC-2026-0196 / -0197** — `cgmath` 0.18.0 unmaintained + `swap_columns`
  UB. Pulled in transitively via the geo/spatial stack (`s2`). No fixed release;
  revisit if the geo dependency is ever swapped.

## Weekly validate.yml failures (2026-08-03 triage)


## The backlog verifier's pytest stalled once in CI (2026-10-21)

- [ ] **A local RM gate's `storage-engine` step failed once on
  `OSError: [Errno 55] No buffer space available` (2026-11-26).**
  `tests/test_getmore_batching.py::test_default_getmore_respects_16mb_byte_budget[rust]`
  failed in the client's `sendall` while it was inserting forty 1 MB
  documents, during the RM-701 subagent's first `gate_rm.py` run on this
  Mac (M2 Pro, 12 cores). Other gates and batch lanes were loading the
  machine at the time, with load averages from 11 to 24 that afternoon.
  It passed 3 of 3 on its own, and the step passed on the full re-run.
  ENOBUFS from a blocking `sendall` on macOS means the kernel's network
  buffer pool was exhausted, rather than the peer being slow to read. Find
  what held the buffers: count the sockets the gate's servers and the
  probe mongods left open (`netstat -m`, `lsof -i -P` during a gate),
  since stopped gates are known to leak mongods (RM-804). Fix the leak, or
  show that the test's own 40 MB send is what needs it, before calling it
  environmental.
  Verify: none -- seen once under load; reproduce by running the gate's storage-engine step beside another gate while sampling `netstat -m`
  Backlog-ID: RM-815

- [ ] **`land_batch.sh` reports a GitHub network error as a red snapshot
  (2026-11-26).** On PR #3048 (d08c97dcc) the MongoDB lander logged
  `Post "https://api.github.com/graphql": net/http: TLS handshake
  timeout` 11 s after opening the wait, then `RED on PR #3048 ...: not
  merging; bisect`, and exited 1. All 15 of the PR's checks were in fact
  green or skipped. `scripts/wait_ci.sh` exits 2 when any `gh` call fails
  (`gh repo view`, `gh pr view`, `gh api`), the same code it uses for a
  timeout, and the lander treats every non-zero code but 1-with-no-bad-
  checks as RED. A transient network failure then stops the landing and
  points a reader at a bisect that has nothing to find. Retry a failed
  `gh` call a few times with a back-off in `wait_ci.sh`, and give an
  unrecoverable tool failure its own exit code that the lander reports as
  such, not as RED. Both batch landers (pg-batch too) use these scripts.
  Verify: none -- tooling; reproduce by running wait_ci.sh with the network down for one call
  Backlog-ID: RM-825
- [ ] **CI's `apt-install` action fails on a third-party apt source that
  refuses every attempt (2026-11-27).** The runner image ships
  `packages.microsoft.com/ubuntu/26.04/prod` in `/etc/apt/sources.list.d/`,
  and on some runs it answers `403 Forbidden` to `InRelease` ("The
  repository ... is no longer signed"). `apt-get update` then exits 100
  on all three of `.github/actions/apt-install`'s attempts ("apt-get
  update exited 100; giving up after 3 attempts") and the job fails in
  its toolchain step before any test runs, while the Ubuntu archive and
  repo.mongodb.org answer fine. Seen twice: rm-batch PR #3073 (run
  45966663001, `pg-oracle (backlog)`) and pg-batch PR #3080 (8f45d44a,
  run 45998998087, job 140505550026, `mdb-binary (live)`); the same jobs
  pass an hour apart, so the 403 comes and goes. The retries are built
  for a stalled Ubuntu mirror, not for a source that refuses. No job
  installs from Microsoft's repository: move that source aside before
  `apt-get update` in the action (or treat an update failure confined to
  sources no job needs as non-fatal), so a refusal there cannot fail a
  job. Both batch landers read this failure as RED.
  Verify: none -- CI infrastructure; reproduce by adding a source that answers 403 to a runner's sources.list.d
  Backlog-ID: RM-835

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

## The storage model is not mongod's (tasks/mongod-storage-model-plan.md, 2026-11-19)

- [ ] **A `$near`'s `executionStats` leave out its inner scans and count
      other keys.** Found closing RM-777 (2026-11-24, mongod 8.2.11). For
      `{flat: {$near: [3, 4]}, t: 1}` over `{flat: "2d", t: 1}` mongod's
      GEO_NEAR_2D stage lists `inputStages` (one FETCH over IXSCAN for each
      ring its search read, the equality on `t` as the scan's filter and the
      ring's covering as its bounds) and examines 87 keys; `secantusd-mdb`
      lists none and examines 79. The winning plan's root, its index and
      the stage above it (the FETCH for a clause off the index) are
      mongod's.
      Verify: tests/backlog/test_rm_geo_wildcard_plans.py::test_a_near_stage_reports_its_inner_scans_and_counts_their_keys
      Backlog-ID: RM-780
- [ ] **A hinted `$near` over a compound `2dsphere` index whose leading
      field no predicate bounds is read nearest first, not in the index's
      key order.** Found closing RM-783 (2026-11-24, mongod 8.2.11). Hinted
      to `{t: 1, pts: "2dsphere"}`, a `$near` with `{z: 3}`, `t` under
      `$ne` or no other predicate is planned by mongod as a FETCH over a
      whole-index IXSCAN (bounds `[MinKey, MaxKey]` on both fields, the
      `$near` the fetch's filter) and returns the documents in the index's
      key order (`[12, 48, 21, 57, 30, 3, 39]` for `{z: 3}`).
      `secantusd-mdb` returns them nearest first, and `explain` shows
      GEO_NEAR_2DSPHERE for `t: {$ne: 2}`.
      Verify: tests/backlog/test_rm_geo_wildcard_plans.py::test_a_hinted_near_over_an_unbounded_leading_field_scans_the_whole_index
      Backlog-ID: RM-785
- [ ] **A hinted `$geoNear` stage over a compound `2dsphere` index whose
      leading field no predicate bounds answers, where mongod fails.**
      Found closing RM-783 (2026-11-24, mongod 8.2.11). An `aggregate`
      `$geoNear` with `hint: "t_pts"` and a `query` of `{z: 3}` or `{t:
      {$ne: 2}}` fails 9911902, "Executor error during aggregate command
      ...: Query returned a document that is unexpectedly missing the
      geoNear distance: {...}" (the first document of the whole-index scan,
      printed whole); `secantusd-mdb` answers the documents.
      Verify: tests/backlog/test_rm_geo_wildcard_plans.py::test_a_hinted_geo_near_stage_over_an_unbounded_leading_field_fails_in_the_executor
      Backlog-ID: RM-786
- [ ] **A `$nearSphere` on a GeoJSON point with no `$geometry` is
      refused.** Found closing RM-787 (2026-11-28, mongod 8.2.11). mongod
      reads `{t: 2, pts: {$nearSphere: {type: "Point", coordinates: [5,
      5]}}}` as a near query over `{t: 1, pts: "2dsphere"}`
      (GEO_NEAR_2DSPHERE, the leaf listed as given) and returns its
      documents; `secantusd-mdb` refuses the find and its explain with 2,
      "invalid argument in geo near query: 0". The same point under
      `$geometry` is planned as mongod plans it.
      Verify: tests/backlog/test_rm_geo_wildcard_plans.py::test_a_near_sphere_on_a_bare_geojson_point_is_a_near_query
      Backlog-ID: RM-847

## Found making mongod open our stores (storage-model phase 7, 2026-11-21)

Each was measured by starting mongod 8.2.11 on a store `secantusd-mdb`
wrote and stopped (the forward direction of RM-723).


## Found working RM-734 (index spec fidelity, 2026-11-24)

- [ ] **A `wildcardProjection` is not parsed as a projection.** Found closing
      RM-771 (2026-11-24, mongod 8.2.11). mongod parses the projection of a
      `$**` index as an inclusion or an exclusion and refuses one that mixes
      them (31254, 31253: "Cannot do exclusion on field b in inclusion
      projection"), one that computes a field (51271) and one whose paths
      collide (31250), each as "Failed to parse projection:
      wildcardProjection :: caused by :: ...". `secantusd-mdb` accepts all of
      them and builds the index from whatever its own reading makes of them.
      Verify: tests/backlog/test_rm_index_spec_fidelity.py::test_a_wildcard_projection_is_parsed_as_a_projection_is
      Backlog-ID: RM-794

- [ ] **The `clustered` field of an index spec is refused as unknown.** Found
      closing RM-771 (2026-11-24, mongod 8.2.11). `clustered` is a field of
      mongod's index spec: on its own it is 67 ("The 'unique' field is
      required when 'clustered' is specified"), with `unique` it is 6100900
      ("Cannot implicitly create a new collection with createIndex
      'clustered' option"), `false` is 6492800 and a string 14.
      `secantusd-mdb` answers 197 ("not valid for an index specification")
      to each. Beside any other refused field mongod answers 197 for it too,
      which is not measured further.
      Verify: tests/backlog/test_rm_index_spec_fidelity.py::test_a_clustered_option_on_an_index_spec_is_read_as_mongod_reads_it
      Backlog-ID: RM-795

- [ ] **A collection's `storageEngine.wiredTiger.configString` is checked
      and kept, but not applied to the collection's table.** Found closing
      RM-796 (2026-11-25, mongod 8.2.11), which applied an index's own and a
      collection's `indexOptionDefaults` one. mongod builds a collection's
      table from `type=file,memory_page_max=10m,split_pct=90,
      leaf_value_max=64MB,checksum=on,block_compressor=snappy`, then the
      collection's `configString`, then `exclusive=true,key_format=q,
      app_metadata=(formatVersion=1),log=(enabled=true)` (read off
      WiredTiger's own parse error for `==`, which `create` answers as 2
      with "22: Invalid argument - Error parsing '...' at offset N"): the
      string overrides the defaults, `key_format`, `app_metadata`, `log` and
      `exclusive` it cannot change, and `value_format=S` it can (measured
      against `collStats`' `wiredTiger.creationString`). `secantusd-mdb`
      makes the table from its defaults whatever the string says, and
      accepts `==`. The shared `create_user_configured_table` and
      `wt_parse_error_detail` in `crates/secantus-storage/src/lib.rs` are
      the building blocks. Out of reach and left alone: mongod's server-wide
      defaults (`--wiredTigerCollectionConfigString`).
      Verify: tests/backlog/test_rm_collection_config_string.py::test_a_collection_table_is_made_from_its_config_string
      Backlog-ID: 
      Backlog-ID: RM-802

## Found working RM-746 (storage-model phase 7b, 2026-11-22)

Each was measured by running the same writes on mongod 8.2.11 and on
`secantusd-mdb`, one-member sets, and reading what each logged.


## Found in CI on 2026-11-19

- [ ] **A killed retryable update batch's retry once disagreed with what
      the kill left applied (seen once in CI).** `tests/mongod_live/
      test_interrupts.py::test_a_killed_batch_keeps_the_statements_before`
      failed on PR #2887's `mdb-binary (live)` job (2026-11-19, run
      44516075354, job 135532292126; that PR changed no server code). The
      kill's reply matched mongod 8.2.11's, but after it the retry's
      `retriedStmtIds` was not the statements already applied, and the
      collection did not end with each of the fifty statements applied once
      (`(reply, True, False, False)` against mongod's `(..., True, True,
      True)`). Either the retry re-ran a statement the killed batch had
      committed -- a retryable write applied twice -- or it skipped one.
      Suspected mechanism, not proven: `retry_batch`
      (`crates/secantus-commands/src/session.rs`) drops the statements a
      batch noted when its reply is not ok, so a killed batch leaves the
      in-memory registry without the statements it committed, and only the
      per-statement `config.transactions` row and the oplog say what ran.
      Not reproduced here: 6 of 6 passes on a release build, 8 of 8 on the
      debug build under eight CPU burners (macOS arm64); CI is Linux x86_64.
      Backlog-ID: RM-725
      Verify: none -- intermittent: CI's Linux debug build met it once, and twenty local runs did not; the live test above is what catches it

## Found while working the storage model (2026-11-19)

- [ ] **RM-610's explain test measures wall time, which moves with the
      machine.**
      `tests/backlog/test_rm_found_2026_11_13_contested_reads.py::test_a_contested_read_costs_what_it_returns`
      compares how an `explain` at `queryPlanner` of a contested `find` grows
      in time from 10,000 to 100,000 documents against mongod's (within
      3x). Its sibling for a `find` with a limit and an `updateOne` timed
      the same way and failed on Linux CI about one run in two after
      storage-model phase 4a-4b (PRs #2918, #2920, `main` a70d23e2), and
      again on PG snapshot PR #3127 (run 46122467550, job 140909740969,
      2026-11-28), where mongod's own `find` went from 0.83 to 3.52 ms. That
      test now compares `explain`'s `allPlansExecution` counts instead (both
      servers 101 returned, 101 keys and 101 documents examined, for the
      winning plan and each of the three trial plans, at both sizes; mongod
      8.2.11, 2026-11-28). An `explain` at `queryPlanner` reports no counts,
      so this one can count only through `metrics.query.multiPlanner`,
      which mongod moves for it (202 works at both sizes) and this server
      does not keep (RM-856; `metrics.queryExecutor`, RM-846,
      does not move for an explain at `queryPlanner`); convert it when
      that lands.
      Backlog-ID: RM-728
      Verify: none -- a test-quality item: the behaviour is right on every run measured here; what is wrong is that the test can fail on timing alone

- [ ] **RUST MongoDB server: `serverStatus` has no
      `metrics.queryExecutor`.** mongod 8.2.11 counts the work every read
      and write does there: `scanned` (index keys examined), `scannedObjects`
      (documents examined), `collectionScans` `{nonTailable, total}`, and
      `profiler.collectionScans` `{nonTailable, tailable, total}`. A `find`
      of `{a: {$gt: 10}}` over 50 documents without an index moves it by 0
      keys, 50 documents and one collection scan; a contested `find` with a
      limit of 101 over `{a: {$gte: 0}, b: {$gte: 0}}` (indexes on `a` and
      `b`) by 101 and 101, at 10,000 documents and at 100,000. This server's
      `metrics` has no `queryExecutor` key. It is also what a test can count
      when `explain` reports nothing (RM-728). Found 2026-11-28.
      Verify: tests/backlog/test_rm_found_2026_11_28_query_executor.py::test_a_find_moves_the_query_executor_counters_as_mongod
      Backlog-ID: RM-846

- [ ] **A single-document `update` or `delete` that matches nothing tests its
      filter a second time.** A filter that draws `$rand` answers differently
      each time it runs, so the share of statements that match counts the
      runs. Over 1,500 statements `{$and: [{k: i}, {$expr: {$lt: [{$rand: {}},
      0.5]}}]}` (an unindexed `k`), mongod 8.2.11 matched 720 of `update`
      and 715 of `delete` with `limit: 1`, half, as it tests each document
      once; this server matched 1,133 and 1,125, three quarters: a statement
      the first test turns away is tried again. `multi: true`, `limit: 0`
      and `findAndModify` match half on both. A write that matches nothing
      reads the collection twice. Found 2026-11-28 converting RM-391's test
      to a count (RM-814).
      Verify: tests/backlog/test_rm_found_2026_11_28_single_write_retest.py::test_a_single_update_or_delete_tests_a_miss_once
      Backlog-ID: RM-855

- [ ] **RUST MongoDB server: `serverStatus` has no
      `metrics.query.multiPlanner`.** mongod 8.2.11 counts every plan trial
      there, `explain` at `queryPlanner` included, which reports no counts
      of its own: `classicCount`, `classicNumPlans`, `classicWorks` and
      `stoppingCondition.hitResultsLimit`. An `explain` at `queryPlanner` of
      a `find` over `{a: {$gte: 0}, b: {$gte: 0}}` with limit 101 (indexes on
      `a` and `b`) adds 1, 2, 202 and 2, over 10,000 documents and over
      100,000 alike. This server's `metrics` has no `query.multiPlanner`
      key. It is the count RM-728's explain test can compare in place of
      the time the explain took (`metrics.queryExecutor`, RM-846, does not
      move for an explain at `queryPlanner`). Found 2026-11-28.
      Verify: tests/backlog/test_rm_found_2026_11_28_multiplanner_metrics.py::test_an_explain_at_query_planner_counts_its_trial_in_server_status
      Backlog-ID: RM-856

- [ ] **A write to a collection waits for a running write's whole statement.**
      mongod 8.2.11 lets a second writer update a document while a
      `deleteMany` is still testing it: a `deleteMany` of one slow document
      (about 2 s to test) with an `update` of that document sent 0.7 s in
      returns the update at 0.71 s (`nModified` 1), tests the document again
      after its yield, finds it changed and deletes nothing (`n` 0, on macOS
      arm64 and on `mongo:8.2.11` Linux alike). This server answers the
      update at 4.14 s, after the delete, with `nModified` 0, and the delete
      deletes the document (`n` 1): each statement of a write holds the
      collection's `coll_lock` throughout (`secantus-storage`'s
      `delete_matching_single_txn` and the chunked paths). The update is a
      different statement; only a writer to the same document should wait,
      by write conflict. Found 2026-11-28 (RM-803); the regression test of
      RM-501 compares `serverStatus.batchedDeletes` instead, for want of it.
      Verify: tests/backlog/test_rm_843_concurrent_write.py::test_an_update_does_not_wait_for_a_running_delete_many
      Backlog-ID: RM-843
- [ ] **`secantusd-mdb` refuses `--setParameter internalQueryExecYieldPeriodMS`
      at startup.** mongod 8.2.11 takes it on the command line (the
      `mongod_rs_no_yield` fixture starts one with it); this server answers
      "secantusd-mdb does not take --setParameter 'internalQueryExecYieldPeriodMS'
      at startup" and exits, though `setParameter` takes it at run time.
      Found 2026-11-28 (RM-803).
      Verify: tests/backlog/test_rm_844_startup_yield_parameter.py::test_the_yield_period_is_a_startup_parameter
      Backlog-ID: RM-844
- [ ] **A batched delete's batch size and time are not server parameters,
      and a batch its time target cuts is not its own oplog entry.**
      mongod 8.2.11 sizes a deleteMany's batches by
      `batchedDeletesTargetBatchDocs` (10) and
      `batchedDeletesTargetBatchTimeMS` (5), both taken by `getParameter`
      and `setParameter`, and writes each batch it commits as one oplog
      entry: an `applyOps` of its deletes, a batch of one as a plain `d`.
      With the target at 3 documents, a deleteMany of `_id` 6 to 9 writes
      `applyOps` of 8, 7, 6 and then `d` of 9. This server answers
      `getParameter` of either with InvalidOptions, and its batch size is
      fixed (`secantus-storage`'s `batch_docs`). It also gives a whole
      staged batch one oplog group, so a batch the time target splits in
      `delete_chunk_txn` reads back as one `applyOps` (a lone tail as an
      `applyOps` of one), where mongod writes one entry per commit. Found
      2026-11-28 diagnosing PR #3139's mongod_live failure, where mongod
      cut ids 129 to 120 after 121 and logged 120 as a plain `d`.
      Verify: tests/backlog/test_rm_found_2026_11_28_batched_delete_params.py::test_a_batched_delete_commits_batches_of_its_target_size
      Backlog-ID: RM-848

- [ ] **Stopping a gate run leaves its tests' reference mongods running
      (2026-11-25).** `scripts/detached_run.py stop` on a `gate_rm.py`
      run ends pytest, but the `mongod_rs` / probe fixtures' `mongod`
      processes (`--dbpath .../T/secantus-probe-<pid>-mongod-*`) are
      reparented to PID 1 and keep running, each holding a port and a
      store. Four were found idle on 2026-11-25: two 7.5 hours old, from
      a gate a subagent stopped, and two 1.5 hours old, from one stopped
      for a rebase. Either the fixture starts mongod in its own process
      group and registers an exit handler that a SIGTERM reaches, or
      `detached_run.py stop` signals the whole session it started. Then
      the fixture's store is removed too.
      Verify: none -- tooling; reproduce by stopping a gate in `mongod-live` and listing `ps -Ao pid,ppid,command | awk '$2==1 && /secantus-probe/'`
      Backlog-ID: RM-804

- [ ] **A restore to the timestamp of a batched insert applies part of the
      batch.** `insert_many` of two documents is one `applyOps` entry in
      `local.oplog.rs` (as mongod 8.2 logs a batched insert), but
      `secantusd-mdb restore --to-timestamp <that entry's ts>` restores only
      the first document: the batch is kept as one entry per document
      underneath, and the entry shows the first one's timestamp. mongod
      applies an `applyOps` entry whole, at its one timestamp, so a restore
      to it should give both documents. Found when
      `tests/test_rust_binary_pitr.py::test_rust_binary_restore_to_timestamp`
      took its timestamp from the oplog through the server; that test now
      writes two separate inserts.
      Backlog-ID: RM-727
      Verify: none -- PITR restore is this server's own tool, with no mongod counterpart to compare; reproduce with the test above using insert_many


- [ ] **`count`, `distinct`, `update` and `delete` over an `_id` range of a
      clustered collection still plan through the `_id_` index.** A `find` is
      planned as mongod plans it since RM-739 (a scan of the table between
      two `_id`s, `CLUSTERED_IXSCAN`, or `EXPRESS_CLUSTERED_IXSCAN` for one
      `_id`); the other commands plan from the catalog with the clustered
      key still in it as an index. Measured on mongod 8.2.11 (2026-11-26),
      `_id`s 1..5: `count` of `{_id: {$gt: 1}}` is `COUNT <- CLUSTERED_IXSCAN`
      (`minRecord: 1`, `maxRecord: Infinity`), `count` of `{_id: 2}` the same
      with `[2, 2]` (no IDHACK), an `update` with `multi` `UPDATE <-
      CLUSTERED_IXSCAN`, a `delete` of the range `BATCHED_DELETE <-
      CLUSTERED_IXSCAN` and a `distinct` over it a lone `CLUSTERED_IXSCAN`;
      this server names `COUNT_SCAN`, `IDHACK`, `UPDATE <- FETCH <- IXSCAN`
      and a `FETCH <- IXSCAN`. The `EXPRESS_UPDATE` / `EXPRESS_DELETE` of one
      `_id` already name no index and examine no key. The fix is
      `plan::count`, `plan::distinct` and `plan::write` taking the cluster
      from the catalog as `plan::plan` does (`split_cluster`), and the write
      stages' counters (`docsExamined` of a range, as for a find) with them.
      Verify: tests/backlog/test_rm_clustered_explain.py::test_a_count_over_an_id_range_reads_the_clustered_scan, tests/backlog/test_rm_clustered_explain.py::test_a_distinct_over_an_id_range_reads_the_clustered_scan, tests/backlog/test_rm_clustered_explain.py::test_an_update_over_an_id_range_reads_the_clustered_scan, tests/backlog/test_rm_clustered_explain.py::test_a_delete_over_an_id_range_reads_the_clustered_scan
      Backlog-ID: RM-816

- [ ] **An `$or` of `_id` ranges over a clustered collection is one `COLLSCAN`
      under the `SUBPLAN`, where mongod reads an `OR` of `CLUSTERED_IXSCAN`s.**
      Measured on mongod 8.2.11 (2026-11-26): `{$or: [{_id: {$gt: 8}}, {_id:
      {$lt: 2}}]}` is `SUBPLAN <- OR <- [CLUSTERED_IXSCAN {minRecord: NaN,
      maxRecord: 2}, CLUSTERED_IXSCAN {minRecord: 8, maxRecord: Infinity}]`
      (documents examined 4 of 10 for ids 1..10, `works` 5); an `$or` with a
      branch that names no `_id` (`{_id: 2}` or `{y: 3}`) is a `COLLSCAN`
      under the `SUBPLAN` with the whole `$or` as its filter, as this server
      prints. `or_access` plans a branch from the catalog's indexes, and the
      clustered key is not one since RM-739: a branch needs the bounded scan
      as its candidate (`clustered_scan`), the `OR` its trial and its
      counters.
      Verify: tests/backlog/test_rm_clustered_explain.py::test_an_or_of_id_ranges_is_an_or_of_clustered_scans
      Backlog-ID: RM-817

- [ ] **`min()` / `max()` with a hint of the clustered key do not set the
      scan's ends.** On mongod 8.2.11 (2026-11-26), `find({_id: {$gt:
      0}}).hint({_id: 1}).min({_id: 2}).max({_id: 4})` is a `CLUSTERED_IXSCAN`
      with `minRecord: 2`, `maxRecord: 4` (the filter's own bound stays out
      of it: `$gt: 0` is rechecked, or taken where tighter); a `min` alone
      leaves `maxRecord: Infinity`. This server bounds the scan by the
      filter alone (`minRecord: 0`, `maxRecord: Infinity`), the `Query` it
      plans carrying only that `min` / `max` were given.
      Verify: tests/backlog/test_rm_clustered_explain.py::test_min_and_max_bound_the_clustered_scan
      Backlog-ID: RM-818

- [ ] **A read of a clustered collection goes through no plan trial and
      leaves nothing in the plan cache.** mongod 8.2.11 (2026-11-26) caches
      the winner of `{_id: {$gt: 1}, x: 3}` beside an `x_1` index
      (`$planCacheStats`: one active entry after the second run, its
      `cachedPlan` the `FETCH` over `x_1`, or a `CLUSTERED_IXSCAN` with its
      `minRecord` / `maxRecord` where that scan wins). `Storage::can_stream`
      is false for a clustered collection, so `order::planned` never reaches
      the trial, the plan cache or the buffered first batch, and the
      stream executor (`stream::stage_of`) has no ranged scan to run
      either (it declines a `CLUSTERED_IXSCAN`, which `explain` runs over the
      documents it holds). Streaming a clustered table by `_id` (the
      RecordId is the key) is the storage half; the ranged
      `stream::CollScan` the other.
      Verify: tests/backlog/test_rm_clustered_explain.py::test_a_contested_read_of_a_clustered_collection_is_cached
      Backlog-ID: RM-819

- [ ] **A `$group` over an `_id` range of a clustered collection is not
      reported as the SBE's clustered scan.** mongod 8.2.11 (2026-11-26)
      answers `[{$match: {_id: {$gt: 1}}}, {$group: {_id: null, n: {$sum:
      1}}}]` over `_id`s 1..5 with one top-level `queryPlanner` whose
      `winningPlan.queryPlan` is `GROUP <- CLUSTERED_IXSCAN` (`minRecord:
      1`, `maxRecord: Infinity`, `filter: {}`) over a slot plan whose
      `scan` has `minRecordIdSlot` and `maxRecordIdSlot`, reading 4
      documents; this server explains it as the `find`'s plan under a
      `$cursor` stage and a separate `$group`, as it does for an index scan
      (`sbe_layer` takes only a forward `COLLSCAN` with no range).
      Verify: tests/backlog/test_rm_clustered_explain.py::test_a_group_over_an_id_range_scans_the_clustered_range
      Backlog-ID: RM-820

- [ ] **A `find` hinted with a clustered key's own name is refused, and with
      `_id_` is accepted.** On mongod 8.2.11 (2026-11-26) a collection made
      with `clusteredIndex: {key: {_id: 1}, unique: true, name: "ci"}` is
      hinted `"ci"` (or `{_id: 1}`), and `"_id_"` is BadValue ("hint
      provided does not correspond to an existing index"); `explain` here
      says the same since RM-739, but the commands that run the hint
      (`storage::resolve_hint`, `util::hint_is_known`) know the `_id` index
      as `_id_` alone, so `find().hint("ci")` fails and `hint("_id_")` runs.
      Verify: tests/backlog/test_rm_clustered_explain.py::test_a_clustered_key_is_hinted_by_its_own_name
      Backlog-ID: RM-821

- [ ] **`{_id: {$gt: NaN}}` on a clustered collection is a plain
      `COLLSCAN`, where mongod bounds it (2026-11-26).** mongod 8.2.11
      explains it as `CLUSTERED_IXSCAN` with `minRecord` NaN and
      `maxRecord` Infinity, the number bracket. This server's
      `plan::rid_clause` bounds nothing for a NaN operand. Found by the
      RM-739 fix; probed, no gap test yet.
      Verify: tests/backlog/test_rm_clustered_explain.py::test_an_id_against_nan_is_a_bounded_clustered_scan
      Backlog-ID: RM-822

- [ ] **A partial index of a time-series collection is accepted whatever its
      filter, where mongod refuses one it cannot translate to the buckets
      (2026-11-28).** mongod 8.2.11 refuses `partialFilterExpression` of
      `$type` (by name or number), `$exists: false`, `$ne`, `$regex` and
      `$eq: null` on a time-series collection with code 5916301 ("Error
      translating non-metadata time-series predicate to operate on buckets:
      can't handle this predicate: { v: { $type: [ "number" ] } }", the
      `$eq: null` one "can't handle comparison to null"); `$in` and `$or` of
      comparisons are accepted. This server accepts `$type` and `$eq: null`
      and answers code 67 to `$exists: false`, `$ne` and `$regex`. Found
      probing RM-701.
      Verify: tests/backlog/test_rm_701_time_series_remainder.py::test_a_time_series_partial_index_must_translate_to_the_buckets
      Backlog-ID: RM-854

- [ ] **`{_id: {$gte: NaN}}` on a clustered collection is bounded `NaN`..`NaN`,
      where mongod bounds it `NaN`..`Infinity` (2026-11-28).** mongod 8.2.11
      explains it as `CLUSTERED_IXSCAN` with `minRecord` NaN and
      `maxRecord` Infinity (the whole number bracket, as `$gt: NaN`); this
      server's `maxRecord` is NaN. Found beside RM-822.
      Verify: tests/backlog/test_rm_clustered_explain.py::test_an_id_gte_nan_is_bounded_to_infinity
      Backlog-ID: RM-853

- [ ] **A `tailable` find of an `_id` point on an uncapped clustered
      collection is planned as `EXPRESS`, where mongod refuses it
      (2026-11-26).** mongod 8.2.11 refuses a tailable cursor on a
      collection that is not capped. This server picks the express plan
      first, and its explain shows `EXPRESS_CLUSTERED_IXSCAN`. Measured
      2026-11-28: the `find` itself is refused here as on mongod (code 2);
      it is the `explain` of the find (an `_id` point, `{}`, a range) that
      answers a plan here and code 2 on mongod. Found by the RM-739 fix.
      Verify: tests/backlog/test_rm_clustered_explain.py::test_a_tailable_read_of_an_uncapped_clustered_collection_is_refused
      Backlog-ID: RM-823

- [ ] **The winning plan of a multi-plan trial reports one `works` fewer
      than mongod (2026-11-26).** On a plain collection with two
      competing indexes, the winner's own `executionStages.works` is one
      lower than mongod 8.2.11's (2 against 3, 5 against 6). The rejected
      plans' counters match. `tests/mongod_live/test_plan_trial.py`
      compares the trial's accounting but says it leaves the winner's own
      run out, so nothing catches it. mongod's final run counts the work
      that reports EOF. Found by the RM-739 fix's probes.
      Verify: tests/backlog/test_rm_824_winner_works.py::test_the_winners_works_count_the_final_eof_run
      Backlog-ID: RM-824

- [ ] **A time-series collection's own namespace still has a table and a
      registry entry, and its buckets collection's catalog options are
      short.** mongod keeps `app.ts` as a view (one `system.views` document)
      over `app.system.buckets.ts`, with no table of its own; the buckets
      entry in `_mdb_catalog` has `options.validator` (the `$jsonSchema` of
      the bucket shape), `options.timeseries` (the time-series options with
      `bucketMaxSpanSeconds`), `clusteredIndex: true` and
      `md.timeseriesBucketsMayHaveMixedSchemaData: false`, and its `create`
      oplog entry logs the same (`create: system.buckets.ts` with those, then
      `createIndexes` for `m_1_t_1`, then the `system.views` `create` and
      insert). This server creates `app.ts` with a table, and the buckets
      with `{clusteredIndex: true}` alone. Compare with
      `tools/probes/mongod_store_layout.py --secantusd <binary>`; it belongs
      with phase 7's views.
      Verify: none -- a layout and catalog comparison with the phase 0 fixture (`tests/fixtures/storage_model/mongod-8.2.11-layout.json`), made by the phase 0c comparison tool; no client call shows it
      Backlog-ID: RM-740

## Found working RM-732, hashed indexes (2026-11-23)

Each was measured against mongod 8.2.11 beside a hashed-index probe
(`tests/backlog/test_rm_found_rm732.py`).

- [ ] **A time-series collection with a hashed index on its `metaField`
      accepts a measurement whose meta is an array.** mongod refuses it with
      the write error 16766 ("Error: hashed indexes do not currently support
      array values. Found array at path: meta") and stores nothing; this
      server stores the measurement, its bucket holding `meta: [1, 2]`, under
      an index that lists as built. The hashed index itself is built and
      served on the buckets as `meta` (RM-732); the bucket write does not
      check it.
      Verify: tests/backlog/test_rm_found_rm732.py::test_a_hashed_meta_index_refuses_an_array_meta_field
      Backlog-ID: RM-769

- [ ] **A `sort` by the field of a hashed index the query is hinted to
      leaves documents that compare equal in the wrong order.** The plan is
      right (SORT over FETCH over IXSCAN, as mongod's), and so is every
      other order the hashed scan gives; over 41 values and a document
      missing the field, mongod's sorter leaves the missing-field document
      before the null one and the NaN Decimal128 before the NaN double
      (`std::sort` over documents arriving in hash order), where this server
      leaves them as it read them. Plain indexes sorted this way match, so it
      is the hashed scan's arrival order or key reaching the sorter's
      emulation (`explain/order.rs`, `std_sorted`), not a general sort bug.
      Verify: tests/backlog/test_rm_found_rm732.py::test_a_sort_by_a_hashed_field_leaves_ties_as_mongods_sorter_does
      Backlog-ID: RM-770


