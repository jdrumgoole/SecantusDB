### `wait_ci.sh` stops at once on a conflicting PR

A PR that conflicts with `main` gets no CI run, so `scripts/wait_ci.sh`
used to wait its full two hours for checks that could not start (a fix-one
session sat 70 minutes on #2308). It now exits 3 with a "rebase" message.

#### Changed

- `scripts/wait_ci.sh`: exit 3 when the PR's `mergeable_state` is `dirty`
  and no checks exist; the `fix-one` skill says to rebase and wait again.
