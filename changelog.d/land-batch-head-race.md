### land_batch waits for the commit it pushed

#### Fixed

- `scripts/land_batch.sh` force-pushes a snapshot to its CI branch and then
  waits on the PR's checks. GitHub moves a PR's head a few seconds after a
  push, so `scripts/wait_ci.sh` could read the PREVIOUS commit's finished
  checks as the new one's. On PR #2441 (2026-11-07) it reported d373904b's
  red run against ed796664, whose run had not started. It stopped without
  merging, but for the wrong reason. `wait_ci.sh` now takes the
  pushed sha and waits up to 10 minutes for the PR's head to reach it, and
  `land_batch.sh` passes it.
- When the checks on a commit are red, `land_batch.sh` asks whether this
  PR's own checks passed, in case the red run belongs to another PR. An
  empty list of checks counted as "all passed". It now counts as red.
