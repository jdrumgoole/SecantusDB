# Rust-server session handover

Snapshot for resuming the **Rust server** work on another machine. (This is the
session that builds the Rust server under `crates/`; a parallel session works the
**Python** server under `src/secantus/` + change streams — keep Rust work in
`crates/` to avoid collisions.)

## Current state (as of this handover)

- **`main` is green** at the merge of cursor `min()`/`max()` index bounds
  (`0.5.3-beta.19`). Last CI `Tests` run: `rust-storage: success`, zero failed jobs.
- **Versions are independent (two servers):**
  - Rust server: every `crates/*/Cargo.toml` + `Cargo.lock` in **lockstep** at
    `0.5.3-beta.19` (SemVer pre-release). Bumping patch/minor/major resets the
    beta label to 0. Bump for any `crates/**` change.
  - Python server: `pyproject.toml` + `src/secantus/__init__.py` at `0.5.3b11`
    (PEP 440, the PyPI package). Bump for `src/secantus/**`. **Don't** bump one
    for the other's work.
- **Rust pymongo gauge: 92.0%** (`docs/validation-report-rust-server.md`,
  `--server rust`). Started this session at 88.4%.

## Build / run on a fresh machine

1. `git submodule update --init vendor/wiredtiger` (WiredTiger is vendored; the
   WT-linked crates won't build without it — a fresh/empty submodule is the #1
   first-run failure).
2. Build the embedded Rust server extension (`_secantus_server`) into the venv:
   ```
   SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv sync --extra dev --reinstall-package SecantusDB
   ```
   This compiles the WT-linked crates + builds vendored WiredTiger (slow first time).
3. **For ad-hoc repros, use `uv run --no-sync python ...`** — a bare `uv run`
   re-syncs and rebuilds `secantusdb` WITHOUT the storage-engine flag, clobbering
   `_secantus_server` (then `import _secantus_server` fails and you must rebuild
   with step 2 again).
4. Rust gauge: `uv run python -m invoke validate --server rust` → writes
   `docs/validation-report-rust-server.md` (the "Overall" row is the headline).
   ~17 min; run it as a background command or sub-agent.

## Workflow that's been working

- **Worktrees:** develop on a feature branch in the `SecantusDB-rust-update`
  worktree; merge into `main` from the **primary** `SecantusDB` worktree
  (`git checkout main && git merge --no-ff <branch>`). Re-`git fetch origin` +
  `pull --ff-only` before every merge — the parallel Python session pushes to
  `main` frequently (expect occasional `uv.lock` conflicts → take `--ours`, the
  Python version bump is theirs).
- **Per slice:** implement → `cargo build/test -p secantus-commands` (+ clippy) in
  the clean workspace → fmt → bump beta.N → commit → spawn a background sub-agent
  to rebuild + run an e2e + the gauge → merge when the gauge is up with no
  regression. Keep build/gauge noise out of the main context via the sub-agent;
  it returns only deltas.
- **rustfmt:** use rustup stable (`~/.cargo/bin/cargo +stable fmt`), not brew's —
  CI's `cargo fmt --check` wraps differently. The clean-workspace `cargo fmt`
  covers WT-free crates; rustfmt the WT-linked files by path separately.

## CRITICAL gotcha — the WT-linked clippy/fmt blind spot

`secantus-storage`, `secantus-storage-adapter`, `secantus-wt`, `secantus-storage-py`,
`secantusdb` are **excluded from the clean Cargo workspace**, so a normal
`cargo build`/`cargo clippy -p secantus-commands` NEVER compiles them. The
embedded extension *build* is laxer than clippy. CI's `rust-storage` job runs the
strict checks and catches lints you can't see locally — this caused issue #57 AND
a follow-on `redundant_closure` that kept the job red. **Whenever you touch these
crates (or a method/param they call), run the exact CI checks locally before
claiming green:**
```
WT=$(find build -type d -name wt-build | head -1)
export SECANTUS_WT_INCLUDE="$PWD/$WT/include" SECANTUS_WT_LIB="$PWD/$WT"
( cd crates/secantus-storage-adapter && ~/.cargo/bin/cargo fmt --check && ~/.cargo/bin/cargo clippy --all-targets -- -D warnings )
( cd crates/secantus-storage && ~/.cargo/bin/cargo fmt --check && ~/.cargo/bin/cargo clippy --all-targets -- -D warnings && ~/.cargo/bin/cargo test )
```
And confirm CI by reading the **job conclusion** —
`gh run view <id> --json jobs --jq '.jobs[]|select(.name|test("rust-storage")).conclusion'` —
**never** a `gh run watch ... || echo PASS` exit code (the `||` masks failures).

## What this session shipped (88.4% → 92.0%)

explain → native `$regex` → aggregate source stages (`$listLocalSessions` etc.) →
**multi-document transactions** (real WT-backed: registry + dispatch envelope +
session primitives; txn cluster ~0 → 96.8%) → WriteConflict(112)+transient label →
**fixed the gw1 worker-crash deadlock** (createIndexes-in-txn used a fresh WT
session → +110 recovered tests, skip 596→476) → max-BSON-size rejection (10334) →
`validate`/`profile` + system.profile recording → collection `validator` on
update/replace/findAndModify (121) → unknown-update-modifier + partialFilterExpression
validation → cursor `min()`/`max()` index bounds. Also **closed issue #57**
(rust-storage CI restored to green).

## Next steps (in priority order)

1. **Quick fresh diagnosis first** — bucket `.validation/raw-rust-server.json`
   failures (exclude change-stream + txn/topology/csot out-of-scope) to re-pick,
   since the mix shifts each slice. (Pattern used this session: a throwaway
   Python script over the json grouping by error signature.)
2. **`test_options`** — `listCollections` doesn't report capped options
   (`{capped, size}`) that `create` already stores. Small handler fix in
   `admin.rs::list_collections`. (~1 test)
3. **`test_estimatedDocumentCount`** — wrong count (`2 != 0`). (~1–2)
4. **`test_UpdateOne_using_pipelines`** — pipeline-update result mismatch (~2,
   data-correctness).
5. **Change-stream cluster (~28 fails — the biggest remaining block)** — but it's
   the parallel session's domain and feature-heavy. Root causes already found +
   recorded in `tasks/backlog.md`: DDL events not emitted for `showExpandedEvents`
   (createIndexes/dropIndexes/collMod write no oplog `c`), and `test_split_large_change`
   (>16 MB event, no real splitting). **Coordinate with the Python session** before
   touching this — don't do it unilaterally.
6. Out of reach / features: `$where` (JS), `test_maxtime_ms_message` (error-msg
   format), tailable cursors on capped collections, clustered indexes.

## Canonical references

- `CLAUDE.md` — architecture, two-server model, conventions (authoritative).
- `tasks/backlog.md` — the honest list of stubs/limitations/deferred work
  (§ change-stream limitations has the worker-crash root causes).
- `tasks/rust-server-plan.md` — the north-star Rust-server build-out plan.
- `tasks/rust-transactions-plan.md` — the transactions design (T1/T2/T3a done;
  T3 oplog-buffering for change-stream-in-txn still open, low gauge leverage).
