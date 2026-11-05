### `fix_many.sh` stops at the first failed session

An expired login failed all 23 sessions of a batch in under a minute on
2026-11-05. The first session that exits non-zero now stops the batch: no new
session starts, one already running in the other lane finishes, and
`summary.txt` names the log to read.

#### Changed

- `scripts/fix_many.sh`: writes `.scratch/fix_many/STOP` on a non-zero exit;
  every later item checks it and skips.
