---
name: close-session
description: Close out a SecantusDB working session — land every outstanding change, record what is left with measurements, tear down branches and worktrees, and reconcile the documentation with what actually changed. Fires on "close the session", "wrap up", "finish up", "we're done", "end of session", or when the user stops directing new work. Sequences the per-slice landing (batch-worktree) and the machine cleanup (session-cleanup) and adds the part neither covers: finding the docs, comments and tests that still describe the behaviour you just changed. Refuses to declare the session closed while any of your own work is still in flight, and gates the close on a final process sweep so the session leaves no server, waiter or detached run behind.
---

# Closing a session is a task, not a farewell

A session ends well when someone arriving cold can tell what happened, what is
true now, and what is left — from the repo alone, without this conversation.
That takes four passes, in this order, because each depends on the one before,
and then two gates: **you do not get to close while your own work is still
moving (§5), and you do not get to close while anything you started is still
running (§6).** The process gate is last on purpose — waiting in §5 arms waits
and re-runs suites, so a machine that was clean in §4 is not clean any more by
the time you reach the handover.

Its mirror is **`start-session`**: what this skill records at the end, that one
reads at the beginning. Three sibling skills own parts of the work and are not
repeated here:

- **`batch-worktree`** — landing one branch: claim, commit, PR, watch CI, merge,
  tear down. The traps that make teardown fail live there.
- **`session-cleanup`** — machine residue: daemons, WiredTiger temp stores, the
  `pytest-of-<user>` backlog, wedged background waiters, and whose debris is
  whose.
- **`start-session`** — the opening checks, including the leftovers this skill
  is responsible for not creating.

This skill is the order to run them in, plus the documentation pass.

## 1. Land everything, not just the branch you were on

Find your own work rather than recalling it:

```bash
git status --short                      # in every checkout you touched
git worktree list
git ls-remote --heads origin            # branches you pushed
gh pr list --state open --author '@me' --json number,title,headRefName,mergeStateStatus
```

- **Uncommitted work in a shared checkout is the highest risk in the repo.** A
  parallel session's `git reset --hard` clobbers it, and several sessions run
  here at once. Commit or stash within seconds, not at the end.
- **A PR that was green an hour ago may not be mergeable now** — `main` moves.
  Re-check `mergeStateStatus` immediately before merging, and re-run the gate if
  the rebase pulled in anything that touches your files.
- **Gate-check, merge, and clean up are three separate INSPECTED steps.** A
  chained command cannot stop the merge: `./inv rust-gate > log; echo $?; tail`
  reports `tail`'s status, and a gate that failed with real test failures has
  been reported as "exit 0" that way. Read the summary line, not the exit of a
  chain.
- **Landing includes WAITING for the gate you started.** Opening a PR is not
  landing it; pushing is not merging. If CI is queued, the slice is not landed
  and pass 1 is not finished — see §5.
- Work you decide **not** to land still needs a decision recorded — a branch left
  open with a note beats a branch left open silently.

## 2. Record what is left, with measurements

The backlog is the only honest record of where behaviour diverges. Vague entries
cost the next session more than no entry.

- **File what you measured, not what you concluded.** "`$addToSet` reports the
  whole array on 6.0.16 and `arr.5` on 8.2.11" survives; "array diffs are
  version-dependent" does not.
- **Delete the line when you fix it.** An entry that outlives its bug describes
  finished work as remaining, which is wrong in the expensive direction.
- **Re-read "deliberately not fixed" entries.** Their reasoning is scoped to the
  evidence available when written, and a moved reference server invalidates it
  silently. One such entry — a `rename` event's field order, filed as "probably a
  6.0 artifact, not replicated" — became simply wrong when the target moved to
  8.x, where the same behaviour is present.
- **Check for duplicates.** Two branches landing similar text produce two copies
  of one entry; grep a distinctive phrase from anything you added.
- Mark the plan's checkboxes and phase counts, and say what a surface was
  measured AT — a count with no version is not reproducible.

## 3. Reconcile the docs with what actually changed

**This is the pass that gets skipped, and the one that misleads the next
session.** Code review catches wrong code; nothing catches a correct comment
that describes the old behaviour. Every instance below is real:

- A comment saying the update-error wrapper was **"deliberately NOT emitted"**,
  written when the reference was 6.0. The retarget to 8.x had already changed the
  code — only the comment lagged, so it read as an instruction to undo a correct
  fix.
- `CLAUDE.md` stating that `mongod` on `PATH` is Homebrew `@6.0`, after the
  default was switched to 8.2.11.
- Two tests asserting `fullDocument` sits immediately after `operationType` —
  never measured, passing for months, and pinning a claim no released server
  makes.

So, for each behaviour you changed, grep for what still describes it:

```bash
grep -rn "<the old value, message, or version>" src/ crates/ tests/ docs/ tasks/ CLAUDE.md
```

Then check the surfaces that go stale silently, because nothing fails when they
do: `CLAUDE.md`'s architecture and tooling claims, `docs/**`, the validation
reports, benchmark numbers (they live in several places — see the
`benchmark-numbers-alignment` memory), and `tools/probes/README.md`'s per-probe
results.

**Do not bulk-rewrite citations you did not re-measure.** A source comment
saying "probed 6.0.16" records *when* something was measured; rewriting it to
"8.x" without probing erases the only signal separating a verified claim from an
assumed one. Update the ones you actually checked, and inventory the rest.

Add a `changelog.d/<slug>.md` fragment per user-visible change — never edit
`docs/changelog.md` directly, and never bump a version in a feature PR.

## 4. Then clean the machine

Follow **`session-cleanup`**: processes (SIGTERM, never SIGKILL, for anything
holding a database), temp stores, the pytest backlog, background waiters, and
the attribution rules for deciding what is yours. Branch and worktree teardown
belongs to the merge that created them — see `batch-worktree` — so *once every
slice has actually landed* there should be nothing of yours left to remove. If
something is still awaiting a merge, its worktree and its temp state are not
debris yet; §5 is where you wait for it, and you will come back through here
afterwards.

**This pass is provisional, and §6 is the one that counts.** Everything you
sweep here can be undone by the next thing you do — a waiter armed in §5, a
suite re-run after a rebase, a probe server started to answer one last question.
Do not report the machine as clean from this pass.

## 5. Wait until nothing of YOURS is in flight

**The session is not closable while your own work is still moving.** Waiting is
part of the task, not an interruption of it: a handover written over a queued CI
run describes a state that does not exist yet, and every number in it is a
prediction rather than a measurement.

Check each of these, and re-check rather than remember:

```bash
gh pr list --state open --author '@me' --json number,headRefName,mergeStateStatus
gh run list --branch <each branch you pushed> --limit 5
git -C <each worktree> status --short          # nothing uncommitted
git worktree list && git ls-remote --heads origin   # nothing awaiting teardown
```

**Every session here pushes as the same GitHub account, so `--author '@me'`
lists other sessions' PRs too.** Match on the branches *you* created in this
conversation; do not wait on, or touch, a PR you did not open.

Then the things that leave no git trace: background commands and monitors you
armed, detached runs (`scripts/detached_run.py status --name <n>`), and any
suite, gauge or probe whose result you have not yet read.

**This gate loops back.** Finishing something here usually re-opens pass 1 (a
merge) and pass 4 (the worktree and temp state that merge frees), and may
re-open passes 2 and 3 if the merge pulled in a conflict. Run them again rather
than assuming the earlier pass still holds — `main` moves under you.

**In flight means it finishes on its own if you wait.** A queued CI run, a suite
at 80%, a PR that needs only a merge, a branch that needs only teardown. For all
of them the instruction is the same: **wait, then finish it.** Re-arm an expired
monitor instead of treating the expiry as an answer, and if the wait is long, say
so and keep waiting — a runner queue that takes forty minutes takes forty
minutes. That is cheaper than the alternative, because an unmerged branch is
invisible to every other session and nothing will pick it up.

**Wedged is neither in flight nor blocked — and it is what this skill has
actually been leaving behind.** A wait finishes on its own only while its
condition can still become true. Once the job it watches has died, been
superseded, or never wrote the string the pattern matches, the waiter sleeps
forever, and the session that armed it ends without noticing, because the thing
that would have told it is the notification that never comes. Three such shells
were found alive on this box on 2026-09-29: one waiting two days for `TOTAL=` in
a gauge log that had long since finished, one waiting two days on
`/tmp/suite11.log`, and one waiting **twenty days** for a string in a task
output belonging to a session that no longer existed. None was in flight; none
appeared in its own session's handover.

So for each wait still outstanding, decide which of three it is before you wait
on it — and decide it by checking **the job, not the waiter**
(`scripts/detached_run.py status --name <n>`, the `.exit` file, `gh run list`):

- the job is still running → **in flight**: wait, then finish it;
- the job is finished or gone, so the condition can never fire → **wedged**:
  kill the waiter and read the result straight from its log or `.exit` file;
- it needs someone who is not you → **blocked**: name it, file it, close over it.

A waiter's own silence is evidence of nothing. Silence is exactly what it
produces when wedged, and what it produces while working.

**Blocked is different, and is the only thing you may close over**: it needs
someone who is not you, or a decision that is the user's. Another session's
branch, a gauge needing credentials you do not have, a question you raised that
they have not answered. Name it, file it, close.

**"I'll note it as left open" is not a substitute for finishing it.** On
2026-09-28 a session ran all four passes, wrote an accurate handover, and
declared the session closed with its own PR open and that PR's only CI run still
QUEUED — so the close was written before the evidence for it existed, and the
branch and worktree the handover named were still on disk waiting for a merge
nobody was watching for any more. Everything in it was true except the word
"closed".

## 6. The process gate: nothing you started is still running

**Run this after §5, and re-run it after anything that starts work.** This is
the gate that stands between a clean close and the twenty-day waiter above.

```bash
# A. Anything holding a session scratchpad store — the catch-all. A server
#    launched from a script carries no recognisable name (a bare `Python
#    launch_sd.py` matches no daemon pattern), but every one of them carries
#    its storage path. The [c]haracter class keeps the gate off its own
#    command line.
ps -Ao pid,etime,command | grep '[c]laude-501/-Users-jdrumgoole-GIT-SecantusDB'

# B. Databases and gauge runners by name, for anything storing elsewhere.
pgrep -fl 'secantusd|python -m secantus|[m]ongod|pytest-xdist|gradle|dotnet test|test-libmongoc|psycopg_validation'

# C. Waiter shells — sleep loops, from any session. Same bracket trick: a
#    bare `grep -E 'sleep|until|while'` matches the pipeline running it.
ps -Ao pid,ppid,etime,command | grep '[s]hell-snapshots' | grep -E '[s]leep |[u]ntil |[w]hile '

# D. Detached runs with no exit file — still running, or died un-reaped.
for j in .detached-runs/*.json; do [ -f "${j%.json}.exit" ] || echo "ACTIVE: $j"; done
```

Every line printed either goes away or gets named in the handover. Nothing may
be left merely observed.

**Attribution is the session id in the storage path.** Your scratchpad path
contains this conversation's uuid; a different uuid is a different session,
whose processes you report and do not touch. Before calling any of them
orphaned, check whether that session is still alive — `pgrep -fl 'claude
--resume <uuid>'`. On 2026-09-29 three servers that looked abandoned (a
`mongod`, a `secantusd-rs` and a Python server, on ports 27100–27102) turned out
to belong to a session that was still running and still using them. Age alone
does not settle it; a live owner does.

Kill order is `session-cleanup`'s: **SIGTERM** for anything holding a database,
never SIGKILL; xdist workers by proctitle; a waiter shell takes a plain `kill`.
Then **re-run the gate and confirm each pid is gone** — an unverified kill is a
claim, not a measurement. Harness tasks that owned a waiter you killed will
report `exit code 144` (128 + SIGTERM); that is the kill landing, not a new
failure.

## The handover

Close with what the repo now says, each line backed by a command you just ran:

- what landed (PR numbers, and the verification behind each — gate counts, gauge
  numbers, differential results);
- what is left, and where it is filed;
- anything you deliberately did **not** do, and why — a leftover reported is
  finished work, a leftover unmentioned is a trap;
- anything that outlives the session and would surprise someone: a changed
  default, a machine-wide install (an 8.2.11 `mongod` was added under its own
  prefix on 2026-08-31), a still-running process that belongs to someone else —
  **by pid, with the evidence for whose it is, copied from §6's output.** A
  survivor you did not name is one the next session has to re-derive from
  scratch, and the twenty-day waiter is what that costs.
  **Write the version you measured, not the one you expected** — the `mongod`
  line in `CLAUDE.md` was wrong for a day because a session recorded the
  intended version rather than `mongod --version`'s answer.

And state corrections plainly. If a number you reported earlier was measured
against your own bug rather than a baseline, say so — a pass rate that "improved
from 99.0% to 99.5%" when the real baseline was always 99.5% is a flattering
description of fixing your own regression.

**End by saying the session is closed, in those words.** Not "that should be
everything" or "let me know if you want anything else" — a definite statement
that the session is closed, placed after the handover so it is the last thing
read. Trailing off leaves the reader unsure whether the passes finished or merely
stopped, and an ambiguous ending has had someone re-run cleanup that was already
done.

**Say it only once §5 and §6 are both satisfied**, with §6 re-run after the
last thing you did. If something is genuinely BLOCKED — on
another session, on a decision that is the user's — close on that and name it:
"closed, with X blocked on Y and filed at Z" is an ending. But something merely
*unfinished* is not something to close over, however well you describe it: go
back to §5 and finish it. Silence is not an ending either.
