"""Paired A/B sampler for the lost-update hunt (CI diagnostic, temporary).

Runs the two racing tests N times each with the straddle FIX ON and OFF
(``SECANTUS_STRADDLE_TXN``), alternating, on the same runner — the design
that controls for the runner-health waves that confounded every sequential
A/B round. The pipeline implicit txn is ON for both sides (the race needs
it). Fix-OFF catching losses while fix-ON stays clean is the in-vivo
confirmation that the deterministically-proven mechanism is the one the CI
lanes were hitting. Prints a tally and always exits 0 (the signal is the
counts, not a red lane).
"""

from __future__ import annotations

import os
import subprocess
import sys

N = int(os.environ.get("RACE_SAMPLER_ROUNDS", "6"))
TESTS = [
    "tests/test_pgserver_concurrency.py::test_dual_protocol_txn_vs_autocommit_stall_is_bounded",
    "tests/test_pgserver_concurrency.py::test_autocommit_computed_updates_lose_no_increments",
]
tally = {"1": [0, 0], "0": [0, 0]}  # mode -> [losses, runs]
for i in range(N):
    for mode in ("1", "0"):
        env = {**os.environ, "SECANTUS_PIPELINE_TXN": "1", "SECANTUS_STRADDLE_TXN": mode}
        r = subprocess.run(
            [sys.executable, "-m", "pytest", "-n0", "-q", "-p", "no:cacheprovider", *TESTS],
            env=env,
            capture_output=True,
            text=True,
            timeout=600,
        )
        tally[mode][1] += 1
        if r.returncode != 0:
            tally[mode][0] += 1
            for line in r.stdout.splitlines():
                if "lost increments" in line:
                    print(f"[mode={mode} round={i}] {line.strip()[:300]}")
print(
    f"PAIR-SAMPLER TALLY: fix-ON losses {tally['1'][0]}/{tally['1'][1]}, "
    f"fix-OFF losses {tally['0'][0]}/{tally['0'][1]}"
)
