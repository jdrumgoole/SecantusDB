"""Paired A/B sampler for concurrency-race hunts (CI diagnostic, kept for reuse).

Runs the two racing tests N times each with an env toggle ON and OFF,
alternating, on the same runner — the design that controls for the
runner-health waves that confound sequential A/B rounds. Prints a tally and
always exits 0 (the signal is the counts, not a red lane).

History: this sampler settled the 2026-08 lost-update hunt twice. First it
exonerated the pipeline implicit txn as a *cause* (0/48 vs 0/48 on healthy
runners). Then, re-pointed at the straddle fix (a temporary
``SECANTUS_STRADDLE_TXN=0`` gate re-opening the race), it caught the loss in
vivo — a Windows lane scored fix-OFF 1/6 (n=39, the mixed-mode signature)
while fix-ON on the same runner stayed 0/6, with 0/168 fix-ON losses across
all lanes. To reuse: set ``RACE_SAMPLER_ENV`` to the toggle variable, wire a
temporary gate for it, and add a temporary ``if: always()`` workflow step
after pytest (uv must be installed by then) running this script.
"""

from __future__ import annotations

import os
import subprocess
import sys

N = int(os.environ.get("RACE_SAMPLER_ROUNDS", "6"))
TOGGLE = os.environ.get("RACE_SAMPLER_ENV", "SECANTUS_PIPELINE_TXN")
TESTS = [
    "tests/test_pgserver_concurrency.py::test_dual_protocol_txn_vs_autocommit_stall_is_bounded",
    "tests/test_pgserver_concurrency.py::test_autocommit_computed_updates_lose_no_increments",
]
tally = {"1": [0, 0], "0": [0, 0]}  # mode -> [losses, runs]
for i in range(N):
    for mode in ("1", "0"):
        env = {**os.environ, TOGGLE: mode}
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
    f"PAIR-SAMPLER TALLY: {TOGGLE}=1 losses {tally['1'][0]}/{tally['1'][1]}, "
    f"{TOGGLE}=0 losses {tally['0'][0]}/{tally['0'][1]}"
)
