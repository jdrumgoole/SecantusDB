"""Baseline-aware verdict for the pgjdbc gauge lane.

The weekly lane used to return gradle's raw exit code, and gradle exits
non-zero while ANY test fails — so until the documented standing failures
reach zero the lane was red by construction and its conclusion carried no
signal. The lane now compares the run's failure set against a committed
baseline (`pgjdbc_validation/baseline.json`) and fails only on REGRESSION:

* a (class, test-name) failing that the baseline doesn't list, or
* a listed (class, test-name) failing MORE times than the baseline records
  (parameterized classes repeat bare names — BatchFailureTest's ``run()``
  fails tens of times — so ids alone can't see a partial regression).

A run with fewer failures than baseline stays green and prints the
newly-passing entries as a prompt to tighten the baseline
(``python -m pgjdbc_validation.baseline --update`` regenerates it from the
latest raw artifacts; commit the diff in the PR that fixed the tests).
"""

from __future__ import annotations

import argparse
import datetime as _dt
import json
import sys
from collections import Counter
from pathlib import Path

BASELINE_PATH = Path(__file__).with_name("baseline.json")


def _failure_counts(raw: dict) -> Counter[str]:
    """``"<class>::<test>" -> failure count`` for one aggregated raw blob."""
    out: Counter[str] = Counter()
    for entry in raw.get("classes", []):
        cls = entry.get("class", "?")
        for name in entry.get("failed_tests", []):
            out[f"{cls}::{name}"] += 1
    return out


def _classes_in(raw: dict) -> set[str]:
    return {entry.get("class", "?") for entry in raw.get("classes", [])}


def load_baseline(path: Path | None = None) -> Counter[str]:
    data = json.loads((path or BASELINE_PATH).read_text())
    return Counter({k: int(v) for k, v in data.get("failures", {}).items()})


def compare(
    raw: dict, baseline: Counter[str]
) -> tuple[dict[str, tuple[int, int]], dict[str, tuple[int, int]]]:
    """``(regressions, improvements)`` for one run against the baseline.

    Each maps ``"<class>::<test>"`` to ``(run_count, baseline_count)``.
    Only classes PRESENT in this run are judged — a sharded run sees a
    subset of the suite, and absent classes say nothing either way.
    """
    counts = _failure_counts(raw)
    present = _classes_in(raw)
    regressions: dict[str, tuple[int, int]] = {}
    improvements: dict[str, tuple[int, int]] = {}
    for key, n in counts.items():
        base = baseline.get(key, 0)
        if n > base:
            regressions[key] = (n, base)
    for key, base in baseline.items():
        cls = key.split("::", 1)[0]
        if cls not in present:
            continue
        n = counts.get(key, 0)
        if n < base:
            improvements[key] = (n, base)
    return regressions, improvements


def verdict(raw_path: Path, *, quiet: bool = False) -> int:
    """Lane exit code for one aggregated raw file: 0 unless a regression.

    A missing/unreadable baseline fails loudly (2) — a lane that can't
    tell regression from baseline must not report green.
    """
    try:
        baseline = load_baseline()
    except (OSError, ValueError) as e:
        print(f"pgjdbc baseline unreadable ({e}) — cannot judge the run", file=sys.stderr)
        return 2
    raw = json.loads(raw_path.read_text())
    if raw.get("truncated"):
        print("pgjdbc raw is truncated — not a measurement, refusing a verdict", file=sys.stderr)
        return 124
    regressions, improvements = compare(raw, baseline)
    total_failed = sum(_failure_counts(raw).values())
    if regressions:
        plural = "y" if len(regressions) == 1 else "ies"
        print(f"pgjdbc REGRESSION vs baseline ({len(regressions)} entr{plural}):")
        for key, (n, base) in sorted(regressions.items()):
            print(f"  {key}: {n} failing (baseline {base})")
        return 1
    if not quiet:
        print(
            f"pgjdbc: no regression vs baseline "
            f"({total_failed} standing failure(s) in this run's classes)."
        )
        if improvements:
            print(
                f"  {len(improvements)} baseline entr{'y' if len(improvements) == 1 else 'ies'} "
                "improved — tighten with `python -m pgjdbc_validation.baseline --update`:"
            )
            for key, (n, base) in sorted(improvements.items()):
                print(f"    {key}: {n} failing (baseline {base})")
    return 0


def update_baseline(raw_paths: list[Path], out: Path = BASELINE_PATH) -> Counter[str]:
    """Regenerate the baseline from a set of raw artifacts (the union of a
    full run's shards). Refuses truncated raws."""
    merged: Counter[str] = Counter()
    sources = []
    for p in raw_paths:
        raw = json.loads(p.read_text())
        if raw.get("truncated"):
            raise SystemExit(f"{p} is truncated — refusing to bake a partial baseline")
        merged.update(_failure_counts(raw))
        sources.append(p.name)
    out.write_text(
        json.dumps(
            {
                "generated": _dt.date.today().isoformat(),
                "sources": sources,
                "total": sum(merged.values()),
                "failures": dict(sorted(merged.items())),
            },
            indent=1,
        )
        + "\n"
    )
    return merged


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument(
        "--update",
        nargs="+",
        metavar="RAW_JSON",
        help="regenerate baseline.json from raw artifacts",
    )
    ap.add_argument("--check", metavar="RAW_JSON", help="verdict for one raw artifact")
    args = ap.parse_args()
    if args.update:
        merged = update_baseline([Path(p) for p in args.update])
        print(
            f"baseline.json: {sum(merged.values())} standing failures across {len(merged)} entries"
        )
        return 0
    if args.check:
        return verdict(Path(args.check))
    ap.print_help()
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
