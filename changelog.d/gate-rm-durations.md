### The MongoDB gate shows where a slow run spent its time

`scripts/gate_rm.py` prints the one-minute load average at the start and end of each step beside its time, and its `ci-selected` step logs its slowest tests. The same `ci-selected` tests have taken about 100 seconds in some runs and 400 to 850 seconds in others (2026-11-08/09), while CI's `test` job, which runs them on a quiet machine, held at about two minutes. This instruments that gap rather than guessing at it.

#### Changed

- `scripts/gate_rm.py`: step lines read `name=rc (Ns) load A->B`; `ci-selected` runs with `--durations=25 --durations-min=5` (`tests/test_gate_rm.py`).
