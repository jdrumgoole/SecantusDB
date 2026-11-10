### The batch runner skips an item another fix already closed

When `scripts/fix_many.sh` works from an ID list, it now checks that each ID still has its `Backlog-ID:` line on the branch the fixes land on before starting a session for it. An item that another lane's fix closed in passing, as RM-403's closed RM-397 and RM-408's closed RM-396 on 2026-11-10, used to get a session that found nothing to do and landed nothing, and that stopped the batch. Such an item is now logged as skipped.

#### Fixed

- `scripts/fix_many.sh`: a listed ID that is already closed is skipped, not run (`tests/test_batch_scripts_exit.py`).
