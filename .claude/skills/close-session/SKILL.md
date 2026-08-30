---
name: close-session
description: Close out a SecantusDB working session — land every outstanding change, record what is left with measurements, tear down branches and worktrees, and reconcile the documentation with what actually changed. Fires on "close the session", "wrap up", "finish up", "we're done", "end of session", or when the user stops directing new work. Sequences the per-slice landing (batch-worktree) and the machine cleanup (session-cleanup) and adds the part neither covers: finding the docs, comments and tests that still describe the behaviour you just changed.
---

# Closing a session is a task, not a farewell

A session ends well when someone arriving cold can tell what happened, what is
true now, and what is left — from the repo alone, without this conversation.
That takes four passes, in this order, because each depends on the one before.

Two sibling skills own parts of it and are not repeated here:

- **`batch-worktree`** — landing one branch: claim, commit, PR, watch CI, merge,
  tear down. The traps that make teardown fail live there.
- **`session-cleanup`** — machine residue: daemons, WiredTiger temp stores, the
  `pytest-of-<user>` backlog, wedged background waiters, and whose debris is
  whose.

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
belongs to the merge that created them — see `batch-worktree` — so by this point
there should be nothing of yours left to remove.

## The handover

Close with what the repo now says, each line backed by a command you just ran:

- what landed (PR numbers, and the verification behind each — gate counts, gauge
  numbers, differential results);
- what is left, and where it is filed;
- anything you deliberately did **not** do, and why — a leftover reported is
  finished work, a leftover unmentioned is a trap;
- anything that outlives the session and would surprise someone: a changed
  default (`mongod` is now 8.2.11 here), a machine-wide install, a still-running
  process that belongs to someone else.

And state corrections plainly. If a number you reported earlier was measured
against your own bug rather than a baseline, say so — a pass rate that "improved
from 99.0% to 99.5%" when the real baseline was always 99.5% is a flattering
description of fixing your own regression.
