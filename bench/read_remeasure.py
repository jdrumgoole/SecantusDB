"""Read and mixed-workload arm of the Rust-vs-mongod parity re-measure.

`bench/parity_remeasure.py` settled the write question: the gap is ~2.4x at one
writer widening to ~3.7x at eight, a contention shape rather than the flat per-op
wall the retracted 2026-07-22 VERDICT claimed. That was insert-only, so "the gap
is contention-shaped" is so far a statement about writes alone. This measures the
other half.

Two quantities, per arm:

* **read throughput, uncontended** — find-batches/s with readers alone. This is
  the closest thing to a pure per-operation read comparison.
* **read retention under write load** — the same readers while W writers hammer
  inserts, as a percentage of uncontended. A server whose reads serialise behind
  writes bleeds here; one where reads take no write lock holds up. Retention is
  the interesting number precisely because it is a *ratio*, so it is far less
  sensitive to absolute machine speed than the throughput figures.

Reuses the box-hygiene, pinning and arm-pinning machinery from parity_remeasure
rather than reimplementing it — same refusal to run on a loaded box or an
attached branch, same per-arm mongod binary recording.
"""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

from bench.parity_remeasure import (
    ARMS,
    REPO,
    check_box,
    head_sha,
    is_detached,
    run,
)

# `read_concurrency` prints; it has no --json. Parse its three reported figures.
ALONE_RE = re.compile(r"readers alone:\s+([\d.]+)\s+find-batches/s")
CONTENDED_RE = re.compile(r"readers \+ \d+ writers:\s+([\d.]+)\s+find-batches/s")
RETAINED_RE = re.compile(r"retained under write load:\s+([\d.]+)%")


def parse(text: str) -> dict | None:
    alone, contended, retained = (
        ALONE_RE.search(text),
        CONTENDED_RE.search(text),
        RETAINED_RE.search(text),
    )
    if not (alone and contended and retained):
        return None
    return {
        "alone_batches_per_s": float(alone.group(1)),
        "contended_batches_per_s": float(contended.group(1)),
        "retained_pct": float(retained.group(1)),
    }


def median(xs: list[float]) -> float:
    s = sorted(xs)
    n = len(s)
    return s[n // 2] if n % 2 else (s[n // 2 - 1] + s[n // 2]) / 2


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--out", type=Path, default=REPO / "bench" / "results" / "read-remeasure")
    ap.add_argument("--arms", default="rust,mongod6,mongod8")
    ap.add_argument("--readers", type=int, default=4)
    ap.add_argument("--writers", type=int, default=8, help="write load for the contended phase")
    ap.add_argument("--duration", type=float, default=20.0)
    ap.add_argument("--seed-docs", type=int, default=20000)
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--force", action="store_true")
    args = ap.parse_args()

    if not is_detached():
        raise SystemExit("refusing to measure on an attached branch — use a pinned worktree")
    binary = REPO / "crates" / "secantusdb" / "target" / "release" / "secantusd-rs"
    if not binary.exists():
        raise SystemExit(f"Rust binary not built at {binary}")
    if not shutil.which("mongod"):
        raise SystemExit("mongod not on PATH")

    args.out.mkdir(parents=True, exist_ok=True)
    sha_before = head_sha()
    print(f"pinned at : {sha_before}")
    print(
        f"workload  : {args.readers} readers, contended by {args.writers} writers, "
        f"{args.duration}s x {args.reps} reps, {args.seed_docs} seed docs\n"
    )

    checks = [check_box("before", strict=not args.force)]
    started = time.time()
    results: dict[str, dict] = {}
    rc_all = 0

    # Interleave reps across arms (rep 0 for every arm, then rep 1, ...) so a slow
    # thermal drift over the run hits every arm equally instead of penalising
    # whichever happened to be measured last.
    per_arm_reps: dict[str, list[dict]] = {a: [] for a in args.arms.split(",") if a}
    for rep in range(args.reps):
        for label in list(per_arm_reps):
            server, mongod_bin = ARMS.get(label, (label, None))
            env_extra = {}
            if mongod_bin:
                if not Path(mongod_bin).exists():
                    print(f"    skip {label}: {mongod_bin} missing")
                    continue
                env_extra["SECANTUS_MONGOD_BIN"] = mongod_bin
            rc, text = run(
                [
                    sys.executable,
                    "-m",
                    "bench.read_concurrency",
                    "--server",
                    server,
                    "--readers",
                    str(args.readers),
                    "--writers",
                    str(args.writers),
                    "--duration",
                    str(args.duration),
                    "--seed-docs",
                    str(args.seed_docs),
                ],
                args.out / f"read-{label}-rep{rep}.log",
                env_extra=env_extra,
            )
            rc_all = rc_all or rc
            parsed = parse(text)
            if parsed is None:
                print(f"    {label} rep{rep}: UNPARSEABLE (rc={rc})")
                continue
            parsed["rep"] = rep
            per_arm_reps[label].append(parsed)
            print(
                f"    {label:<8} rep{rep}  alone={parsed['alone_batches_per_s']:>9,.1f}/s  "
                f"contended={parsed['contended_batches_per_s']:>9,.1f}/s  "
                f"retained={parsed['retained_pct']:>5.1f}%"
            )

    for label, reps in per_arm_reps.items():
        if not reps:
            results[label] = {"error": "no parseable reps"}
            continue
        server, mongod_bin = ARMS.get(label, (label, None))
        version = None
        if mongod_bin:
            version = subprocess.run(
                [mongod_bin, "--version"], capture_output=True, text=True
            ).stdout.splitlines()[0]
        results[label] = {
            "binary": mongod_bin,
            "version": version,
            "reps": reps,
            "median_alone": median([r["alone_batches_per_s"] for r in reps]),
            "median_contended": median([r["contended_batches_per_s"] for r in reps]),
            "median_retained_pct": median([r["retained_pct"] for r in reps]),
        }

    checks.append(check_box("after", strict=False, settle_s=180.0))
    sha_after = head_sha()

    artifact = {
        "captured": datetime.now(timezone.utc).isoformat(),
        "sha_before": sha_before,
        "sha_after": sha_after,
        "workload": {
            "readers": args.readers,
            "writers": args.writers,
            "duration_s": args.duration,
            "seed_docs": args.seed_docs,
            "reps": args.reps,
        },
        "box_checks": checks,
        "trusted": (
            all(c["verdict"] == "quiet" for c in checks)
            and sha_before == sha_after
            and rc_all == 0
            and all("error" not in v for v in results.values())
        ),
        "elapsed_s": round(time.time() - started, 1),
        "results": results,
    }
    (args.out / "artifact.json").write_text(json.dumps(artifact, indent=2, default=str))

    print(f"\n{'arm':<10}{'read alone':>14}{'contended':>14}{'retained':>11}")
    for label, v in results.items():
        if "error" in v:
            print(f"{label:<10}  {v['error']}")
            continue
        print(
            f"{label:<10}{v['median_alone']:>14,.1f}{v['median_contended']:>14,.1f}"
            f"{v['median_retained_pct']:>10.1f}%"
        )
    print(f"\nwrote {args.out / 'artifact.json'}   trusted: {artifact['trusted']}")
    if not artifact["trusted"]:
        print("!! Do NOT quote these numbers.")
    return 0 if rc_all == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
