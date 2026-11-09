### A fix-one batch keeps the Mac from sleeping, not only from idling

The previous change held `caffeinate -i`, which blocks idle sleep but did not keep an already sleeping Mac awake. In each short dark wake the Mac went "back to sleep" again, still pausing every batch process for 15 minutes at a time. `scripts/fix_many.sh` and `scripts/land_batch.sh` now hold `caffeinate -i -s`. The `-s` assertion prevents system sleep, which macOS honours on AC power.

#### Fixed

- `scripts/fix_many.sh`, `scripts/land_batch.sh`: `caffeinate -i -s -w $$` in place of `-i` alone.
