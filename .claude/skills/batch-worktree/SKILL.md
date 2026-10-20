---
name: batch-worktree
description: Set up an isolated worktree for a SecantusDB change batch and land it cleanly. Fires when starting a feature/fix batch, creating a worktree, provisioning a worktree venv, running a gauge from a worktree, or preparing to commit/PR/merge. Holds the venv recipe that avoids silently skipping 1700 tests, how to run and baseline a gauge from a worktree, the claim protocol for parallel sessions, the invoke-first Rust task list (`rust-pgserver-test` / `rust-pgserver-build` / `rust-test-one` / `rust-gate`) with the provenance gates that abort pytest on a stale binary or venv, the relative-path trap that edits the main checkout instead of the worktree, and the landing sequence including the traps that make teardown fail — notably that a worktree which ran a gauge cannot be removed by `git worktree remove` at all.
---

# Set up a batch worktree, and land it without leaving debris

Several sessions run in parallel against this repo. **Never work in the main
checkout** — another session usually has it on their branch, and a
`git reset --hard` from a release session will clobber uncommitted work.

**File-tool paths are resolved against the session's launch cwd, not your
worktree.** A session launched from the main checkout that edits
`tasks/backlog-rust-postgres.md` by relative path writes into the MAIN
checkout (found and reverted three times in one 2026-10-17 session — each
mistake looked like a successful edit because the file did exist there).
Use absolute paths for every edit inside a worktree, or verify with
`grep -n <anchor> <worktree-path>` before moving on.

## Claim before you start

1. `git fetch origin --prune`, then `git ls-remote --heads origin` and
   `gh pr list --state open`. A branch or open PR matching your item means
   another session holds it — pick something else.
2. **Reproduce the item before working it.** The backlog lags reality: entries
   have described work as remaining that was already done, and understated
   bugs that were worse than filed.
3. Claim by creating the branch and **pushing it immediately, before the first
   commit**. An unpushed local branch claims nothing.

```bash
git worktree add ../SecantusDB-<slug> -b <branch> origin/main
cd ../SecantusDB-<slug> && git push -u origin <branch>
```

Branch off `origin/main`, not local `main` — local can be a hundred commits
behind.

Release a claim by merging and deleting the branch, or by deleting your own
branch if you abandon the item. **Never delete or force-push another
session's claim branch**, however stale it looks: report it and let Joe
decide.

## Provision the venv COMPLETELY

A fresh worktree has no `vendor/wiredtiger`, and the main `.venv`'s editable
finder beats `PYTHONPATH`. Build a dedicated venv:

```bash
uv venv --python 3.12 .venv-test
cp -R <main-repo>/.venv/lib/python3.12/site-packages/wiredtiger \
      .venv-test/lib/python3.12/site-packages/
echo ".venv-test/" >> "$(git rev-parse --git-path info/exclude)"
uv pip install --python .venv-test/bin/python -q \
  invoke pymongo s2sphere shapely python-dateutil pytest pytest-xdist \
  pytest-json-report pytest-benchmark pytest-timeout trustme cryptography \
  fastapi "uvicorn[standard]" jinja2 httpx python-multipart anyio starlette \
  pg8000 sqlalchemy "sqlglot==30.12.0" "psycopg[binary]==3.3.4"
```

**Then build and install `_secantus_core`, every time:**

```bash
uvx maturin build --release --manifest-path crates/secantus-core-py/Cargo.toml
uv pip install --python .venv-test/bin/python --reinstall --no-cache \
  crates/target/wheels/secantus_core-*.whl
```

**Its absence is silent.** Without it the ~1700 engine-parity tests do not
collect and the suite still exits 0 — a green run that is 1700 tests short.
The only thing that catches it is comparing the count to the previous batch
(~8700 at the end of 2026-08). Pin the versions: `uv pip install` ignores the
lock, and an unpinned `sqlglot` fails ~5 SQL tests as a pure venv artifact.

Run with `PYTHONPATH=src .venv-test/bin/python -m pytest ...`.

**For Rust-server work, a full `uv sync` is the simpler provisioning.** The
recipe above suits engine-parity and gauge runs; a change to the Rust servers
usually needs the repo's own suite, and `./inv sync` (i.e.
`uv sync --all-extras --reinstall-package secantus-core`) builds
`_secantus_core` properly from the worktree's sources and installs every
extra the suites import. It needs the WiredTiger submodule first:

```bash
git config submodule.vendor/wiredtiger.url <main-repo>/vendor/wiredtiger
git -c protocol.file.allow=always submodule update --init vendor/wiredtiger
```

(the local URL avoids a full network clone; git's file-transport guard
otherwise refuses with `transport 'file' not allowed`).

**Do NOT shortcut the venv by symlinking or copying the main checkout's
`.venv`.** The pytest collection hook (`tools/provenance.py`) compares the
installed `_secantus_core`'s stamped source tree against the checkout and
ABORTS the whole run when they differ — a stale extension can no longer
silently produce phantom failures, it now produces no run at all. The same
abort fires for the `secantusd-pg`/`secantusd-rs` binaries
("built from different sources than this checkout"); rebuild with
`./inv rust-pgserver-build` / `./inv rust-binary-build` rather than bypassing
it. `SECANTUS_ALLOW_STALE_ARTIFACT=1` exists for deliberately measuring an
old build; it is not a way out of a forgotten rebuild.

**Provenance stamps move when the tree moves.** After committing, or after
merging `origin/main` into the branch, the previously-built artifacts are
stale against the new tree state — rebuild the binary and re-sync before any
pytest run, or every test file aborts at collection.

**Gauges need the same venv, and `./inv validate-*` will not use it.** The
invoke tasks run under the worktree's own `uv` environment, which has no
WiredTiger — so the gauge's daemon cannot start and you get
`pg daemon at 127.0.0.1:<port> did not become ready within 15.0s`, not an
import error naming the real cause. Drive the runner directly instead; it
spawns its daemon with `sys.executable`, so the interpreter you choose is the
one the daemon gets:

```bash
PYTHONPATH=src .venv-test/bin/python -m psycopg_validation.runner
PYTHONPATH=src .venv-test/bin/python -m sqlalchemy_validation.runner
PYTHONPATH=src .venv-test/bin/python -m slt_validation.runner
```

**A gauge number means nothing against the committed report.** The reports in
`docs/validation-report-*.md` were generated in a fully-provisioned environment;
a worktree venv is missing extras those suites need, which shows up as failures
that have nothing to do with your change (psycopg's `test_typing.py` alone is
125 failures without a mypy toolchain). To judge a regression, **run the gauge
twice — once with `git stash push src/` and once without** — and diff the
`FAILED` lists. Do that before concluding anything from a delta; two apparent
regressions in one 2026-08-30 batch both dissolved this way, one of them a leak
test giving 3/3/2 failures at baseline versus 1/2/3 with changes on identical
code.

**Do not stash while a suite or gauge is running.** The files change underneath
it and the run is silently invalid — kill it and restart rather than report its
number.

## Batch several slices per branch

The suite is ~8,700 tests and 13–20 minutes. Running it per one-file change
makes the ceremony dwarf the work. While iterating, run only the targeted files
plus whatever probe proves the slice; accumulate related slices on one branch;
run the **full** suite once before the batch's commit.

## Before committing

- `./inv fmt` then `./inv lint` (ruff check **and** format --check). Run `fmt`
  before `lint`, and never while a pytest run is in flight.
- Rust edits: run the build and test activity through **invoke**, not bare
  cargo — `./inv rust-pgserver-test` (the PG crate's `fmt --check` + clippy
  `-D warnings` + `cargo test`), `./inv rust-pgserver-build` (the debug binary
  the slice tests need), `./inv rust-test-one --crate <c> --test <t> --name
  <n>` (one test, fast loop), and `./inv rust-gate` for the full pre-commit
  gate. The invoke tasks fill `SECANTUS_WT_INCLUDE` / `SECANTUS_WT_LIB`
  themselves (`_rust_env()` finds any built WiredTiger, e.g. the main
  checkout's `build/rust-wt`), which is what makes them work inside a
  worktree; only a hand-run `cargo` needs those set, plus `LIBCLANG_PATH` for
  bindgen. Set `CARGO_BUILD_JOBS=3` (CLAUDE.md's machine budget — invoke does
  not set it for you) and capture the task's real exit code (`> log; echo $?`)
  rather than piping through grep, which masks a failure. The WT-linked crates
  are excluded from the clean workspace, so `rust-gate`'s clean-workspace step
  never formats or lints them — the per-crate task is the only fmt gate, and
  CI fails on unformatted Rust.
- Never bump a version. Both version lines are assigned at release.
- Add a `changelog.d/<slug>.md` fragment; do not edit `docs/changelog.md`.
- `rust-gate` does not fmt/clippy every excluded crate: `secantusdb` (and
  `secantus-server-py`) are only gated by CI's `rust-storage` lane, so an
  edit under `crates/secantusdb` must be checked by hand before committing —
  `cd crates/secantusdb && cargo fmt --check && cargo clippy --all-targets --
  -D warnings` (learned 2026-10-17: CI failed on the new embedded test's
  formatting, green locally). `--test-threads` aside, `rust-binary-test`
  covers the crate's tests.

## Landing

Push the branch, open a PR, **watch the PR's CI run**, then merge. Do not push
to `main` — every session pushing the same ref lands in one CI concurrency
group and cancels each other's runs.

**Gate-check, merge, and clean up are three separate inspected steps.** Confirm
the checks are `success` before merging; a chained gate exit cannot stop a
merge in the same command.

```bash
gh pr merge <N> --squash                       # merge first
git worktree remove ../SecantusDB-<slug>       # worktree BEFORE branch
git branch -D <branch>                         # -d refuses squash-merged work
git push origin --delete <branch>
```

Four traps, all hit for real:

- **Remove the worktree before deleting the branch.** `gh pr merge
  --delete-branch` aborts with "cannot delete branch ... used by worktree" —
  and having failed, skips the *remote* delete too, so it looks like it cleaned
  up when it deleted nothing.
- **`git branch -d` refuses branches that did land.** Squash-merged commits are
  never ancestors of `main`. Confirm `gh pr view <N> --json state` is `MERGED`,
  then use `-D`.
- **A worktree that ran a gauge cannot be removed by `git worktree remove` at
  all.** Any gauge (`validate-psycopg`, `validate-slt`, …) runs
  `git submodule update --init vendor/<x>` in *your* worktree, and from then on
  git refuses: `fatal: working trees containing submodules cannot be moved or
  removed`. `git submodule deinit -f <paths>` does **not** lift it — the refusal
  is unconditional once the worktree has submodule entries, and `--force` does
  not bypass it either. The fallback is a manual remove plus a prune:

  ```bash
  gh pr view <N> --json state -q .state          # must be MERGED
  git -C ../SecantusDB-<slug> status --short     # must be empty
  git -C ../SecantusDB-<slug> rev-parse HEAD     # must equal the pushed ref
  rm -rf ../SecantusDB-<slug>
  git worktree prune
  ```

  Run those three checks *first* — `rm -rf` cannot be undone, and a gauge
  worktree can be carrying a gigabyte of vendored corpus you do not want to
  re-clone by accident. (`vendor/sqllogictest` alone is ~1.1 GB.)
- **Never remove a worktree or branch you did not create in this conversation.**
  This includes **stashes**, which are repo-global and therefore visible from
  every worktree: `git stash list` in your worktree will show other sessions'
  entries. Leave them. Only pop what you pushed in this conversation.

Tear down in the SAME session that merges: nobody else can tell your
abandoned branch from live work, and one audit found **235 remote branches
and 11 worktrees**, nearly all merged months earlier, hiding the few that
mattered.

**Auditing branches after the fact is much harder than not creating the mess.** `git merge-base --is-ancestor`, `git cherry`, and `git merge-tree` each gave a confidently wrong answer during that audit (squash merges defeat the first two; the third reports conflict markers as though they were content). The signal that actually held up was the PR's merge state, cross-checked against whether `main` already contains the branch's distinctive files. And before reviving any long-lived unmerged branch, check whether `main` has since *rejected* it — one such branch looked like a valuable unmerged CI fix, but `tests/conftest.py` carries a deliberate tripwire against exactly that change because it was tried and caused a `WT_PANIC` cascade.

The branch and worktree are only half of it. Servers still bound to ports,
WiredTiger temp stores, the `pytest-of-<user>` backlog, and background waiters
still armed all outlive the session too — see the **`session-cleanup`** skill
for those and for the closing verification pass. To close out a whole session
rather than one branch — landing every outstanding change, recording what is
left, and reconciling the docs with what changed — see **`close-session`**.

## If work lands on a branch whose PR is already open

Do not commit it there — that changes a PR mid-review. Save and restore:

```bash
git diff > /path/to/scratch/work.patch
git checkout -- <the files>          # branch back to its pushed state
# after the PR merges, in a fresh worktree off updated main:
git apply /path/to/scratch/work.patch
```

## Verify the paper trail at the end

Check the *records*, not your memory of them: `git show origin/main:<file>` for
each changelog fragment and test file, `gh pr view <N> --json state` for each
PR, and re-read the plan/backlog entries you touched. A verification pass at
the end of one campaign found two defects — a plan section still marking
finished surfaces as untouched, and a backlog item that outlived its bug by
several hours. Both would have sent the next session to re-do finished work.
