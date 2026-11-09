### A fix-one batch keeps the Mac awake while it runs

An idle Mac on AC power falls into "Maintenance Sleep" for 10 to 20 minutes at a time, with short dark wakes in between, and every process stops while it sleeps. During an overnight MongoDB batch, the batch lander's five-minute poll came back after 63 minutes, and the fix sessions beside it stalled in the same way. A finished fix waited an hour for its PR. `scripts/fix_many.sh` and `scripts/land_batch.sh` now hold a no-idle-sleep assertion (`caffeinate -i`) for as long as they run, and release it when they exit.

#### Fixed

- `scripts/fix_many.sh`, `scripts/land_batch.sh`: hold `caffeinate -i -w $$` while running. Fixes RM-443, a watch item; its cause was found in `pmset -g log`.
