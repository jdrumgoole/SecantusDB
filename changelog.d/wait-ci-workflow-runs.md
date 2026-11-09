### A CI run whose jobs never started no longer reads green

`scripts/wait_ci.sh`, which the batch lander trusts before it merges a snapshot, judged a pull request by its check runs. A job that never starts leaves no check run. During a GitHub Actions incident on 2026-11-09, snapshot PR #2563's `Tests` run created only three of its eight jobs and concluded `failure`, and the PR was merged on "all 6 checks passed". The script now also waits for every workflow run of the PR's branch on that commit and requires each to pass. The lander stops on such a run instead of merging it. (Nothing untested reached `main` that time: the snapshot's previous run had passed the missing jobs, and the one new commit changed a test that ran.)

#### Fixed

- `scripts/wait_ci.sh`: a failed workflow run is RED even when every check run that exists passed (`tests/test_wait_ci.py`).
- `scripts/land_batch.sh`: its stop message names that case.
