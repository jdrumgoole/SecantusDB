### Unattended fix-one sessions are granted their worktree, and MongoDB fixes gate the way CI does

Headless fix-one sessions could not do any work on the MongoDB backlog. A `claude -p` session started in the main checkout was refused the worktree it had just created, then exited successfully having changed nothing, so the batch carried on. `scripts/fix_one.sh` now always gives a headless session a lane worktree, grants it with `--add-dir`, and runs the session in auto permission mode. `scripts/fix_many.sh` stops the batch when a session reports success without landing anything. A MongoDB fix now passes `scripts/gate_rm.py` before it lands. That gate runs the checks CI runs for the Rust MongoDB server, rather than the full test suite, which is mostly the deprecated Python servers.

#### Added

- `scripts/gate_rm.py`: the pre-landing gate for a Rust MongoDB server change. It covers the cargo lanes, both server builds, the `test` job's selection minus the files CI deliberately does not run, the `storage-engine` job's named files (read from `test.yml`), the concurrency invariants, the backlog and `tests/mongod_live/` against mongod 8.2, lint and ruff.
- `scripts/fix_many.sh -l FILE`: work a list of backlog IDs picked in advance, one per session, in order.

#### Changed

- `scripts/fix_one.sh`: `-p` runs with `--permission-mode auto --add-dir <lane>`. Without a `lane`, it makes a one-off worktree and removes it afterwards unless it holds work.
- `scripts/fix_many.sh`: in integration mode a session must report `landed on <branch> at <sha>`, and that sha must be on the branch, or the batch stops. Lane worktrees are named for their integration branch (`../SecantusDB-lane-<branch>-<k>`), so a PG batch and a MongoDB batch never share one.
- `scripts/land_batch.sh`: an `rm*` branch's snapshot PR is titled for the Rust MongoDB server and names `gate_rm.py`.
- Fix-one skill: RM items gate with `scripts/gate_rm.py` instead of `rust-gate` plus the full suite.
