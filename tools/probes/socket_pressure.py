"""Watch socket pressure during a psycopg gauge run.

Testing the one hypothesis that would make the unexplained 40ms `connect()`
failures OUR bug rather than a property of the machine: the harness exhausts a
finite socket resource, so a later connect fails at once instead of timing out.

Three candidates, because the first is already implausible here (`ulimit -n` is
1048576):
  fds        -- descriptors held by the runner process
  timewait   -- sockets in TIME_WAIT, which hold an ephemeral port
  ephemeral  -- distinct local ports in use in the ephemeral range

A monotonic climb toward the ephemeral range's size is the leak. Flat is not.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import time


def _run(cmd: list[str]) -> str:
    return subprocess.run(cmd, capture_output=True, text=True).stdout


def fd_count(pid: int) -> int:
    return max(0, len(_run(["lsof", "-p", str(pid)]).splitlines()) - 1)


def socket_pressure(lo: int, hi: int) -> tuple[int, int]:
    """(TIME_WAIT sockets, distinct ephemeral local ports in use)."""
    out = _run(["netstat", "-an", "-p", "tcp"])
    timewait = 0
    ports: set[int] = set()
    for line in out.splitlines():
        if "TIME_WAIT" in line:
            timewait += 1
        m = re.search(r"127\.0\.0\.1[.:](\d+)", line)
        if m:
            p = int(m.group(1))
            if lo <= p <= hi:
                ports.add(p)
    return timewait, len(ports)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--pattern", default="psycopg_validation.runner")
    ap.add_argument("--interval", type=float, default=20.0)
    ap.add_argument("--range", default="49152-65535", help="ephemeral port range lo-hi")
    args = ap.parse_args()
    lo, hi = (int(x) for x in args.range.split("-"))
    span = hi - lo + 1

    fds: list[int] = []
    tws: list[int] = []
    eps: list[int] = []
    started = False
    try:
        while True:
            pids = _run(["pgrep", "-f", args.pattern]).split()
            if pids:
                started = True
            elif started:
                break
            tw, ep = socket_pressure(lo, hi)
            tws.append(tw)
            eps.append(ep)
            if pids:
                fds.append(fd_count(int(pids[0])))
            print(
                f"{time.strftime('%H:%M:%S')} fds={fds[-1] if fds else '-':>6} "
                f"time_wait={tw:>6} ephemeral={ep:>6}/{span}",
                flush=True,
            )
            time.sleep(args.interval)
    except KeyboardInterrupt:
        pass

    def summarise(name: str, series: list[int]) -> None:
        if series:
            print(
                f"{name:>10}: first={series[0]} max={max(series)} last={series[-1]} n={len(series)}"
            )

    print("\n-- summary (a leak climbs monotonically; churn sawtooths)")
    summarise("fds", fds)
    summarise("time_wait", tws)
    summarise("ephemeral", eps)
    print(f"ephemeral range size: {span}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
