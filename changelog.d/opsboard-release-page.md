### Ops Board: confirm-gated release page and external-process discovery

The Ops Board gains a **Release** page. Because a release is irreversible and
outward-facing — `release-prepare` pushes a tag that triggers publication — the
page leads with a readiness checklist rather than a button: are you on `main`, is
the tree clean, is it in sync with `origin`, is there a changelog fragment, and
is recent CI on `main` green. Blocking failures stop the release outright, and
the policy is deliberately fail-safe: a check that cannot be verified blocks just
as a failing one does, because "we couldn't tell" is not a good reason to publish.

Starting a release then requires typing the exact version as confirmation — not
a checkbox and not the word "yes" — and the board only ever runs the project's
own sanctioned `invoke` tasks; it never invents release mechanics of its own.

The Jobs page also now lists **external processes**: build and test tooling
running on this machine that wasn't started through `./inv`. These are shown
honestly as command and elapsed time only — the board didn't spawn them, so there
is no log to attach to. Anything started via `./inv` remains fully tracked with a
live log.

#### Added

- `/release` page: readiness checklist (fail-safe — unknown blocks), version +
  typed-confirmation gate, explicit override for blocking checks.
- `secantus.opsboard.readiness`: local git/changelog checks with an advisory CI
  check that never blocks.
- `secantus.opsboard.discovery`: Tier-3 process-table scan for untracked build
  processes, filtered against journal-tracked pids.
