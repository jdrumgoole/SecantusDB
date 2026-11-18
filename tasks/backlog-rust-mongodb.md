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
      command's locks. Still different,
      measured against a one-member replica set:
      - an `insert` of several documents one of which fails: mongod inserts
        them again a document at a time, each under a transition, global,
        database and collection lock of its own
        (`insertBatchAndHandleErrors`), and counts an interrupt check for
        each lock; this server takes no such rounds, and its line prints no
        `numInterruptChecks` (RM-360);
      - an `update` or `delete` keeps its session row per statement, under
        the statement's own locks, and takes each statement's round as the
        statement runs (this server takes every round first and writes the
        row once, after); the profiled statement's entry's locks are taken
        without interrupt checks here (the command's `numInterruptChecks`
        is mongod's without them);
      - `$out` takes more transition locks than this server's rounds
        (measured on a standalone, RM-185), and an `$out`'s line here has
        one global IS more than mongod's (7 to 6, measured 8.2.11
        2026-11-12); a `collMod` of a collection not there is not measured;
      - each global lock taken apart takes an execution ticket, so where
        these locks differ the line's `queues.execution` admissions differ
        too (RM-355);
      - a statement of a transaction shows the transaction's initial locks
        in its profile entry on mongod (its line prints `{}` here too since
        2026-11-17); the entry is not compared yet;
      - a collectionless `aggregate` takes no lock of its own since
        2026-11-09, as on mongod, but one whose `$lookup` or `$unionWith`
        reads a collection takes the global IS three times on mongod's line
        (four in its entry, four execution admissions) and once here.
      Backlog-ID: RM-357
      Verify: tests/backlog/test_rm_355_364.py::test_more_writes_report_mongods_locks
- [ ] **A `$lookup` from `{db: "local", coll: "oplog.rs"}` is refused.**
      mongod 8.2.11 runs a `$lookup` whose `from` is `{db, coll}` naming a
      collection in `local` or `config`; this server answers FailedToParse
      (9), as mongod does only for any other database. Found 2026-11-17
      (RM-358).
      Verify: tests/backlog/test_rm_found_2026_11_17_rm358.py::test_a_lookup_from_the_oplog_runs
      Backlog-ID: RM-717
- [ ] **`$out` to a new time-series collection does not check the time
      field.** mongod 8.2.11 fails the `aggregate` (BadValue, 2) when a
      document it writes has no `timeField` date; this server writes it.
      Found 2026-11-17 (RM-358).
      Verify: tests/backlog/test_rm_found_2026_11_17_rm358.py::test_out_to_time_series_needs_the_time_field
      Backlog-ID: RM-718
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
- [ ] **`test_a_write_while_an_index_builds` reads mongod's answer off a
      5 ms lock wait.** `tests/mongod_live/test_txn_catalog_changes.py`
      asserts mongod 8.2.11 refuses a transaction's insert while an index
      build is paused (`hangAfterStartingIndexBuild`) with WriteConflict
      (112). In the RM gate at load 16-29 (2026-11-12, RM-475's run) mongod
      answered LockTimeout (24, "Unable to acquire IX lock ... within
      5ms") instead, before the Rust server was asked; a rerun at load 6
      passed. Which answer mongod gives depends on whether the lock request
      outlasts `maxTransactionLockRequestTimeoutMillis`, so the test's
      reference answer is a race under load. Found 2026-11-12 (RM-475).
      Verify: none -- an intermittent failure under load, 1 in 2 gate runs
      Backlog-ID: RM-558
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
- [ ] **A 2dsphere index a MultiPoint gives several keys is not multikey.**
      mongod 8.2.11 marks it multikey: the IXSCAN of each GEO_NEAR_2DSPHERE
      ring (and of a `$geoWithin`) reports `isMultiKey: true` and
      `multiKeyPaths: {loc: ["loc"]}`. This server's catalog records the
      index as not multikey, so its ring scans report `false` and `[]`.
      Found 2026-11-17 (RM-707).
      Verify: tests/backlog/test_rm_found_rm707.py::test_a_multipoint_2dsphere_near_ring_scan_is_multikey
      Backlog-ID: RM-712
- [ ] **A `$near` with a predicate on a compound 2d index's trailing field
      is not planned as GEO_NEAR_2D.** mongod 8.2.11 answers `{loc: {$near:
      [0, 0]}, z: 1}` over `{loc: "2d", z: 1}` with GEO_NEAR_2D, the `z`
      predicate a key filter (`filter: {z: {$eq: 1}}`) on each ring's IXSCAN
      over `[MinKey, MaxKey]`; the ring search here already counts that
      filter (`geoorder::scan`), but the plan this server explains is a FETCH
      over another stage, and the IXSCAN's `filter` field is not rendered.
      Found 2026-11-17 (RM-707).
      Verify: tests/backlog/test_rm_found_rm707.py::test_a_near_over_a_compound_2d_index_with_its_trailing_field_plans_geo_near
      Backlog-ID: RM-713
- [ ] **A LIMIT over a GEO_NEAR stage reports no work counts.** mongod
      8.2.11's LIMIT over GEO_NEAR_2D (`find` with `limit: 3`) reports
      `works` (one more than its input's), `advanced` and `needTime`; this
      server's LIMIT there prints none of them (the plan has no trial to
      take them from, `exec::graft_work`). Found 2026-11-17 (RM-707).
      Verify: tests/backlog/test_rm_found_rm707.py::test_a_limit_over_a_geo_near_reports_its_works
      Backlog-ID: RM-714
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
      whole, or (since 2026-11-18 asked for a first batch of none too) whose stages after its cursor stage are `$project`,
      `$unset`, `$addFields`, `$set`, `$replaceRoot`, `$replaceWith`,
      `$unwind` and `$skip`, and the `getMore`s of its cursor,
      `listIndexes`, `listCollections`, `collStats`, `ping`, `buildInfo`,
      and an `insert`, `update` or `delete` on a member of a replica set
      prints mongod's count (86 shapes in
      `tests/mongod_live/test_interrupt_checks.py`). Every other line
      prints none, where mongod prints one:
      - a read with more than one candidate plan, a plan from the plan
        cache, or a subplanned `$or`: mongod checks once per index scan of
        every plan its `PlanEnumerator` builds (with indexes on `x` and
        `y`, `{x: {$gt: 1}, y: 1}` checks 8 times on mongod: four leaves,
        its index intersections' included; three indexes 13) and not at
        all for a cached plan, and this planner's candidates are not its
        enumeration;
      - the other aggregates: one with a `$match` or `$redact` left after
        its read, a `$limit` there (it stops its source short
        of the documents this server runs in one go), a `$lookup`,
        `$unionWith`, `$facet`, `$geoNear` or `$out` / `$merge`, a `$group`
        run classic over the cursor stage (a `$bucket` of 400 buckets: 16
        and a `getMore` of 4 on mongod), one over a
        view or a time-series collection, and a `$sort` after an SBE
        `$group` asked for a first batch of none (mongod 5; its `getMore`
        here 5 under two global IS, mongod's 4 under one);
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
- [ ] **A statement of a transaction does not count the transaction's
      locks.** mongod 8.2.11's line for it counts
      `MultiDocumentTransactionsBarrier` w 1, `ReplicationStateTransition`
      w 3 and a flow control ticket beside the statement's Global /
      Database / Collection, and the command's line has `locks: {}`; this
      server's statement line has the statement's three only, no
      `flowControl`, and its command line counts them again. Nothing takes
      `Resource::MdtBarrier`. Of a collection and of a view alike (a write
      of a view is refused under the transaction's locks since RM-589).
      Found while working RM-589, 2026-11-17.
      Verify: tests/backlog/test_rm_view_namespaces.py::test_a_transaction_statement_counts_the_transactions_locks
      Backlog-ID: RM-715
- [ ] **An empty `$densify.range` or `$fill.output` is refused with the
      wrong answer.** mongod 8.2.11 parses `range` as it reads it, so
      `{$densify: {range: {}}}` is 40414 "BSON field
      '$densify.range.bounds' is missing but a required field" (before the
      missing `field`), and `{$fill: {output: {}}}` is 6050203 "The output
      field in '$fill' must contain an object with at least one element".
      This server names the missing `field` for the first and answers
      BadValue (2), "not supported by the Rust server", for the second.
      Found while working RM-604, 2026-11-17.
      Verify: tests/backlog/test_rm_found_rm604.py::test_an_empty_densify_range_or_fill_output_is_refused_as_mongod_refuses_it
      Backlog-ID: RM-716
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
        `$group` (`count_documents`; since 2026-11-18 a `$group` on the
        meta field that only counts, which mongod runs over the buckets,
        counts as mongod's does), an inclusion `$project` (RM-456), a
        filter on the meta field (RM-457), a hint or a collation. mongod
        counts the buckets its cursor stage buffered (five of five for a
        sorted `find`). Since RM-455 a `find` is the aggregation mongod
        converts it to, and counts where a time-series `aggregate` streams,
        and since 2026-11-17 under a `$sort`;
      - a geo, `$near` or `$text` read (`$geoWithin` over a `2d` index:
        mongod 5 keys and 4 documents, this server none).
      Measured 2026-10-27, 2026-11-07, 2026-11-10 and 2026-11-13.
      Backlog-ID: RM-363
      Verify: tests/backlog/test_rm_355_364.py::test_a_counted_time_series_read_counts_what_it_examined
- [ ] **A time-series `$group` on the meta field that sums `1.0` answers a
      double.** mongod 8.2.11 rewrites `{$group: {_id: "$m", n: {$sum:
      1.0}}}` over a time-series collection to a sum of each bucket's
      `control.count`, and answers `n: 60` (an int); this server sums 1.0
      per measurement and answers `60.0`. Measured 2026-11-18. Found while
      working RM-363.
      Verify: tests/backlog/test_rm_355_364.py::test_a_meta_group_summing_one_point_zero_answers_an_int
      Backlog-ID: RM-719
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
- [ ] **A transaction's insert counts more dirty bytes than mongod's.**
      mongod 8.2.11's line for an insert of `{_id: 700, y: 1}` inside a
      transaction, into a collection indexed on `_id` and `x`, counts
      `txnBytesDirty` 305-345 over its 3 updates; this server's about 391.
      Over fifty documents in one `insert` the mean is 70-71 bytes more per
      document here, every run. An update or a delete of the same document
      counts the same on both (96 and 192), so the surplus is in what an
      insert links into a page: the record and its index keys, whose layout
      here is not mongod's KeyString. (An insert's count varies by 8 bytes a
      skiplist level on both servers: WiredTiger draws each new key's depth
      at random.) Measured 2026-11-10. Found while working RM-385.
      The surplus is this store's shared
      tables. Every key an insert writes carries the namespace as strings,
      because collections share sixteen document shards
      (`key_format=SSq`), every `_id` index is a row in
      `secantus_natural_seq` (`SSu`), and every secondary index is a row in
      `secantus_index_entries` (`SSSu`, plus the index name). mongod keeps
      one WiredTiger table per collection (`key_format=q`) and one per
      index (KeyString keys), so no key names its namespace. Measured with
      the gap test's insert of fifty documents into `c`, with the `_id` and
      `x` indexes (`secantusd-mdb` from `rm-batch` at 6bcd8f41, mongod
      8.2.11): a 9-character database name gives 362.8 bytes per document
      here against 311.6 on mongod. A 49-character name gives 481.4 here
      against 313.8: 40 more characters cost 118.6 bytes here (three keys
      times 40) and nothing on mongod. Of the 51 bytes left at 9
      characters, about 40 are the prefixes (`db\0c\0` three times, plus
      `x_1\0`). The remaining ~11 are the encodings: the `_id` sort key
      against KeyString, and the index entry's value.
      **Decided (Joe, 2026-11-13): mongod's layout.** A WiredTiger table
      per collection keyed by RecordId (`key_format=q`) and a table per
      index keyed by KeyString, with the catalog mapping each namespace to
      its ident, as mongod does. It closes this item and the per-table half
      of RM-040 (`collStats.wiredTiger` / `indexDetails`, real
      `storageSize` / `indexSize`, `$listCatalog` idents), and makes the
      RM-192 `compact` scope decision unnecessary. It changes the on-disk
      format both Rust servers share: a format bump that refuses an old
      store or a migration, replacing `DOC_SHARDS`, and the KeyString keys
      change the on-disk sort-key rank (`sortkey.rs`, `entryFormat`).
      Several PRs, storage first, then KeyString; each says in its PR that
      the Rust PG server links it, and the PG session is told before the
      format changes.
      Verify: tests/backlog/test_rm_355_364.py::test_an_insert_in_a_transaction_counts_mongods_dirty_bytes
      Backlog-ID: RM-499

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
  (9086700, since 2026-11-18) and, last, `mongod startup complete`
  (8423403). mongod writes about fifty more there that this server does
  not: the network lines, the spill WiredTiger's open (10158000) and its own 22430 messages, the
  durable catalog, FCV, FTDC and flow-control lines (measured 2026-10-24,
  2026-11-07, 2026-11-10, 2026-11-13 and 2026-11-16). Two limits on the
  22430 lines: they are held by the connection and written together once
  the open returns, not as WiredTiger writes them, and a message written
  after the open (a checkpoint's or compaction's progress, the shutdown's
  recovery lines) is held (up to 4096) but never logged, where mongod logs
  each. The vendored WiredTiger 7.0 writes four on a new store where
  mongod's newer engine writes fourteen. Found with RM-302.
  Backlog-ID: RM-305
  Verify: tests/backlog/test_rm_001_028.py::test_startup_opens_the_spill_wiredtiger_after_the_session_cache
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
      (since 2026-11-18), a page at a time, writing each page first. The
      router's other index reads (`try_index_id_keys`: an equality prefix
      of a compound index, `$in` / `$eq`, `$and`, `$or`, `$elemMatch`,
      geo) are still read whole before the first page, and a budget spent
      there writes nothing. The work is to stream a clustered collection's
      plan as an ordinary one's (its natural order is `_id`'s, which the
      stream does not yet read), or to page each of those reads into the
      write as `bounded_span_pages` does; none of those shapes is probed
      against mongod yet.
      Backlog-ID: RM-390
      Verify: none -- the compound residual read this item's gap test
      measured is paged (tests/backlog/test_rm_found_2026_11_10_rm390.py,
      now backlog_fixed); the remaining shapes have no gap test until one
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
      for 28 MB, 204800 for 60 MB (mongod 8.2.11, 2026-11-13). Its tables
      are zstd-compressed at level -7 (`spillWiredTigerBlockCompressor`),
      which the vendored WiredTiger is not built with, and it is WiredTiger
      12.0; this server's uncompressed table leaves its file at 4096 or
      writes megabytes (2420736 at 60 MB), as its eviction threads happen
      to run. Found 2026-11-13 working RM-325.
      Verify: tests/backlog/test_rm_found_2026_11_13_rm325.py::test_a_window_spill_the_engine_writes_out_is_mongods_size
      Backlog-ID: RM-611
- [ ] **An SBE `$group`'s spill counts bytes, and spills, by this server's
      estimate, not mongod's.** With its budget
      (`internalQuerySlotBasedExecutionHashAggApproxMemoryUseInBytesBeforeSpill`,
      taken at runtime and at startup since RM-612's first part) at 50000,
      mongod 8.2.11 spills 300 single-value groups 7 times for each shape
      below, and its `groupSpilledBytes` follows the spilled value rows:
      `{$push: "$s"}` of a 1000-byte string 312300, with a `$sum` beside it
      321900, `{$first: "$s"}` 304200, a `$sum` keyed on a 1000-byte string
      12300, no accumulator at all 2700 (9 a group). This server reports its
      memory estimate at each spill (306900 for each of the first four),
      and keyed on a 1000-byte string spills 6 times, 304990 bytes: SBE's
      hash-table accounting (`HashAggStage`'s memory check and its
      `spilledBytes`) needs porting. The spill itself now goes to one
      table of the spill engine, as mongod's does (spills, records and the
      4096-byte storage size match). Found 2026-11-13 working RM-325;
      measured 2026-11-18.
      Verify: tests/backlog/test_rm_found_2026_11_13_rm325.py::test_an_sbe_group_spills_as_mongods
      Backlog-ID: RM-612
- [ ] **An approximate `$median` / `$percentile` ranks the values, where
      mongod interpolates its t-digest.** mongod 8.2.11 answers from
      `TDigest` (delta 2000, a flush every 6000 values that merges
      centroids, then interpolation between them): over 5999 values of
      `(i * 7919) % 30011 * 0.37` it answers 5552.219999999999 and over
      20000 5551.609969919785, where this server answers the discrete rank
      of the values it keeps (5552.22, 5551.48). The port also owes the
      memory a flush's centroids take (`group::tdigest_buffer` counts only
      the buffer) and the merged centroids a spill writes
      (`group_spill::tdigest_state` writes every value as a centroid of
      weight one, mongod's format while its flush leaves values alone).
      Found 2026-11-18 working RM-613.
      Verify: tests/backlog/test_rm_found_2026_11_18_rm613.py::test_an_approximate_median_is_mongods_tdigest
      Backlog-ID: RM-720
- [ ] **`{$meta: "sortKey"}` in a `$facet` is refused, not `[]`.** mongod
      8.2.11 gives `[]` in a `$facet` pipeline, behind the facet's own
      `$sort` too (its ranks tie there, which this server matches); this
      server refuses the `$meta` with code 2. Found 2026-11-13 (RM-521).
      Verify: tests/backlog/test_rm_found_2026_11_13_rm521.py::test_a_facet_sort_key_is_empty
      Backlog-ID: RM-625
- [ ] **A rank behind `$documents` and a `$sort` is answered, not
      refused.** mongod 8.2.11 refuses a `$_internalSetWindowFields`
      `$rank` there with 9973200 "Attempt to get sort key which has not been
      populated", collectionless and in a `$lookup` pipeline alike; this
      server ties both. A `$sort` the pipeline runs itself writes no key
      (only the query layer's does, RM-523), and with no read of a
      collection in front the documents have no key at all; the marking of
      keyless documents RM-522 added (`agg_meta::DROPS_SORT_KEY`) does not
      cover `$documents`. Found 2026-11-13 (RM-521).
      Verify: tests/backlog/test_rm_found_2026_11_13_rm521.py::test_a_sort_behind_documents_sets_no_key
      Backlog-ID: RM-626
- [ ] **A `$unionWith` ends the sort key.** mongod 8.2.11 keeps the outer
      documents' key through a `$unionWith`, so a rank behind it ranks them
      by the `$sort` in front, and the documents it adds carry their
      sub-pipeline's key (the empty one with no `$sort`) to a later
      `{$meta: "sortKey"}` -- but not to a rank, which refuses them (RM-522).
      This server ends the key at the `$unionWith` (it is not in
      `agg_meta::KEEPS_SORT_METADATA`): every `$meta` reads `[]` and every
      rank ties. Found 2026-11-13 (RM-522).
      Verify: tests/backlog/test_rm_found_2026_11_13_rm522.py::test_a_union_passes_the_sort_key_on
      Backlog-ID: RM-630
- [ ] **A `$densify` sets no sort key.** mongod 8.2.11 leaves each
      document the key of the field `$densify` sorted by: `{$meta:
      "sortKey"}` reads `[n]` and a rank behind it ranks by it. This server
      gives `[]` and ties every document. Found 2026-11-13 (RM-522).
      Verify: tests/backlog/test_rm_found_2026_11_13_rm522.py::test_a_densify_sets_the_sort_key
      Backlog-ID: RM-631
- [ ] **The sort key is lost at a `$graphLookup` or a `$fill`.** mongod
      8.2.11 keeps it through both, as it does through the stages RM-520
      names; this server ends it at each, so a rank behind ties every
      document. Found 2026-11-13 (RM-522).
      Verify: tests/backlog/test_rm_found_2026_11_13_rm522.py::test_a_graph_lookup_and_a_fill_keep_the_sort_key
      Backlog-ID: RM-632
- [ ] **`{$meta: "sortKey"}` gives an empty array's key as null.**
      mongod 8.2.11's key for an empty array is `undefined`, which pymongo
      decodes as `None`: `$type` reads `"undefined"`, and a `$group` on the
      key keeps the empty array's document and a null's apart. This server
      reads the key through `$ifNull` (`agg_meta::sort_key_read`), since its
      expressions take `undefined` for MISSING. The ranks already tell the
      two apart (RM-410). Found 2026-11-10. **Blocked on RM-635**
      (measured 2026-11-13): handing `$meta` the real `undefined` still
      reads "missing" in `$type` of `$$this` and folds into null's
      `$group`, so this closes with that one, not before.
      Verify: tests/backlog/test_rm_found_2026_11_10_rm410.py::test_an_empty_arrays_sort_key_is_undefined
      Backlog-ID: RM-525
- [ ] **A stored `undefined` reads as MISSING in aggregation
      expressions.** The expression engine spells MISSING `Bson::Undefined`
      (`secantus-core` `expressions.rs`, `sbe_ops.rs`; ~145 sites in 22
      files), so a real `undefined` -- a field, an array element, a `$let`
      variable -- gives `$type` "missing" where mongod 8.2.11 gives
      "undefined"; an `$addFields` copy of it is dropped; a `$group` on it
      joins null's group, and one on `[undefined]` is refused (code 2, "not
      supported by the Rust server"). The fix gives MISSING a representation
      of its own, apart from every BSON value. Found 2026-11-13 (RM-525).
      Verify: tests/backlog/test_rm_found_2026_11_13_rm525.py::test_a_stored_undefined_is_not_missing, tests/backlog/test_rm_found_2026_11_13_rm525.py::test_a_group_keeps_undefined_apart_from_null
      Backlog-ID: RM-635
- [ ] **A `$match` that reads a missing sort key names it `undefined`.**
      With no `$sort` ahead of it, `{$match: {$expr: {$gt: [{$size:
      {$meta: "sortKey"}}, 0]}}}` is mongod 8.2.11's 17124 "Executor error
      during aggregate command on namespace: ... :: caused by :: The
      argument to $size must be an array, but was of type: missing"; this
      server's has no envelope and says `undefined`. (`$addFields` reads
      the key as `[]` on both.) The `undefined` is RM-635's MISSING; the
      envelope is this item's own. Found 2026-11-13 (RM-524).
      Verify: tests/backlog/test_rm_found_2026_11_13_rm524.py::test_a_match_reads_a_missing_sort_key
      Backlog-ID: RM-633
- [ ] **A `$project` of `{$meta: "sortKey"}` with no `$sort` is refused as
      unsupported.** mongod 8.2.11 refuses `{$project: {t: {$type: {$meta:
      "sortKey"}}}}` with code 2 "Failed to determine whether query system
      can provide a DISTINCT_SCAN grouping :: caused by :: cannot use
      sortKey $meta projection without a sort"; this server's code 2 says
      "aggregation pipeline uses a stage or operator not supported by the
      Rust server". Found 2026-11-13 (RM-524).
      Verify: tests/backlog/test_rm_found_2026_11_13_rm524.py::test_a_sort_key_projection_without_a_sort
      Backlog-ID: RM-634
- [ ] **`$$ROOT` shows a document's hidden sort key.** Behind a `$sort`
      whose key a later stage reads, `{$addFields: {r: "$$ROOT"}}` and a
      `$lookup` `let: {r: "$$ROOT"}` give the document with a
      `__secantus_meta_sortKey` field, which mongod 8.2.11 keeps as
      metadata and never shows. The key is a field of the document here
      (`agg_meta`); the `$geoNear` fields are alike. Found 2026-11-13
      (RM-520).
      Verify: tests/backlog/test_rm_found_2026_11_13_rm520.py::test_root_does_not_show_the_sort_key
      Backlog-ID: RM-620
- [ ] **A `{$meta: "sortKey"}` in a window's output reads no key.** Behind
      a `$sort`, mongod 8.2.11 gives each document's key to a `$push: {$meta:
      "sortKey"}` in a `$setWindowFields` or `$_internalSetWindowFields`;
      this server refuses it in `$setWindowFields` (code 2) and gives null in
      `$_internalSetWindowFields`, which hold the key off the documents while
      they run. Found 2026-11-13 (RM-520).
      Verify: tests/backlog/test_rm_found_2026_11_13_rm520.py::test_a_windows_output_reads_the_sort_key
      Backlog-ID: RM-621
- [ ] **A missing `newRoot` is named null, not MISSING.** mongod 8.2.11's
      40228 for `$replaceRoot: {newRoot: "$nope"}` (and `$replaceWith`) reads
      "resulting value was: MISSING. Type of resulting value: 'missing'";
      this server's reads "null" and 'null'. Found 2026-11-13 (RM-520).
      Verify: tests/backlog/test_rm_found_2026_11_13_rm520.py::test_a_missing_new_root_is_missing
      Backlog-ID: RM-622
- [ ] **A `newRoot` error names a pruned document when a later stage
      reads the sort key.** In `$sort, $replaceRoot: {newRoot: "$n"}, $addFields:
      {m: {$meta: "sortKey"}}`, mongod 8.2.11's 40228 names the whole input
      document; this server names only the fields the `newRoot` reads, as
      both do with no reader of the key after. Found 2026-11-13 (RM-520).
      Verify: tests/backlog/test_rm_found_2026_11_13_rm520.py::test_a_new_root_error_behind_a_sort_names_the_document
      Backlog-ID: RM-623
- [ ] **A document inside an array a stage reads or writes into is sized
      as if it were read.** mongod 8.2.11 gives each document in an array a
      cache of its own when a stage reads a field through the array
      (`$addFields: {q: "$t.y"}`) or sets one (`$set: {"t.w": 1}`), and the
      sort and window behind count it: (236000, 259600) and (279200, 302800)
      bytes over 200 documents of `t: [1, <30 bytes>, {q: 1, y: <50 bytes>}]`.
      This server's storage model (`docstore`) follows a sub-document's
      storage since RM-515, but not an array's elements: (195600, 219200)
      and (206600, 230200). Found 2026-11-13 (RM-515).
      Verify: tests/backlog/test_rm_found_rm515.py::test_an_arrays_documents_a_stage_touched_are_sized_by_their_storage
      Backlog-ID: RM-636
- [ ] **A `$sort` behind an exclusion `$project` counts each document
      as the BSON it encodes.** mongod 8.2.11's exclusion `$project` keeps
      the input's own storage and writes each excluded field out of it,
      and projects a sub-document (`{"e.y": 0}`) into one of its own;
      behind an `$unwind`, the `$sort` after it counts 4640 bytes over ten
      documents for `{b: 0}` and 6930 for `{"e.y": 0}`. This server counts
      2350 and 2850: the storage model (`docstore`, RM-408) models an
      inclusion `$project` (RM-519) and stops at an exclusion. Found
      2026-11-13 (RM-519).
      Verify: tests/backlog/test_rm_found_rm519.py::test_a_sort_counts_what_an_exclusion_project_left
      Backlog-ID: RM-637
- [ ] **A `$project` behind a `$group` pushed into SBE is sized as if it
      were read.** `[{$group: {_id: "$n", c: {$sum: 1}}}, {$project: {c:
      1}}, {$sort: {c: 1}}]`: mongod 8.2.11's `$project` builds each
      document (a `MutableDocument` with room for its two fields), and the
      `$sort` counts 2450 bytes over ten groups; this server counts the
      BSON each encodes, 1170, as it keeps no storage behind an SBE
      `$group`. Found 2026-11-13 (RM-519).
      Verify: tests/backlog/test_rm_found_rm519.py::test_a_project_after_a_pushed_down_group_builds_its_documents
      Backlog-ID: RM-638
- [ ] **A `$sort` behind a `$setWindowFields` that sorted by the same key
      is kept.** `[{$unwind: "$t"}, {$set: {w: 1}}, {$setWindowFields:
      {sortBy: {n: 1}, ...}}, {$sort: {n: 1}}]`: mongod 8.2.11 drops the
      trailing `$sort`, its explain ending at `$_internalSetWindowFields`;
      this server keeps it and lists a `$sort` after the window, as it does
      with a `$project` in the `$set`'s place. Behind the `$unwind` alone
      it drops it, as mongod does. Found 2026-11-13 (RM-519).
      Verify: tests/backlog/test_rm_found_rm519.py::test_a_sort_after_a_window_on_its_key_is_dropped
      Backlog-ID: RM-639
- [ ] **A `$setWindowFields` behind an `$unwind` hands back tied documents
      in the order they were read.** mongod 8.2.11 hoists the window's
      `$sort` ahead of an `$unwind` whose path it does not sort on, so it
      sorts the documents the `$unwind` took, each then unwound in place:
      `[{$unwind: "$a"}, {$setWindowFields: {sortBy: {k: 1}, ...}}]` over
      60 documents of `a: [i % 4, 9]`, `k: i % 3` gives (0, 0), (0, 9),
      (3, 3), (3, 9), (30, 2), ... on macOS. This server sorts nothing
      there and keeps the order read, (6, 2) fifth. The fix sorts at the
      `$unwind` (after the `$match`es in front, which mongod runs first),
      held to the budget as that sort is. Found 2026-11-14 working RM-527.
      Verify: tests/backlog/test_rm_found_rm527.py::test_window_sort_hoisted_past_unwind_orders_the_source_documents
      Backlog-ID: RM-640
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
- [ ] **A malformed stage spec inside a sub-pipeline is not refused as the
      pipeline parses.** mongod 8.2.11 checks the stages of a `$facet`'s
      and a `$lookup`'s pipelines as it parses the outer one, so over an
      empty collection it refuses `{$facet: {a: [{$match: 5}]}}` with
      15959, a `$lookup` in a facet whose `let` is `5` with 14, and a
      `$lookup` in a `$lookup` pipeline with a lone `localField` with 9.
      This server's `argtypes::stage_spec_error` reads the top level only:
      it answers 2 to the first and runs the other two. Found 2026-11-14
      working RM-528.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm528.py::test_stage_specs_in_a_sub_pipeline_are_checked_as_it_parses
      Backlog-ID: RM-645
- [ ] **`$lookup` runs a `$`-prefixed join field, an empty `as` and a bad
      `let` name.** mongod 8.2.11 refuses `localField: "$a"` with 16410
      "Consider using $getField or $setField for a field path with '.' or
      '$'. :: caused by :: FieldPath field names may not start with '$',
      given '$a'.", `as: ""` with 40352 "FieldPath cannot be constructed
      with empty string", and `let: {X: 1}` with 9 "'X' starts with an
      invalid character for a user variable name". This server runs all
      three (the empty `as` writes a field named ""). Found 2026-11-14
      working RM-528.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm528.py::test_lookup_refuses_a_bad_path_and_a_bad_let_name
      Backlog-ID: RM-646
- [ ] **A `find` sorted on `$natural` is recorded with the sort, not as a
      hint.** mongod 8.2.11's `$queryStats` key for `{find: "c", sort:
      {$natural: -1}}` has no `sort` in its shape and `hint: {$natural:
      -1}`, a different `keyHash`. This server keeps the sort in the shape
      and prints no hint. Found 2026-11-14 working RM-532.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm532.py::test_a_natural_sort_is_recorded_as_a_hint
      Backlog-ID: RM-643
- [ ] **A hybrid search's key names no `otherNss`.** mongod 8.2.11 lists
      the collection of the `$unionWith` a `$rankFusion` (or
      `$scoreFusion`) desugars its later input pipelines into under the
      key's `otherNss`, the aggregate's own collection, and hashes the key
      with it. This server walks the pipeline as written, finds no other
      namespace, and prints none: the entry's `keyHash` differs. Found
      2026-11-14 working RM-532.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm532.py::test_a_hybrid_search_names_the_collection_it_unions
      Backlog-ID: RM-644
- [ ] **A `$facet` element that is not a stage is refused without naming
      it.** mongod 8.2.11's 40171 names the element: "elements of arrays in
      $facet spec must be non-empty objects, b argument contained an
      element of type int: 0: 5" (its type, its index and its value). This
      server ends the message "... b argument contained an invalid
      element". Found 2026-11-14 working RM-530.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm530.py::test_a_facet_element_refusal_names_the_element
      Backlog-ID: RM-647
- [ ] **A collectionless `$unionWith` without a `$documents` runs when it
      is nested.** mongod 8.2.11 refuses a `$unionWith` naming no `coll`
      whose pipeline does not begin with `$documents` (9, "$unionWith stage
      without explicit collection must have a pipeline with $documents as
      first stage") wherever it stands. This server refuses it at the top
      level, but inside a `$facet`, `$lookup` or `$unionWith` runs its
      pipeline over the collection the outer stage reads and unions those
      documents in. Found 2026-11-14 working RM-530.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm530.py::test_a_nested_union_with_no_collection_must_make_its_documents
      Backlog-ID: RM-648
- [ ] **A nested `$lookup`'s bad `let` runs.** mongod 8.2.11 refuses a
      `$lookup` whose `let` is not a document (14) or that has `localField`
      without `foreignField` (9) inside a `$facet` or `$unionWith` as at
      the top level (RM-528). This server runs the first there and answers
      the second 2 ("$lookup requires localField+foreignField or
      pipeline"). Found 2026-11-14 working RM-530.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm530.py::test_a_nested_lookup_spec_is_checked_as_it_is_parsed
      Backlog-ID: RM-649
- [ ] **`{$project: {}}` drops its envelope, and is "not supported" when
      nested.** mongod 8.2.11 answers 51272 "Invalid $project :: caused by
      :: projection specification must have at least one field" at the top
      level and in a `$facet` or `$unionWith`. This server drops the
      "Invalid $project :: caused by ::" at the top level and answers
      BadValue (2, "not supported by the Rust server") nested. Found
      2026-11-14 working RM-530.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm530.py::test_an_empty_projection_is_an_invalid_project
      Backlog-ID: RM-650
- [ ] **`$documents`'s parse refusals in a sub-pipeline.** mongod 8.2.11
      refuses a `$documents` that is not its pipeline's first stage with
      40602 ("$documents is only valid as the first stage in a pipeline")
      under a `$unionWith` that reads a collection, and `{$documents: {}}`
      with 51270 "Invalid $documents :: caused by :: Invalid empty
      sub-projection: _tempDocumentsField". This server answers the first
      73 ("'$documents' can only be run with {aggregate: 1}") and drops
      the second's envelope. Found 2026-11-14 working RM-530.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm530.py::test_documents_refusals_in_a_sub_pipeline
      Backlog-ID: RM-651
- [ ] **A `$facet`'s 40600 comes after a later stage's parse error.**
      mongod 8.2.11 parses a pipeline a stage at a time, so a `$facet`
      holding a stage it refuses is refused before a later stage's
      refusal: `[{$facet: {a: [<$documents>]}}, {$project: {}}]` is 40600,
      and so is a facet's `$out` before a later `$match`'s `$where`. This
      server checks every stage's expressions, projections and `$where`
      in passes over the whole pipeline before the nested-pipeline pass
      (`nested_stage_problem`), and answers the later stage's refusal.
      Found 2026-11-14 working RM-530.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm530.py::test_a_facet_refusal_comes_before_a_later_stage
      Backlog-ID: RM-652
- [ ] **A `$documents` element that is not a document answers 2.**
      mongod 8.2.11 refuses `$documents: [{a: 1}, 2]` at run time with 40228
      "Executor error during aggregate command on namespace:
      <db>.$cmd.aggregate :: caused by :: elements within the array passed
      to $documents must evaluate to an object, but resulting value was: 2.
      Type of resulting value: 'int'. Input document: {_tempDocumentsField:
      2}" (the outer collection's namespace inside a `$lookup`). This server
      answers 2 "each $documents element must be an object". Found
      2026-11-14 working RM-531.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm531.py::test_a_documents_element_that_is_not_a_document_is_40228
      Backlog-ID: RM-653
- [ ] **A `$documents` expression that fails answers "expression not
      supported".** mongod 8.2.11 answers `$documents: {$divide: [1, 0]}`
      (or an element `{x: {$divide: [1, 0]}}`) with 2 "Failed to optimize
      pipeline :: caused by :: can't $divide by zero", and one reading a
      `$lookup` `let` variable under the executor prefix as well. This
      server answers 2 "$documents expression not supported" to all three,
      discarding the evaluator's own error. Found 2026-11-14 working RM-531.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm531.py::test_a_documents_expression_error_is_the_expressions
      Backlog-ID: RM-654
- [ ] **`$$NOW` is unbound inside a `$unionWith` pipeline.** mongod 8.2.11
      runs `{$unionWith: {pipeline: [{$documents: [{t: "$$NOW"}]}]}}` (and a
      later stage reading `$$NOW` there) with the command's date. This
      server answers 2 "$documents expression not supported", or "not
      supported by the Rust server" from a later stage. Found 2026-11-14
      working RM-531.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm531.py::test_now_is_bound_in_a_union_with_pipeline
      Backlog-ID: RM-655
- [ ] **A run-time error in a sub-pipeline or after `$documents` names the
      wrong namespace.** mongod 8.2.11 names the COMMAND's namespace under
      "Executor error during aggregate command on namespace:" wherever the
      failing stage runs: `<db>.$cmd.aggregate` for a collectionless
      aggregate, the outer collection inside a `$lookup` / `$unionWith`
      (with or without `from`). This server names `<db>.` (an empty
      collection) after `$documents` and the FOREIGN collection inside a
      `$lookup` / `$unionWith` with one: `run_segmented` builds the prefix
      from its own `coll`, not from `COMMAND_NS`. Found 2026-11-14 working
      RM-531.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm531.py::test_a_run_time_error_names_the_commands_namespace
      Backlog-ID: RM-656
- [ ] **A FETCH's residual `$or` lists its branches in `parsedQuery`'s
      order.** mongod 8.2.11 lists the `$or` a FETCH filters by behind an
      IXSCAN (`{k: {$gte: 2}, $or: [{"x.a": 1}, {x: {$type: [4]}}, {x: {$not:
      {$type: [3]}}}]}` with an index on `k`) as `[{x: {$not: {$type: [3]}}},
      {x: {$type: [4]}}, {"x.a": {$eq: 1}}]`, in a `find` and in the `$or` an
      aggregate's `$replaceRoot` rewrite builds, though its `parsedQuery`
      sorts the branches `x.a` first. This server lists the residual as
      `parsedQuery` does. A rooted `$or` (SUBPLAN, COLLSCAN) agrees. Found
      2026-11-14 working RM-536.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm536.py::test_a_residual_or_lists_its_branches_as_mongod_does
      Backlog-ID: RM-658
- [ ] **`explain` with `executionStats` answers where a `$replaceRoot`
      fails.** mongod 8.2.11 runs the pipeline for `executionStats` and
      returns the executor's error: `{$replaceRoot: {newRoot: "$x.b"}}` over
      `{x: {a: 1, b: 1}}` is 40228, naming the document the read handed it
      (`{x: {b: 1}}`), with no "Executor error" prefix. This server answers
      with an `explain` document: `count_inline` (`explain/mod.rs`) takes a
      run that failed for one of 0 documents, the root RM-617 (a
      `$setWindowFields`) likely shares. Found 2026-11-14 working RM-536.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm536.py::test_explain_reports_a_replace_roots_run_time_error
      Backlog-ID: RM-659
- [ ] **A query's `parsedQuery` keeps three leaves as written.** mongod
      8.2.11 parses an `$elemMatch` document whose FIRST field is a path as
      a query (`{$elemMatch: {b: 1, $and: [{q: 1}], a: {$gt: 0}}}` is
      `{$and: [b, q, a]}`; it is the value form only when the first field is
      an operator other than `$and` / `$or` / `$nor` / `$expr` ...), a
      `$not` of two operators as `{$nor: [{$and: [...]}]}`, and drops a
      `{a: {$nin: []}}` beside another clause. This server's
      `canonical_match` takes any `$`-key for the value form and echoes the
      document, keeps `{$not: {$lt: 9, $gt: 3}}` whole, and keeps
      `{a: {$not: {$in: []}}}` in the `$and`. A pipeline `$match`
      (`match_as_written`) already parses all three as mongod does. Found
      2026-11-14 working RM-537.
      Verify: tests/backlog/test_rm_found_2026_11_14.py::test_parsed_query_parses_these_leaves_as_mongod
      Backlog-ID: RM-660
- [ ] **Explain lists a `$match` behind a `$group` or a `$$ROOT.x` new
      root where mongod's optimizer does not leave it.** mongod 8.2.11 drops
      a `$match` of nothing behind a `$group` (`{}`, and `{$sampleRate: 1}`,
      which it parses into nothing), and its query then takes the whole
      pipeline; it leaves `{$alwaysTrue: 1}` there, which this server moves
      into the query. It moves a `$match` behind `{$replaceRoot: {newRoot:
      "$$ROOT.x"}}` into the query renamed onto `x`, as behind `"$x"`
      (RM-536), which this server leaves behind the stage. Found 2026-11-14
      working RM-538.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm538.py::test_a_match_after_a_group_as_the_optimizer_leaves_it, tests/backlog/test_rm_found_2026_11_14_rm538.py::test_a_match_moves_ahead_of_a_root_variable_new_root
      Backlog-ID: RM-662
- [ ] **`explain` of a `$sampleRate` the run refuses succeeds.** mongod
      8.2.11 refuses to explain `{$match: {$sampleRate: 2}}` (BadValue,
      "numeric argument to $sampleRate must be in [0, 1]") and `{$sampleRate:
      "x"}` (BadValue, "argument to $sampleRate must be a numeric type"), as
      it refuses to run them; this server's run refuses both, but its
      `explain` answers, listing the `$sampleRate` as written. Found
      2026-11-14 working RM-538.
      Verify: tests/backlog/test_rm_found_2026_11_14_rm538.py::test_explain_refuses_a_sample_rate_the_run_refuses
      Backlog-ID: RM-663
- [ ] **A `$lookup` over a collection reports nothing it examined.** At
      `executionStats` mongod 8.2.11 gives a `$lookup` whose pipeline reads
      a collection its `totalDocsExamined`, `totalKeysExamined`,
      `collectionScans` and `indexesUsed`, summed over its runs: an
      uncorrelated pipeline is run once and cached (2 documents, 1 scan for
      3 outer documents), a correlated one at each (6 and 3). This server
      reports them only for a pipeline over `$documents` (zeros, RM-539).
      Found 2026-11-14 (RM-539).
      Verify: tests/backlog/test_rm_found_2026_11_14_rm539.py::test_a_lookup_over_a_collection_reports_what_it_examined
      Backlog-ID: RM-682
- [ ] **A `$lookup` on `localField` / `foreignField` is explained as a
      classic stage.** mongod 8.2.11 pushes it into SBE over a collection
      and explains it so (`explainVersion` "2", no `stages`, an `EQ_LOOKUP`
      on top of the query's plan); this server lists a `$cursor` and a
      `$lookup` stage. Found 2026-11-14 (RM-539).
      Verify: tests/backlog/test_rm_found_2026_11_14_rm539.py::test_a_lookup_on_join_fields_is_explained_as_sbe_runs_it
      Backlog-ID: RM-683
- [ ] **A 12-byte string is counted as held outside its `Value`.** mongod
      8.2.11's classic `Value` keeps a string of up to 12 bytes inline (a
      `$push` of one counts 136 bytes, a sort of a `$documents` document
      holding one 245); `memlimit::SHORT_STR` is 11, so this server counts
      36 bytes more for each such string, in every memory figure and
      budget. Found 2026-11-14 (RM-539).
      Verify: tests/backlog/test_rm_found_2026_11_14_rm539.py::test_a_twelve_byte_string_is_kept_in_its_value
      Backlog-ID: RM-684
- [ ] **A `$sort` that took a `$limit` reports the size of every document
      it sorted.** mongod 8.2.11's `$sort` with a `limit` is a top-k sorter:
      its `totalDataSizeSortedBytesEstimate` is the size of the documents it
      keeps (490 for two `$documents` documents of 245), and 0 for a `limit`
      of 1, in a `$facet` and outside one. Since RM-538 (2026-11-14) this
      server lists the one `{$sort: {sortKey, limit}}` mongod does, but
      measures each stage from a run that stops at it, so the `$sort` counts
      every document it took (735 for three). Found 2026-11-14 (RM-539,
      RM-538; the fold was RM-685).
      Verify: tests/backlog/test_rm_found_2026_11_14_rm538.py::test_a_top_k_sort_reports_the_documents_it_keeps
      Backlog-ID: RM-689
- [ ] **A run of `$documents` counts its documents as read BSON.** mongod
      8.2.11 counts each document a `$documents` makes as the storage its
      expression object built (`metrics.query.sort.totalBytesSorted` grows
      by 490 for `[{a: 1, b: 3}, {a: 2}]` sorted, as explain reports since
      RM-539); `run_segmented` evaluates the stage and hands the documents
      on as read, so a run counts 255, and its memory budget is held to
      that. Found 2026-11-14 (RM-539).
      Verify: tests/backlog/test_rm_found_2026_11_14_rm539.py::test_a_documents_sort_counts_the_documents_built
      Backlog-ID: RM-686
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
      Verify: none -- the missing fields are mongod's internal accounting; the registry's own tests are backlog_fixed
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
      Verify: none -- no gap test yet for the direct write to
      `config.clusterParameters`
      Backlog-ID: RM-418
- [ ] **A `find` under `queryFramework: "sbe"` that is sorted or
      index-served is explained in the classic shape.** mongod 8.2.11 runs
      it in SBE and explains it so (`nlj` over an `ixseek` and a `seek` for
      an index; a `sort` over a `project` of `getSortKeyAsc` for a sort);
      this server builds the slot program only for a forward collection
      scan, its filter (comparisons and `$in`s), skip, limit and a simple
      projection of top-level fields (`explain/sbe/find.rs`). Also not
      built: an `$in` holding null (mongod's `typeMatch` program), a
      projection of a path or a computed field, and a projection under both
      a skip and a limit. For an `aggregate` (RM-542) the `$addFields`
      after its query are built when they set new top-level fields to
      constants or one field path; an `aggregate` whose query is sorted or
      `$project`ed, or whose `$addFields` compute, is explained classic
      (its `queryHash` and `planCacheKey` are mongod's). Found 2026-11-10
      (RM-418).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm418.py::test_sbe_find_shapes_are_explained_as_sbe
      Backlog-ID: RM-543
- [ ] **A `$sort` on a field and `{$meta: "randVal"}` before a `$project`
      is not refused.** mongod 8.2.11 refuses `[{$sort: {a: 1, s: {$meta:
      "randVal"}}}, {$project: {a: 1, _id: 0}}]` with 16410 ("FieldPath
      field names may not start with '$', given '$computed1'", from its
      pipeline rewrite); the same sort alone, or `{$meta: "randVal"}` as the
      only key, answers on both, and this server sorts the pipeline. Found
      2026-11-14 while working RM-423.
      Verify: tests/backlog/test_rm_found_2026_11_08_rm332.py::test_a_rand_sort_beside_a_field_under_a_project_is_refused
      Backlog-ID: RM-691

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
      Not workable as one fix (a fix-one session declined it, 2026-11-14): it
      bundles about ten gaps (buildInfo's per-OS block, latencyStats,
      connectionStatus privileges, hostInfo.extra on Linux, $indexStats
      following the trial, $listCatalog fields, sessions in
      config.system.sessions) besides the per-table sizes RM-499 decides.
      Split it into one item per gap, each with its own gap test, before
      batching.
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
      Backlog-ID: RM-442
      Verify: tests/backlog/test_rm_340_342_plan_cache_gaps.py::test_an_entry_reports_its_estimated_size_on_linux
- [ ] **A `$cursor` the pipeline stops reading reports a plan run to its
      end.** Under `[{$match: {a: {$gt: 2}}}, {$unwind: "$b"}, {$limit: 3}]`
      (100 documents, `a_1`), mongod 8.2.11's `DocumentSourceCursor` loads one
      batch of 32 documents and stops: its FETCH and IXSCAN report
      `nReturned` 32, 32 works, `saveState` 2, `restoreState` 1, `isEOF` 0.
      This server's classic run goes to the end (70 documents), and leaves the
      work accounting out where its stages' counts are not mongod's
      (`cursor_yields`). Found 2026-11-14 (RM-434).
      Verify: tests/backlog/test_rm_found_2026_11_14_rm434.py::test_a_cursor_the_pipeline_stops_reading_reports_one_batch
      Backlog-ID: RM-692
- [ ] **A `$group` whose `_id` takes `$mod`, or whose accumulator takes an
      expression object, is not pushed into SBE.** `[{$group: {_id: {$mod:
      ["$b", 2]}}}, {$sort: {_id: 1}}]`, and `{_id: "$a", q: {$push: {s:
      "$s"}}}` (`$addToSet` too): mongod 8.2.11 runs the `$group` in SBE,
      `explainVersion` "2" and no `$group` in `stages`; this server explains
      version "1" with a classic `$group`. The `$sort`'s byte count after it
      already matches. Found 2026-11-14 (RM-435).
      Verify: tests/backlog/test_rm_found_rm435.py::test_a_group_mongod_pushes_into_sbe_is_pushed
      Backlog-ID: RM-693
- [ ] **`estimatedSizeBytes` is still left out of some entries whose
      index tree this server does not build.** It sizes an entry's
      `PlanCacheIndexTree` from the canonical query and the index tags of
      its winner (`plan::index_tags`, `plan::or_tags`,
      `entry_size::tagged_tree`): one index, or an `OR` of single-predicate
      branches beside other clauses, and an untagged `$elemMatch` over
      operators. mongod 8.2.11 sizes every entry; this server leaves the
      field out for a `$not` inside `$elemMatch` (3350 for `{a: {$lt: 5}, b:
      {$elemMatch: {$not: {$gt: 3}}}}` over `a_1` and `b_1` on macOS
      arm64), the object form of `$elemMatch` and an `$elemMatch` the
      winner's index reads, an `$or` that is the root or whose clauses
      mongod pushes into the branches (`OrPushdownTag`), a branch of several
      predicates, `$nor`, a `$not` over several predicates, `$jsonSchema`,
      and an `AND_SORTED` winner. Found 2026-11-14 (RM-436).
      Verify: tests/backlog/test_rm_found_2026_11_08_rm341.py::test_a_not_inside_elem_match_entry_reports_its_size
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
      on one field, an `$in` of such values, the `$not: {$eq}` a `$ne` is
      (since 2026-11-14), and the `$and`s of those (`explain/sbe/filter.rs`), with a skip and a limit where the
      group reads no field (RM-072); whose `_id` is a field path, `$$ROOT`,
      a scalar constant or an object of those, whose arguments are field
      paths or `$$ROOT` (numeric constants for `$sum`), with `$sum`,
      `$count`, `$avg`, `$first`, `$last`, `$min`, `$max`, `$push` and
      `$addToSet`, and at most one distinct dotted path; and the `limit 0`
      / `coscan` plan over a collection that does not exist. Still the
      classic `$cursor` / `stages` shape: any other filter (an `$in` holding
      null, `$exists`'s `exists`, a dotted `$eq: null`, `$or`, ...), a sorted,
      projected or index-served front (`IXSCAN` / `FETCH`; for `count` on
      a view, RM-695; the `COUNT_SCAN` of a single-field
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
- [ ] **`count` on a view whose filter a `FETCH` over an index scan
      finishes** is explained by mongod 8.2.11 as an SBE plan
      (`explainVersion: "2"`, a `queryPlan` of `GROUP` over `FETCH` /
      `IXSCAN`, a `slotBasedPlan` of an `nlj` of an `ixseek` that reads the
      key, record id, snapshot and index ident into slots, and a `limit 1`
      `seek` of the document, under a `filter` of `traverseF`); this server
      reports the classic cursor with `FETCH` / `IXSCAN` and the `$count`'s
      `$group`. Reproduce: a view `{$match: {a: {$gt: 1}}}` over indexes
      `a_1` and `b_-1`, `explain` of `{count: <view>, query: {b: 2}}`.
      Where the index serves the whole filter and the count reads no
      field, the `GROUP` over a `COUNT_SCAN` is built since 2026-11-14
      (RM-072). Needs the `nlj` and `seek` stages and their stats, and the
      index-key slots of `ixseek`. `explain_commands.py`: `a view` 5.
      **Decided 2026-10-16 (Joe):** as RM-071, transcribe and execute.
      Verify: tests/backlog/test_rm_057_084.py::test_explain_of_count_on_a_view_fetching_from_an_index
      Backlog-ID: RM-695
- [ ] **A fusion input pipeline's `$unionWith` lists no `$cursor`.**
      mongod 8.2.11 explains the `$unionWith` a `$rankFusion` or
      `$scoreFusion` input pipeline runs in as any `$unionWith` over a
      collection (RM-426): a `$cursor` of its own, then the stages it did
      not absorb. This server lists that pipeline's stages desugared
      (RM-200) with no `$cursor`: it runs hidden stages the explain of a
      sub-pipeline cannot parse. Split from RM-426 2026-11-15.
      Verify: tests/backlog/test_rm_found_2026_11_15_rm426.py::test_a_fusion_input_union_opens_with_its_cursor
      Backlog-ID: RM-696
- [ ] **A `$group` read by a `DISTINCT_SCAN` hashes as another query.**
      mongod 8.2.11 explains `[{$match: {x: {$gt: 2}}}, {$group: {_id:
      "$x"}}]` over an index `{x: 1}` with `queryHash` /
      `planCacheShapeHash` `76A99463` and `planCacheKey` `09B22F21`; this
      server's plan is the same `PROJECTION_COVERED` over `DISTINCT_SCAN`
      but hashes `FA2103B6` / `CBA7C409`, top-level and inside a
      `$unionWith` alike. Found 2026-11-15 working RM-426.
      Verify: tests/backlog/test_rm_found_2026_11_15_rm426.py::test_a_distinct_scan_group_hashes_as_mongod_does
      Backlog-ID: RM-697

### 7.074 Change streams, command by command -- 2026-10-11

`tools/probes/change_stream_commands.py`, mongod 8.2.11 as a replica-set
member: 115 scenarios over a raw connection (what `$changeStream` accepts,
every event's shape, `getMore`, the invalidating operations, resuming by
`resumeAfter` / `startAfter` / `startAtOperationTime`, the three scopes,
pre-images, transactions), each on a database of its own. The Rust server
differs on none (re-measured 2026-11-09 after RM-454; 1 earlier that day, 2 on 2026-10-16, 5 before). The harness salts database names per run, because
mongod keeps the oplog of earlier runs and a stream started in the past
would see them. What is left:

- [ ] **RUST MongoDB server: a `$match` after a `$changeStream` on an
      event field other than `operationType` is not rewritten into the
      oplog filter.** mongod 8.2.11 rewrites predicates on `fullDocument`,
      `documentKey`, `ns`, `to`, `updateDescription`,
      `fullDocumentBeforeChange`, the renamed `clusterTime` / `lsid` /
      `txnNumber` (also into the transaction filter), and an `$expr` over
      them, and adds them to the read's filter (`fullDocument.a: 1`:
      `queryHash` 193DCAB5, not 90A52575; 2026-11-15). RM-452 ported the
      `operationType` rewrite only; the rest of
      `change_stream_rewrite_helpers.cpp` is to transcribe (found working
      RM-452).
      Verify: tests/backlog/test_rm_found_2026_11_15_rm452.py::test_a_match_on_another_event_field_joins_the_oplog_filter
      Backlog-ID: RM-698
- [ ] **RUST MongoDB server: the unwind filter of a change stream is not
      Boolean-simplified.** mongod 8.2.11 optimizes
      `internalUnwindTransaction`'s filter with its Boolean expression
      simplifier (the oplog filter it optimizes without): after
      `operationType: "drop"` (`rename`, `dropDatabase`) the writes branch
      and the event-type `$or` fall away, as only a command can match. This
      server prints the unsimplified `$and`, which matches the same entries
      (2026-11-15, found working RM-452).
      Verify: tests/backlog/test_rm_found_2026_11_15_rm452.py::test_the_unwind_filter_is_simplified
      Backlog-ID: RM-699
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

- [ ] **RUST MongoDB server: a time-series read under a hint, or over a
      collection with an index of its own, reads the collection at the
      command.** Hinted to `m_1_t_1` (`{$match: {v: {$gte: 0}}}`), mongod
      8.2.11 scans that index over the buckets, buffering 32 series by
      series, and returns 1,279 more after the delete; this server reads it
      whole: 3,999. So too a read under a collation, and any read of a
      collection with an index besides the default one. Since RM-457 the
      default `m_1_t_1` index is built over the buckets at create and a
      filter on the meta field scans it; an index `createIndexes` adds is
      kept on the collection only, `dropIndexes` leaves the bucket copy of
      `m_1_t_1` in place, and `collStats` still reports index sizes of 0.
      Found while working RM-457, 2026-11-15.
      Verify: tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_hinted_time_series_read_scans_the_index_over_the_buckets
      Backlog-ID: RM-701
- [ ] **RUST MongoDB server: an insert into a time-series bucket the TTL
      monitor removes fails.** Inserting into a bucket that has expired
      while the TTL monitor runs every second, mongod 8.2.11 never fails an
      insert (it writes a new bucket); this server returns the write error
      112, "the time-series bucket was removed while it was written"
      (`bucket_catalog.rs`: the update of the open bucket matched nothing),
      to the client. Seen once in `tests/mongod_live/test_time_series_buckets.py::test_ttl_expires_whole_buckets`
      in a gate run, reproduced by the gap test (a race: it inserts until
      the first failure, up to 90 TTL passes). Also read, not yet probed:
      `commit` returns `Ok` without writing when the bucket it staged into
      is no longer open (`ts_store`'s delete clears it first), which may
      lose a staged insert silently. Found working RM-456, 2026-11-15.
      Verify: tests/backlog/test_rm_found_rm456.py::test_an_insert_into_a_bucket_the_ttl_monitor_removes_succeeds
      Backlog-ID: RM-702
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
Rust server differs on 2 (2026-11-11): a `hashed` index (a scope decision)
and `collStats`' sizes -- its storage size, as on every collection (RM-040),
and its index sizes, 0 while the indexes are not built over the buckets
(RM-457); a third, `listCollections` order, differs in the runs where
mongod's own order comes out the other way.
Since 2026-11-11 this server keeps a time-series collection as mongod does:
bucket documents in `system.buckets.<name>`, each data field mongod's
compressed column, placed by mongod's bucket catalog
(`crates/secantus-commands/src/bucket_catalog.rs`). What is left:

- [ ] **RUST MongoDB server: the bucket catalog keeps no account of its
      memory.** mongod 8.2.11 reports a `bucketCatalog` section in
      `serverStatus` once a time-series collection is written to
      (`numBuckets`, `numOpenBuckets`, `numIdleBuckets`,
      `numArchivedBuckets`, `memoryUsage`, the execution counters,
      `stateManagement`), and closes or archives idle buckets when the
      catalog's memory passes `timeseriesIdleBucketExpiryMemoryUsageThreshold`
      (a share of the machine's memory). This server reports no section and
      never expires an idle bucket. Everything but the memory is the
      catalog's to report; `memoryUsage`, and so the expiry, is the bytes
      mongod's own C++ structures allocate through its tracking allocators,
      which this server does not make. Measured 2026-11-11.
      **Decided (Joe, 2026-11-11):** model mongod's accounting: charge each
      bucket, its column builders, summaries, schema and the catalog's maps
      the bytes mongod's structures allocate through its tracking
      allocators, so `memoryUsage` and idle-bucket expiry follow mongod's.
      The size limit's memory accounting (`getCacheDerivedBucketMaxSize`,
      decided 2026-10-16) does not depend on it and is built.
      Too large for one fix (declined 2026-11-15). Proposed split: (1)
      serverStatus.bucketCatalog's counts, counters and stateManagement from
      state the catalog keeps; (2) mongod's memory accounting, so memoryUsage
      matches; (3) idle-bucket expiry past
      timeseriesIdleBucketExpiryMemoryUsageThreshold, which needs (2). The gap
      test stays red until (2) lands. Awaiting Joe's go-ahead to file the
      split.
      Verify: tests/backlog/test_rm_found_2026_11_11_rm088.py::test_server_status_reports_the_bucket_catalog
      Backlog-ID: RM-549
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
writes). The Rust server differs on 2 (2026-11-09): hashed and text
indexes, which it does not build. It compares through the ICU
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
- **Make Rust the recommended default**: decided 2026-10-16 (Joe): Rust first everywhere — README (the PyPI page) and docs lead with the Rust servers, the Python servers move to a marked-deprecated section, and the `secantusdb` alias names `secantusd-mdb`. Removing the Python servers stays a separate decision.
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


## The backlog verifier's pytest stalled once in CI (2026-10-21)

- [ ] **RM-008's limiter test reads mongod's answer under load as a
  different answer.** `test_ingress_rate_limiter_admits_queues_and_closes`
  opens six connections against each server with the limiter at one a
  second and a burst of one, and compares which were answered. In the
  local RM gate's `backlog verify` step (2026-11-07, load average about
  11 on 12 cores) mongod answered two connections with no queue, where
  it answers one: the six connects took longer than a second to land, so
  the bucket refilled. The test passed three runs out of three on its own
  at low load. The same happened to this server's side in the RM gate of
  2026-11-09 (load average about 9: it answered two with `maxQueueDepth`
  2, where mongod answered one); three runs on their own passed. In the
  RM gate of 2026-11-13 (load average up to 18) the same queue-2 case
  differed in a wait instead: this server answered its third connection
  after a wait that rounded to 3 seconds, mongod after 2; four runs on
  their own passed, at load 18. A later RM gate that day (RM-515) saw
  mongod answer two with no queue, as on 2026-11-08; three runs on their
  own passed. So did the RM-533 gate of 2026-11-14 (load average up to
  32), and again three runs on their own passed. The test depends on six connects landing within one
  second, so it is flaky by construction: it needs a measurement that
  load cannot change (connect all six behind a barrier and record how
  long they took to land, or compare with the answer mongod gives when
  they land within the second), not a retry.
  Backlog-ID: RM-372
  Verify: none -- a timing flake under load; the test passes when the connects land within a second
- [ ] **The profiler's level-two test fails when its `find` touches the
  disk or waits on a WiredTiger lock.** `profiler_wt.rs`'s
  `level_two_writes_each_operation_in_mongods_shape` compares a `find`
  entry's keys with a fixed list that has no `storage`. The entry carries
  `storage` whenever the operation read or wrote bytes, or waited on the
  cache, the schema lock or a handle lock (`opdebug::storage_section`), as
  mongod's does, so the list holds only while WiredTiger stays out of the
  way. In the local RM gate's `adapter-test` step (2026-11-09) the entry
  had `storage` between `flowControl` and `responseLength`; the file then
  passed 30 runs out of 30. Which counter was set was not captured: the
  test should print the entry's `storage` when it differs, and then
  either rule that counter out (a store with nothing to read from disk)
  or accept `storage` where mongod would give it, never drop the check.
  Backlog-ID: RM-448
  Verify: none -- an intermittent failure, 1 in 31 runs, whose counter is unknown

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
