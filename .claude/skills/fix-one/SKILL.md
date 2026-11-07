---
name: fix-one
description: Take ONE item from a named SecantusDB backlog, fix it, commit it and merge it into main, end to end. Fires on "/fix-one", "fix one", "fix one item from the <x> backlog", "pick a backlog item and resolve it". Arguments are the backlog -- `postgres` / `pg` / `rp` (tasks/backlog-rust-postgres.md), `mongodb` / `mongo` / `rm` (tasks/backlog-rust-mongodb.md), or a backlog file name -- and optionally an item ID (`RP-296`) or a choice rule ("hardest", "smallest", "oldest"). Sequences the backlog, batch-worktree and ci-check protocols; holds the order of steps and the stop conditions, not their detail.
---

# Fix one backlog item, from claim to merged main

To start from a clean context, run it in a new session:
`scripts/fix_one.sh postgres [ID|rule]` (add `-p` for headless). A skill
cannot run `/clear` itself. Several items: `scripts/fix_many.sh postgres
<count>` runs fix_one sessions in two lanes.

**Headless (`-p`) sessions end when your turn ends.** Never end a turn
while a background build, gate, CI wait or agent of yours is still
running: the work dies with the session (2026-11-01: a run ended mid-gate
and left its fix uncommitted). Wait for each result before you stop, and
commit as soon as the fix exists so nothing is lost.

**Integration mode: `into <branch>` in the arguments** (`/fix-one postgres
into pg-batch`; `scripts/fix_many.sh -i pg-batch` passes it). The fix lands
on that integration branch instead of `main`, after the local gate and with
no CI wait: CI runs once on the integration branch's PR to `main`, which the
batch's orchestrator lands in snapshots with `scripts/land_batch.sh` (Joe, 2026-11-06: a session spent
~40 of its 35-60 min waiting on CI). Where a step below says `origin/main`,
read `origin/<branch>`; step 7 has the landing. Never push to `main`.

**Lane mode: `lane <path>` in the arguments** (fix_many passes it with
`-i`). Work in that existing worktree instead of creating one: it is the
lane's, reused item after item so its cargo target and venv stay warm
(step 2 has the switch, step 8 what to leave). Never remove it.

One item, one branch, one PR, one merge, one teardown. Every step below
points at the skill that owns its detail -- read that skill when you reach
the step, do not work from memory.

## 0. Resolve the backlog

| argument | file | IDs |
| --- | --- | --- |
| `postgres`, `pg`, `rp`, `rust-postgres` | `tasks/backlog-rust-postgres.md` | `RP-nnn` |
| `mongodb`, `mongo`, `rm`, `rust-mongodb` | `tasks/backlog-rust-mongodb.md` | `RM-nnn` |
| a file name under `tasks/backlog-*.md` | that file | its prefix |

**Refuse the Python backlogs** (`backlog-python-*.md`): they are a record,
not worked (CLAUDE.md). No argument: ask which backlog, one question.

This session now works ONE Rust server: the one this backlog belongs to
(CLAUDE.md). Load `rust-pg-server` for RP, `rust-mongodb-explain` when an RM
item touches the planner or explain.

## 1. Pick the item

- **PG backlog: check `main`'s psycopg Windows gauge first.** PRs that do
  not touch the wire, TLS or auth no longer run it, so the push-to-main run
  is where a regression shows:
  `gh run list --workflow psycopg-windows.yml --branch main --event push --limit 1 --json conclusion,headSha,url`.
  If it is `failure`, that regression is the item: reproduce it, fix it,
  file it if it is not one PR.
- An ID was given: take it.
- Otherwise list the open items (`grep -n '^- \[ \] \*\*' <file>`), read
  the candidates, and pick by the rule given (default: the most
  consequential wrong answer that fits one PR). Skip items marked
  `**Decision needed`** (they are Joe's) and `Verify: none --` items whose
  remainder no test can check. Say which item and why in one line.

## 2. Claim it (`batch-worktree`, CLAUDE.md "Claim a backlog item")

```bash
git fetch -q
git ls-remote --heads origin | grep -i <id>      # someone else's claim?
gh pr list --state open                          # an open PR for it?
git log origin/main --oneline -S'<ID>' | head    # closed already?
# integration mode: also `git log origin/<branch> -S'<ID>'`, and base on it
git worktree add ../SecantusDB-<slug> -b <slug> origin/main
cd ../SecantusDB-<slug> && git push -u origin <slug>   # the claim
```

**Lane mode** replaces the `worktree add` line:

```bash
cd <lane path>
git status --short                     # must be clean (ignored files aside)
git fetch -q origin && git switch -C <slug> origin/<branch>
git -c protocol.file.allow=always submodule update --init vendor/wiredtiger
git push -u origin <slug>              # the claim
```

The `-c protocol.file.allow=always` is needed on Joe's Mac: its checkout
points the submodule at a local path, which git refuses by default
(`transport 'file' not allowed`), so `./inv sync` then fails.

If the lane is not clean, a previous item left work behind: save it with
`git diff HEAD > .scratch/lane-leftover-<date>.diff`, say so in your report,
then `git reset --hard && git clean -fd` (never `-x`: that deletes the warm
target and venv).

Claimed elsewhere: pick another item, never share or touch their branch.

## 3. Reproduce before building (`backlog`, `differential-probe`)

Build from the worktree (`./inv rust-pgserver-build` / `./inv
rust-binary-build`, `CARGO_BUILD_JOBS=3`), provision its venv with `./inv
sync` (needs the submodule update above first), then
run the item's `Verify:` tests. In a lane worktree both are incremental
and take a minute or two; still run both, as the branch moved.

```bash
.venv/bin/python -m pytest -o addopts= -m "backlog or backlog_fixed" <verify test>
```

- `FIXED?` (it passes): confirm with a probe, then close it (step 5) with
  no code change.
- `BROKEN`: repair the gap test first. It asserts the real product's
  answer, so never weaken it.
- Size it from what the probe shows. If the fix turns out to be a
  decision (a stand-in mechanism, a generated artifact, a changed default),
  stop and ask Joe as `backlog` "Decisions" says, faithful option first.

## 4. Fix it

- Build the most faithful version of the real product's behaviour
  (CLAUDE.md design constraints).
- Shared storage crates are allowed. Say in the PR that the other server
  links the change, and verify that server's backlog too (step 6).
- Add a regression test beside the code where one fits (a crate test, a
  `tests/` file), as well as the gap test.
- Anything else you find gets filed with its own gap test (`backlog`, "Found
  while working"). Do not widen this PR to include it.

## 5. Close the item in the same change (`backlog`, "Working an item")

- Gap test marker `backlog("ID")` -> `backlog_fixed("ID")`, docstring in
  the past tense.
- Delete the item's lines from the backlog file by an anchored span, then
  read the removed lines: `git diff tasks/ | grep '^-'`.
- `changelog.d/<slug>.md`: headline, lede, `#### Fixed`, and the line
  `Fixes <ID> (<test path>::<test>)`. No version bump, no
  `docs/changelog.md` edit.

## 6. Gate, once, at the end

**PG backlog: run `scripts/gate_pg.sh`** (`--storage` if you touched the
shared storage crates) through `scripts/detached_run.py`, and wait for it.
It runs every step below that a PG change can reach -- the PG crate and
the clean workspace, a rebuild, the suites CI's pg-oracle lane runs, the
login comparisons, the backlog verify with `--regressions`, lint and ruff
-- writes each step's exit code to `.scratch/gate/<step>.rc`, and ends
with `gate=0|1`. It replaces `rust-gate` and the full pytest suite for a
PG change: their extra cost is the MongoDB parity files, which CI no
longer runs. Read every `.rc` that is not 0, and the log beside it.

- **Integration mode: `scripts/gate_pg.sh --fast`.** It skips `pg-suites`
  (20-27 of the gate's ~40 min), which CI's pg-oracle job runs on the
  integration branch's PR before anything reaches `main`. Default mode
  keeps the full gate: that PR's CI runs the same list, but a full local
  run saves a wasted CI round.
- The cheap checks (fmt, lint, ruff) run first and stop the gate when one
  fails, in seconds. Fix and rerun.
- **After a failure, rerun only what the fix can reach.** A format or lint
  fix: `scripts/gate_pg.sh --only fmt` (or `lint`, `ruff-format`, ...;
  comma-separated), and read its table of every step on record. A fix to
  code or tests: the whole gate again (with `--fast` in integration mode).

**RM backlog: run `scripts/gate_rm.py`** through `scripts/detached_run.py`,
and wait for it. It runs what CI runs for the Rust MongoDB server -- the
clean workspace and every WiredTiger-linked crate, both server builds from
HEAD, the `test` job's selection less the files CI deliberately does not
run, the `storage-engine` job's named files (read from `test.yml`), the
concurrency invariants, the backlog verify with `--regressions` and
`tests/mongod_live/` against mongod 8.2, lint and ruff -- with the same
`.scratch/gate/<step>.rc` files and `gate=0|1` as `gate_pg.sh`. It replaces
`rust-gate` and the full pytest suite, whose extra cost is the deprecated
Python servers' tests and the parity suites CI no longer runs. A change to
the shared storage crates also runs `scripts/gate_pg.sh --storage`, since
the PG server links them.

Read every `.rc` that is not 0, and the log beside it; do not trust an exit
code taken through a pipe or a trailing `echo`. Expected from the backlog
step: this item gone, no `REGRESSED`, no new `FIXED?` or `BROKEN`.

A failure is a bug to fix, never a test to skip (CLAUDE.md "Never ignore or
discount an error").

## 7. Commit, PR, CI, merge (`batch-worktree`, `ci-check`)

**Integration mode** replaces this step's PR, CI and merge:

```bash
git status --short          # read it: no build output, no .scratch/
git add <paths> && git commit -m "<server>: <headline> (<ID>)"
git fetch -q origin <branch> && git rebase origin/<branch>
git push origin HEAD:<branch>   # fast-forward only, never --force
```

- One commit per item, so a red batch run can be bisected to it.
- If the rebase moved anything (another lane landed first), rerun the gate
  (`--fast`) only when the rebase had a conflict or the commits it brought
  in touch a file yours touches:
  `git diff --name-only <old base> origin/<branch>` against
  `git show --name-only --format= HEAD`. Otherwise push: the batch PR's CI
  checks the commits together. A rejected push means the same: fetch,
  rebase, the same overlap test, push again. (A session gated three times
  for other lanes' unrelated commits, 150 min, on 2026-11-06.)
- Check the landing: `git log origin/<branch> --oneline -1` is your commit.
  Report `landed on <branch> at <sha>` (no PR number). Then step 8.

```bash
git status --short          # read it: no build output, no .scratch/
git add <paths> && git commit -m "<server>: <headline> (<ID>)"
git push && gh pr create --base main --title "..." --body "... Fixes <ID> ..."
```

- Watch the PR's own run with `scripts/wait_ci.sh <N>`, in the background
  (exit 0 green, 1 red, 2 timed out, 3 the PR conflicts with main: rebase
  onto origin/main, push, and wait again). Do not write your own polling loop:
  two hand-written ones wedged for half an hour each on 2026-10-31. Fix
  every red check and push again (`ci-check`).
- Merge only after `gh pr checks <N>` is all green: `gh pr merge <N>
  --squash`. Then check `gh pr view <N> --json state` says `MERGED`, and
  grep `origin/main` for the deliverable (a merged PR is not landed
  content).

## 8. Tear down (same session)

Remove the worktree (in the background, it is large), then `git branch -D`
and `git push origin --delete <slug>`, only after `MERGED` (integration
mode: after your commit is on `origin/<branch>`).

**Lane mode: keep the worktree.** `git switch --detach`, then delete the
branch locally and on origin as above, stop the servers and private
cluster you started, and remove `.scratch/gate` and your probe stores.
Leave `target/` and `.venv`: the lane's next item builds on them. Delete what you
put in `.scratch/`. Report: the item, what was wrong, the fix, the test that
pins it, the PR number, and anything filed along the way.

## Stop and report instead of merging when

- the item needs one of Joe's decisions;
- CI stays red on something you cannot root-cause;
- the fix widened into the other server's code beyond the shared crates;
- a gate could not run (say which and why, never "green").
