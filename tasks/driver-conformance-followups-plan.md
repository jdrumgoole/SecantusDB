# Driver-conformance follow-ups after `enableTestCommands` (plan)

Written 2026-09-28, at `main` `c5febff3`. Scope: what is left after #1569 /
#1571 turned on driver failpoint tests, as measured, not as remembered. Re-verify
every claim below before working it -- parallel sessions move this surface.

## State this plan was written against

- Landed on `main`: #1569 (failpoint `appName` scoping, `enableTestCommands`,
  Rust session-end aborts, change-stream token fix), #1571
  (`maxTimeAlwaysTimeOut`), #1582 (`apiStrict`, both servers), #1585
  (transaction `errorLabels`, **Python only**).
- In flight, claimed by another session: `fix-txn-codenames`
  (`Interrupted` / `PreparedTransactionInProgress`, **Python only**).
- Unmerged bot refresh `validation-report-20260928`: pymongo gauge Python
  1,203 / 7 / 290 skipped (99.4%), Rust 1,195 / 15 / 290 (98.7%).
- The remaining gap is mostly the Rust server trailing the Python one.

## Steps, in order

### 1. Merge the report refresh -- small, verify first

- Confirm the refresh was measured on a tree containing #1569, #1571, #1582 and
  #1585. 290 skipped is the right shape for post-flag code (the old reports
  skipped 424); check the SHA it was built from anyway.
- Pair it with one line in the release notes: the Python rate moves 99.5% ->
  99.4% while 132 more tests pass, because failpoint tests that used to skip
  now run.

### 2. Port the transaction fixes to Rust -- the biggest win (8 of Rust's 15)

- Rust-only failures: the `TransientTransactionError` retry set, the
  `ErrorLabels` set, `Location50`, `Interrupted`. Python has #1585 and (soon)
  `fix-txn-codenames`; Rust has neither.
- **Blocker:** `fix-txn-codenames` is another session's claim. Wait for it to
  merge, then port both in one Rust branch. Starting now edits the same code
  table under an open claim.
- **Risk:** the exemplar is `mongod`, not the Python server (CLAUDE.md). Only
  #1582 added a differential test; check whether #1585 and the codeName change
  were probed. Run `tests/test_mongod_differential.py` against the Rust server
  too -- a Rust-vs-Python comparison proves nothing here.
- **Verify:** the Rust gauge should reach 1,203 / 7, matching Python.

### 3. Decide the test-command gate -- Joe's call

- `docs/security-reports/2026-08-10.md` recommends mongod's opt-in shape.
  `configureFailPoint` is live without `--auth` (as it was before #1569; the
  flag only stopped hiding that from drivers).
- Proposal: keep it on by default in the embedded test handles
  (`SecantusDBServer`, `RustServer` via Python), opt-in (default off) for the
  standalone daemons `secantusd-rs` / `secantusd-py`.
- Cost: the non-Python driver gauges launch daemons, so every gauge task must
  pass the flag. Miss one and that gauge silently loses its failpoint coverage.
- Counter-argument: SecantusDB is a test tool with a loopback default bind;
  deny-by-default adds friction for its intended users against a local DoS the
  report itself rated WARNING.

### 4. Small `mongod` probes, batched

- `getParameter` reports `featureCompatibilityVersion: "7.0"` against an 8.x
  target.
- Under `maxTimeAlwaysTimeOut`, Python `createIndexes` answers with the
  index-build envelope and Rust with the bare message. Probe which one mongod
  gives, then align both.
- Needs `fastdl.mongodb.org` on the environment's allowlist **at session
  start**; it does not reach a running session.

### 5. Re-run the other-language gauges on both servers

- Go / Node / Java / Kotlin / Ruby / Rust-driver / PHP / C / C++ / .NET were
  not re-measured after #1569. Their failpoint tests now run too.
- Expect both pass counts and failure counts to rise. Treat new failures as bug
  reports: the pymongo rerun is how the Rust session-abort and change-stream
  token bugs were found.

### 6. Real `maxTimeMS` enforcement in Rust -- a project, not a fix

- Today a slow Rust command with `maxTimeMS` runs to completion. The failpoint
  works; the limit does not.
- Needs a cooperative deadline polled by the storage scan, the aggregation
  pipeline and the index build, as `src/secantus/deadline.py` does.
- **Risk:** those polls sit in the flagship server's hottest loops. Benchmark
  before and after (`invoke release-benchmark`); do not assume the cost.

### 7. Leave alone (both servers)

- Text and hashed indexes: out of scope per CLAUDE.md.
- `$where` (`test_where`, `test_maxtime_ms_message`): needs a JavaScript engine.
- `test_to_list_csot_applied`: unexplained. Reproduce before deciding; it may be
  timing-sensitive.

## Environment problems seen in the session that wrote this

These belong to the environment settings, not the code. How settings reach a
session is inferred, not documented.

- **Deleting a branch returned 403 while pushes worked.** Reconnect GitHub, or
  enable "Automatically delete head branches" in the repo settings.
- **Network-allowlist changes did not reach the running session.** Add
  `fastdl.mongodb.org` before starting a session that needs `mongod`.
- **The disk allowance is too small for one full suite run.** The suite keeps
  every test's WiredTiger store (deleting mid-run WT_PANICs), roughly 65 GB for
  a full run. Either raise the environment's disk or give `invoke test` a
  chunked mode (the session ran 16 chunks with `rm -rf /tmp/pytest-of-root`
  between them).
