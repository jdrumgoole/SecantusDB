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
        admission (mongod returns both and takes them again, RM-360); an
        `awaitData` getMore returns its ticket for each 100 ms of its wait,
        where mongod's yields once per wait; `abortTransaction`'s line
        counts two execution admissions, where mongod's unstash makes it
        three (`commitTransaction`'s three match, measured 2026-11-12);
        and an `update` or `delete` statement's line counts one admission
        fewer, its command's line one fewer per statement, because the
        statement's line is written with the command's locks still held
        (RM-357's statement rounds), where mongod's statement has let its
        own go and takes a fresh ticket to read its statistics;
      - the line of a command that never got its ingress ticket names
        `<db>.$cmd` on mongod, its collection here; and mongod's first
        retryable write after startup takes a second, exempt, ingress
        admission (not traced), both measured 8.2.11 2026-11-08; an index
        build's `$currentOp` row has no `queues` here (mongod's not yet
        measured).
      **Decided (Joe, 2026-11-08): build mongod's execution admission control in full, the faithful option: the ingress controller, then the read and write ticket pools with `throughputProbing` (replacing `admission.rs`'s opt-in write tickets), then the per-command admission counts, so the concurrency cap is on by default as on mongod. Several PRs, in that order.** The ingress controller landed 2026-11-08, the ticket pools 2026-11-09, and the per-command admission counts on the line 2026-11-09; what is left is above.
      Backlog-ID: RM-355
      Verify: tests/backlog/test_rm_355_364.py::test_ending_a_transaction_takes_mongods_execution_tickets
- [ ] **An aggregate through its cursor stage counts too few execution
      admissions.** mongod 8.2.11 lets an aggregate's locks go once its
      pipeline is built, so each buffer its cursor stage fills takes the
      global IS and an execution ticket of its own: an `$addFields` over
      300 documents read 50 at a time is `Global: {r: 3}` and 4 execution
      admissions on the `aggregate` line (the build, two buffers, the
      storage statistics). Since RM-360 this server takes the global IS for
      each buffer too, and its line has mongod's `locks`, but it holds the
      statement's own locks to the end of the command, so the buffers' IS
      ride its ticket and the line says 2. Its `getMore`s, which take no
      lock of their own, count mongod's. Found 2026-11-12 (RM-360).
      Verify: tests/backlog/test_rm_found_2026_11_12_rm360.py::test_each_cursor_stage_buffer_takes_an_execution_ticket
      Backlog-ID: RM-579
- [ ] **The connection establishment rate limiter has no bypass list.**
      mongod 8.2.11's `ingressConnectionEstablishmentRateLimiterBypass` (a
      CIDR range list, `{ranges: []}` by default, set at startup or at
      runtime) exempts the clients it names from the connection
      establishment rate limiter, which counts them in
      `queues.ingressSessionEstablishment.exemptedAdmissions` and
      `connections.establishmentRateLimit.exempted`. This server does not
      register the parameter (`getParameter` answers 72), so every client
      is limited and both counts stay 0. Found working RM-355, 2026-11-12.
      Backlog-ID: RM-550
      Verify: tests/backlog/test_rm_355_364.py::test_the_connection_rate_limiter_has_its_bypass_list
- [ ] **Some writes' `locks` are not mongod's.** Since 2026-11-07 a write
      counts mongod's flow control tickets (one per global IX taken apart)
      and a retryable write's session checkout and session row, and a
      journal wait (`j`, `w: "majority"`) its transition lock and global
      IS, so an insert, `drop` or `create` reads as mongod 8.2.11's; since
      2026-11-12 a command that took the global lock in IX and logged
      nothing (`create` of a collection there, `drop` or `dropIndexes` of
      one not there) reads the latest optime for its last op as mongod's
      entry point does (`lastop::entry_point_fixes`). Still different,
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
        row once, after); a profiled statement's own entry is counted in its
        command's locks (`Metadata: {W: 1}`, one more global IX), taken
        without interrupt checks (the command's `numInterruptChecks` is
        mongod's without them);
      - a retryable `findAndModify` writes its image under its locks (one
        more transition, global and collection IX), and one that removes
        takes a single round;
      - `collMod` and `$out` take more transition locks than this server's
        rounds (`$out`'s were measured on a standalone, RM-185), and an
        `$out`'s line here has one global IS more than mongod's (7 to 6,
        measured 8.2.11 2026-11-12);
      - each global lock taken apart takes an execution ticket, so where
        these locks differ the line's `queues.execution` admissions differ
        too (RM-355; `collMod`'s line has 3 here, 4 on mongod);
      - a statement of a transaction shows `{}` on mongod's line and the
        transaction's initial locks in its entry;
      - a collectionless `aggregate` takes no lock of its own since
        2026-11-09, as on mongod, but one whose `$lookup` or `$unionWith`
        reads a collection takes the global IS three times on mongod's line
        (four in its entry, four execution admissions) and once here.
      Backlog-ID: RM-357
      Verify: tests/backlog/test_rm_355_364.py::test_more_writes_report_mongods_locks
- [ ] **A standalone's write concern is a replica set's.** `secantusd-rs
      --standalone` keeps the member's write-concern machinery: a write sent
      with no write concern takes the implicit `w: "majority"` and waits for
      the journal (one more transition IX and global IS in its `locks`), and
      `setDefaultRWConcern` and `getDefaultRWConcern` answer. mongod 8.2.11
      as a standalone implies `w: 1` (no journal wait) and refuses both with
      51300 (measured 2026-11-12). RM-185's lock comparison, which runs this
      server as a standalone, sets a stored `w: 1` default until this is
      fixed (`tests/backlog/test_rm_185_locks.py`, `rust_mongo`): the fix
      drops that call.
      Backlog-ID: RM-551
      Verify: tests/backlog/test_rm_standalone_write_concern.py::test_a_standalone_implies_w_1
- [ ] **Some queries have no `queryShapeHash`.** Since 2026-11-07 a
      `find`'s, `distinct`'s and `aggregate`'s line, profile entry and
      `explain` carry mongod's `queryShapeHash` (`secantus-commands`
      `query_shape.rs`); since 2026-11-09 it covers geo, `$text`, `$where`
      and `$sampleRate` predicates, the aggregation operators and
      accumulators with arguments of their own, the stages mongod desugars,
      change streams, `aggregate: 1` and DBPointer literals
      (`tests/mongod_live/test_query_shape_hash.py`, against mongod
      8.2.11); since 2026-11-12 `$rankFusion` and `$scoreFusion`, as the
      pipelines mongod desugars them into, and a `$geoNear` with no
      `distanceField`. A query using one of these prints none, where mongod
      prints one:
      - `$jsonSchema`, which mongod shapes as the `$_internalSchema*`
        predicates its JSON Schema parser translates the schema into
        (`json_schema_parser.cpp`, about 1,950 lines in 8.2.11, and the
        serializer of each of the ~18 internal predicates it builds);
      - a `$rankFusion` or `$scoreFusion` input of `$search` or
        `$vectorSearch`, which need mongot;
      - options measured nowhere yet: a `$lookup` `from` naming another
        database, `$out` to a time-series collection, change stream
        options beyond `fullDocument`, `fullDocumentBeforeChange`,
        `allChangesForCluster` and `showExpandedEvents`.
      Each is mongod's serializer for that node, transcribed and checked as
      the shaped ones were.
      Backlog-ID: RM-358
      Verify: tests/backlog/test_rm_355_364.py::test_a_query_of_every_kind_names_its_query_shape
- [ ] **An `explain` of `$geoNear` fails.** mongod 8.2.11 explains
      `aggregate` of `[{$geoNear: {near: [0, 0]}}]` over a collection with
      a `2d` index, with or without `distanceField`, and prints its
      `queryShapeHash`. This server runs the stage itself, but its
      `explain` runs the pipeline against a namespace of its own
      (`explain.dry.run`, `explain/mod.rs`) and fails with 26 (`$geoNear
      requires a geo index to run, but <db>.explain.dry.run does not
      exist`), as RM-466 did for `$planCacheStats`. A `$rankFusion` with a
      `$geoNear` input fails the same way. Found 2026-11-12 (RM-358).
      Verify: tests/backlog/test_rm_found_rm358.py::test_an_explain_of_geo_near_runs
      Backlog-ID: RM-552
- [ ] **`$scoreFusion` combines by `"average"`, not `"avg"`.** mongod
      8.2.11's `combination.method` is `"avg"` or `"expression"` (its IDL
      enum, `kAvg: "avg"`) and refuses `"average"` with 2 (`Enumeration
      value 'average' for field '$scoreFusion.combination.method' is not a
      valid value.`); `"average"` is only the method's name in
      `scoreDetails`. This server takes `"average"` and refuses `"avg"`
      (`hybrid.rs`). Found 2026-11-12 (RM-358).
      Verify: tests/backlog/test_rm_found_rm358.py::test_score_fusion_combines_by_avg
      Backlog-ID: RM-553
- [ ] **A later `$match` does not replace the one `$planCacheStats` absorbed.**
      mongod 8.2.11's `$planCacheStats` absorbs the `$match` right after
      it, and absorbs again, in place of the first, each `$match` a later
      stage swaps ahead of itself (`doOptimizeAt` overwrites
      `_absorbedMatch`): `[{$planCacheStats: {}}, {$match: {isActive:
      false}}, {$project: {isActive: 1, works: 1}}, {$match: {works: {$gte:
      0}}}]` returns every entry, the first filter lost. This server's stage
      (`aggregate.rs`) keeps the first `$match` and runs the second after
      the `$project`, returning none of three active entries. `explain`
      already lists it as mongod does (`plan_cache::absorb_matches`, RM-466),
      which the run could share. Found 2026-11-12 (RM-466).
      Verify: tests/backlog/test_rm_found_rm466.py::test_a_later_match_replaces_the_one_plan_cache_stats_absorbed
      Backlog-ID: RM-554
- [ ] **An `$or` of one index sorts documents, not index keys.** mongod
      8.2.11 answers `find({$or: [{y: 0}, {y: {$gt: 1}}]}).sort({x: 1})`
      over `{y: 1, x: 1}` with one two-interval IXSCAN whose keys it sorts
      below the FETCH (`SUBPLAN <- FETCH <- SORT <- IXSCAN`, `type:
      "default"`, 520 counted for eight members). This server sorts the
      fetched documents above the FETCH (`simple`, 480). Found 2026-11-12
      (RM-476).
      Verify: tests/backlog/test_rm_found_rm476.py::test_an_or_of_one_index_sorts_its_keys
      Backlog-ID: RM-557
- [ ] **A SORT_MERGE beside a null scan breaks a tie another way than
      mongod.** mongod 8.2.11 answers `find({a: {$in: [7, "b", 5,
      null]}}).sort({b: 1})` over `{a: 1, b: 1}` with `FETCH <- SORT_MERGE`
      of four scans, and returns `_id` 14 (no `a`, from the null scan)
      before 13 (`a: 7`), which tie on `b`: `[3, 14, 13, 17]`. This server
      plans the same merge and returns `[3, 13, 14, 17]`. Without the null
      in the `$in` the two agree. These are the three `order` replies of
      `tools/probes/find_order.py` (step 100 over `{a: 1, b: 1}`, `{a: 1,
      b: -1}` and `{a: -1, b: 1}`). Found 2026-11-12 (RM-477).
      Verify: tests/backlog/test_rm_found_rm477.py::test_a_sort_merge_beside_a_null_scan_breaks_ties_as_mongod
      Backlog-ID: RM-559
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
- [ ] **A spilled SORT of index keys under a collation writes the
      strings, not their collation keys.** mongod 8.2.11's SORT below a
      FETCH computes each member's sort key under the command's collation
      and spills that key: 2000 `"s{i}"` strings over `{y: 1, x: 1}` with
      `{locale: "fr"}` under a 100000-byte budget spill 157780 bytes. This
      server writes the string values (`stream.rs`'s `member_values`):
      149780. What it counts in memory already agrees (140890). Found
      2026-11-12 (RM-475).
      Verify: tests/backlog/test_rm_found_rm475.py::test_a_spilled_sort_of_index_keys_writes_collation_keys
      Backlog-ID: RM-555
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
- [ ] **A sort under `returnKey` is a `simple` sort, and a limited one
      leaves ties in another order.** mongod 8.2.11's SORT under a
      RETURN_KEY is `type: "default"` (it keeps each member's index key),
      and `find({y: {$gte: 0}}).sort({z: 1}).limit(2).returnKey()` over
      `{y: 1, x: 1}` returns `{y: 0, x: 6}` before `{y: 0, x: 0}`, which
      tie on `z`. This server's SORT is `simple` (`Node::Sort`'s `on_keys`
      says both "sorts keys below the FETCH" and the `default` type, as for
      RM-472), and it returns `x` 0 before 6. Found 2026-11-12 (RM-470).
      Verify: tests/backlog/test_rm_found_rm470.py::test_a_sort_under_return_key_is_a_default_sort
      Backlog-ID: RM-566
- [ ] **A `returnKey` point lookup on `_id` is not IDHACK.** mongod 8.2.11
      answers `find({_id: 3}).returnKey()` by `RETURN_KEY <- IDHACK`; this
      server plans `RETURN_KEY <- IXSCAN` (`find_plan` leaves the point
      lookups to reads without `returnKey`). Found 2026-11-12 (RM-470).
      Verify: tests/backlog/test_rm_found_rm470.py::test_return_key_of_an_id_point_lookup_is_idhack
      Backlog-ID: RM-567
- [ ] **`returnKey` drops a projection's `{$meta: "sortKey"}` field.**
      mongod 8.2.11 keeps a projection's `sortKey` fields under `returnKey`
      (`extractSortKeyMetaFieldsFromProjection`): `find({z: 0}, {k:
      {$meta: "sortKey"}}).sort({z: 1}).returnKey()` returns `{k: [0]}`
      for each document. This server returns the sort's field instead,
      `{z: 0}`, and over an index `{y, x, z}` where mongod gives `{y, x,
      k}`. Found 2026-11-12 (RM-470).
      Verify: tests/backlog/test_rm_found_rm470.py::test_return_key_keeps_a_sort_key_meta_field
      Backlog-ID: RM-568
- [ ] **A `default` sort of documents counts the documents, not its
      working-set members.** mongod 8.2.11's SORT of a read that keeps
      each member's metadata (`showRecordId`, a projection's `{$meta:
      "recordId"}`; `type: "default"` since RM-472) counts each member it
      sorts: 1122 bytes `totalDataSizeSorted` for the six documents of
      `find({z: 0}).sort({z: 1}).showRecordId()` (374 for two under a
      `limit` of 2, 2244 for twelve over a FETCH), where the same read
      without `showRecordId` sorts `simple` and counts 402. This server
      counts 402 both ways: it sorts the documents as its `simple` sort
      does. A SORT over a covered projection counts its members since
      RM-473 (`sorter::sort_owned_objs`), the model this needs for a
      member that holds a fetched document. Found 2026-11-12 (RM-472).
      Verify: tests/backlog/test_rm_found_rm472.py::test_a_default_sort_of_documents_counts_its_members
      Backlog-ID: RM-577
- [ ] **A `$near` read over a `2d` index explains as a COLLSCAN.** mongod
      8.2.11 plans `find({loc: {$near: [0, 0]}})` over a `{loc: "2d"}`
      index as `GEO_NEAR_2D` (`SORT <- GEO_NEAR_2D` under a `sort`); this
      server's `explain` names a `COLLSCAN` for the same read. The
      documents come back in mongod's order either way. Found 2026-11-12
      (RM-472).
      Verify: tests/backlog/test_rm_found_rm472.py::test_a_near_read_explains_as_geo_near_2d
      Backlog-ID: RM-578
- [ ] **Covered fields come back in the projection's order, not the key's.**
      mongod 8.2.11 builds a covered row from the index key, so `{_id: 0,
      x: 1, y: 1}` over `{y: 1, x: 1}` returns `{y: .., x: ..}`, from one
      scan (`PROJECTION_COVERED`) or an `$or` of them (`PROJECTION_DEFAULT`
      over the OR; a fetched branch's rows keep the document's order). This
      server returns every row in the document's order, `{x: .., y: ..}`:
      a field order a client sees. Found 2026-11-12 (RM-471).
      Verify: tests/backlog/test_rm_found_rm471.py::test_covered_fields_come_back_in_the_key_patterns_order
      Backlog-ID: RM-569
- [ ] **A computed projection over index keys fetches.** mongod 8.2.11
      answers `{_id: 0, x: 1, k: {$add: ["$x", 1]}}` from the keys of
      `{y: 1, x: 1}` (`PROJECTION_DEFAULT <- IXSCAN`; over an `$or`, no
      FETCH either): every field it reads is in the index. This server
      fetches (`PROJECTION_DEFAULT <- FETCH <- IXSCAN`); `covers` refuses a
      projection with an expression. Found 2026-11-12 (RM-471).
      Verify: tests/backlog/test_rm_found_rm471.py::test_a_computed_projection_reads_index_keys
      Backlog-ID: RM-570
- [ ] **An `$or` over two indexes returns its branches out of order.**
      mongod 8.2.11 reads `{$or: [{y: 0}, {x: {$gt: 8}}]}` over `{y: 1,
      x: 1}` and `{x: 1}` branch by branch as its plan lists them,
      `y_1_x_1` first: `_id` 0, 3, 6, 9, then 10, 11. This server's explain
      lists the same order, but it returns the `x_1` branch first (9, 10,
      11, 0, 3, 6). Sorted on `y` with `limit: 2` it then returns different
      rows (`x` 9 and 0, where mongod returns 0 and 3), since the sort's
      ties keep the read order. Found 2026-11-12 (RM-471).
      Verify: tests/backlog/test_rm_found_rm471.py::test_an_or_of_two_indexes_returns_its_branches_in_order
      Backlog-ID: RM-572
- [ ] **An `{_id: 1}` projection is not covered.** mongod 8.2.11 answers
      `find({y: 0}, {_id: 1})` from `{y: 1, _id: 1}`
      (`PROJECTION_COVERED <- IXSCAN`), and from an `$or` over `_id_` and
      that index (`PROJECTION_DEFAULT <- OR`). This server fetches each
      document: `covers` refuses a projection of `_id` alone. Found
      2026-11-12 (RM-471).
      Verify: tests/backlog/test_rm_found_rm471.py::test_an_id_projection_is_covered
      Backlog-ID: RM-573
- [ ] **A clause beside an `$or` is rechecked in a FETCH.** mongod 8.2.11
      pushes `x: {$lt: 10}` beside `{$or: [{y: 0}, {y: 1, x: {$gt:
      3}}]}` into each branch's bounds over `{y: 1, x: 1}` and rechecks
      nothing, so `{_id: 0, x: 1}` is answered from the keys
      (`PROJECTION_DEFAULT <- OR <- IXSCAN`). This server's bounds are the
      same, but it rechecks the clause in a FETCH over the OR (`FETCH`
      filter `{x: {$lt: 10}}`), so it also fetches for a covered
      projection. Found 2026-11-12 (RM-471).
      Verify: tests/backlog/test_rm_found_rm471.py::test_a_clause_beside_an_or_is_answered_by_its_bounds
      Backlog-ID: RM-574
- [ ] **A sort over an `$or` with a fetched branch sorts documents.**
      mongod 8.2.11's OR of `FETCH <- IXSCAN w_1` and `IXSCAN y_1_x_1` is
      not fetched, and every branch carries `x` (a fetched one carries every
      field), so `.sort({x: 1})` sorts the OR's members below a FETCH:
      `SUBPLAN <- FETCH <- SORT (default) <- OR`; with `{_id: 0, x: 1}`,
      `SORT <- PROJECTION_DEFAULT <- OR`, no FETCH. This server sorts
      fetched documents above it (`SORT (simple) <- FETCH <- OR`): its
      `keys_hold` takes only index-scan branches. Found 2026-11-12
      (RM-471).
      Verify: tests/backlog/test_rm_found_rm471.py::test_a_sort_over_an_or_with_a_fetched_branch_sorts_below_the_fetch
      Backlog-ID: RM-575
- [ ] **A rooted `$or`'s profile `planSummary` names one index twice.**
      mongod 8.2.11's profile entry for `{$or: [{a: {$gte: 2}, b: {$gte:
      3}}, {a: 0, b: 0}]}` over `a_1` and `b_1` says `IXSCAN { a: 1 },
      IXSCAN { b: 1 }`, the index each branch won with. This server's
      explain names the same two, but its profile entry says `IXSCAN { a: 1
      }, IXSCAN { a: 1 }`. Found 2026-11-12 (RM-471).
      Verify: tests/backlog/test_rm_found_rm471.py::test_a_rooted_ors_plan_summary_names_each_branchs_index
      Backlog-ID: RM-576
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
      with the cursor from batch to batch. The line of a `find`, `count`
      or `distinct` of a collection with one candidate plan, a `getMore` of
      a `find`, an `aggregate` of a collection mongod's query layer runs
      whole, or whose stages after its cursor stage are `$project`,
      `$unset`, `$addFields`, `$set`, `$replaceRoot`, `$replaceWith`,
      `$unwind` and `$skip`, and the `getMore`s of its cursor,
      `listIndexes`, `listCollections`, `collStats`, `ping`, `buildInfo`,
      and an `insert`, `update` or `delete` on a member of a replica set
      prints mongod's count (74 shapes in
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
        its read (mongod's optimizer moves some where this server's does
        not: RM-538, RM-580), a `$limit` there (it stops its source short
        of the documents this server runs in one go), a `$lookup`,
        `$unionWith`, `$facet`, `$geoNear` or `$out` / `$merge`, a stage
        read whole that SBE does not take whole (a `$sort` after a
        `$group`, `$count`, `$bucket`: 5 on mongod), a pipeline whose
        expressions run `$map`, `$filter` or `$reduce` (this server ticks
        the checker once an element, mongod 8.2.11 not at all), one over a
        view or a time-series collection, and one asked for a first batch
        of none;
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
- [ ] **An `update` or `delete` statement's line leaves out the command's
      `comment`.** mongod 8.2.11's `Slow query` line for each statement
      shows it with its command's `comment` (`{q, limit, comment}`,
      `{q, u, multi, upsert, comment}`); this server's shows the statement
      alone, so a log search for the comment misses the statement's line.
      Found while working RM-478, 2026-11-13.
      Verify: tests/backlog/test_rm_statement_line_comment.py::test_a_statement_line_carries_its_commands_comment
      Backlog-ID: RM-581
- [ ] **A time-series collection's `collMod` and `collStats` lines name the
      collection, not its buckets.** mongod 8.2.11's name
      `<db>.system.buckets.ts`, `collectionType: "timeseriesBuckets"`; this
      server's name `<db>.ts`, `normal`. Found while working RM-380,
      2026-11-10.
      Verify: tests/backlog/test_rm_view_namespaces.py::test_a_time_series_ddl_line_names_its_buckets
      Backlog-ID: RM-484
- [ ] **A `dataSize` of a view answers.** mongod 8.2.11 refuses
      `{dataSize: "<db>.v"}` of a view (166, CommandNotSupportedOnView);
      this server answers `ok: 1`, sizing nothing. Found while working
      RM-485, 2026-11-13.
      Verify: tests/backlog/test_rm_view_ddl_refusals.py::test_a_data_size_of_a_view_is_refused
      Backlog-ID: RM-587
- [ ] **A `dropIndexes` of `system.views` is refused.** mongod 8.2.11 drops
      `<db>.system.views`' indexes (`ok: 1`, its line and profile entry
      naming `<db>.system.views`, `system`); this server refuses it with
      NamespaceNotFound (26), though `system.views` exists. Found while
      working RM-485, 2026-11-13.
      Verify: tests/backlog/test_rm_view_ddl_refusals.py::test_a_drop_indexes_of_system_views_answers
      Backlog-ID: RM-588
- [ ] **An `explain` of a write is profiled.** mongod 8.2.11 writes no
      profile entry for an `explain` of an `update` (a `delete` or
      `findAndModify` not measured); this server writes one, named for the
      collection. Found while working RM-380, 2026-11-10.
      Verify: tests/backlog/test_rm_view_namespaces.py::test_an_explain_of_a_write_is_not_profiled
      Backlog-ID: RM-486
- [ ] **An `update` or `delete` of a view writes no statement line.**
      mongod 8.2.11 refuses the statement and writes its line (`<db>.v`,
      `normal`, `ok: 0`, the refusal) before the command's; this server
      writes the command's line alone. Found while working RM-380,
      2026-11-10.
      Verify: tests/backlog/test_rm_view_namespaces.py::test_a_write_to_a_view_has_a_statement_line
      Backlog-ID: RM-487
- [ ] **A refused `listIndexes` or `collStats` of a view is not profiled.**
      mongod 8.2.11 writes an entry for each (`<db>.v`, `ok: 0`); this
      server writes none. Found while working RM-380, 2026-11-10.
      Verify: tests/backlog/test_rm_view_namespaces.py::test_a_refused_list_indexes_of_a_view_is_profiled
      Backlog-ID: RM-488
- [ ] **A `getMore` of a `find` of a view names a write concern.** mongod
      8.2.11's line and profile entry for it carry no `writeConcern`, as
      one of a `find` of a collection; this server's carry the one an
      `aggregate`'s `getMore` runs under, since it runs the `find` as an
      `aggregate`. Found while working RM-380, 2026-11-10.
      Verify: tests/backlog/test_rm_view_namespaces.py::test_a_get_more_of_a_find_of_a_view_names_no_write_concern
      Backlog-ID: RM-489
- [ ] **A `distinct` of a view is counted as failed.** A view's `distinct`
      answers `ok` as an int32, as mongod's does, and about 270 readers of
      a reply's `ok` (`get_f64("ok")`) take it for a failure:
      `serverStatus` counts it in `metrics.commands.distinct.failed`, where
      mongod 8.2.11 does not, and the transaction, interrupt and
      write-concern paths read it the same way. The profiler reads it with
      `util::reply_ok` since RM-380. Found while working RM-380,
      2026-11-10.
      Verify: tests/backlog/test_rm_view_namespaces.py::test_a_distinct_of_a_view_is_not_counted_failed
      Backlog-ID: RM-490
- [ ] **`metrics.commands.<name>` has no `rejected`.** mongod 8.2.11 counts
      `failed`, `rejected` and `total` for a command; this server counts
      `failed` and `total`. Found while working RM-380, 2026-11-10.
      Verify: tests/backlog/test_rm_view_namespaces.py::test_a_command_metric_counts_rejections
      Backlog-ID: RM-491
- [ ] **A `distinct` of a view counts nothing it examined.** mongod 8.2.11
      runs it as an aggregation over the collection beneath the view, and
      its line counts what that plan examined (`keysExamined: 11,
      docsExamined: 11, cursorExhausted: true` over an `IXSCAN { x: 1 }`),
      as an `aggregate` of the view's does here; this server reads the
      view's documents another way (`aggregate::collection_documents`) and
      prints none of the three. Since RM-381 it plans that read, for the
      `planningTimeMicros`, without running it. Found while working
      RM-381, 2026-11-10.
      Verify: tests/backlog/test_rm_view_lines.py::test_a_distinct_of_a_view_counts_what_it_examined
      Backlog-ID: RM-493
- [ ] **A `count` or `aggregate` of a view names a COLLSCAN beneath it.**
      Over a view `{$match: {x: {$gte: 1}}}` on a collection indexed on
      `x`, mongod 8.2.11's line for a `count_documents({})` names
      `COUNT_SCAN { x: 1 }` (`docsExamined: 0`) and for a `$group` names
      `IXSCAN { x: 1 }`; this server's name `COLLSCAN`, while counting 11
      keys and 11 documents examined. Its explain of such a `count` is
      RM-072. Found while working RM-381, 2026-11-10.
      Verify: tests/backlog/test_rm_view_lines.py::test_a_count_and_an_aggregate_of_a_view_name_the_plan_beneath
      Backlog-ID: RM-494
- [ ] **A `Slow query` line takes a command's string argument for its
      collection.** mongod 8.2.11's lines for a `dataSize: "<db>.c"`, a
      `getLog` and a `compact: "c"` name `<db>.$cmd`; this server's name
      `<db>.<db>.c`, `<db>.startupWarnings` and `<db>.c`
      (`slowop::target_collection` reads any command's string value as
      one; a `validate`'s names `<db>.$cmd` since RM-485). Found while
      working RM-383, 2026-11-10; `compact` measured while working RM-485,
      2026-11-13.
      (This was RM-480: A `dataSize` line names `<db>.<db>.c`.) mongod 8.2.1's
      `Slow query` line for a `dataSize`, whose argument is a full namespace,
      names `<db>.$cmd`; this server's joins the command's database to the
      argument. Found while working RM-378, 2026-11-09.
      Verify: tests/backlog/test_rm_drop_reports.py::test_a_command_s_string_argument_is_not_its_collection, tests/backlog/test_rm_slow_query_namespace.py::test_a_data_size_line_names_its_own_namespace
      Backlog-ID: RM-495
- [ ] **Operations this server runs another way than mongod's plan say
      less.** Since RM-023 a read or a write's read that a stream runs as
      mongod's plan counts its keys and documents examined, and since
      RM-359 its profile entry carries its stages (`execStats`); these do
      neither:
      - a read two or more plans could serve, which mongod chooses by
        trial, other than a `find`'s: an `aggregate`'s, a `count`'s, a
        `distinct`'s, a write's. Its documents come from the store's
        router. mongod counts what the winner examined in its trial, and
        since RM-349 a `find` streams the winner after its trial's buffer
        and counts as mongod's does; the other readers still decline a
        contested plan (`order::stream_plan`);
      - a read of a time-series collection made whole: under a `$sort`, a
        `$group` (`count_documents`), an inclusion `$project` (RM-456), a
        filter on the meta field (RM-457), a hint or a collation. mongod
        counts the buckets its cursor stage buffered (five of five for a
        sorted `find`). Since RM-455 a `find` is the aggregation mongod
        converts it to, and counts where a time-series `aggregate` streams;
      - a geo, `$near` or `$text` read (`$geoWithin` over a `2d` index:
        mongod 5 keys and 4 documents, this server none).
      Measured 2026-10-27, 2026-11-07 and 2026-11-10.
      Backlog-ID: RM-363
      Verify: tests/backlog/test_rm_355_364.py::test_a_contested_aggregate_counts_what_it_examined, tests/backlog/test_rm_355_364.py::test_a_time_series_read_made_whole_counts_what_it_examined
- [ ] **A read of a time-series collection's meta index names the index
      as `listIndexes` shows it.** mongod 8.2.11's line for a `find({m: 1})`
      of a time-series collection says `IXSCAN { meta: 1, control.min.t: 1,
      control.max.t: 1 }`, the index as its buckets keep it; this server's
      says `IXSCAN { m: 1, t: 1 }`. Found while working RM-363, 2026-11-10.
      Verify: tests/backlog/test_rm_found_2026_11_10_rm363.py::test_a_read_of_the_meta_index_names_the_index_over_the_buckets
      Backlog-ID: RM-496
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
      Verify: tests/backlog/test_rm_355_364.py::test_an_insert_in_a_transaction_counts_mongods_dirty_bytes
      Backlog-ID: RM-499

- [ ] **mongod's startup lines this server does not write.** Since RM-302
  the log is mongod's structured log, and since 2026-11-10 this server
  starts it as mongod 8.2.11 does on `initandlisten`: `MongoDB starting`,
  `Build Info` (23403), `Operating System` (51765) and `Options set by
  command line` (21951, which carries RM-425's `parsed` until that is
  fixed). mongod then writes about seventy more there -- the network
  lines, `Opening WiredTiger` (22315) and the WiredTiger messages under
  it, `WiredTiger opened` (4795906), the recovery, catalog, FCV, FTDC and
  flow-control lines -- and ends with `mongod startup complete`
  (8423403); this server writes none of them (measured 2026-10-24,
  2026-11-07 and 2026-11-10). Found with RM-302.
  Backlog-ID: RM-305
  Verify: tests/backlog/test_rm_001_028.py::test_startup_writes_the_storage_and_startup_complete_lines
- [ ] **`buildInfo` names the architecture as Rust does.** mongod 8.2.11 on
  macOS arm64 gives `arm64` as `buildEnvironment.distarch` and
  `target_arch` (and in the `Build Info` startup line); this server gives
  Rust's `std::env::consts::ARCH`, `aarch64` (measured 2026-11-10). Found
  while working RM-305.
  Backlog-ID: RM-500
  Verify: tests/backlog/test_rm_found_2026_11_10.py::test_build_info_names_the_architecture_as_mongod_does
- [ ] **A command its handler fails to parse is counted as a connection's
  first command.** mongod 8.2.11 writes `Received first command on ingress
  connection` (6788700) when a command is about to run: one its IDL parse,
  its generic arguments, its concerns, `maxTimeMS` or its authorization
  refuses is passed over, and the next command writes the line, timed from
  the `hello`. This server writes it at the same point of its dispatcher
  (since 2026-11-10), so the checks the dispatcher makes are passed over;
  but its handlers parse their own fields inside the run, so a command
  refused there -- `{find: "c", bogus: 1}` (40415), `{insert: "c",
  documents: 1}` (14), an unknown pipeline stage (40324), `lsid: 1` (14),
  and the like -- still counts. The fix is a parse step for each handler
  ahead of its run, which the line can follow (measured 2026-11-10). Found
  with RM-305.
  (This was RM-481: A command refused as it parses writes a `Slow query`
  line.) mongod 8.2.1 writes no line for a command its IDL parse refuses -- a
  mistyped or missing field (14, 40414: `renameCollection: 5`, no `to`), an
  unknown one (40415: `find` with `bogus`), a collection name that is not a
  string (73: `find: 5`, `count: 5`), an unknown pipeline stage (40324: an
  `aggregate` of a collection or a view, measured 8.2.11 while working RM-380)
  -- as the operation never ran; this server writes one for each, since its
  handlers parse and run in one step (as RM-386 has it). Found while working
  RM-378, 2026-11-09.
  Backlog-ID: RM-386
  Verify: tests/backlog/test_rm_001_028.py::test_a_command_its_handler_fails_to_parse_is_not_the_first_command, tests/backlog/test_rm_slow_query_namespace.py::test_a_command_that_fails_to_parse_writes_no_line
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

- [ ] **A write of many through an index, or on a contested plan, reads
      every candidate before it writes one.** mongod writes each document
      as its plan returns it, whatever the plan, so an updateMany or
      deleteMany out of `maxTimeMS` part-way has written some (mongod
      8.2.11, 2026-11-10, 100,000 documents, `maxTimeMS: 30`: through index
      `a_1` of a collection clustered by `_id` 4,599 updated and 11,280
      deleted; a contested plan over `a_1` and `b_1` 1,536 updated and
      10,030 deleted). A plan this server's write cannot stream -- a
      clustered or time-series collection, a contested or geo plan --
      leaves the write to the store, which since 2026-11-10 reads its
      candidates a page at a time and writes each page first, so a
      collection scan out of time keeps what it wrote. Two reads still run
      whole before the first write, and a budget spent in them writes
      nothing: the router's RecordIds of an index (`try_index_id_keys`,
      read whole before the first page), and the read that picks a
      contested plan (`explain::order::through_cache` runs `ordered_rows`
      over every match; on a replica set a deleteMany's `delete_order`
      does the same to work out mongod's order; both read from the code,
      the nothing-written measured). The work is to read the
      index a page at a time into the write, and to choose the contested
      plan by mongod's trial period rather than a whole read; a geo plan
      is the same shape, not yet probed.
      Backlog-ID: RM-390
      Verify: tests/backlog/test_rm_found_2026_11_10_rm390.py::test_a_write_of_many_out_of_time_keeps_what_it_wrote_whatever_its_plan
- [ ] **A batched deleteMany tests each document's filter once, where
      mongod tests it twice.** mongod's batched multi-delete
      (`batchUserMultiDeletes`, on by default) tests a document when it
      stages it and again when it deletes the batch, the snapshot having
      moved (`ensureStillMatches`): a deleteMany of one document whose
      filter takes 1.0 s to test takes 1.95 s, three take 5.9 s, and with
      the parameter off they take 0.98 s and 2.95 s (mongod 8.2.11,
      2026-11-10, a one-member replica set). This server tests it once,
      so a deleteMany fits in a `maxTimeMS` mongod runs out of (code 50).
      Found while closing RM-391, whose once-per-document rule holds for
      every other write.
      Backlog-ID: RM-501
      Verify: tests/backlog/test_rm_found_2026_11_10_rm391.py::test_a_batched_delete_many_tests_each_document_twice

### 7.086 The Rust MongoDB server, audited -- 2026-10-20

An audit of the command, expression and wire layers, each finding reproduced
against mongod 8.2.11 (standalone, or a one-member replica set where it
needs sessions) with a `secantusd-rs` built from tree `af2629ec`. The gap
tests are in `tests/backlog/test_rm_found_2026_10_20b.py` and ask mongod
live. Not filed here: `$rankFusion` / `$score` / `$scoreFusion` (RM-198),
`j: true` durability (RM-186), `storageSize` (RM-040).

- **Scope decision — a snapshot read at a past time needs a store whose
  data tables are not logged** (Joe, 2026-10-25). WiredTiger keeps no
  history for a logged table, so a store created with `--data-logged`, or by
  the PostgreSQL server, refuses `atClusterTime` before the newest time with
  246 SnapshotUnavailable, by name, rather than answer as of now; without
  `atClusterTime` it reads the latest data, which is correct. `secantusd-rs`
  creates its stores the other way, mongod's (RM-210).
- [ ] **A `$setWindowFields`'s spill takes this server's size on disk,
      not mongod's.** A sort's and a classic `$group`'s spill files are
      mongod 8.2's sorter files to the byte (`test_spill_file_storage_size`,
      `test_group_spill_storage_size`). mongod spills a `$setWindowFields`'s
      partition cache to a WiredTiger temporary table
      (`spilledDataStorageSize: 4096`), where this server writes a file of
      its own (310800); the engine crate cannot reach a WiredTiger table
      today. Two more spills are this server's and unmeasured: an SBE
      `$group` (mongod: a WiredTiger record store; its budget is not
      settable at runtime) and a `$median` / `$percentile` state (mongod
      writes its t-digest; this server keeps, and writes, the values).
      Backlog-ID: RM-325
      Verify: tests/backlog/test_rm_found_2026_10_26.py::test_spill_storage_size
- [ ] **An SBE sort keeps a field only its key reads.** mongod 8.2.11's
      slot-based sort in front of `{$group: {_id: "$k", f: {$first:
      "$s"}}}` keeps `k` and `s` beside its key `n`, a row of 1064 bytes for
      documents of a 1000-byte `s`, and over 300 of them under a 50000-byte
      budget counts 274512 in `totalBytesSorted` and
      `sortTotalDataSizeBytes` (six spills of 43 rows while reading). This
      server's row also keeps `n` (1073), so it counts 276834. Found
      2026-11-10 (RM-398).
      Verify: tests/backlog/test_rm_326_sort_counts.py::test_sbe_sort_row_leaves_out_a_field_only_the_key_reads
      Backlog-ID: RM-504
- [ ] **An SBE sort's spilled records are a byte longer than mongod's.**
      mongod 8.2.11's slot-based sort in front of a `$group`, over 300
      documents of a 1000-byte `s` under a 50000-byte budget, writes 13 runs
      of 900 records in 927900 bytes (1031 a record); this server writes the
      same runs in 928800 (1032). Found 2026-11-10 (RM-398).
      Verify: tests/backlog/test_rm_326_sort_counts.py::test_sbe_sort_spilled_bytes
      Backlog-ID: RM-506
- [ ] **A `range` window's `"current"` bound takes in tied documents.**
      mongod 8.2.1 reads `"current"` in a `range` window as the document
      answered (`getEndpointsRangeBased`: offset 0), so over `n` of 1, 1,
      2, 2, 2, 3 a `$push` of `_id` over `["unbounded", "current"]` gives
      `[0], [0, 1], [0, 1, 2], ...`; this server bounds the window by the
      current sort value (`range_window_bounds`), taking in every document
      tied with it: `[0, 1], [0, 1], [0, 1, 2, 3, 4], ...`. The same for a
      `"current"` lower bound. A wrong answer for any sort key with ties.
      Found 2026-11-10 (RM-404).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm404.py::test_a_range_windows_current_is_the_document
      Backlog-ID: RM-508
- [ ] **`$integral` and the covariances count no memory in a window.**
      mongod 8.2.1 counts what they hold: `maxFunctionMemoryUsageBytes`
      3888 for a `["unbounded", 4]` `range` `$integral` over 40 documents,
      568 for a `["unbounded", -1]` `$covariancePop`. This server counts
      nothing for either (`FunctionMemory::Nothing`), so `explain` shows 0
      and the stage runs under budgets mongod refuses (500 against 4300,
      375 against 818). Found 2026-11-10 (RM-404).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm404.py::test_integral_and_covariance_count_their_memory
      Backlog-ID: RM-509
- [ ] **A partitioned window's `maxTotalMemoryUsageBytes` is 16 bytes off.**
      With `partitionBy` over three partitions of 40 small documents,
      macOS mongod 8.2.1 reports 748 for a `range [-4, 4]` `$sum` (here
      764), 2180 for a whole-partition `$sum` (2196), 480 for a running
      `$sum` (464) and 432 for `$expMovingAvg` (416). Both directions, one
      partition key's size; the least budgets agree, so the moment the
      tracker sees differs, not what the budget counts. Cause not yet
      traced. Found 2026-11-10 (RM-404).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm404.py::test_a_partitioned_windows_total_memory
      Backlog-ID: RM-510
- [ ] **The sort key is lost at a `$replaceRoot`, a `$lookup` or a
      `$setWindowFields`.** mongod 8.2.11 keeps a document's sort key
      through a `$replaceRoot`, a `$lookup` and a `$setWindowFields` with no
      `sortBy`, and a `$setWindowFields` that sorts leaves its own (the
      partition key, then `sortBy`). This server ends the key at each
      (`agg_meta::KEEPS_SORT_METADATA`), so a `$_internalSetWindowFields`
      after one ranks every document 1 and `{$meta: "sortKey"}` reads `[]`.
      Found 2026-11-10 (RM-410).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm410.py::test_the_sort_key_passes_through_stages_that_keep_it
      Backlog-ID: RM-520
- [ ] **A `$lookup` sub-pipeline's `$sort` sets no sort key.** In a
      `$lookup` pipeline, mongod 8.2.11 ranks a `$_internalSetWindowFields`
      by the key the sub-pipeline's `$sort` set; this server does not run
      `agg_meta::rewrite_sort_metadata` over a sub-pipeline, so every
      document ties. Found 2026-11-10 (RM-410).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm410.py::test_a_lookup_sub_pipeline_sort_sets_the_key
      Backlog-ID: RM-521
- [ ] **A rank with no sort key at all is answered, not refused.** After a
      `$group` or a `$unionWith` a document carries no sort key
      (`{$meta: "sortKey"}` is missing there, not `[]`), and mongod 8.2.11
      refuses a `$_internalSetWindowFields` `$rank` with 9973200 "Attempt to
      get sort key which has not been populated"; this server ties the
      documents. A document read from the collection with no `$sort` has the
      empty key, which ties, on both. Found 2026-11-10 (RM-410).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm410.py::test_a_rank_with_no_sort_key_is_refused
      Backlog-ID: RM-522
- [ ] **A `$sort` behind another stage sets the sort key where mongod's
      keeps the earlier one.** Behind a leading `$sort` and an `$addFields`,
      mongod 8.2.11's key after a second `$sort` is still the first's; a
      `$sort` behind a leading `$addFields` or `$project` leaves the empty
      key. `{$meta: "sortKey"}` and a `$_internalSetWindowFields`'s ranks
      show it alike; this server takes the last `$sort`'s key. (Behind an
      `$unwind` mongod does take the later `$sort`'s.) Cause in mongod not
      traced: measured, not derived. Found 2026-11-10 (RM-410).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm410.py::test_which_sort_sets_the_key
      Backlog-ID: RM-523
- [ ] **A `$sort` that a later `$sort` overrides still orders the ties.**
      mongod 8.2.11 drops the earlier `$sort` of `$sort: {k: 1}, $sort: {n:
      1}`, also across an `$unwind`, so documents tied on `n` come in the
      collection's order; this server runs both, and the ties keep the
      first sort's order. A different document order. Found 2026-11-10
      (RM-410).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm410.py::test_a_sort_before_another_sort_is_dropped
      Backlog-ID: RM-524
- [ ] **`{$meta: "sortKey"}` gives an empty array's key as null.**
      mongod 8.2.11's key for an empty array is `undefined`, which pymongo
      decodes as `None`: `$type` reads `"undefined"`, and a `$group` on the
      key keeps the empty array's document and a null's apart. This server
      reads the key through `$ifNull` (`agg_meta::sort_key_read`), since its
      expressions take `undefined` for MISSING. The ranks already tell the
      two apart (RM-410). Found 2026-11-10.
      Verify: tests/backlog/test_rm_found_2026_11_10_rm410.py::test_an_empty_arrays_sort_key_is_undefined
      Backlog-ID: RM-525
- [ ] **A sub-document a stage reads or writes into is sized as if it
      were read.** mongod 8.2.11 gives a sub-document a cache of its own
      when a stage reads a field inside it (`$addFields: {q: "$e.y"}`) or
      sets one (`$set: {"e.z": 1}`), and the sort and window behind count
      it: (349500, 369000) and (305100, 324600) bytes over 300 documents.
      This server's storage model (`docstore`, RM-408) caches top-level
      fields only, and sizes the sub-document as the BSON it was read from:
      (288900, 308400) and (290700, 288300).
      Verify: tests/backlog/test_rm_found_rm408.py::test_a_sub_document_a_stage_touched_is_sized_by_its_storage
      Backlog-ID: RM-515
- [ ] **`explain` counts every document a `$sort` with a `$limit` read.**
      For `[{$addFields: {q: "$b"}}, {$sort: {n: -1}}, {$limit: 200}]`
      over 300 documents mongod 8.2.11's `totalDataSizeSortedBytesEstimate`
      is the 200 its top-k sort kept (131400); this server's is all 300
      (197100), as for the same pipeline without the `$limit`. A pipeline
      run outside `explain` takes the `$limit` (`limit_for_pushdown`), so
      look first at the run `explain` reads the `$sort`'s counters from.
      Found 2026-11-10 (RM-408).
      Verify: tests/backlog/test_rm_found_rm408.py::test_explain_counts_the_documents_a_sort_with_a_limit_kept
      Backlog-ID: RM-516
- [ ] **`explain` lists a `$sort` that a window's own sort replaces.** For
      `[{$unwind: "$t"}, {$sort: {t: 1}}, {$setWindowFields: {sortBy: {n:
      1}, ...}}]` mongod 8.2.11 drops the `$sort` on `t` and moves the
      window's `$sort` on `n` ahead of the `$unwind`, listing `$unwind` and
      `$_internalSetWindowFields` alone. This server lists both `$sort`s
      after the `$unwind`, and counts both. Found 2026-11-10 (RM-408).
      Verify: tests/backlog/test_rm_found_rm408.py::test_a_sort_a_window_supersedes_is_not_listed
      Backlog-ID: RM-517
- [ ] **`$_internalInhibitOptimization` is refused as an unknown stage.**
      mongod 8.2.11 runs it -- it passes its documents on and keeps the
      stages after it out of the scan -- so a pipeline with it answers as
      one without. This server refuses it (40324, "Unrecognized pipeline
      stage name"). Found 2026-11-10 (RM-408).
      Verify: tests/backlog/test_rm_found_rm408.py::test_internal_inhibit_optimization_runs
      Backlog-ID: RM-518
- [ ] **A document an inclusion `$project` built is sized as if it were
      read.** Behind an `$unwind` (so not taken by the scan), mongod
      8.2.11's `$project: {t: 1, n: 1}` builds each document it returns,
      and the `$sort` after it counts that storage: 351300 bytes over 900
      documents. This server counts each as the BSON it encodes, 188700:
      the storage model (`docstore`, RM-408) stops at a `$project`. Found
      2026-11-10 (RM-408).
      Verify: tests/backlog/test_rm_found_rm408.py::test_a_document_a_project_built_is_sized_as_built
      Backlog-ID: RM-519
- [ ] **On macOS documents whose sort keys tie come in another order
      than mongod's.** mongod's sorter sorts what it holds with
      `std::sort`, which is not stable, so documents whose keys compare
      equal come in the C++ library's order. This server sorts as
      libstdc++, the Linux build's library, does (RM-402: Linux mongod
      8.2.12 and this server agree on every case below), and on macOS keeps
      them in the order they came, where mongod 8.2.11's libc++ reorders
      them. Over 300 1-KB documents under a 50000-byte budget, a sort by
      `{k: -1}` (seven values of `k`) stores 51260 bytes for its 13 spills
      on mongod and 51242 here -- `find` and a pipeline's leading `$sort`
      alike -- and `{k: 1}` 51240 and 51244 (this was RM-503); the 40
      documents over three values of `k` a `$sort` after `$project`
      returns come back 39, 3, 27, 9, ... from mongod and in their order
      here. A sort whose keys are all different agrees. The fix is
      libc++'s `std::sort`, step for step, where `stl::LIBCXX`. Checked on
      macOS only. Found 2026-11-10 while working RM-395 and RM-400.
      Verify: tests/backlog/test_rm_found_2026_11_10_rm395.py::test_tied_sort_spill_storage_size, tests/backlog/test_rm_326_sort_counts.py::test_descending_and_tied_spill_storage_size, tests/backlog/test_rm_found_2026_11_10_rm395.py::test_macos_tied_sort_order
      Backlog-ID: RM-526
- [ ] **`find`, a leading `$sort` and `$setWindowFields` hand back
      documents whose sort keys tie in the order they were read.** mongod
      runs each through the same sorter as any other sort, and its
      `std::sort` reorders ties: 40 documents over three values of `k`
      sorted by `{k: 1}` come back 39, 3, 27, 9, ... from all four of
      `find`, `[{$sort}]`, `$setWindowFields` and a `$sort` after `$project`
      on macOS mongod 8.2.11, and libstdc++'s `std::sort` (g++ 15.2), the
      Linux build's, gives 0, 21, 18, 24, .... Since RM-402 the sorter here
      (`sorter::sort_held`) gives the Linux order for a `$sort` after
      another stage, and since RM-312 so do `find` and a leading `$sort`
      (the read's `SORT`, `explain/order.rs` and `explain/stream.rs`); the
      window stage's own sort is still stable. On macOS the order also
      needs RM-526. Found 2026-11-10 working RM-402.
      Verify: tests/backlog/test_rm_found_2026_11_10_rm402.py::test_tied_order_of_find_leading_sort_and_window
      Backlog-ID: RM-527
- [ ] **A classic `$group` counts its held values' and `_id`'s memory
      unlike mongod, so it spills at other points.** Over 300 documents of a
      1000-byte string, budget 5000, mongod 8.2.11 spills a `$first` /
      `$last` / `$firstN` / `$maxN` of the string 60 times, as it does a
      `$push` (each accumulator counts the `Value` it holds), where this
      server spills 8 to 13 times: it counts a classic `$first` / `$last`
      as its fixed size alone, a `$firstN` / `$maxN` / `$topN` / `$bottom`
      short of mongod's (`$topN`: 38 spills to mongod's 75), and a
      `$median` over it (20 to 17). An object `_id` goes the other way: mongod
      counts the key it groups on (one field's value, or an array of the
      fields'), this server the built `_id` document (`{_id: {n: "$n"}}`:
      15 spills to 9; two fields under a `$push`: 75 to 60). The spill
      file is mongod's (RM-325's fix); its size follows the runs. Found
      2026-11-10 working RM-325.
      Backlog-ID: RM-502
      Verify: tests/backlog/test_rm_found_2026_10_26.py::test_group_held_value_spills
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
- [ ] **`$queryStats`' remainder: cache size, explains, transformed
      identifiers.** Since 2026-11-10 the query stats store records
      `find`, `aggregate`, `distinct` and `count` under mongod's key, with
      its metrics (`tests/mongod_live/test_query_stats.py`). mongod 8.2.11
      also: reports `internalQueryStatsCacheSize` ("1%"), sizes the store
      by it and evicts the least recent entries past it, and turns
      `$queryStats` off (6579000) at "0MB"; samples by
      `internalQueryStatsSampleRate`; records an `aggregate` that explains,
      its key saying `explain: "queryPlanner"`; and lists every identifier
      (field paths, namespaces, `let` names) as its HMAC-SHA-256 under
      `transformIdentifiers`. This server reports neither parameter, keeps
      every entry, records no explain, and refuses `transformIdentifiers`
      (238) once the store has entries to print. Unmeasured: the key's
      `writeConcern` and `$readPreference` (copied as the command gave
      them), the order of `otherNss` past one namespace (mongod's is a hash
      set's), and `fromMultiPlanner` for a rooted `$or` the subplanner
      plans (counted false here). Found 2026-11-10 working RM-215.
      Verify: tests/backlog/test_rm_found_2026_11_10_rm215.py::test_query_stats_remainder
      Backlog-ID: RM-532
- [ ] **A date-part operator's argument as an array of one fails.**
      mongod 8.2.11 takes `{$year: ["$t"]}` (any of `$year` ... `$isoWeek`,
      `$dayOfWeek`) as `{$year: "$t"}`: null for a missing field, the part
      of a date. This server evaluates the array itself and fails with
      16006 "can't convert from BSON type array to Date"; with
      `[{date: "$t"}]` mongod's 16006 names an object where this server's
      names an array. Found 2026-11-10 working RM-215, running the
      query-shape corpus's expressions.
      Verify: tests/backlog/test_rm_found_2026_11_10_rm215.py::test_a_date_part_takes_an_array_of_one
      Backlog-ID: RM-533
- [ ] **`$lookup` runs a `let` that is not a document, and a lone join field.**
      mongod 8.2.11 refuses `let: 5` with 14 "BSON field '$lookup.let' is
      the wrong type 'int', expected type 'object'", and `localField`
      without `foreignField` (or the reverse) with 9 "$lookup requires both
      or neither of 'localField' and 'foreignField' to be specified". This
      server runs the first, runs the lone field when there is a pipeline,
      and answers 2 without one. Found 2026-11-10 (RM-413).
      Verify: tests/backlog/test_rm_found_2026_11_10_lookup.py::test_lookup_refuses_a_bad_let_and_a_lone_join_field
      Backlog-ID: RM-528
- [ ] **A `$lookup` whose `localField` is an empty array joins nothing.**
      mongod 8.2.11 joins it as it joins null: to the foreign documents
      whose `foreignField` is missing, null or `[null]`, in the simple form
      and with a pipeline. This server's `lookup_match` finds no element to
      compare and joins none. Found 2026-11-10 (RM-413).
      Verify: tests/backlog/test_rm_found_2026_11_10_lookup.py::test_an_empty_local_array_joins_what_null_joins
      Backlog-ID: RM-529
- [ ] **`$documents` in a `$facet` is not refused with 40600.** mongod
      8.2.11 refuses it with 40600 "$documents is not allowed to be used
      within a $facet stage", and names the stage that holds it when a
      `$unionWith` or `$lookup` in the facet does ("$documents inside of
      $unionWith is not allowed ..."). This server answers 73 to the first
      and runs the other two. Found 2026-11-10 (RM-413).
      Verify: tests/backlog/test_rm_found_2026_11_10_lookup.py::test_documents_is_refused_in_a_facet
      Backlog-ID: RM-530
- [ ] **`$documents` of a non-array answers 2.** mongod 8.2.11 refuses
      `$documents: 5` at run time with 5858203 "Executor error during
      aggregate command on namespace: <db>.$cmd.aggregate :: caused by ::
      an array is expected" (the outer collection's namespace inside a
      `$lookup`). This server answers 2 "$documents argument must evaluate
      to an array". Found 2026-11-10 (RM-413).
      Verify: tests/backlog/test_rm_found_2026_11_10_lookup.py::test_documents_of_a_non_array_is_an_executor_error
      Backlog-ID: RM-531
- [ ] **A `$match` behind a `$replaceRoot` stays behind it over a
      collection.** mongod 8.2.11 moves a `$match` behind
      `{$replaceRoot: {newRoot: "$x"}}` (or `$replaceWith: "$x"`) ahead of
      it, renamed onto `x` in an `$or` with `{x: {$type: [4]}}` and
      `{x: {$not: {$type: [3]}}}` (which let through what the
      `$replaceRoot` then refuses), and the collection's query takes it; it
      also moves a `$skip` ahead of a `$project` / `$addFields` /
      `$replaceRoot`. This server leaves both where they were written and
      reads the whole collection. The rewrite runs for a collectionless
      pipeline since RM-414 (`hoist::hoist_after_source`); over a collection
      it would change the plan, so it is not applied there. Found
      2026-11-10 (RM-414).
      Verify: tests/backlog/test_rm_found_2026_11_10_explain_rewrites.py::test_a_match_moves_ahead_of_a_replace_root_into_the_query
      Backlog-ID: RM-536
- [ ] **A pipeline `$match` lists its clauses sorted.** mongod 8.2.11 lists
      a `$match` stage's clauses in the order they were written
      (`{d: {$gt: 0}, c: 1}` as `$and: [d, c]`; an `$expr`'s
      `$_internalExprEq` ahead of the `$expr`; a `$match` moved ahead of a
      `$replaceRoot` keeps its own order inside the `$or`); only a query's
      `parsedQuery` is sorted. This server lists each with
      `parsed::parsed_query`, which sorts (`canonical_match` and `tidy` sort
      too). Found 2026-11-10 (RM-414).
      (This was RM-444: `explain` lists a `$match` after the cursor with its
      clauses sorted.) mongod 8.2.11 lists a `$match` stage it does not move
      into the query (behind a `$group` or a `$limit`) with its clauses in the
      order they were written, and an `$expr`'s `$_internalExprEq` ahead of
      the `$expr`: `[{$limit: 3}, {$match: {x: {$exists: true}, y: 1}}]` lists
      `$and: [{x: {$exists: true}}, {y: {$eq: 1}}]`. This server lists the
      clauses in the order mongod gives only the query's `parsedQuery`
      (`MatchExpression::sortTree`): `y` first, and the `$expr` before its
      `$_internalExprEq`. The documents are the same. Found 2026-11-09 working
      RM-311.
      Verify: tests/backlog/test_rm_found_2026_11_10_explain_rewrites.py::test_a_pipeline_match_lists_its_clauses_as_written, tests/backlog/test_rm_found_2026_11_09.py::test_explain_lists_a_match_after_the_cursor_in_written_order
      Backlog-ID: RM-537
- [ ] **Explain lists stages mongod's optimizer rewrites as written.**
      mongod 8.2.11 drops a `$replaceWith: "$$ROOT"`, folds a `$limit` into
      the `$sort` before it (`{$sort: {sortKey, limit: 1}}`, no `$limit`
      stage), moves a `$match` on a field an `$addFields` / `$set` renames
      (`{b: "$a"}`) ahead of it onto `a`, and lists `$sampleRate: 0.5` as
      `$expr: {$lt: [{$rand: {}}, {$const: 0.5}]}`. This server lists
      each stage as written. Found 2026-11-10 (RM-414).
      (This was RM-427: `explain` lists a `$sort` and the `$limit` after it as
      two stages where the cursor does not absorb them.) mongod 8.2.11 lists
      one `$sort: {sortKey, limit}` after a `$replaceRoot`, in a `$facet`
      sub-pipeline, and after a `$rankFusion` (its own `$sort` takes the
      `limit`), and the stages before it then count every document, not the
      `limit` the `$limit` pulled. Found 2026-11-08 working RM-200.
      Verify: tests/backlog/test_rm_found_2026_11_10_explain_rewrites.py::test_the_pipeline_rewrites_mongod_lists, tests/backlog/test_rm_found_2026_11_08_explain_listing.py::test_sort_and_limit_after_the_cursor_are_one_stage
      Backlog-ID: RM-538
- [ ] **Explain leaves a `$match` after a `$redact` where it was
      written.** mongod 8.2.11 copies a `$match` that follows a `$redact`
      ahead of it and into the query (`[{$redact: "$$KEEP"}, {$match: {z:
      0}}]` explains as a `$cursor` reading `{z: {$eq: 0}}`, then the
      `$match`, the `$redact` and the `$match` again); this server lists
      the two stages as written over a whole-collection `$cursor`, and reads
      the collection so. Found 2026-11-12 (RM-360).
      Verify: tests/backlog/test_rm_found_2026_11_12_rm360.py::test_a_match_after_a_redact_is_copied_ahead_of_it
      Backlog-ID: RM-580
- [ ] **Explain's stages lack some of mongod's statistics.** At
      `executionStats` mongod 8.2.11 gives the stages inside a `$facet` and
      a `$unionWith` their own `nReturned` (and lists the `$unionWith`'s
      `$documents` as the `$queue` / `$project` / `$unwind` /
      `$replaceRoot` it runs), a `$lookup` its `totalDocsExamined`,
      `totalKeysExamined`, `collectionScans` and `indexesUsed`, the `$group`
      a `$count` makes its `maxAccumulatorMemoryUsageBytes` and
      `totalOutputDataSizeBytes`, and a `$sort` after `$documents` a
      `totalDataSizeSortedBytesEstimate` of 245 for one document where this
      server says 117. Found 2026-11-10 (RM-414).
      Verify: tests/backlog/test_rm_found_2026_11_10_explain_rewrites.py::test_each_stage_reports_what_mongod_does
      Backlog-ID: RM-539
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
- [ ] **`dbCheck` refuses `extraIndexKeysCheck`, and `applyOps` some entries.**
      mongod 8.2.11 (one-member replica set) runs `dbCheck` with
      `validateMode: "extraIndexKeysCheck"` (115 here): it walks the named
      index in batches of `maxDocsPerBatch` keys, a batch ending only where
      the key changes, and writes a `batch` oplog entry per batch whose `md5`
      is the MD5 of the batch's keys as KeyString V1 bytes, each with its
      `kEnd` discriminator (4) and without its RecordId (checked by hand for
      int keys, 2026-11-10), with `batchStart` / `batchEnd` /
      `lastKeyChecked` as keys; an index that does not exist gets a
      `dbCheckBatch` warning in `local.system.healthlog`. Building it needs a
      KeyString encoder for every BSON type (key_string.cpp's doubles and
      Decimal128 included) and each index kind's keys (hashed, 2dsphere,
      wildcard, collated). mongod also takes `applyOps` `importCollection`,
      `createDatabaseMetadata` and `modifyCollectionShardingIndexCatalog`
      entries (115 here). `dbCheck` also runs before it answers (mongod runs
      it in the background), walks a database's collections in name order
      (mongod's is its catalog's internal order) and does not apply
      `maxBytesPerBatch` / `maxBatchTimeMillis` / `maxCountPerSecond`.
      Carried over from RM-219 when its last commands landed, 2026-11-08.
      Verify: tests/backlog/test_rm_219_diag_query_settings.py::test_db_check_extra_index_keys
      Backlog-ID: RM-416
- [ ] **`applyOps` applies a `u` replacement of a missing document.** A `u`
      entry whose `o` is a whole document with `o2`'s `_id`, for a document
      that does not exist and without `b: true`, is inserted here; mongod
      8.2.11 does not apply it (`false` in `results`, the command 8), as this
      server's own module comment says. A `u` upsert of a document past
      16 MB is 10334 "object to insert too large" here, 17420 "Document to
      upsert is larger than 16777216" on mongod, and an `i` entry replacing
      an existing document with one past 16 MB is 10334 here, 17419 on
      mongod. Found 2026-11-10 while working RM-416.
      Verify: tests/backlog/test_rm_219_diag_query_settings.py::test_apply_ops_update_entries
      Backlog-ID: RM-540
- [ ] **FTDC is a sample in memory only.** Since 2026-11-10 the sample
      carries `transportLayerStats` and `diagnosticDataCollectionEnabled` /
      `diagnosticDataCollectionPeriodMillis` switch it and set its period.
      Open: nothing is written to `diagnostic.data` (mongod 8.2.11 keeps
      `metrics.<time>-00000` files and `metrics.interim` there, and reports
      the directory in `diagnosticDataCollectionDirectoryPath`), so the
      other `diagnosticData*` parameters, which tune that writer, are not
      registered (72 here). In `transportLayerStats` mongod also lists its
      Unix domain socket (`/tmp/mongodb-<port>.sock`), which this server
      does not listen on, and on Linux measures each TCP listener's backlog
      depth and counts the connections it discards for a client that left;
      macOS mongod reports 0 for both, as this server does everywhere (the
      Linux measurement is not in the macOS binary; not probed on Linux).
      Verify: tests/backlog/test_rm_219_diag_query_settings.py::test_samples_are_written_to_diagnostic_data
      Backlog-ID: RM-417
- [ ] **`setParameter` takes a double just past an int's range.**
      mongod 8.2.11 refuses `2147483647.9` and `-2147483648.5` for an int
      parameter (`transactionLifetimeLimitSeconds`,
      `diagnosticDataCollectionPeriodMillis`) with 2 "Failed validating
      <name>: Out of bounds coercing to integral value", and takes
      `2147483647.0`: it bounds the double before truncating it. This
      server's `params::integral` truncates first, so it stores 2147483647
      for the first. Found 2026-11-10 (RM-417).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm417.py::test_an_int_parameter_bounds_the_double_before_truncating
      Backlog-ID: RM-541
- [ ] **`$querySettings` gives no `debugQueryShape`, and query settings
      miss two of mongod's mechanisms.** Since 2026-11-08
      `setQuerySettings` / `removeQuerySettings` / `$querySettings` keep
      mongod's settings, a read of their shape is refused (411) or planned
      over its `indexHints` (RM-219), and since 2026-11-10 its
      `queryFramework` chooses the engine (RM-418). Open:
      `showDebugQueryShape` gives no `debugQueryShape` (mongod's shape with
      each literal a debug type string, `{x: {$eq: "?number"}}`, which
      RM-215's `query_shape::stats_shape` now serializes for `$queryStats`);
      a query whose shape `query_shape.rs` does not compute
      is refused 115; and a write made to `config.clusterParameters`
      directly is read only at the next start (mongod's op observer
      reloads it).
      Verify: tests/backlog/test_rm_219_diag_query_settings.py::test_show_debug_query_shape
      Backlog-ID: RM-418
- [ ] **Under `queryFramework: "sbe"` an `aggregate` and a `distinct` are
      pushed into SBE as without the setting.** mongod 8.2.11's
      `trySbeEngine` runs a `distinct`, and an `aggregate` whose stages it
      does not push past the query (`[{$match: ...}]`, a `$project` or
      `$addFields` after it), in SBE (`explainVersion` "2", the
      `queryHash` of the SBE-run query); this server keeps
      `trySbeRestricted`'s rule there, which pushes only a `$group` or
      `$lookup`, and runs them classic. Found 2026-11-10 (RM-418).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm418.py::test_sbe_framework_pushes_an_aggregate_and_a_distinct
      Backlog-ID: RM-542
- [ ] **A `find` under `queryFramework: "sbe"` that is sorted, projected,
      index-served or filtered beyond comparisons is explained in the
      classic shape.** mongod 8.2.11 runs it in SBE and explains it so
      (`nlj` / `ixseek` / `seek` over an index, `sort` and `project`
      stages, `$in`'s `isMember`), as RM-071 describes for a pushed-down
      `$group`'s front; this server runs it in SBE (its expressions answer
      as SBE's) but builds the slot program only for a forward collection
      scan, its comparison filter, skip and limit (`explain/sbe/find.rs`).
      Found 2026-11-10 (RM-418).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm418.py::test_sbe_find_shapes_are_explained_as_sbe
      Backlog-ID: RM-543
- [ ] **A query's `planCacheKey` hashes its `indexHints` setting as unset.**
      mongod 8.2.11 folds the read's query settings into the key
      (`query_settings::hash`): the framework and `reject` are hashed here
      since 2026-11-10 (RM-418), but a setting's `indexHints` is hashed as
      an empty `boost::optional`, where mongod hashes the hints: `find({x:
      1})` with `allowedIndexes: [{$natural: 1}]` keys as without settings
      here and differently on mongod. Found 2026-11-10 (RM-418).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm418.py::test_plan_cache_key_hashes_index_hints
      Backlog-ID: RM-544
- [ ] **A `find`'s `explain` gives no `optimizationTimeMillis`.** mongod
      8.2.11's `queryPlanner` carries it (0 on a small collection) after
      `planCacheKey`, for a `find` with or without query settings; this
      server's lists it only for a change stream. The live comparisons
      drop it as a timing. Found 2026-11-10 (RM-418).
      Verify: tests/backlog/test_rm_found_2026_11_10_rm418.py::test_find_explain_gives_optimization_time
      Backlog-ID: RM-545
- [ ] **A `$near` key inside a VALUE is refused as a misplaced `$near`.**
      `{a: {$eq: {$near: 1}}}`, `{a: {$ne: {$nearSphere: 1}}}`, `{a: {x:
      {$near: 1}}}` and `{a: {$all: [{x: {$near: 1}}]}}` are equalities on
      mongod 8.2.11, which answers them; this server refuses each with 2 "geo
      $near must be top-level expr", since `near.rs` `holds_near` looks for
      the key anywhere in the filter, values included. The same check
      answers `{a: {$all: [{$near: 1}]}}` with that message where mongod
      gives `$all`'s "no $ expressions in $all". Found 2026-11-10 while
      working RM-421.
      Verify: tests/backlog/test_rm_found_2026_11_10_rm421.py::test_a_near_key_inside_a_value_is_an_equality
      Backlog-ID: RM-546
- [ ] **A hinted index that no predicate bounds sorts its FETCH filter by
      path.** mongod 8.2.1 keeps the canonical order there: `{s: {$gte:
      "a", $lte: "z"}, t: 1}` hinted to `u_1` fetches with `[t $eq, s $lte,
      s $gte]`, this server with `[s $lte, s $gte, t $eq]`. Over an index
      that does bound a predicate mongod sorts the rest by path, as this
      server does. Found 2026-11-08 while working RM-331.
      Backlog-ID: RM-422
      Verify: tests/backlog/test_rm_found_2026_11_08_rm331.py::test_a_hint_that_bounds_nothing_keeps_the_canonical_filter_order
- [ ] **An aggregate `$sort` on a `$meta` key is refused.** mongod 8.2.1
      sorts `{$sort: {s: {$meta: "randVal"}}}` (and a `searchScore` key,
      which has no value without `$search` and so ties); this server
      refuses any `$meta` key in the stage with 2 "aggregation pipeline
      uses a stage or operator not supported by the Rust server". A `find`
      sort by `{$meta: "randVal"}` answers on both. Found 2026-11-08 while
      working RM-332.
      Verify: tests/backlog/test_rm_found_2026_11_08_rm332.py::test_a_sort_on_a_meta_key_runs
      Backlog-ID: RM-423
- [ ] **A retryable write retried after a crash under `--data-nonlogged` is
      applied again.** A restarted server reads a session's
      `config.transactions` row, its oplog entries and its
      `config.image_collection` image back (since 2026-11-08), but that mode
      writes the row and the image without an oplog entry and recovers its
      data tables by replaying the oplog, so a SIGKILL loses both: a retried
      insert fails on a duplicate key, a retried `findAndModify` changes the
      document a second time. mongod 8.2.11 rebuilds the row and the image
      from the oplog in recovery (its `SessionUpdateTracker`; the image from
      the document the replayed write changed) and answers both retries as
      after a clean restart. The fix is in `recover_from_oplog`
      (`secantus-storage`), which the PG server links too.
      Backlog-ID: RM-350
      Verify: tests/backlog/test_rm_350_retry_after_restart.py::test_a_retry_after_a_crash_under_data_nonlogged_applies_nothing_twice

### 7.085 The shared Rust storage, audited -- 2026-10-20

A read-only audit of `secantus-storage` / `secantus-wt`, the layer both
Rust servers use. The two items with gap tests were reproduced against
mongod 8.2.11 with a `secantusd-rs` built from tree `af2629ec`; the rest
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
- [ ] **`getCmdLineOpts` does not report the command line.** mongod 8.2.1
      answers with `argv` as the process was started and `parsed` holding
      each option given, under its configuration-file path (`--port 27031
      --dbpath D` is `{net: {port: 27031}, storage: {dbPath: D}}`); this
      server answers `argv: ["secantus"]` (and `--auth`) and `parsed: {net:
      {}, storage: {}}` whatever it was started with. The same `parsed` is
      the `cmdLine` of each `local.startup_log` record, which shares the
      gap. Found while fixing RM-319 (2026-11-08).
      Backlog-ID: RM-425
      Verify: tests/backlog/test_rm_cmd_line_opts.py::test_get_cmd_line_opts_reports_the_command_line

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
- [ ] **`serverStatus.locks`: locks taken before a command parses, the
      transaction commands, and the commands not measured.** The lock
      manager (`lock_manager.rs`, RM-034) takes, outside a transaction, the
      rounds mongod 8.2.11 takes (`statement_rounds`, measured per command):
      reads, the write commands, `findAndModify`, `create` / `drop` /
      `dropDatabase`, index builds, `dropIndexes`, `$out`, `dbStats`, and
      `$merge`, which takes a write round per document as it writes
      (`write_rounds`, two more when it creates its target, and with
      `whenMatched: "fail"` mongod's retries after a duplicate). Since
      2026-10-24 also the commands that read no data (none), `rolesInfo` /
      `usersInfo`, `dataSize` / `filemd5` / the plan-cache commands,
      `explain`, `dbHash` and `listDatabases` (one more global IS per
      database whose size it reads), and the global IS of a slow
      operation's log line (RM-299). All count as mongod's (the regression
      tests in `tests/backlog/test_rm_185_locks.py`). An index build's count
      of the global IS is not fixed on mongod: its build runs on a thread of
      its own and the command takes the IS each time it looks at it, 1 to 4
      times in one shape measured. This server takes mongod's usual count (2,
      and 4 with every operation logged as slow: macOS 8.2.11 gave exactly
      that in 20 of 20 runs; the Linux build sometimes polls once less), and
      the tests accept any count mongod gave in its five runs for that one
      lock (Joe, 2026-10-27). mongod takes a command's locks inside it,
      after it parses, so a malformed one takes none: since 2026-11-08
      `explain`, `dataSize`, the plan cache commands and
      `validateDBMetadata` parse before their locks here too
      (`parse_before_locks`), and `validateDBMetadata` takes a round on each
      database it reads and on each of its collections. Still different:
      the other commands find a malformed field in their handler, after
      their locks -- `find` with a wrongly typed `filter` or an unknown
      field, `aggregate` without `pipeline` or on `1` with no stage to read
      from, `getMore` of a name, `listCollections` and `listDatabases` with
      a wrongly typed `filter`, `insert`, `update`, `createIndexes` and
      `dropIndexes` without their required field, and `dbStats` with a
      wrongly typed `scale` take their chain or rounds where mongod takes
      none; the transaction commands, which need a replica set, were measured on a
      one-member mongod 8.2.11 set (2026-10-17) cannot be pinned the same
      way: its replication threads take the global IS about twice in any
      window, and its writes take rounds for the oplog and the session table
      too (a plain insert three where a standalone takes one; a one-insert
      transaction with its commit 3 collection IX, 2 global IX and 8 on the
      state transition lock), machinery this server does not run; and the
      commands neither measured on 2026-10-24 (a sweep of the 115 both
      servers list, in valid forms) nor named above still take the generic
      chain -- those that write, wait, change the server or need a replica
      set were left out of the sweep. mongod's internal resources
      (`Metadata`, `oplog`, `Mutex`, `MultiDocumentTransactionsBarrier`) are
      locked by its background threads only and are absent here.
      **Time-based plan yields** (measured 2026-10-21): mongod's executor
      yields after `internalQueryExecYieldPeriodMS` (10 ms), and each re-take
      of a lock counts. `$out` reads 7 global IS on Linux x86-64 8.2.12 (CI,
      the exemplar build: Joe, 2026-10-21), 8 on macOS arm64 idle and 9 on a
      loaded Mac; with the period off, macOS reads 7 too. So this server
      takes Linux's 7, and the regression tests' reference mongod runs with
      the period off, which gives Linux's counts on any machine and stops
      them moving with load. The counts that still moved under load were
      mongod's slow-operation log (RM-274, closed 2026-10-24): the tests set
      `slowms` out of reach on both servers.
      Backlog-ID: RM-185
      Verify: tests/backlog/test_rm_185_locks.py::test_lock_counts_of_other_malformed_commands
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
- [ ] **A rooted `$or` in front of stages SBE runs caches no branch.**
      mongod 8.2.11 caches each branch as a classic entry (`worksType:
      "works"`) under a key that is neither the classic branch's nor the `s`
      key of the same clause with the pipeline's projection (`CA4248A6`
      for the test's clause, where those two are `0B947F35` and
      `6F377EC6`): its encoding is still to be found. Until then such a
      branch is not cached here, so that it cannot share a classic
      query's entry.
      Backlog-ID: RM-429
      Verify: tests/backlog/test_plan_cache_or_branches.py::test_an_or_branch_before_sbe_stages_is_cached
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
      Backlog-ID: RM-442
      Verify: tests/backlog/test_rm_340_342_plan_cache_gaps.py::test_an_entry_reports_its_estimated_size_on_linux
- [ ] **A compound index scan whose second field has several intervals
      examines a key more than mongod's for each value of the first.**
      Over `a_1_c_1` (2,000 documents, `a: i % 100`, `c: i % 7`), mongod
      8.2.11 scans `{a: {$in: [3, 60]}, c: {$in: [1, 5]}}` examining 16 keys
      with 5 seeks and `{a: 3, c: {$in: [1, 5]}}` 7 with 2; this server's
      scan model (`exec::Scan`, which the trial steps) counts 18 with 7 and
      8 with 3, in `explain` and in a plan cache entry's
      `creationExecStats`, where the extra works also move the scores.
      Found 2026-11-08 (RM-342).
      Verify: tests/backlog/test_rm_found_2026_11_08_rm342.py::test_a_scan_of_two_in_lists_examines_mongods_keys
      Backlog-ID: RM-433
- [ ] **`explain`'s winning plan has no work accounting.** mongod 8.2.11's
      `executionStats` gives each stage its `works`, `advanced`, `needTime`,
      `needYield`, `isEOF` and timings, a FETCH its `alreadyHasObj`, an
      IXSCAN its `seeks`, `dupsTested` and `dupsDropped`. This server's
      `executionStages` give a stage only its `nReturned` and examined
      counts (`allPlansExecution` and `$planCacheStats` do carry the
      accounting the trial counted). `tools/probes/explain_commands.py`
      leaves these fields out of its comparison. Found 2026-11-08 (RM-342).
      (This was RM-446: A DISTINCT_SCAN plan's `executionStats` gives no work
      counters.) mongod 8.2.11's `executionStages` for a DISTINCT_SCAN plan --
      of `distinct`, and of a `$group` it answers from one -- gives every
      stage's `works`, `advanced`, `needTime`, `needYield`, `saveState`,
      `restoreState` and `isEOF` (`distinct("x", {x: {$gt: 0}})` over `x_1`:
      `works: 4`). This server's gives `nReturned` and the scan's own fields,
      none of them. The trial's counters, in `allPlansExecution`, are given.
      Found 2026-11-08 (RM-310).
      Verify: tests/backlog/test_rm_found_2026_11_08_rm342.py::test_explain_gives_each_stage_its_work_accounting, tests/backlog/test_rm_found_2026_11_08_rm310.py::test_a_distinct_scan_run_reports_its_work_counters
      Backlog-ID: RM-434
- [ ] **A `$sort` after a `$group` mongod pushes into SBE counts its
      documents smaller than mongod's.** `[{$group: {_id: "$a"}}, {$sort:
      {_id: 1}}]` over ten documents: mongod 8.2.11's `$sort` reports
      `totalDataSizeSortedBytesEstimate` 735 for the three groups the SBE
      plan hands it, this server's 351, its estimate of the same three
      documents as the classic engine builds them. Found 2026-11-08
      (RM-071).
      Verify: tests/backlog/test_rm_found_2026_11_08_rm071.py::test_a_sort_after_a_pushed_down_group_sizes_what_sbe_hands_it
      Backlog-ID: RM-435
- [ ] **`estimatedSizeBytes` is left out of entries whose index tree this
      server does not build.** It sizes an entry's `PlanCacheIndexTree`
      from the canonical query and the clauses the winning plan's one index
      was assigned (`plan::index_tags`, `entry_size::tagged_tree`). mongod
      8.2.11 sizes every entry; this server leaves the field out where the
      tree's nodes or tags are not modelled: a predicate inside
      `$elemMatch` (its `ELEM_MATCH_*` node and the tags under it), an
      `$or` or `$nor` that is not the root (an `OR` may take an
      `OrPushdownTag` of its own), a `$not` over several predicates,
      `$jsonSchema`, a winner that reads two indexes (`AND_SORTED` or an
      `OR`, where `tagForSort` picks the lowest-numbered index), and an
      `OR` stage among the candidates (`OrStats`). Found 2026-11-08 (RM-341).
      Verify: tests/backlog/test_rm_found_2026_11_08_rm341.py::test_entries_this_does_not_size_report_their_size
      Backlog-ID: RM-436
- [ ] **A SORT_MERGE candidate's trial counts are not mongod's.** For
      `{a: {$in: [1, 2]}, b: {$lt: 90}}` sorted by `c` over `a_1_c_1` and
      `b_1`, mongod 8.2.11's winning SORT_MERGE reports 82 `works` and its
      `dupsTested` and `dupsDropped` in `creationExecStats`; this server's
      reports 83 works and neither field. Found 2026-11-08 (RM-341).
      Verify: tests/backlog/test_rm_found_2026_11_08_rm341.py::test_a_sort_merge_candidate_counts_as_mongods
      Backlog-ID: RM-437
- [ ] **A scan over several intervals of one field seeks and returns
      differently in the trial.** mongod 8.2.11's losing candidate for
      `{a: {$lt: 5}, b: {$nin: [3, 4]}}` reads `b_1` over three intervals,
      examining 101 keys in 3 seeks and returning 99; this server's scan
      model seeks twice and returns 100, in `creationExecStats` (and so in
      its score). Perhaps RM-433's cause on one field. Found 2026-11-08
      (RM-341).
      Verify: tests/backlog/test_rm_found_2026_11_08_rm341.py::test_a_nin_candidate_reads_mongods_keys
      Backlog-ID: RM-438
- [ ] **A candidate's FETCH filter puts an inexact `$exists` after the
      other predicates.** mongod 8.2.11's losing candidate for `{a: {$lt:
      5}, b: {$exists: true}}` over `b_1` rechecks `{$and: [{b: {$exists:
      true}}, {a: {$lt: 5}}]}`; this server's puts `a` first. Found
      2026-11-08 (RM-341).
      Verify: tests/backlog/test_rm_found_2026_11_08_rm341.py::test_an_exists_candidates_filter_is_in_mongods_order
      Backlog-ID: RM-439
- [ ] **A `$type` index filter returns too few keys in the trial.** mongod
      8.2.11's losing candidate for `{a: {$lt: 5}, b: {$type: "int"}}` over
      `b_1` rechecks `$type` against each key and hands all 101 it examines
      (every `b` an int) to its FETCH; this server's, with the same bounds
      and filter, hands over 6 -- the documents the FETCH keeps -- so its
      FETCH's `docsExamined` is 6, not 101. Found 2026-11-08 (RM-341).
      Verify: tests/backlog/test_rm_found_2026_11_08_rm341.py::test_a_type_candidate_reads_mongods_keys
      Backlog-ID: RM-440
- [ ] **A multikey index does not cover a field that holds no array.**
      mongod 8.2.11 answers `find({a: {$lt: 5}}, {a: 1, _id: 0})` from
      `a_1_c_1` with a PROJECTION_COVERED over the IXSCAN when only `c`
      holds arrays (`multiKeyPaths: {a: [], c: ["c"]}`); this server
      fetches every document (PROJECTION_SIMPLE over a FETCH), which also
      changes the trial and the cached plan. Found 2026-11-08 (RM-341).
      Verify: tests/backlog/test_rm_found_2026_11_08_rm341.py::test_a_multikey_index_covers_a_field_that_holds_no_array
      Backlog-ID: RM-441

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
      Backlog-ID: RM-170
      Verify: tests/backlog/test_rm_found_2026_10_16.py::test_include_query_stats_metrics_adds_a_metrics_document

- [ ] **RUST MongoDB server: `insert`, `update` and `delete` take fields
      mongod refuses as unknown.** `originalQuery` and `originalCollation`
      on any of the three, and a command-level `sampleId` on `update`:
      mongod 8.2.11 answers 40415 (`BSON field 'insert.originalQuery' is an
      unknown field.`) and writes nothing; this server takes them, ignores
      them and runs the write. Its field lists (`write_command_problem`,
      `update_command_problem` in `crud.rs`) name them. Found 2026-11-09
      while working RM-087.
      Backlog-ID: RM-460
      Verify: tests/backlog/test_rm_found_2026_11_09_rm087.py::test_a_write_refuses_fields_mongod_does_not_know

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
      a number, string or boolean on a path of named fields and the `$and`s
      of those (`explain/sbe/filter.rs`), with a skip and a limit where the
      group reads no field (RM-072); whose `_id` is a field path, `$$ROOT`,
      a scalar constant or an object of those, whose arguments are field
      paths or `$$ROOT` (numeric constants for `$sum`), with `$sum`,
      `$count`, `$avg`, `$first`, `$last`, `$min`, `$max`, `$push` and
      `$addToSet`, and at most one distinct dotted path; and the `limit 0`
      / `coscan` plan over a collection that does not exist. Still the
      classic `$cursor` / `stages` shape: any other filter (`{a: null}`'s
      `typeMatch`, `$in`'s `isMember`, `$ne`'s `!`, `$or`, ...), a sorted,
      projected or index-served front (`COUNT_SCAN`, `IXSCAN` /
      `FETCH`; for `count` on a view, RM-072), a skip or limit with a group that reads a field (mongod
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
- [ ] **`count` on a view whose filter an index serves** is explained by
      mongod 8.2.11 as an SBE plan (`explainVersion: "2"`, a `queryPlan` of
      `GROUP` over a `COUNT_SCAN` where the count reads no field, over
      `FETCH` / `IXSCAN` where a filter is left); this server reports the
      classic cursor with `FETCH` / `IXSCAN` and the `$count`'s `$group`.
      Over a collection scan it matches mongod since 2026-11-08
      (`tests/mongod_live/test_sbe_filtered_front_explain.py`). Needs the
      SBE index scan stages, and the `COUNT_SCAN` a `$count` that reads no
      field gets (also `aggregate: stages after the cursor` 7 and
      `executionStats: aggregate` 6, under RM-071). `explain_commands.py`:
      `a view` 4 and 5.
      **Decided 2026-10-16 (Joe):** as RM-071, transcribe and execute.
      Backlog-ID: RM-072
      Verify: tests/backlog/test_rm_057_084.py::test_explain_of_count_on_a_view_over_an_index
- [ ] **A DISTINCT_SCAN bounded on a trailing field examines fewer keys
      than mongod's.** mongod 8.2.11's DISTINCT_SCAN of `x_1_y_1` for
      `distinct("x", {x: {$gt: 0}, y: {$gt: 3}})` over `x = i % 4, y = i`
      (i 1 to 24) returns three keys and examines six, in eight works, in
      its `executionStats` and in a `$group`'s trial. This server's
      examines three, in four works. The plans ranked are the same. Not
      RM-377's key past the bounds (fixed 2026-11-09, and none is past
      these): by the source, `explain`'s DISTINCT_SCAN (`explain/exec.rs`)
      counts one key per value it returns plus a key past the bounds,
      never a key a seek lands on that the bounds on `y` reject. Found
      2026-11-08 (RM-310).
      Backlog-ID: RM-447
      Verify: tests/backlog/test_rm_found_2026_11_08_rm310.py::test_a_distinct_scan_bounded_on_a_trailing_field_examines_mongods_keys
- [ ] **`explain` of a `$unionWith` lists its sub-pipeline as written.**
      mongod 8.2.11 explains the sub-pipeline as it does the outer one: a
      `$cursor` of its own (its `queryPlanner`, and `executionStats` above
      that verbosity) holding the stages it absorbs, then the rest as
      listed. `[{$unionWith: {coll: "s", pipeline: [{$match: {x: 1}},
      {$project: {x: 1}}]}}]` is `$cursor` (`{x: {$eq: 1}}`,
      `PROJECTION_SIMPLE`) on mongod and `$match`, `$project` here. A
      `$rankFusion` or `$scoreFusion` input pipeline's `$unionWith` lists
      its stages desugared (RM-200) but without that `$cursor`. Found
      2026-11-08 working RM-200.
      Backlog-ID: RM-426
      Verify: tests/backlog/test_rm_found_2026_11_08_explain_listing.py::test_union_with_sub_pipeline_is_explained
- [ ] **A `$facet` whose sub-pipeline keeps a sort key plans no
      `SORT_KEY_GENERATOR`.** mongod 8.2.11's `$cursor` under a `$facet`
      holding a `$sort` with `outputSortKeyMetadata` (a `$setWindowFields`
      `$rank`, a `$rankFusion` input) is `SORT_KEY_GENERATOR` over the
      `COLLSCAN`; this server's is the `COLLSCAN`. Found 2026-11-08 working
      RM-200.
      Backlog-ID: RM-428
      Verify: tests/backlog/test_rm_found_2026_11_08_explain_listing.py::test_facet_that_reads_sort_keys_has_a_key_generator
- [ ] **`explain` of a write misses a required field mongod's parser
      finds first.** mongod parses the explained command in full before it
      explains it, required fields before the namespace's type:
      `{explain: {insert: "x"}}` is 40414 "BSON field 'insert.documents' is
      missing but a required field", where this server answers 20 "Cannot
      explain cmd: insert"; `{explain: {update: 1}}` and `{explain: {delete:
      1}}` name the missing `updates` / `deletes` (40414) where this server
      refuses the namespace (73). Measured 8.2.11; found 2026-11-08 working
      RM-185.
      Backlog-ID: RM-432
      Verify: tests/backlog/test_rm_431_432.py::test_explain_parses_the_write_it_explains

### 7.074 Change streams, command by command -- 2026-10-11

`tools/probes/change_stream_commands.py`, mongod 8.2.11 as a replica-set
member: 115 scenarios over a raw connection (what `$changeStream` accepts,
every event's shape, `getMore`, the invalidating operations, resuming by
`resumeAfter` / `startAfter` / `startAtOperationTime`, the three scopes,
pre-images, transactions), each on a database of its own. The Rust server
differs on none (re-measured 2026-11-09 after RM-454; 1 earlier that day, 2 on 2026-10-16, 5 before). The harness salts database names per run, because
mongod keeps the oplog of earlier runs and a stream started in the past
would see them. What is left:

- [ ] **RUST MongoDB server: a `$match` after a `$changeStream` is not
      rewritten into the oplog filter.** mongod rewrites a `$match` on the
      event's fields (`operationType`, `fullDocument`,
      `fullDocumentBeforeChange`, `documentKey`, `ns`, ...) into predicates
      on the oplog entry and adds them to the read's filter, so the
      `explain`'s `parsedQuery` and its hashes change (`operationType:
      "insert"` adds `op: {$eq: "i"}` beside the writes: `queryHash`
      2CFDBDA8, not 90A52575; mongod 8.2.11, 2026-11-09). This server lists
      the `$match` as mongod does after the read and leaves the filter as it
      is without one. Needs mongod's per-field rewrites transcribed (found
      working RM-076).
      Verify: tests/backlog/test_rm_found_2026_11_09_rm076.py::test_a_match_on_an_event_field_joins_the_oplog_filter
      Backlog-ID: RM-452
- [ ] **RUST MongoDB server: `executionStats` of a change stream counts
      nothing.** mongod 8.2.11 runs the oplog read under `executionStats`:
      a stream started before three inserts counts the three entries at
      every stage, and the `COLLSCAN` its `docsExamined` (2026-11-09). This
      server reports what a new stream at the end of the oplog finds -- no
      entry, at every stage -- which is mongod's answer only for that
      stream. `docsExamined` also counts mongod's own oplog entries (its
      periodic no-ops), which this server's oplog does not keep (found
      working RM-076).
      Verify: tests/backlog/test_rm_found_2026_11_09_rm076.py::test_execution_stats_count_what_the_read_found
      Backlog-ID: RM-453

### 7.073 The cursor commands -- 2026-10-11

`tools/probes/cursor_commands.py`, mongod 8.2.11: 85 scenarios (`find`'s
cursor options, `getMore`, `killCursors`, the cursors of `aggregate` and the
listing commands, tailable cursors, `count`, `distinct`). The Rust server
differs on 5 scenarios. What is left:

- [ ] **RUST MongoDB server: a tailable `find` of a collection that is not
      capped does not print its query.** mongod 8.2.11 refuses it (2) with
      the query its planner could not plan, `error processing query:
      ns=<db>.pTree: $and\nSort: {}\nProj: {}\n tailable cursor requested
      on non capped collection`; this server's message is `ns=<db>.p
      tailable cursor ...`, with no query. Found while working RM-363,
      2026-11-10.
      Verify: tests/backlog/test_rm_found_2026_11_10_rm363.py::test_a_tailable_find_of_a_collection_that_is_not_capped_prints_its_query
      Backlog-ID: RM-498
- [ ] **RUST MongoDB server: a time-series `aggregate` that needs only
      some fields reads the collection at the command.** Behind an inclusion
      `$project` (`{$project: {v: 1}}`) mongod 8.2.11 reads the buckets one
      at a time as the cursor asks, not 32 at once: over the same 100
      buckets it returns 71 more after the delete (the rest of the first
      bucket), and this server, which streams only stages that need the
      whole measurement, 3,999. Likely its slot-based engine, which takes
      `$_internalUnpackBucket` once the fields it must unpack are known;
      not yet confirmed from `explain`. Measured 2026-11-09.
      Backlog-ID: RM-456
      Verify: tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_projection_reads_a_bucket_at_a_time
- [ ] **RUST MongoDB server: a time-series read an index over the buckets
      could answer reads the collection at the command.** A filter on the
      meta field (`{m: {$lt: 40}}`) is an index scan of the default
      `m_1_t_1` index for mongod 8.2.11, which buffers 32 buckets in that
      index's order -- series by series -- and returns 1,279 more after the
      delete; this server reads such a read (and any over a collection with
      an index of its own, or under a hint or a collation) whole: 3,199.
      The buckets are stored since 2026-11-11, but the indexes are not
      built over them: a time-series collection's index specs are kept on
      the collection, every read scans the buckets in `_id` order, and
      `collStats` reports its index sizes as 0. Streaming it means indexes
      over the buckets and a bucket scan in an index's order. Measured
      2026-11-09.
      Backlog-ID: RM-457
      Verify: tests/backlog/test_rm_found_2026_11_09_rm348.py::test_a_time_series_read_of_the_meta_index_buffers_its_buckets
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

- [ ] **RUST MongoDB server: smaller differences.** A write conflict on
      an insert has an empty cause where mongod names its index and table
      (`Caused by :: WiredTigerIdIndex::_insert: index: _id_; uri:
      table:<ident> ::`). A retryable insert counts a null among its
      `documents` against `stmtIds` and refuses it (16), where mongod drops
      the null first and inserts. The other session errors are worded for a
      session with no user (not measured with one). A `createIndexes` that
      waits on a transaction past `maxTimeMS` (the probe's "an index built
      under a transaction"): mongod 8.2.11 adds a `writeConcernError` (50)
      to its 50 in 2 runs of 2 on 2026-11-09, where this server answers a
      plain 50; 12 runs of 12 had answered a plain 50 before. (The request
      and oplog-entry dumps of the statement-id and `findAndModify` retry
      errors are mongod's since 2026-11-09.)
      Backlog-ID: RM-087
      Verify: tests/backlog/test_rm_085_112.py::test_an_insert_conflict_names_the_index_table, tests/backlog/test_rm_085_112.py::test_a_retryable_insert_does_not_count_a_null

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

- [ ] **RUST MongoDB server: `listIndexes` of `system.buckets.<name>`
      shows a TTL or partial index as the time-series collection keeps it.**
      mongod 8.2.11 keeps a TTL index on the time field as
      `{control.min.<time>: 1}` on the buckets, its partial filter on the
      meta field as one on `meta` (`{meta: {$eq: "a"}}`), and the index as
      asked in `originalSpec`; this server lists the bound pair
      `{control.min.t: 1, control.max.t: 1}` and the filter as given, with
      no `originalSpec`. The index specs are kept on the time-series
      collection and translated for the listing (`ts_bucket_ns::bucket_indexes`);
      the TTL pass already expires buckets by `control.min.<time>`.
      Measured 2026-11-11.
      Verify: tests/backlog/test_rm_found_2026_11_11_rm088.py::test_bucket_indexes_list_a_ttl_index_as_mongod_keeps_it
      Backlog-ID: RM-548
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
      Verify: tests/backlog/test_rm_found_2026_11_11_rm088.py::test_server_status_reports_the_bucket_catalog
      Backlog-ID: RM-549
- [ ] **RUST MongoDB server: a measurement holding a BSONColumn binary
      is stored** (found 2026-11-09 while building the bucket encoder).
      mongod 8.2.11 refuses a measurement whose data field is, or holds at
      any depth, a binary of subtype 7: `12506300`, `time-series insert
      failed: <ns> :: caused by :: Timeseries collections cannot store
      BSONColumn data`, and stores nothing; this server inserts it. The
      check is `BSONColumnBuilder::append`'s, so it covers the data fields
      only (the meta field is not appended to a column; not measured).
      Backlog-ID: RM-462
      Verify: tests/backlog/test_rm_462.py::test_a_measurement_cannot_hold_a_bsoncolumn
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

- [ ] **One CI run of the storage-engine (ubuntu) backlog step stalled for
  300 s and was killed.** PR #2082's run 39691311797, first attempt: the
  `tools/backlog.py verify --regressions` pytest reported no test for 301 s
  ("=== pytest session stalled ==="), three tests of
  `tests/backlog/test_rm_found_2026_10_20b.py` never ran
  (`test_snapshot_read_at_cluster_time`,
  `test_snapshot_session_keeps_its_snapshot`, `test_diagnostic_stages`), and
  the step failed. The re-run passed, `main` had passed six runs in a row,
  and the module ran in 23 s locally. Not known: which test hung. The
  stall dump prints the CONTROLLER's threads only, so the next stall should
  dump the workers' stacks too (and the servers they started) -- that is
  the change that makes this diagnosable.
  Backlog-ID: RM-254
  Verify: none -- an intermittent CI stall whose hung test is unknown
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
  their own passed, at load 18. The test depends on six connects landing within one
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
