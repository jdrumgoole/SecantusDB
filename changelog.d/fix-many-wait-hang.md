### The batch runner exits when its lanes finish

`scripts/fix_many.sh` keeps the Mac awake with a `caffeinate` child that lives until the script exits, and then waited for every child with a bare `wait` -- including that one, so a batch whose lanes had all finished never reached its end, and the lander that waits for it never stopped either. It now waits for its lanes by process id.

#### Fixed

- `scripts/fix_many.sh` no longer hangs after its last item (`tests/test_batch_scripts_exit.py`).
