### Fix-one sessions spend less time in the local gate

A fix-one session took 50–150 minutes, and 70–85% of that was `scripts/gate_pg.sh`, run one to three times (measured over four sessions on 2026-11-06). This change cuts that time.

#### Changed

- `scripts/gate_pg.sh` runs the cheap checks first: fmt, lint and ruff. If one of them fails, the gate stops after seconds instead of 40 minutes.
- `scripts/gate_pg.sh --only <steps>` reruns just the named steps. It prints every step's result and flags any from an older commit.
- `scripts/gate_pg.sh --fast` skips `pg-suites` (20–27 min). Use it only for fixes landing on an integration branch, whose PR runs the same suites in CI's pg-oracle job. The skip is printed in the log.
- `--list` prints the steps a run would take.
- `scripts/fix_many.sh -i` gives each lane one worktree, `../SecantusDB-lane<k>`, reused from item to item so the cargo target and venv stay warm.
- Lanes now take items from a shared counter, so `-j 3` works.
- The fix-one skill gains a lane mode. Integration mode uses the `--fast` gate. After rebasing onto the integration branch, a session regates only on a conflict or when the incoming commits touch the same files.
