### `scripts/land_batch.sh` lands an integration branch in snapshot PRs

With three fix-one lanes pushing to `pg-batch`, a PR straight from that branch had its CI cancelled by every push. `scripts/land_batch.sh <branch> [batch-pid]` instead freezes the branch on `<branch>-ci` and opens a PR from that. It waits for CI on that fixed commit and merges with a merge commit, pinned to the commit CI passed. It repeats until the batch has exited and nothing is left to land.

It judges a PR by that PR's own checks. If another run of the same commit failed, it stops with exit 4, so an intermittent failure is read before anything merges. This is how RP-303's ninth sighting was found, on 2026-11-06.

#### Changed

- `scripts/fix_many.sh -i` no longer opens a PR at the end. `land_batch.sh` does the landing.
