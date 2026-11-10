### The batch runner prunes uv's cache between items

`scripts/fix_many.sh` now runs `uv cache prune` before each item, alongside its cargo incremental-cache prune and before its free-disk check. uv's cache keeps every wheel that each lane's `./inv sync` builds. It grew 13 to 26 GB a day under a two-lane batch, and twice took the disk below the batch's 40 GB floor on 2026-11-09 and 2026-11-10.

#### Changed

- `scripts/fix_many.sh`: `uv cache prune` before each item (`tests/test_batch_scripts_exit.py`).
