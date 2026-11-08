### A fix-one lane's build output is capped at a size, not only pruned

Clearing a lane's incremental caches before each item was not enough. cargo also keeps every superseded compiled dependency, so a reused lane still grew with each item: one reached 43 GB after about eight, and the batch was near its free-space floor. Before an item, `scripts/fix_many.sh` now checks the lane's size. Past `SECANTUS_LANE_MAX_GB` (default 35) it wipes the lane's cargo target directories, keeping its `.venv` and WiredTiger build, and records that in the batch summary. The next build is cold, about 10–15 minutes, instead of the batch stopping.

#### Changed

- `scripts/fix_many.sh`: a per-lane size cap that wipes the lane's cargo targets when it is exceeded.
