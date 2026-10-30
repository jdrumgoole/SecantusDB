---
name: fix-one
description: Take ONE item from a named SecantusDB backlog, fix it, commit it and merge it into main, end to end. Fires on "/fix-one", "fix one", "fix one item from the <x> backlog", "pick a backlog item and resolve it". Arguments are the backlog -- `postgres` / `pg` / `rp` (tasks/backlog-rust-postgres.md), `mongodb` / `mongo` / `rm` (tasks/backlog-rust-mongodb.md), or a backlog file name -- and optionally an item ID (`RP-296`) or a choice rule ("hardest", "smallest", "oldest"). Sequences the backlog, batch-worktree and ci-check protocols; holds the order of steps and the stop conditions, not their detail.
---

# Fix one backlog item, from claim to merged main

To start from a clean context, run it in a new session:
`scripts/fix_one.sh postgres [ID|rule]` (add `-p` for headless). A skill
cannot run `/clear` itself.

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
git worktree add ../SecantusDB-<slug> -b <slug> origin/main
cd ../SecantusDB-<slug> && git push -u origin <slug>   # the claim
```

Claimed elsewhere: pick another item, never share or touch their branch.

## 3. Reproduce before building (`backlog`, `differential-probe`)

Build from the worktree (`./inv rust-pgserver-build` / `./inv
rust-binary-build`, `CARGO_BUILD_JOBS=3`), provision its venv with `./inv
sync` (needs `git submodule update --init vendor/wiredtiger` first), then
run the item's `Verify:` tests:

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

Run every one of these and read each exit code itself. Do not trust one
taken through a pipe or a trailing `echo`.

- the crates you touched: `./inv rust-storage-test` (WT-linked crates are
  not covered by workspace commands), `./inv rust-gate` for server changes;
- rebuild the binaries from HEAD, then `python tools/backlog.py verify
  --file backlog-rust-postgres.md --regressions` (the bare file name, as
  `--file` refuses a `tasks/` path; `backlog-rust-mongodb.md` for RM),
  plus the other Rust backlog when shared code moved. Expected: this item gone, no `REGRESSED`, no new `FIXED?`
  or `BROKEN`;
- the full pytest suite once (`build-and-test` for long runs);
- `./inv lint` and repo-wide `ruff check .` / `ruff format --check .`.

A failure is a bug to fix, never a test to skip (CLAUDE.md "Never ignore or
discount an error").

## 7. Commit, PR, CI, merge (`batch-worktree`, `ci-check`)

```bash
git status --short          # read it: no build output, no .scratch/
git add <paths> && git commit -m "<server>: <headline> (<ID>)"
git push && gh pr create --base main --title "..." --body "... Fixes <ID> ..."
```

- Watch the PR's own run by polling REST run status with a sleep, in the
  background (`ci-check`). Fix every red check and push again.
- Merge only after `gh pr checks <N>` is all green: `gh pr merge <N>
  --squash`. Then check `gh pr view <N> --json state` says `MERGED`, and
  grep `origin/main` for the deliverable (a merged PR is not landed
  content).

## 8. Tear down (same session)

Remove the worktree (in the background, it is large), then `git branch -D`
and `git push origin --delete <slug>`, only after `MERGED`. Delete what you
put in `.scratch/`. Report: the item, what was wrong, the fix, the test that
pins it, the PR number, and anything filed along the way.

## Stop and report instead of merging when

- the item needs one of Joe's decisions;
- CI stays red on something you cannot root-cause;
- the fix widened into the other server's code beyond the shared crates;
- a gate could not run (say which and why, never "green").
