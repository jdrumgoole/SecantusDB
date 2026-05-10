"""Phase 3.1 gate: pure C + pthread vs Python + SWIG, side-by-side scaling.

Runs ``wt_pthread_bench`` (pure C, calls libwiredtiger directly) and
``wt_swig_bench.py`` (Python threads driving the SWIG bindings) against
a fresh WT home directory each, at N=1,2,4,8 threads. Each thread
writes COUNT rows to its own table. Same workload, same WT config.

The single-thread baselines should be similar; the gap at N=2/4/8 tells
us what a Cython rebind would unlock. If the C path scales linearly
while the Python path flatlines, the GIL bottleneck is in WT's SWIG
wrapper. If both paths flatline, the bottleneck is below — WT's own C
locks — and the rebind plan should be parked.
"""

from __future__ import annotations

import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
PTHREAD_BIN = HERE / "wt_pthread_bench"

# Matches the single-line summary both backends print:
# threads=N count=K total=T elapsed=E rate=R errors=X
_LINE_RE = re.compile(
    r"threads=(?P<threads>\d+)\s+count=(?P<count>\d+)\s+total=(?P<total>\d+)\s+"
    r"elapsed=(?P<elapsed>[\d.]+)\s+rate=(?P<rate>[\d.]+)\s+errors=(?P<errors>\d+)"
)


def _parse(text: str) -> dict[str, float] | None:
    m = _LINE_RE.search(text)
    if not m:
        return None
    return {
        "threads": int(m["threads"]),
        "elapsed": float(m["elapsed"]),
        "rate": float(m["rate"]),
        "errors": int(m["errors"]),
    }


def run_pthread(home: Path, n: int, count: int) -> dict[str, float]:
    p = subprocess.run(
        [str(PTHREAD_BIN), str(home), str(n), str(count)],
        capture_output=True, text=True, timeout=600,
    )
    if p.returncode != 0:
        raise RuntimeError(f"pthread bench failed: {p.stderr}")
    parsed = _parse(p.stdout)
    if parsed is None:
        raise RuntimeError(f"could not parse pthread output: {p.stdout!r}")
    return parsed


def run_swig(home: Path, n: int, count: int) -> dict[str, float]:
    p = subprocess.run(
        [sys.executable, "-m", "bench.wt_poc.wt_swig_bench",
         str(home), str(n), str(count)],
        capture_output=True, text=True, timeout=600,
        cwd=str(REPO),
    )
    if p.returncode != 0:
        raise RuntimeError(f"swig bench failed: {p.stderr}")
    parsed = _parse(p.stdout)
    if parsed is None:
        raise RuntimeError(f"could not parse swig output: {p.stdout!r}")
    return parsed


def fresh_home() -> Path:
    return Path(tempfile.mkdtemp(prefix="wt-poc-"))


def main() -> int:
    counts = {1: 50_000, 2: 25_000, 4: 12_500, 8: 6_250}
    # Each thread writes COUNT rows. We scale COUNT down with thread
    # count so each *cell* writes the same total volume (50,000 rows
    # total for every N) — apples-to-apples scaling read.

    results: list[tuple[str, int, dict[str, float]]] = []

    for backend in ("pthread", "swig"):
        for n in (1, 2, 4, 8):
            home = fresh_home()
            try:
                count = counts[n]
                if backend == "pthread":
                    r = run_pthread(home, n, count)
                else:
                    r = run_swig(home, n, count)
                results.append((backend, n, r))
                print(
                    f"  {backend:<8} N={n}  "
                    f"elapsed={r['elapsed']:6.2f}s  "
                    f"rate={r['rate']:>10,.0f} rows/s",
                    flush=True,
                )
            finally:
                shutil.rmtree(home, ignore_errors=True)

    # Compute scaling ratios.
    pthread_baseline = next(r["rate"] for b, n, r in results if b == "pthread" and n == 1)
    swig_baseline = next(r["rate"] for b, n, r in results if b == "swig" and n == 1)

    print()
    print("=" * 78)
    print(f"{'N':<4} {'pthread (rows/s)':>18} {'pthread scale':>16} "
          f"{'swig (rows/s)':>18} {'swig scale':>16}")
    print("-" * 78)
    for n in (1, 2, 4, 8):
        pth = next(r for b, nn, r in results if b == "pthread" and nn == n)
        swi = next(r for b, nn, r in results if b == "swig" and nn == n)
        print(
            f"{n:<4} "
            f"{pth['rate']:>18,.0f} "
            f"{pth['rate'] / pthread_baseline:>15.2f}x "
            f"{swi['rate']:>18,.0f} "
            f"{swi['rate'] / swig_baseline:>15.2f}x"
        )
    print("=" * 78)
    print(
        "\nGate: if pthread scales > 1.5x at N=2 and > 2.5x at N=4 while swig "
        "stays near 1.0x, the\n      Cython rebind plan is justified. If "
        "pthread also flatlines, WT itself is the\n      ceiling and the "
        "rebind won't deliver."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
