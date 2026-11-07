### The MongoDB gate rebuilds a lane's PostgreSQL server binary too

`scripts/gate_rm.py` rebuilt the two MongoDB server artifacts from HEAD, but not a `secantusd-pg` left in the worktree by an earlier `gate_pg.sh --storage` run. The test suite's provenance check refuses to run while any artifact it knows is stale. So after a rebase, every pytest step of the gate stopped before running a test. The gate now rebuilds `secantusd-pg` whenever the worktree has one.

#### Fixed

- `scripts/gate_rm.py`: rebuilds `secantusd-pg` when the worktree holds one, so the provenance check does not abort the gate's test steps.
