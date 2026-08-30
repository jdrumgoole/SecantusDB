---
name: session-cleanup
description: Leave the machine and the repo as clean as you found them at the end of a SecantusDB session. Fires when the user asks "is this cleaned up?", "clean up", "anything left running?", when a PR has just merged, or when a session is winding down. Holds what a session actually leaks (servers, WiredTiger temp stores, pytest temp backlog, wedged background waiters), how to tell your debris from another session's, and the verification pass that turns "I think it's clean" into evidence.
---

# Cleaning up is part of finishing, not a separate chore

A merged PR with a branch still lying around, a `mongod` still bound to a port,
and 150 GB of temp stores is unfinished work. It has to happen **in the session
that made the mess**, because nobody else can safely clean it: a later session
cannot tell your abandoned branch from live work, or your orphaned daemon from
one a running test needs. Anything you leave survives indefinitely. One audit
found 235 remote branches and 11 worktrees, nearly all merged months earlier.

Git-side teardown — worktree before branch, `-D` not `-d`, the
`--delete-branch` trap — lives in the **`batch-worktree`** skill. This one is
everything *else* a session leaves behind.

## Verify every claim with a command

"Clean" is a measurement, not a feeling. Each line of the final report should
come from a command run just now. Assertions rot within minutes in a repo with
parallel sessions: `origin/main` moves, another session starts a suite, a
background task fires.

## 1. Uncommitted work, before anything else

```bash
git status --short          # in EVERY checkout you touched
git branch --show-current   # are you where you think you are?
git stash list
```

Land your own `tasks/*.md` plan and backlog edits in the same slice as the
code — they are the record of what you did, and they are worthless in a
working tree nobody will read. Note which entries in `git status` **pre-date
your session** (compare against the session-start snapshot); do not claim them
as debris and do not clean them.

## 2. Processes you started

Databases, not scripts — treat them accordingly.

```bash
pgrep -fl "python -m secantus|secantusd|mongod|pgserver"
pgrep -f "pytest-xdist|python -m pytest" | wc -l
ps -Ao pid,ppid,comm | awk '$2==1 && $3 ~ /zsh/' | wc -l   # orphaned shells
uptime                                                     # load should settle
```

- **Kill a storage process with SIGTERM, never SIGKILL.** WiredTiger closes its
  tables and checkpoints on a clean shutdown; `kill -9` is the durability
  scenario the test suite exists to catch, and there is no reason to inflict it
  on a leftover daemon.
- **Kill xdist workers by proctitle** (`pytest-xdist`), not the controller's
  cmdline, or the workers survive and thrash the box.
- Ad-hoc probe servers started inside a script must be stopped in a `finally`;
  an exception between `start()` and `stop()` leaks a bound port and a store.
- Orphaned `zsh` snapshot shells with PPID 1 are the ones that once produced a
  load of 40 on a 12-core box and looked exactly like thermal throttling.
  Shells with a live parent at 0% CPU are idle waiters, not leaks.

## 3. Temp directories — the largest and least visible leak

Two distinct sources, both measured on this box:

- **`tempfile.mkdtemp()` in ad-hoc scripts.** Every probe that does
  `SecantusDBServer(storage_path=tempfile.mkdtemp())` leaves a WiredTiger store
  behind. One evening of probing left **79 such stores** (715 MB when measured
  a few minutes before deletion). Nothing ever collects them.
- **`$TMPDIR/pytest-of-<user>/`.** This one actively damages future runs. A
  pytest process with no `--basetemp` registers an `atexit` hook that `rmtree`s
  every stale run except the newest three, so a backlog makes **every future
  suite pay an unbounded deletion at exit** — that is the real cause of "the
  suite hung after printing its summary" (root-caused to 1479 of 1559 samples
  in `os.unlink` during `Py_FinalizeEx`). It reached 241 dirs / 415 GB once, and
  148 GB again a fortnight later.

```bash
T=$TMPDIR
du -sh $T/pytest-of-$USER 2>/dev/null
find $T -maxdepth 1 -type d -name 'tmp*' -newermt '<session start>' | wc -l
```

Deleting is safe **only** with a pre-flight and a time guard:

```bash
# nothing may be running
for p in "python -m pytest" "pytest-xdist" "python -m secantus" "gradle" "dotnet test"; do
  echo "$p: $(pgrep -f "$p" | wc -l)"
done
# -mmin +60 so a just-started run is never hit
find $T -maxdepth 1 -type d -name 'tmp.*' -mmin +60 -print0 | xargs -0 rm -rf
find $T/pytest-of-$USER -maxdepth 1 -type d -name 'pytest-*' -mmin +60 -print0 | xargs -0 rm -rf
```

Pass `--basetemp <dir>` to any **nested** pytest a test spawns; an explicit
basetemp skips the cleanup hook entirely.

## 4. Background waiters and tasks

Cancel every background wait you armed. Two failure modes, both real:

- **A stale waiter fires later with old data.** Waits armed for a first run
  fire when a *second* run ends and print the **first** run's log — a gate whose
  log said "2 failed" resurfaced hours after those failures were fixed. Stop
  them (`TaskStop`) the moment they are superseded, or you will read a false
  signal at exactly the wrong moment.
- **A `pgrep` waiter can match itself.** `while pgrep -qf "validate-all"; do
  sleep 120; done` puts the pattern in its own shell's command line, so it
  waits forever on itself. One sat wedged for **five days**, blocking its
  session on a notification that could never arrive. Wait on a sentinel file
  (`cmd; echo $? > run.exit`) and match a pattern only the real job has.

## 5. Whose debris is it?

Never kill or delete another session's work; report it and let the user decide.
Attribution is evidence, not intuition:

```bash
ps -o pid,ppid,etime,lstart,command -p <pid>   # started before your session?
```

- **Age settles it.** Five `pgserver` daemons looked like this session's until
  `lstart` showed they began 13 days earlier, from a campaign whose worktree no
  longer existed.
- **A storage path naming a worktree** identifies the owner.
- **The snapshot id in a `zsh -c` cmdline** differs per session; a different id
  is a different session.
- **`pgrep -f <word>` matches any process whose script merely contains the
  word** — including editors, waiters, and your own shell. Ten "stray pytest
  processes" were zsh shells at 0% CPU with the word in a heredoc. Confirm with
  the real work processes before acting.

## The closing pass

Run it, then report what it printed:

```bash
git branch --show-current && git log --oneline -1 && git status --short
git worktree list
git ls-remote --heads origin | grep <your-branch>      # expect nothing
pgrep -fl "python -m secantus|mongod|pytest-xdist"     # expect nothing of yours
df -h / | tail -1 && uptime
```

Then say which items were yours, which pre-existed, and which you deliberately
left for the user — with the reason. A leftover reported honestly is finished
work; a leftover unmentioned is a trap for the next session.
