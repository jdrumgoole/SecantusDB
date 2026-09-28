# Driver-conformance follow-ups after `enableTestCommands` (plan)

Written 2026-09-28 at `main` `c5febff3`; **re-verified against `2fabed24` the same
day**, and every number below now comes from that re-check rather than from the
first draft. Scope: what is left after #1569 / #1571 turned on driver failpoint
tests, as measured, not as remembered. Re-verify before working an item anyway --
parallel sessions move this surface, and this file has already been wrong once.

## State, re-measured at `2fabed24`

- Landed on `main`: #1569 (failpoint `appName` scoping, `enableTestCommands`,
  Rust session-end aborts, change-stream token fix), #1571
  (`maxTimeAlwaysTimeOut`), #1582 (`apiStrict`, both servers), #1585
  (transaction `errorLabels`, **Python only**), and **#1597**
  (`Interrupted` / `PreparedTransactionInProgress` codeNames, **Python only**) --
  which the first draft listed as in flight under another session's claim. It
  merged as `daa855a8`. **Step 2 is unblocked and unclaimed**: no open branch or
  PR touches the Rust transaction code (`provenance-reach`,
  `validation-report-20260928`, `pg-storage-concurrency`, `rust-pg-constraint`).
- Unmerged bot refresh `validation-report-20260928` (`60bd8c1b`): pymongo gauge
  Python 1,203 / 7 / 290 skipped (99.4%), Rust 1,195 / 15 / 290 (98.7%).
  **It has #1582 and #1585 as ancestors but NOT #1597** -- the PR was opened at
  07:17Z and #1597 landed at 08:48Z. Merging it as-is publishes numbers that
  understate the server by exactly the two tests #1597 closed.
- **The gauge-number drift is tracked in `tasks/backlog.md` §5, not here.**
  #1599 filed a backlog item for it while this plan's Step 1 described the same
  thing; the two were folded into one tracker, which also turned up the Java
  gauge turning its whole failure set over between two runs a day apart. Step 1
  is now a pointer to it.
- The remaining gap is almost entirely the Rust server trailing the Python one --
  and the shape of it is sharper than the first draft said. See Step 2.

## Steps, in the order they are now worth doing

### 1. Re-run the gauges -- tracked in `tasks/backlog.md`, not here

**Owned by the "THE tracker for stale gauge numbers" item in
`tasks/backlog.md` §5.** This step used to describe the drift itself, and #1599
filed a second, shorter description of the same thing in the backlog. Two
trackers for one problem is how one of them goes stale, so the backlog item is
now the only one: it holds which artifact is stale in which direction, what to
re-run, the expected numbers, and the unresolved Java-gauge anomaly. Work it
from there, and delete it when it closes.

In one line, so this plan still reads end to end: the one refresh that existed
(PR #1595) was closed unmerged because it predated #1597, so this needs a fresh
run rather than a merge -- and note that the published rate did not fall from
99.5% to 99.4%; it dipped to 98.7% when 134 failpoint tests stopped skipping,
and is on its way back to ~99.6%.

### 2. Port the transaction fixes to Rust -- 10 of Rust's 15, and the whole gap

**DONE — #1606 (`9f3446a2`), 2026-09-28.** Measured rather than assumed: the
Rust server is **0 divergent of 63** against mongod 8.2.11 on `codeName` and
`errorLabels` across 21 codes on `insert` / `commitTransaction` /
`abortTransaction`, and `test_transactions_unified` against the standalone
`secantusd-rs` went **10 failures -> 3** -- the three survivors being the
secondary-read cases in §7 that cannot pass on a single node.

Three things the work found that this section did not predict, all worth
carrying forward:

- **The label depends on WHICH COMMAND failed**, not only on the code. Ending
  the transaction splits the transient set, giving `RetryableWriteError` to the
  13 codes about reaching the node. No server here modelled that.
- **The first fix BROKE two passing spec tests.** mongod treats a supplied
  `errorLabels` -- including an explicit `[]` -- as authoritative, and the parser
  collapsed that with the key being absent. Only running the whole file caught
  it; every targeted test was green.
- **Both servers were missing 134 and 262 together**, because the Rust comment
  cited `commands.py` as its authority. Neither the parity suite nor an
  engine-vs-engine sweep can see that shape.

The original text follows, since its sizing is what the work was planned from.

The Rust gauge's 15 failures are 5 out-of-scope (§7's list) plus **10** in
`test_transactions_unified` -- not the 8 the first draft claimed. The Python
refresh's 7 are those same 5 plus the 2 that #1597 has since closed. So:

- **Python is at its ceiling.** Post-#1597 it should read **1,205 / 5 = 99.6%**,
  and its remaining 5 are exactly §7's leave-alone list.
- **Rust's transaction cluster is therefore the entire remaining gap**, and
  closing it puts the two servers level.

Three concrete defects, all in `crates/secantus-commands`:

| | Python | Rust |
| --- | --- | --- |
| transient-code set | `_TRANSIENT_TXN_CODES` has `267` | `is_transient_txn_code` (`lib.rs:1160`) -- **no 267** |
| failpoint code-name table | 17 probed names incl. `112, 251, 267, 11601` | `fail_code_name` (`failpoints.rs:311`) -- **missing 24, 112, 246, 251, 267, 11601**, all falling to `Location{code}` |
| failpoint short-circuit -> `errorLabels` | routed through `_finish_txn_statement` (#1585) | not routed |

- **One of the 10 is NOT covered by this port.**
  `test_commit_is_not_retried_after_MaxTimeMSExpired_error` passes on Python even
  before #1585 / #1597 and fails on Rust, so it has a separate cause. Diagnose it
  on its own rather than assuming the port closes it.
- **Related unfiled gap:** #1597's probe recorded
  `50 MaxTimeMSExpired -> ['UnknownTransactionCommitResult']`, and **neither
  server emits that label anywhere** -- zero occurrences in `src/` or `crates/`.
  Probably the same root cause as the bullet above; probe before assuming.
- **On the exemplar risk:** #1597 *was* probed -- its message documents 17 names
  read off a single-node replica-set mongod 8.2.11, and it explicitly refused the
  obvious guess (267 in, 11601 out, because mongod gives 11601 no labels at all).
  The real gap is that **neither #1585 nor #1597 added a single test file**, and
  `PreparedTransactionInProgress` has zero occurrences under `tests/`. The probe
  exists and nothing pins it. **Land the missing differential tests for both
  servers in this branch** -- it is the same code table, and the Rust port is the
  moment the unpinned probe costs something.
- **Verify:** the Rust gauge should reach 1,205 / 5, matching Python.

### 3. Decide the test-command gate -- Joe's call

- `docs/security-reports/2026-08-10.md` recommends mongod's opt-in shape.
  `configureFailPoint` is live without `--auth` (as it was before #1569; the
  flag only stopped hiding that from drivers).
- **There is no gate to flip today.** `enableTestCommands` is a hardcoded `true`
  in the `getParameter` reply on both servers (`commands.py:1828`,
  `diagnostics.rs:194`) -- an advertisement, not a switch. This step is building
  the mechanism, not changing a default.
- Proposal: on by default in the embedded test handles (`SecantusDBServer`,
  `RustServer` via Python), opt-in (default off) for the standalone daemons
  `secantusd-rs` / `secantusd-py`.
- Cost: the non-Python driver gauges launch daemons, so every gauge task must
  pass the flag. Miss one and that gauge silently loses its failpoint coverage.
- Counter-argument: SecantusDB is a test tool with a loopback default bind;
  deny-by-default adds friction for its intended users against a local DoS the
  report itself rated WARNING.

### 4. Small `mongod` probes -- one site, and runnable today

**The first draft's blocker does not apply to this machine.** mongod **8.2.11 is
on `PATH`** here (with 6.0.16 and 8.3.4 in Cellar), so no `fastdl.mongodb.org`
allowlist is needed. That note came from the sandbox that wrote the draft -- see
the environment section below.

**DONE — both items probed 2026-09-28. One needed a fix; the other needed
none, and this section had it pointing the wrong way.**

- `featureCompatibilityVersion` — **fixed in #1607 (`b9e969a1`).** mongod 8.2.11
  answers `"8.2"` (the running binary's major.minor), and the advice above to
  probe rather than assume `"8.0"` was right to give. The interesting part was
  not the string: the Rust server already advertised `buildInfo.version`
  `8.2.11` and `maxWireVersion` 27, so `"7.0"` contradicted **its own
  handshake**, not just mongod. Both servers now DERIVE it from
  `SERVER_VERSION_ARRAY`, because a literal is what survived the 6.0 -> 8.x
  retarget untouched. Nothing caught it because the only two tests that read the
  parameter asserted the KEY was present and never its value — the assertion a
  stale literal always passes.
- `createIndexes` under a timeout — **no Rust change needed; "align both" points
  at PYTHON.** mongod gives the BARE `code 50 / MaxTimeMSExpired / "operation
  exceeded time limit"` with no index-build envelope, and the Rust server already
  matches it exactly. Probed in BOTH the failpoint and real-expiry paths on
  purpose, since the failpoint fires before an index build exists and could have
  differed legitimately — it does not. Filed in `tasks/backlog.md` §5.

  That probe also produced **§6's reproducer** as a side effect: see below.

### 5. Re-run the other-language gauges on both servers -- after Step 2

- Go / Node / Java / Kotlin / Ruby / Rust-driver / PHP / C / C++ / .NET were
  not re-measured after #1569. Their failpoint tests now run too.
- **Sequence this after Step 2**, so the sweep measures a Rust server that has
  the transaction fixes rather than producing a baseline that is obsolete on
  publication -- which is precisely how PR #1595 went stale.
- Expect both pass counts and failure counts to rise. Treat new failures as bug
  reports: the pymongo rerun is how the Rust session-abort and change-stream
  token bugs were found.

### 6. Real `maxTimeMS` enforcement in Rust -- a project, not a fix

- Confirmed at `2fabed24`: Rust validates `maxTimeMS` thoroughly
  (`argtypes.rs:521`) and enforces it nowhere -- there is no equivalent of
  `src/secantus/deadline.py`. A slow Rust command with `maxTimeMS` runs to
  completion. The failpoint works; the limit does not.
- **Now DEMONSTRATED, not just described** (2026-09-28, a side effect of §4's
  second probe). Insert 100,000 documents, then `createIndexes` with
  `maxTimeMS: 1`:

      mongod 8.2.11      -> ok: 0, code 50, MaxTimeMSExpired
      rust secantusd-rs  -> ok: 1.0        (index fully built)

  Two lines to reproduce, so this no longer needs to be taken on trust. The
  budget is parsed and recognised — the failpoint path answers 50 correctly — and
  simply never checked.
- Needs a cooperative deadline polled by the storage scan, the aggregation
  pipeline and the index build, as `src/secantus/deadline.py` does.
- **Risk:** those polls sit in the flagship server's hottest loops. Benchmark
  before and after (`invoke release-benchmark`); do not assume the cost.

### 7. Leave alone (both servers)

These five are the whole of Python's remaining gauge failure list, and five of
Rust's fifteen. Nothing else is deferred.

- `test_index_text`, `test_index_hashed`: text and hashed indexes are out of
  scope per CLAUDE.md.
- `test_where`, `test_maxtime_ms_message`: `$where` needs a JavaScript engine.
- `test_to_list_csot_applied`: unexplained. Reproduce before deciding; it may be
  timing-sensitive.

## Environment problems -- SANDBOX ONLY, not this machine

These were recorded by the session that wrote the first draft, which ran in a
cloud sandbox. **Re-checked on the local Mac at `2fabed24`: none of them apply
here.** Keep them for whoever next works from a sandbox; do not let them stop a
local session.

- **Deleting a branch returned 403 while pushes worked.** Reconnect GitHub, or
  enable "Automatically delete head branches" in the repo settings.
- **Network-allowlist changes did not reach the running session.** Add
  `fastdl.mongodb.org` before starting a session that needs `mongod`. Locally,
  three Homebrew builds are already installed -- see CLAUDE.md's probe table.
- **The disk allowance is too small for one full suite run.** The suite keeps
  every test's WiredTiger store (deleting mid-run WT_PANICs), roughly 65 GB for
  a full run; the sandbox session ran 16 chunks with `rm -rf /tmp/pytest-of-root`
  between them. The local box had 633 GB free when this was re-checked. A chunked
  mode for `invoke test` would still be worth having.
