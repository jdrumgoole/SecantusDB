---
name: start-session
description: Get a new SecantusDB session into gear — establish what is actually true about the repo and the machine before touching anything, then set up to work. Fires at the start of a session, on "let's start", "what should I work on", "get set up", "what's the state of things", or before the first substantive change. Holds the freshness checks that stop you working from a stale picture, the leftovers a previous session may have left running, and the claim protocol that stops two sessions building the same thing.
---

# Start by finding out what is true, not what you remember

Everything below is a check, not a ritual. Each one exists because starting
without it has cost real work here: a duplicated feature, a measurement against
a moving tree, an hour spent on a bug someone already fixed.

The symmetric skill is **`close-session`**. What that one records at the end,
this one reads at the beginning.

## 1. The repo is probably not where you think

```bash
git fetch origin --prune
git status --short                 # uncommitted work? whose?
git log --oneline -1 && git log --oneline -1 origin/main
git worktree list
git stash list
```

- **Local `main` has been more than 100 commits behind `origin/main`.** Branch
  feature work off `origin/main`, not off whatever your checkout happens to hold.
- **Uncommitted changes you did not make belong to a parallel session** — this
  repo runs several at once. Do not stash, reset, or commit them. Check
  `git stash list` too, and leave entries you did not create alone.
- **A worktree you did not create is someone else's live work.** Never remove
  one to tidy up.
- If your own last session left something uncommitted here, land or stash it
  before starting anything new: a parallel session's `git reset --hard` will
  take it.

## 2. The machine may still be running the last session

```bash
pgrep -fl "mongod|python -m secantus|pytest-xdist"
uptime                             # load should be near idle
df -h /                            # WiredTiger stores are large
```

A leftover daemon holding a port, or a pytest backlog inflating every later
run's exit time, will be blamed on your change if you don't notice it first.
**`session-cleanup`** has the attribution rules — age, storage path, the
snapshot id in a `zsh -c` cmdline — for deciding what is yours before you kill
anything. A process older than your session is not yours.

## 3. Read the record, then distrust it enough to check

- `tasks/remaining-work-plan.md` — phases and what is claimed done.
- `tasks/backlog.md` — the honest list of divergences; the top of it says why.
- `git log --oneline -20` — what landed while you were away, which is often the
  answer to "is this still open?".

**Re-verify an item by reproducing it before you build anything.** The backlog
lags reality in both directions: entries here have described work as remaining
that was already done, and have understated bugs by naming one symptom of
several. Several campaigns were planned off text and found substantially
finished when measured.

## 4. Claim before you build

Two sessions have independently built the same thing here more than once.

```bash
git ls-remote --heads origin          # someone's branch on your item?
gh pr list --state open --json number,title,headRefName
```

A matching branch or open PR means another session holds it — pick something
else. To claim: create the branch and **push it immediately, before the first
real commit**. An unpushed local branch is invisible and claims nothing. The
setup, venv provisioning and landing sequence are in **`batch-worktree`**.

## 5. Know which reference server you are measuring against

Findings come from executing against a real server, so establish which one
before you draw conclusions:

```bash
which mongod && mongod --version | head -1
```

**mongod 8.x is the only version SecantusDB targets** (CLAUDE.md). `PATH` here
should be 8.2.11; 6.0.16 and 8.3.4 sit at their keg paths for probing version
differences. If `PATH` gives 6.0, `tests/test_mongod_differential.py` **silently
skips** — a green local run of that file then means it did not run. The probe
recipe, normalisation rules and version hazards are in **`differential-probe`**.

## Before the first substantive change

Say what you found: which commit you are on, whether anything was already
running, what you are claiming, and — if the plan or backlog pointed you at it —
whether you reproduced it. An item you could not reproduce is a finding worth
reporting on its own.
