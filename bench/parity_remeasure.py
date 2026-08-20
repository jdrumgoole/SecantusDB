"""Clean re-measure of the Rust-server vs mongod write-throughput gap.

Replaces the numbers behind `## VERDICT (2026-07-22)` in
`tasks/rust-parity-forward-plan.md`, which concluded that mongod write-parity is
not reachable. That verdict is already annotated as suppressed by hidden CPU
contention, and its "no per-op lever exists" claim was falsified by shipped #608.
This script exists so the replacement numbers cannot be dismissed the same way.

Three things it does that the original A/B did not:

1. **Refuses to run on a loaded box.** Load average, orphaned shell processes and
   stray daemons are checked BEFORE and re-checked AFTER; a run that started
   quiet but finished loaded is reported as untrustworthy rather than quietly
   averaged in.

2. **Pins the tree.** The repo must be a detached worktree at a fixed SHA, and the
   SHA is re-read at the end. `main` moving mid-run is exactly what invalidated an
   earlier scaling curve (CLAUDE.md, "Pin a worktree to a commit").

3. **Measures a FAIR mongod arm.** `bench/concurrency.py` spawns mongod
   standalone — no `--replSet`, so mongod keeps **no oplog at all** while the Rust
   server always writes one. The historical 5-6x gap was measured across that
   asymmetry. This script runs the oplog-tax A/B (`bench/mongod_replset_ab.py`)
   alongside, so the gap can be quoted both ways: vs the mongod the old table used,
   and vs a mongod that is actually doing the same work.

Nothing here interprets the result. It produces a JSON artifact and a markdown
table; the conclusion is a human's.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
LOAD_CEILING = 4.0  # tasks/rust-parity-forward-plan.md "Standing lesson"

# `which("mongod")` on this box is a 2024 symlink to mongodb-community@6.0, so
# every benchmark to date has measured a two-year-old server while 8.3.4 sat
# installed and unlinked. Naming both explicitly makes the target version a
# recorded choice instead of an accident of PATH.
MONGOD_6 = "/opt/homebrew/Cellar/mongodb-community@6.0/6.0.16/bin/mongod"
MONGOD_8 = "/opt/homebrew/Cellar/mongodb-community/8.3.4/bin/mongod"

# label -> (bench.concurrency --server value, explicit binary or None for PATH)
ARMS: dict[str, tuple[str, str | None]] = {
    "rust": ("rust", None),
    "mongod6": ("mongod", MONGOD_6),
    "mongod8": ("mongod", MONGOD_8),
    "mongod": ("mongod", None),  # whatever PATH gives — the historical arm
}


# --------------------------------------------------------------- box hygiene
def load_average() -> float:
    return os.getloadavg()[0]


def stray_processes() -> list[str]:
    """Daemons and orphaned shells that would steal CPU from the run."""
    out = subprocess.run(["ps", "-Ao", "ppid,command"], capture_output=True, text=True).stdout
    stray = []
    for line in out.splitlines()[1:]:
        ppid, _, cmd = line.strip().partition(" ")
        if "secantusd" in cmd or re.search(r"\bmongod\b", cmd):
            stray.append(f"daemon: {cmd[:90]}")
        elif ppid == "1" and "shell-snapshots" in cmd:
            stray.append(f"orphan shell: {cmd[:90]}")
        elif "pytest-xdist" in cmd or "[pytest" in cmd:
            stray.append(f"test worker: {cmd[:90]}")
    return stray


def check_box(phase: str, *, strict: bool, settle_s: float = 0.0) -> dict:
    """Judge whether the box is quiet enough to trust.

    `settle_s` matters for the post-run check. Load average is a ~1-minute
    decaying mean, so sampling it the instant the last writer exits measures the
    run's OWN load winding down, not contamination — the first version of this
    script marked a clean run untrusted for exactly that reason. Wait for the
    decay, then judge; record both readings so the distinction stays visible.
    """
    immediate = load_average()
    la = immediate
    deadline = time.monotonic() + settle_s
    while la >= LOAD_CEILING and time.monotonic() < deadline:
        time.sleep(5)
        la = load_average()

    stray = stray_processes()
    verdict = "quiet" if (la < LOAD_CEILING and not stray) else "LOADED"
    settled = "" if settle_s == 0 else f" (immediately after: {immediate:.2f})"
    print(f"[{phase}] load={la:.2f}{settled} stray={len(stray)} -> {verdict}")
    for s in stray:
        print(f"    {s}")
    if strict and verdict != "quiet":
        raise SystemExit(
            f"refusing to measure: load {la:.2f} (ceiling {LOAD_CEILING}) / {len(stray)} stray "
            "processes. Quiet the box or pass --force to record an untrusted run."
        )
    return {
        "phase": phase,
        "load": la,
        "load_immediate": immediate,
        "settle_s": settle_s,
        "stray": stray,
        "verdict": verdict,
    }


def head_sha() -> str:
    return subprocess.run(
        ["git", "-C", str(REPO), "rev-parse", "HEAD"], capture_output=True, text=True
    ).stdout.strip()


def is_detached() -> bool:
    out = subprocess.run(
        ["git", "-C", str(REPO), "symbolic-ref", "-q", "HEAD"], capture_output=True, text=True
    )
    return out.returncode != 0  # detached HEAD has no symbolic ref


# ------------------------------------------------------------------- the runs
def run(cmd: list[str], log: Path, *, env_extra: dict[str, str] | None = None) -> tuple[int, str]:
    env = {**os.environ, **(env_extra or {})}
    pin = env_extra.get("SECANTUS_MONGOD_BIN") if env_extra else None
    print(f"    $ {' '.join(cmd[:6])} ...{f'  [mongod={pin}]' if pin else ''}")
    with log.open("wb") as fh:
        proc = subprocess.run(cmd, stdout=fh, stderr=subprocess.STDOUT, cwd=REPO, env=env)
    return proc.returncode, log.read_text(errors="replace")


def main() -> int:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--out", type=Path, default=REPO / "bench" / "results" / "parity-remeasure")
    ap.add_argument("--writers", default="1,2,4,8")
    ap.add_argument("--duration", type=float, default=30.0)
    ap.add_argument(
        "--runs", type=int, default=3, help="repeats per writer count; median is what counts"
    )
    ap.add_argument(
        "--servers", default="rust,mongod6,mongod8", help="comma-separated arm labels from ARMS"
    )
    ap.add_argument(
        "--skip-oplog-tax", action="store_true", help="skip the mongod standalone-vs-replset arm"
    )
    ap.add_argument(
        "--oplog-tax-mongod",
        default=MONGOD_8,
        help="mongod binary for the oplog-tax arm "
        "(default 8.3.4; 6.0.16 was measured in the prior run)",
    )
    ap.add_argument(
        "--force",
        action="store_true",
        help="measure even on a loaded box (marks the artifact untrusted)",
    )
    args = ap.parse_args()

    if not is_detached():
        raise SystemExit(
            "refusing to measure on an attached branch — use a pinned detached worktree:\n"
            '  git worktree add --detach ../SecantusDB-measure "$(git rev-parse origin/main)"'
        )

    binary = REPO / "crates" / "secantusdb" / "target" / "release" / "secantusd-rs"
    if not binary.exists():
        raise SystemExit(f"Rust binary not built at {binary}")
    if not shutil.which("mongod"):
        raise SystemExit("mongod not on PATH")

    args.out.mkdir(parents=True, exist_ok=True)
    sha_before = head_sha()
    env_note = subprocess.run(["mongod", "--version"], capture_output=True, text=True).stdout
    mongod_version = env_note.splitlines()[0] if env_note else "unknown"

    print(f"pinned at   : {sha_before}")
    print(f"rust binary : {binary}")
    print(f"mongod      : {mongod_version}")
    print("NOTE: the VERDICT table this replaces was taken against mongod STANDALONE")
    print("      (no --replSet, therefore no oplog) while the Rust server writes one.\n")

    checks = [check_box("before", strict=not args.force)]
    started = time.time()

    os.environ["SECANTUSDB_BIN"] = str(binary)
    results: dict[str, object] = {}

    # 1. The scaling sweep, same instrument as the original table. bench.concurrency
    #    takes ONE server per invocation, so drive it once per arm back-to-back —
    #    every arm sees the same box, which is the whole point of one invocation
    #    rather than bolting a later arm onto earlier results.
    rc = 0
    sweeps: dict[str, object] = {}
    for label in [s.strip() for s in args.servers.split(",") if s.strip()]:
        server, binary = ARMS.get(label, (label, None))
        env_extra = {}
        version = None
        if binary:
            if not Path(binary).exists():
                sweeps[label] = {"error": f"binary missing: {binary}"}
                rc = rc or 3
                continue
            env_extra["SECANTUS_MONGOD_BIN"] = binary
            version = subprocess.run(
                [binary, "--version"], capture_output=True, text=True
            ).stdout.splitlines()[0]

        sweep_json = args.out / f"concurrency-{label}.json"
        arm_rc, _ = run(
            [
                sys.executable,
                "-m",
                "bench.concurrency",
                "--server",
                server,
                "--writers",
                args.writers,
                "--duration",
                str(args.duration),
                "--runs",
                str(args.runs),
                "--json",
                str(sweep_json),
            ],
            args.out / f"concurrency-{label}.log",
            env_extra=env_extra,
        )
        rc = rc or arm_rc
        sweeps[label] = {
            "server": server,
            "binary": binary,
            "version": version,
            "data": json.loads(sweep_json.read_text()) if sweep_json.exists() else None,
            "rc": arm_rc,
        }
    results["concurrency_rc"] = rc
    results["concurrency"] = sweeps

    # 2. The fairness arm: what mongod itself pays for keeping an oplog. Its
    #    `replset-w1` arm (oplog double-write, no fsync wait) is the only
    #    like-for-like comparison against the Rust server, which always writes an
    #    oplog — the standalone arm above keeps none at all.
    if not args.skip_oplog_tax:
        tax_env = {"SECANTUS_MONGOD_BIN": args.oplog_tax_mongod} if args.oplog_tax_mongod else {}
        rc2, tail = run(
            [
                sys.executable,
                "-m",
                "bench.mongod_replset_ab",
                "--writers",
                args.writers,
                "--duration",
                str(args.duration),
            ],
            args.out / "oplog_tax.log",
            env_extra=tax_env,
        )
        results["oplog_tax_mongod"] = args.oplog_tax_mongod or "PATH"
        results["oplog_tax_rc"] = rc2
        results["oplog_tax_tail"] = tail[-4000:]

    # Give the run's own load a chance to decay before judging the box.
    checks.append(check_box("after", strict=False, settle_s=180.0))
    sha_after = head_sha()

    artifact = {
        "captured": datetime.now(timezone.utc).isoformat(),
        "sha_before": sha_before,
        "sha_after": sha_after,
        "sha_stable": sha_before == sha_after,
        "mongod_version": mongod_version,
        "mongod_arm": "standalone (no --replSet, no oplog) — see oplog_tax for the fair comparison",
        "writers": args.writers,
        "duration_s": args.duration,
        "runs": args.runs,
        "box_checks": checks,
        # Trusted means all three: the box stayed quiet, the tree did not move, AND
        # every arm actually ran. A failed arm with a green box is not a result.
        "trusted": (
            all(c["verdict"] == "quiet" for c in checks)
            and sha_before == sha_after
            and rc == 0
            and results.get("oplog_tax_rc", 0) == 0
        ),
        "elapsed_s": round(time.time() - started, 1),
        "results": results,
    }
    out_json = args.out / "artifact.json"
    out_json.write_text(json.dumps(artifact, indent=2, default=str))

    print(f"\nwrote {out_json}")
    print(f"trusted: {artifact['trusted']}  (sha stable: {artifact['sha_stable']})")
    if not artifact["trusted"]:
        print("!! Do NOT quote these numbers — the box moved or the tree moved under the run.")
    return 0 if rc == 0 else 1


if __name__ == "__main__":
    raise SystemExit(main())
