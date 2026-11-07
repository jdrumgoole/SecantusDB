### A fix-one batch keeps its lanes' build caches in check and stops before the disk fills

Each fix-one lane reuses one worktree for every item it works, and cargo never removes a build's incremental state. After about six items each, the two lanes of a MongoDB batch held 41 GB and 50 GB, 36 GB of it incremental caches, and the disk filled during a gate. `scripts/fix_many.sh` now clears a lane's incremental caches before each item. The compiled dependencies stay, so the next build is not cold. The batch also stops, recording why, instead of starting an item while free space is under `SECANTUS_MIN_FREE_GB` (default 40).

#### Fixed

- `scripts/fix_many.sh`: clears each lane's `target/*/incremental` before an item, and stops the batch below a free-space floor.
