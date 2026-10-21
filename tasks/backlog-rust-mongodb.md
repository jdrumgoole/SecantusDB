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
- **`OP_COMPRESSED`** — no longer out of scope: the Rust server negotiates snappy, zstd and zlib since 2026-10-15; its `serverStatus` counters are RM-147.
- **Text search** (`$text`, `$meta: "textScore"`, text indexes) — would need a full-text index implementation.
- **Geo: exact mongod error-string matching** — chase work without a clear payoff unless a driver test pins exact wording.
- **`$where`** — runs JavaScript. We don't ship a JS runtime (confirmed by Joe 2026-10-16; MongoDB deprecated it in 8.0).
- **`$function`, `$accumulator`** (aggregation expressions) — same reason: both evaluate user-supplied JavaScript and need an embedded JS engine + sandbox + BSON↔JS shim layer. Would also require a `--javascriptEnabled` gate (mongod gates JS behind this; many prod deployments disable it). Adding the runtime would let us implement these three operators + `mapReduce` together for ~600–1000 LOC + a heavyweight binary dep (PythonMonkey / QuickJS / V8 — each with maintenance trade-offs). Most pipelines that reach for `$function` can be expressed in the existing aggregation expression library. **`lang: "python"` as a SecantusDB extension was considered and rejected (May 2026)**: would break the conformance contract (pipelines that work locally would explode in production with `Unrecognized lang value: 'python'`), and CPython's sandboxing story is worse than embedded JS engines. If a real user need ever surfaces, the right escape hatch is a server-side trusted-plugin registry — `{$secantusFunction: {name: "<pre-registered>", args: [...]}}` — not user code at query time.
- **`mapReduce`** — same JS-runtime dependency as `$where` / `$function` / `$accumulator`, and deprecated by MongoDB (removed from the Stable API in 5.0). **WILL NOT BUILD** (decided 2026-09-27). The canonical `emit(this.<field>, 1)` + `values.length` "count by field" pattern is translated to an equivalent `$group` aggregation; anything else, a named `out: "<coll>"` included, is refused as a `--noscripting` mongod 8.2.11 refuses it (31264, re-measured 2026-10-20). mongo-java-driver's `testMapReduceWithGenerics` writes to a NAMED output collection and so fails permanently; that failure is accepted rather than tracked.
- **Profiling: execution statistics in `system.profile` entries** — no longer out of scope: decided in scope 2026-10-16 (Joe), counted during execution. Tracked as RM-023. A `$jsonSchema` profile filter still reads back as given, where mongod expands it.

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
  query, 2 in an aggregation `$match`, 31264 for mapReduce); MongoDB
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
- [ ] **RUST MongoDB server: the `Slow query` line does not say where the time
      went.** `mongod`'s carries `storage.timeWaitingMicros` (cache, schema
      lock) and bytes written by the operation; ours has only the duration.
      WiredTiger keeps these per session (`statistics:session`), but the
      storage layer opens a session per operation and exposes none of it.
      Also not on the line: `keysExamined` and `docsExamined`. Those count
      the plan's own work; `explain` derives them only by reading the whole
      collection, which a log line must not do, so they need counters from
      the read itself, and PER BATCH: mongod 8.2.11 counts each `find` /
      `getMore` batch's own work (2, 2, 2, then 0 for a `batchSize: 2` read
      of 6 documents), so a whole-query `explain` total would be wrong.
      A `getMore` line also lacks the cursor's `planSummary` (mongod repeats
      it on every batch) and `cursorExhausted: true` on the last one.
      (The command document, `planSummary` on the opening command,
      per-batch `nreturned` and `reslen` are on the line since 2026-10-16.)
      **Decided 2026-10-16 (Joe):** count keys and documents examined during
      execution (with the streaming cursors), and keep the originating
      command on a cursor, so the `Slow query` line and `system.profile`
      entries carry the real counts. Derived counts from `explain`'s walk
      are not to be used.
      Backlog-ID: RM-023
      Verify: tests/backlog/test_rm_001_028.py::test_slow_query_line_says_where_the_time_went

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

- [ ] **`$expMovingAvg` refuses a null, missing or Decimal128 input.**
      `ema_values` defers on any input `as_number` does not read, so a
      partition with one such row answers code 2. mongod 8.2.11 averages
      past them: a null or missing row answers null and leaves the average
      where it was, and a Decimal128 input makes the average decimal.
      Backlog-ID: RM-225
      Verify: tests/backlog/test_rm_found_2026_10_21.py::test_exp_moving_avg_over_null_missing_and_decimal
- [ ] **Window specs mongod refuses answer code 2, not mongod's code.**
      `$linearFill` without a one-field `sortBy` (mongod: 605001), `$shift`
      with a `window` (9, "$shift does not accept a 'window' field"), range
      bounds with a two-field `sortBy` (5339902) and a range window over a
      null sort value (5429414, "Invalid range: Expected the sortBy field to
      be a number, but it was null"). Each is a deferral where a refusal by
      name (`Fallback::mongo(code, msg)`) belongs.
      Backlog-ID: RM-226
      Verify: tests/backlog/test_rm_found_2026_10_21.py::test_window_spec_errors_carry_mongods_codes
- [ ] **`$convert` to a target it cannot produce answers code 2, and
      ignores `onError`.** `convert_target` defers on any `to` outside the
      nine convertible types (`expressions.rs`). mongod 8.2.11 tells three
      cases apart: an unknown name ("uuid", "foo", `{type: "uuid"}`) is 2
      "Unknown type name: uuid", `onError` or not; a number that is no BSON
      type is 9 "In $convert, numeric value for 'to' does not correspond to
      a BSON type: 99"; a real BSON type `$convert` cannot produce ("array",
      "timestamp", "null", "regex", 4) is 241 "Unsupported conversion from
      string to array in $convert with no onError value" -- and with
      `onError` mongod answers the `onError` value, where this server
      errors. Found by `tests/mongod_live/test_operator_tail.py`, 2026-10-21.
      Backlog-ID: RM-228
      Verify: tests/backlog/test_rm_found_2026_10_21.py::test_convert_to_a_target_it_cannot_produce
- [ ] **A change stream's post-image is the document now, not as the
      update left it.** On a collection with `changeStreamPreAndPostImages:
      {enabled: true}`, `fullDocument: "whenAvailable"` / `"required"` makes
      mongod 8.2.11 answer each update event with the post-image it stored
      when that update ran: two updates setting `a` to 2 then 3 read back as
      `a: 2`, `a: 3`. This server looks the document up when the event is
      read (`a: 3`, `a: 3`) -- `updateLookup`'s semantics -- so once a later
      write deletes it, "whenAvailable" answers null and "required" fails
      the stream (47, post-image not found). A silent wrong answer.
      Pre-images here already match mongod
      (`tests/mongod_live/test_change_streams.py`); probe how mongod
      derives the post-image before building it. Found 2026-10-21.
      Backlog-ID: RM-229
      Verify: tests/backlog/test_rm_found_2026_10_21.py::test_change_stream_serves_the_stored_post_image
- [ ] **A retryable write retried while its first attempt is still running
      runs twice.** A driver that times out on a slow write retries it with
      the same `lsid` and `txnNumber`. mongod 8.2.11 makes the second
      attempt wait for the session the first has checked out, then answers
      it from the first's result: both answer `{n: 30000}`. This server runs
      the two side by side, and whichever comes second answers E11000 on the
      documents the other wrote (`n: 0`), so the driver reports a failed
      insert that succeeded; an update or delete retried this way could
      apply twice (not yet measured). Found 2026-10-21 by RM-187's
      regression test on CI's slower runner, where a 5,000-document insert
      at an 8 MB cache outlasted pymongo's 20 s socket timeout. Sequential
      retries (the first attempt finished) are answered correctly
      (`tests/mongod_live/test_retryable_writes.py`).
      Backlog-ID: RM-230
      Verify: tests/backlog/test_rm_found_2026_10_21.py::test_a_retry_waits_for_the_attempt_still_running
- [ ] **`$near` returns documents tied at one distance in the macOS build's
      order, not the Linux build's.** mongod's builds disagree: for four
      points at distance 1, Linux x86-64 8.2.12 (CI) answers `[2, 1, 3, 0]`,
      macOS arm64 8.2.11 / 8.2.12 answer `[2, 3, 0, 1]`, and this server
      returns the macOS order. **Decided (Joe, 2026-10-21): the Linux build
      is the exemplar** where builds differ. The tie order is not a rounding
      matter (the distances are exact), so probe what orders Linux's ties
      before building. The other build split measured, a window `$stdDev`'s
      last digit (the macOS build fuses `count * x - sum` into one rounding),
      was fixed in the same batch (`windowfields.rs` rounds twice, as Linux
      does; `test_std_dev_window_rounds_as_the_linux_build`). Not yet
      measured on Linux, and written to the macOS build's fused arithmetic:
      the `$group` deviation (`group.rs`, `delta.mul_add`) and the 2d
      distances (`geohash2d.rs`, two `mul_add`s).
      Backlog-ID: RM-231
      Verify: tests/backlog/test_rm_029_056.py::test_near_ties_in_scan_order

### 7.086 The Rust MongoDB server, audited -- 2026-10-20

An audit of the command, expression and wire layers, each finding reproduced
against mongod 8.2.11 (standalone, or a one-member replica set where it
needs sessions) with a `secantusd-rs` built from tree `af2629ec`. The gap
tests are in `tests/backlog/test_rm_found_2026_10_20b.py` and ask mongod
live. Not filed here: `$rankFusion` / `$score` / `$scoreFusion` (RM-198),
`j: true` durability (RM-186), `storageSize` (RM-040).

- [ ] **A snapshot read at a point in time reads the latest data.**
      `readConcern: {level: "snapshot", atClusterTime: T}` outside a
      transaction is only checked for being too old (239, §7.078);
      `T` never chooses what is read (`secantus-commands/src/lib.rs`, the
      read-concern check). Measured: insert `n: 0`, take the `ping`
      `operationTime` T, set `n: 9`, then find at T: mongod answers 0, this
      server 9. A `start_session(snapshot=True)` session is the same: it
      reads (0, 7) across another client's write, where mongod reads (0, 0).
      A silent wrong answer. WiredTiger can read at a timestamp
      (`read_timestamp`), which is mongod's own mechanism.
      Backlog-ID: RM-210
      Verify: tests/backlog/test_rm_found_2026_10_20b.py::test_snapshot_read_at_cluster_time,tests/backlog/test_rm_found_2026_10_20b.py::test_snapshot_session_keeps_its_snapshot
- [ ] **Aggregation has no memory limits and nothing spills.** Over 110
      one-megabyte documents mongod 8.2.11 refuses: `$group` with
      `allowDiskUse: false` (292 QueryExceededMemoryLimitNoDiskUseAllowed),
      an array past 100 MB from `$push` or `$addToSet` (146
      ExceededMemoryLimit), `$setWindowFields` (5643011), a `$lookup`
      result (4568) and `$facet` (4031700). This server runs every one,
      holding it all in memory; `allowDiskUse` is only type-checked
      (`aggregate.rs`). A blocking `$sort` already agrees. mongod's
      mechanism is per-stage memory accounting with spilling to disk when
      `allowDiskUse` permits it. **The cost is far past the data's size**
      (measured 2026-10-21, debug build): the gap test's 110 MB of documents
      take `secantusd-rs` to a 14.5 GB peak RSS, where mongod peaks at 90 MB
      refusing them -- enough to kill a 16 GB CI runner, so CI's verifier
      runs with `--skip RM-211`; run its gap test by hand.
      Backlog-ID: RM-211
      Verify: tests/backlog/test_rm_found_2026_10_20b.py::test_aggregation_memory_limits
- `$convert` to binData subtype 2 (the deprecated "old binary") is
  written with the old format's inner length, as the bson crate writes
  every subtype-2 value. mongod 8.2.11 writes the bytes alone, which is not
  a valid subtype-2 value: pymongo refuses to decode the reply. Out of
  scope; nothing reads the old subtype as mongod emits it.
- [ ] **`$planCacheStats` and `$queryStats` are unknown stages** (40324).
      mongod 8.2.11 runs each on a one-member replica set (an empty result
      on a fresh one, filling as queries run, so an empty answer here would
      be a stand-in). `$planCacheStats` needs a plan cache, which this server
      does not keep (§7.075); `$queryStats` needs query-shape statistics,
      which RM-170 (`includeQueryStatsMetrics`) also lacks.
      (`$listSampledQueries` landed 2026-10-20: it lists none, truthfully,
      as no query analyzer exists here.)
      Backlog-ID: RM-215
      Verify: tests/backlog/test_rm_found_2026_10_20b.py::test_diagnostic_stages
- [ ] **The operation rows `currentOp` lists lack mongod's detail.** Since
      2026-10-20 a registry of running operations stands behind
      `currentOp`, `$currentOp` (with `idleCursors`) and `killOp`, which
      ends the operation it names with 11601 wherever it next checks: a
      waiting getMore, or any scan's deadline poll. Open:
      - a row has no `currentOpTime`, `lsid`, `planSummary`,
        `queryFramework`, `numYields`, `locks`, `queues` or `waitingForLock`;
        a getMore's `cursor` block has only its id; an idle cursor's has no
        `createdDate`, `lastAccessDate`, `nDocsReturned`, `nBatchesReturned`
        or `originatingCommand` (the cursor registry does not keep them; its
        clock is monotonic);
      - `$currentOp`'s `allUsers`, `idleConnections` and `idleSessions`
        options are not applied.
      Backlog-ID: RM-216
      Verify: none -- the missing fields are mongod's internal accounting; the registry's own tests are backlog_fixed
- [ ] **Four commands mongod runs are 59 CommandNotFound here.**
      mongod 8.2.11 (one-member replica set) answers each with success or
      an error of its own:
      - diagnostics: `getDiagnosticData` (mongod's FTDC sample);
      - settings: `setQuerySettings` / `removeQuerySettings` (each setting
        is keyed by mongod's `queryShapeHash`, which is not a plain SHA-256
        of the shape's BSON -- the debug shape, representative literals and
        likely extra fields were tried, 2026-10-20 -- so the hash needs
        mongod's own shape serialization first), `autoCompact` (needs
        background compaction, which RM-192 lacks).

      `filemd5` with `partialOk` is refused by name (115): its `md5state` is
      mongod's own MD5 implementation's state. Thirteen of the original
      seventeen landed 2026-10-20 (`applyOps`, `rotateCertificates`,
      `validateDBMetadata`, `dbCheck`,
      `invalidateUserCache`, `replSetGetRBID`,
      `replSetFreeze`, `replSetMaintenance`, `dropConnections`,
      `releaseMemory`, `filemd5`, `setUserWriteBlockMode`,
      `setClusterParameter`, whose
      `defaultMaxTimeMS` limits reads; the other parameters govern sharding,
      queryable encryption, search and serverless change streams, and are
      kept and reported only). Each left is its own slice, and query
      settings are a real feature rather than a reply. `applyOps` still
      refuses a document over 16 MB (mongod applies one a little larger,
      as oplog application allows) and `importCollection`,
      `createDatabaseMetadata` and `modifyCollectionShardingIndexCatalog`
      entries (115). `dbCheck` runs before it answers (mongod runs it in the
      background), walks a whole database's collections in name order
      (mongod's is its catalog's internal order), does not apply
      `maxBytesPerBatch` / `maxBatchTimeMillis` / `maxCountPerSecond`, and
      refuses `validateMode: "extraIndexKeysCheck"` (115).
      Backlog-ID: RM-219
      Verify: tests/backlog/test_rm_found_2026_10_20b.py::test_commands_mongod_runs_are_commands_here
- [ ] **A string that is not valid UTF-8 is refused.** An insert whose
      string holds the bytes `0xff 0xfe` answers 1 "BSON error: utf-8
      encoding error"; mongod 8.2.11 stores it (n: 1). Data mongod keeps is
      refused, and data read back from a mongod dump could not be
      restored. Reading strings as raw bytes where they are not inspected
      would keep them.
      Backlog-ID: RM-222
      Verify: tests/backlog/test_rm_found_2026_10_20b.py::test_insert_string_with_invalid_utf8
- [ ] **`local.oplog.rs` entries are not mongod's shape.** Since
      2026-10-20 an update (mongod's `$v: 2` diff, stored beside the
      change-stream description as `o.md`), a batch of inserts (one
      `applyOps` per group of 500 documents or 256 KiB), a committed
      transaction (one `applyOps` under its session), a multi-document
      delete (`applyOps` batches of ten), a delete, a drop
      (`o2: {numRecords}`), a rename (`stayTemp`), a one-index
      `createIndexes` and a `dropIndexes` read as mongod 8.2.11 writes them,
      with its `t` and `v`, and a retryable insert, update or delete with
      its session (`lsid`, `txnNumber`, `stmtId`, `prevOpTime`). Open:
      - a `bulkWrite` command's writes carry no session (retryable
        `insert` / `update` / `delete` / `findAndModify` carry theirs, a
        `findAndModify` its `needsRetryImage` and its image in
        `config.image_collection`, since 2026-10-20); and an image is never
        deleted, where mongod deletes a session's image (an oplogged delete)
        when its logical-session reaper expires the session, which this
        server does not run;
      - an index build over documents is one `createIndexes`, where mongod
        writes, for the whole command's new indexes as ONE build (measured
        8.2.11): an insert into `config.system.indexBuilds` (`{_id: build
        uuid, collectionUUID, commitQuorum: "votingMembers", indexNames}`),
        `startIndexBuild` (`{startIndexBuild: coll, indexBuildUUID,
        indexes}`, with `ui`), an update adding `commitReadyMembers:
        ["<host:port>"]`, then `commitIndexBuild` -- or, when the build
        fails, `abortIndexBuild` with a `cause` (the error document, `ok:
        false` first) -- and the row's delete. An empty collection, or one
        the command creates, gets the single `createIndexes`. The commit has
        to stay atomic with the catalog write (`commit_index`), the
        handler's per-spec undo has to become one build's abort, and the
        change-stream projector and replay have to read `commitIndexBuild`
        as they read `createIndexes` now.
      A tool that reads the oplog (`mongodump --oplog`, then
      `mongorestore --oplogReplay`) gets entries mongod would not write.
      Backlog-ID: RM-224
      Verify: tests/backlog/test_rm_219_apply_ops.py::test_oplog_entries_have_mongods_shape

### 7.085 The shared Rust storage, audited -- 2026-10-20

A read-only audit of `secantus-storage` / `secantus-wt`, the layer both
Rust servers use. The two items with gap tests were reproduced against
mongod 8.2.11 with a `secantusd-rs` built from tree `af2629ec`; the rest
were confirmed by reading the code path and need a fault no quick test can
inject (a panic, a power loss, a failing checkpoint).

- [ ] **An index build holds every write to its collection off until it
      finishes.** The build is chunked now (fixed 2026-10-20: it had run as
      one transaction and never returned under a small cache; at 32 MB it
      builds over 300,000 300-byte documents in 4 s). But it still holds
      the global storage lock and the collection lock from its first read
      to its catalog row, so every DDL, and every write to that collection,
      waits for the whole build. mongod 8.2.11's build lets writes go on: a
      side table records the writes made while it scans, and they are
      applied before the index commits, within
      `maxIndexBuildMemoryUsageMegabytes`. A build that sorted externally
      would also stop holding all its entries in memory at once.
      Backlog-ID: RM-187
      Verify: none -- a concurrency budget (writes during a long build), not an answer
- [ ] **`count({})`, `collStats`, `dataSize` and `dbStats` walk every
      row.** They stream now, in constant memory (fixed 2026-10-20: each
      read the whole collection into a `Vec`, and `dbStats` did that three
      times per collection). But each still takes time proportional to the
      collection, where mongod answers all of them from its size-and-count
      metadata (the size storer), kept up to date by every write. RM-080
      covers `find` / `aggregate` cursors; RM-087 covers fast-count
      visibility, not its cost.
      Backlog-ID: RM-191
      Verify: none -- a time budget, not an answer
- [ ] **Disk space is never given back, and there is no repair path.**
      `compact` answers `bytesFreed: 0` without doing anything
      (`secantus-commands/src/capped_admin.rs`, "There is never anything to
      free here"), and `secantus-wt` has no binding for
      `WT_SESSION::compact`. Collections share the 16 document shards, one
      index-entries table and the oplog shards, so a dropped or emptied
      collection's pages are only ever reused inside those files. mongod runs
      WiredTiger's compaction (and `autoCompact` in 8.x). The reply itself is
      not testable from a small shape: on 40,000 documents with 95% deleted,
      mongod 8.2.11 also reported `bytesFreed: 0`. There is no
      `--repair` / salvage either; `secantus-pg/src/main.rs` already
      recognises `WT_TRY_SALVAGE` and has nothing to offer.
      Backlog-ID: RM-192
      Verify: none -- reclaiming disk space; no small shape where mongod reports bytesFreed > 0 was found

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
- [ ] **`explain` of `$score`, `$rankFusion` and `$scoreFusion` lists the
      stage as written.** The stages run since 2026-10-20 (RM-198, fixed),
      desugared as mongod desugars them, but `explain` shows the stage
      itself where mongod 8.2.11 shows its desugaring: for `$rankFusion` of
      two pipelines, `$cursor`, `$replaceRoot`, `$_internalSetWindowFields`
      (`$rank`), `$addFields`, a `$unionWith` holding the same for the
      second pipeline, `$group`, two `$addFields`, `$setMetadata`, `$sort`
      (`sortKey: {score: -1, _id: 1}`) and `$replaceRoot`; `$score` is
      `$setMetadata` and, normalised, `$_internalSetWindowFields` with
      `$minMaxScaler`. Also not reproduced: either fusion stage as the first
      stage of a `$facet` sub-pipeline, which mongod accepts and answers
      with the documents in collection order (in a `$lookup` or
      `$unionWith` sub-pipeline both run, as on mongod).
      Backlog-ID: RM-200
      Verify: tests/backlog/test_rm_added_in_82.py::test_explain_of_rank_fusion_is_its_desugaring
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
- [ ] **`serverStatus.locks`: `$merge`'s retries, the transaction commands,
      and the commands not measured.** The lock manager (`lock_manager.rs`,
      RM-034) takes, outside a transaction, the rounds mongod 8.2.11 takes
      (`statement_rounds`, measured per command): reads, the write commands,
      `findAndModify`, `create` / `drop` / `dropDatabase`, index builds,
      `dropIndexes`, `$out`, `dbStats`, and `$merge`, which takes a write
      round per document as it writes (`write_rounds`, two more when it
      creates its target), all count as mongod's (the regression tests in
      `tests/backlog/test_rm_185_locks.py`). An index build's count of the
      global IS is not fixed on mongod: its build runs on a thread of its
      own and the command takes the IS each time it looks at it, 1 to 4
      times in one shape measured; this server takes the least mongod was
      seen to, and the tests compare that count as a floor. Still different:
      `$merge` with `whenMatched: "fail"` that meets a duplicate retries its
      batch on mongod, taking more rounds (3 for 4 documents with one
      duplicate, 4 with two) where this server takes the batch's one; the
      transaction commands, which need a replica set, were measured on a
      one-member mongod 8.2.11 set (2026-10-17) cannot be pinned the same
      way: its replication threads take the global IS about twice in any
      window, and its writes take rounds for the oplog and the session table
      too (a plain insert three where a standalone takes one; a one-insert
      transaction with its commit 3 collection IX, 2 global IX and 8 on the
      state transition lock), machinery this server does not run; and no
      other command was measured, so each still takes the generic chain. mongod's internal resources
      (`Metadata`, `oplog`, `Mutex`, `MultiDocumentTransactionsBarrier`) are
      locked by its background threads only and are absent here.
      **Time-based plan yields** (measured 2026-10-21): mongod's executor
      yields after `internalQueryExecYieldPeriodMS` (10 ms), and each re-take
      of a lock counts. `$out` reads 7 global IS on Linux x86-64 8.2.12 (CI,
      the exemplar build: Joe, 2026-10-21), 8 on macOS arm64 idle and 9 on a
      loaded Mac; with the period off, macOS reads 7 too. So this server
      takes Linux's 7, and the regression tests' reference mongod runs with
      the period off, which gives Linux's counts on any machine and stops
      them moving with load. Still unexplained: at 12 test workers on a
      12-core Mac, `$merge creating its target` read 4 global IS (2 idle) in
      all five reference runs, with yielding off as well; at CI's 4 workers
      it reads 2.
      Backlog-ID: RM-185
      Verify: tests/backlog/test_rm_185_locks.py::test_lock_counts_of_merge_fail_with_duplicates
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
- [ ] `buildInfo` has no per-OS block; `collStats` has no `wiredTiger` or
      `indexDetails` (they are WiredTiger statistics of the collection's
      and each index's own table, and here collections share a documents
      shard table and every index one entries table, so no per-collection
      statistics exist -- which is also why `storageSize` in `collStats` /
      `dbStats` is `dataSize` copied rather than the bytes on disk, and
      `indexSize` an estimate; the shard table's own would count other
      collections too); `latencyStats` is zeros; `config.system.sessions`
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
      Backlog-ID: RM-040
      Verify: tests/backlog/test_rm_029_056.py::test_coll_stats_wiredtiger_and_index_details

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
  scores, AND_SORTED intersections, and one plan per predicate on an
  index's array field. That trial also picks the read order.
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
- [ ] **A build before number-key format 2 writing into a store already at
      format 2.** An older build of either server (the PyPI wheel included)
      that inserts a document with a numeric `_id` writes a format-1 `_id`
      key, which the store never re-keys: a lookup by that `_id` misses the
      document. An index such a build adds is caught, because it has no
      `numberKeys` stamp and is rebuilt on the next open; the `_id` keys
      have nothing to stamp. Format-1 keys are recognisable
      (`sortkey::is_format1_number_key`), so a sweep on open would close it.
      Backlog-ID: RM-153
      Verify: none -- needs an older build to write the store
- [ ] **The cost of the plan trial.** Release builds against 43463aa6
      (2026-10-15): a query two indexes could serve costs about 5 times as
      much on 2,000 documents (`{a: 5, b: 3}` over `a_1` and `b_1`), 2
      times for a range; the trial reads and decodes the collection and
      encodes its keys. `update_one` by an indexed value is 0.9 times the
      build before and 1.2 times an update by `_id`
      (`order::store_order_is_the_plans`); `{a: {$ne: 5}}` over `a_1` 0.7
      times, a regex prefix 0.01 times, now that the router reads the index.
      The lever is mongod's plan cache (RM-154).
      Backlog-ID: RM-056
      Verify: none -- performance budget (timings against an earlier build)
- [ ] **mongod's plan cache is not reproduced.** mongod runs the trial once
      per query shape, caches the winner and reuses it, replanning when it
      does badly, until the entry is evicted. Here every query runs its
      trial. That decides which plan, and so which order, a repeated query
      shape gets: mongod's second `{a: 1, b: 9}` uses the plan cached for
      the shape, which this server picks afresh. It is also the cost lever
      for RM-056.
      Backlog-ID: RM-154
      Verify: none -- needs a scenario measured on mongod first; file a gap test with it

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

A document `_id` takes a claim by value in `UNIQ_TABLE`
(`Storage::document_id_claim`), on top of the `_id` index keyed by BSON
bytes (`encode_id_key`, on disk and shared with the Python server). A
document stored before that is given its claim the first time this version
opens the store (`backfill_document_id_claims`, once for a store; a row in
`UNIQ_TABLE` under an empty namespace records it). Two stored documents
already equal by value both stay, and the older holds the claim.

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
      `cpuNanos`, but not `fromPlanCache` (no plan cache), `bytesRead` /
      `readingTimeMicros` (no WiredTiger read statistics plumbed) or
      `numInterruptChecks` (a different mechanism).
      **Decided 2026-10-16 (Joe):** leave it open; no stand-in values. Build
      it when those mechanisms exist.
      Backlog-ID: RM-170
      Verify: tests/backlog/test_rm_found_2026_10_16.py::test_include_query_stats_metrics_adds_a_metrics_document

### 7.077 `update` and `findAndModify` -- 2026-10-12

`tools/probes/update_commands.py`, mongod 8.2.11: 30 scenarios, 1,295
replies over a raw connection. Each case runs against a collection of its
own and compares the reply AND the documents left behind. The Rust server
differs on 17 of 1,295. Pinned by
`crates/secantus-storage-adapter/tests/update_commands_wt.rs`.

What is left, on the RUST MongoDB server:

- [ ] **The field order of a document upserted from two or more
      equalities.** mongod: `{a: 1, b: 2, c: 3, d: 4}` gives `c, a, b, d`;
      `{d: 1, c: 2, b: 3, a: 4}` gives `d, b, a, c`; an explicit `$and`
      keeps the order written. This server sorts them.
      **Decision needed (2026-10-17):** there is no rule to find. mongod
      collects the equalities in an `EqualityMatches`, a `StringDataMap`
      (an `absl::flat_hash_map`), and builds the document in its iteration
      order (`update_driver.cpp` / `path_support.cpp`, 8.2), and absl seeds
      its hash per process. Measured on three mongod 8.2.11 processes at
      once: `{a, b, c, d}` gave `c, a, b, d` / `c, a, b, d` / `b, a, c, d`,
      and one process ordered `{a, b, c, d}` and `{d, c, b, a}` differently.
      Options: (1) a hash order of this server's own, seeded per process --
      the mechanism, but never the same order as a given mongod; (2) keep
      the sorted order and record the difference as a scope decision, since
      no fixed order can match a server that has none.
      Backlog-ID: RM-059
      Verify: tests/backlog/test_rm_057_084.py::test_upsert_field_order_from_several_equalities
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

- [ ] **`aggregate` stages mongod runs in its slot-based engine** (14
      replies). mongod 8.2.11 pushes a `$group` not served by a
      DISTINCT_SCAN, and `$count`, `$sortByCount`, `$bucket` and `$lookup`,
      into SBE and explains them with `explainVersion: "2"`, a
      `winningPlan` of `{queryPlan, slotBasedPlan}` and executionStats made
      of SBE stages with slot expressions. This server reports the classic
      `$cursor` / `stages` shape. The `queryPlan` tree could be derived
      from the rules this server follows; `slotBasedPlan` and the SBE
      statistics are the engine's own programs and are not reproduced.
      Sending `queryPlan` alone is Joe's call. `explain_commands.py`
      replies: `aggregate: stages after the cursor` 6, 7, 8, 11;
      `aggregate: $group and $lookup` 1, 2, 3, 5, 6, 7; `executionStats:
      aggregate` 5, 6; `a collection that does not exist` 9. The rest of
      the aggregate explain is reproduced (the cursor's dependency
      projection, `$groupByDistinctScan`, `$match` and `$sort` moved ahead
      of `$unwind`, stages as mongod parses them, per-stage counts under
      `$limit`). Also open in listings: mongod constant-folds (`{$abs: -1}`
      is `{$const: 1}`) and orders the fields of object-form operators its
      own way (`$dateToString`).
      **Decided 2026-10-16 (Joe):** reproduce SBE faithfully: transcribe
      mongod 8.2.11's SBE stage builders for these shapes and EXECUTE the
      slot programs, so `queryPlan`, `slotBasedPlan` and the SBE execution
      stats all come from running the plan. Not a printed stand-in.
      Backlog-ID: RM-071
      Verify: tests/backlog/test_rm_057_084.py::test_explain_of_a_group_pushed_into_the_query
- [ ] **`count` on a view** is explained by mongod 8.2.11 as an SBE plan
      (`explainVersion: "2"`, a `queryPlan` of `GROUP` over the view's
      scan); this server reports the classic cursor and the `$count`
      stage. The same decision as RM-071. `distinct` and `aggregate` on a
      view match mongod. `explain_commands.py`: `a view` 4 and 5.
      **Decided 2026-10-16 (Joe):** as RM-071, transcribe and execute.
      Backlog-ID: RM-072
      Verify: tests/backlog/test_rm_057_084.py::test_explain_of_count_on_a_view

### 7.074 Change streams, command by command -- 2026-10-11

`tools/probes/change_stream_commands.py`, mongod 8.2.11 as a replica-set
member: 115 scenarios over a raw connection (what `$changeStream` accepts,
every event's shape, `getMore`, the invalidating operations, resuming by
`resumeAfter` / `startAfter` / `startAtOperationTime`, the three scopes,
pre-images, transactions), each on a database of its own. The Rust server
differs on 2 results (re-measured 2026-10-16; 5 before). The harness salts database names per run, because
mongod keeps the oplog of earlier runs and a stream started in the past
would see them. What is left:

- [ ] **RUST MongoDB server: a resume token in mongod's own format whose
      number is a decimal built from a double gets 40647.** A token in
      mongod's format is read by a transcription of mongod's KeyString
      decoder (`secantus-commands/src/resume_keystring.rs`). Since
      2026-10-20 that includes `_typeBits`, read as mongod's `TypeBits`
      reader reads them, and a decimal that is a whole number or a teeny
      decimal, whose exponent `adjustDecimalExponent` rescales (695 + 1,384
      tokens, no difference). What is left is a decimal mongod rebuilds from
      a double plus a continuation: a small or large magnitude, or a
      fraction, whose type bits say decimal. That needs Decimal128's rounding
      from a double; such a token answers 40647, where mongod answers the
      decoder's own error (50830, for example) or reads it.
      Backlog-ID: RM-075
      Verify: tests/backlog/test_rm_057_084.py::test_a_decimal_from_a_double_in_a_token
- [ ] **RUST MongoDB server: `explain` of a `$changeStream` is this
      server's plan.** mongod explains the oplog scan it would run, with
      the full match over `local.oplog.rs`; not reproduced, like the rest
      of mongod's planner output (1 of the 2). Sized 2026-10-20: mongod's
      reply is a `$cursor` over `local.oplog.rs` whose `parsedQuery` (about
      2,100 characters for a collection stream, 2,400 for the cluster) is the
      oplog filter mongod's change-stream filter builders make from the
      scope and options, a `PROJECTION_SIMPLE` of the oplog fields, then four
      internal `$changeStream` stages (`internalUnwindTransaction` with the
      same filter, `internalTransform` with the options,
      `$_internalChangeStreamCheckInvalidate`,
      `$_internalChangeStreamCheckResumability`) and a `$match` on
      `operationType`. Reproducing it means transcribing those builders per
      scope and option, not printing the namespace alone.
      Backlog-ID: RM-076
      Verify: tests/backlog/test_rm_057_084.py::test_explain_of_a_change_stream_scans_the_oplog

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

`tools/probes/transactions.py`, mongod 8.2.11 as a replica-set member: 251
scenarios, each a script of commands over raw connections with the session
fields written out (the transaction state rules, what a transaction refuses,
conflicts, collections made in a transaction, cursors, the session commands,
retryable writes). The Rust server differs on 11 scenarios. What is left:

- [ ] **RUST MongoDB server: smaller differences.** A `count` with no query
      does not see another session's uncommitted insert (mongod's fast count
      does: it answered 3 for 2 committed documents). An error that names
      the request or mongod's oplog entry has the code and a shorter text:
      40606 / 40607 / 40608 / 40611 / 40612 on an incompatible
      `findAndModify` retry (which cases refuse is mongod's since
      2026-10-20, RM-203), and the two statement-id errors. A write conflict
      on an insert has an empty cause where mongod names its index table.
      The other session errors are worded for a session with no user.
      (A write that waits on a transaction's document past `maxTimeMS`
      answers a plain 50, which is what mongod 8.2.11 answered in 12 runs
      of 12; the `writeConcernError` one probe run saw did not recur.)
      Backlog-ID: RM-087
      Verify: tests/backlog/test_rm_085_112.py::test_fast_count_sees_another_sessions_uncommitted_insert

### 7.071 Time-series collections -- 2026-10-11

`tools/probes/time_series.py`, mongod 8.2.11: 191 scenarios on a fresh
database each (what `create` accepts, inserts, reads, where measurements are
placed, the restricted writes, indexes, `collMod`, rename and the rest). The
Rust server differs on 5, and on a sixth (`listCollections` order) in the
runs where mongod's own order comes out the other way (2026-10-16).
mongod keeps the rows as buckets in `system.buckets.<name>` and presents the
collection as a view; this server keeps ordinary documents, and most of what
is left follows from that:

- [ ] **RUST MongoDB server: the buckets are derived, not stored**
      (2026-10-11; 6 of 187 still differ). The server keeps measurements as
      ordinary documents, the format the Python server shares, and works the
      buckets out when asked (`crates/secantus-commands/src/ts_buckets.rs`):
      reads of `system.buckets.<name>` and `system.views`, the listings, the
      scan order, `validate`, `collStats` and `$collStats`. What that leaves:
      - **A bucket never closes for size.** (Batches are sorted by time,
        and a field changing type rolls a bucket over, as on mongod, since
        2026-10-16.) mongod's size limit depends on its cache size and on
        how many buckets the whole server has open
        (`getCacheDerivedBucketMaxSize`), and a bucket of fewer than 10
        measurements stays open for large ones: modelling that state is a
        decision for Joe. The reopening query's own size filter is left out
        for the same reason.
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
      **Decided 2026-10-16 (Joe):** model mongod's server-wide bucket
      catalog, memory accounting across every collection included
      (`getCacheDerivedBucketMaxSize`), so buckets close for size as on
      mongod.
      **Decided 2026-10-16 (Joe):** store real buckets in mongod's
      compressed column format (BSONColumn), kept by the server-wide
      catalog, in place of measurements kept as documents. Existing stores
      are converted on open. This also closes the `data` bytes, the bucket
      writes, the `collStats` counters and the whole-collection read above.
      Backlog-ID: RM-088
      Verify: tests/backlog/test_rm_085_112.py::test_bucket_collection_can_be_deleted_from
- **Scope decision — a `hashed` index on a time-series collection** is
  refused as on any collection: hashed indexes are out of scope.
- **Scope decision — `listCollections` order around time-series buckets.**
  This server puts each bucket namespace after its collection and
  `system.views` last; mongod's order is not stable between two runs of the
  same commands, so there is none to match.

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
      found in 24 locales. ICU4X cannot turn normalization off (its
      `CollatorOptions` documents it as always on, no BCP47 `kk`), so this
      needs ICU itself or a collator of our own: a decision, not a fix
      (checked 2026-10-16, icu_collator 2.3.1).
      **Decided 2026-10-16 (Joe):** link the C ICU 57.1 that mongod links,
      in place of ICU4X, so `normalization: false` and the `alternate:
      "shifted"` cases match.
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
      **Decided 2026-10-16 (Joe):** `explain` reports mongod's plan, as it
      does; where the router reads differently, as here, the router is
      brought to the plan. Each such place is a performance bug.
      Backlog-ID: RM-094
      Verify: none -- explain already reports IXSCAN (derived from mongod's rules); the gap is the router scanning, a cost no reply shows
- [ ] **RUST MongoDB server: five collation types are refused.** `search`,
      `searchjl`, `big5han`, `gb2312han` and `phonetic` (as
      `@collation=<type>`) are not in ICU4X's bundled data; the server
      answers 2 naming the type, mongod accepts them. Sized 2026-10-16:
      current CLDR still has `search` (root, ko), `searchjl` (ko) and
      `phonetic` (ln), so ICU4X data generated with them (icu4x-datagen,
      a baked data module) could carry those three; `big5han` and
      `gb2312han` are no longer in CLDR at all and exist only in ICU 57's
      data. Either way a build-time data pipeline: a decision for Joe.
      **Decided 2026-10-16 (Joe):** as RM-091: with ICU 57.1 linked, all
      five types come from its data.
      The same ICU 57 work also settles a duplicate-key error over a
      collated key: its key bytes are ICU4X's (`2b0105`) where mongod's are
      ICU 57's (`290105`), and for a string inside a document this server
      quotes the string where mongod quotes the key (this was RM-096's last
      residual).
      Backlog-ID: RM-095
      Verify: tests/backlog/test_rm_085_112.py::test_five_collation_types_are_accepted


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

`tools/probes/index_admin.py`, mongod 8.2.11: the Rust server differs on 5
of 90: `text` and `hashed` are not built (out of scope) and so cannot be
dropped (four), and `listIndexes` lists by name.

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
- **Make Rust the recommended default**: decided 2026-10-16 (Joe): Rust first everywhere — README (the PyPI page) and docs lead with the Rust servers, the Python servers move to a marked-deprecated section, and the `secantusdb` alias names `secantusd-rs`. Removing the Python servers stays a separate decision.
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
  into `rustls-pki-types`. Still a direct dependency of `secantus-server`
  (`src/lib.rs`'s cert / key loading and `tests/tls.rs`). The PostgreSQL
  crates no longer use it (`secantus-pgwire`'s examples moved to
  `rustls_pki_types::pem::PemObject`, 2026-10-19).
- **RUSTSEC-2026-0196 / -0197** — `cgmath` 0.18.0 unmaintained + `swap_columns`
  UB. Pulled in transitively via the geo/spatial stack (`s2`). No fixed release;
  revisit if the geo dependency is ever swapped.

## Weekly validate.yml failures (2026-08-03 triage)


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
