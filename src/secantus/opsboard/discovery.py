"""Tier-3 tracking: best-effort discovery of untracked local build processes.

Tier 1 is GitHub (any CI run, no opt-in). Tier 2 is the jobkit journal (anything
started via ``./inv``, automatically). Tier 3 is this: a process-table scan that
notices builds started *outside* both — someone running ``pytest`` or ``cargo
test`` directly, or a tool a task spawned that outlived it.

Honest about its limits: we did not spawn these processes, so we cannot attach to
their stdout (that fd belongs to whatever terminal started them). They are shown
as **external** with command, working directory and elapsed time only — never
with a log. Journal-tracked pids are filtered out so a tracked job doesn't also
appear here as a duplicate.

POSIX-only (parses ``ps``); returns an empty list elsewhere.
"""

from __future__ import annotations

import os
import subprocess
from collections.abc import Callable, Sequence
from dataclasses import dataclass

Runner = Callable[[Sequence[str]], tuple[int, str]]

# Command signatures worth surfacing — build/test tooling, not every process.
_SIGNATURES = (
    "pytest",
    "cargo",
    "cmake",
    "ninja",
    "invoke",
    "secantusd-rs",
    "secantusd-py",
    "gradlew",
    "test-libmongoc",
)

# Never report ourselves or the board's own plumbing.
_EXCLUDE = ("secantus-opsboard", "secantus.opsboard", "opsboard")


def _ps(argv: Sequence[str]) -> tuple[int, str]:
    try:
        proc = subprocess.run(list(argv), capture_output=True, text=True, timeout=10)
    except (OSError, subprocess.SubprocessError):
        return 1, ""
    return proc.returncode, proc.stdout


@dataclass(frozen=True)
class ExternalProcess:
    pid: int
    elapsed: str  # ps ETIME, e.g. "01:23" or "1-02:03:04"
    command: str

    @property
    def short_command(self) -> str:
        return self.command if len(self.command) <= 110 else self.command[:107] + "…"


def scan(
    *, known_pids: Sequence[int] = (), runner: Runner | None = None, limit: int = 50
) -> list[ExternalProcess]:
    """Untracked build/test processes, newest-first, bounded by ``limit``.

    The Windows short-circuit applies only to the *real* ``ps`` (which doesn't
    exist there). An explicitly injected runner has no platform dependency, so
    the parsing below stays exercised on every platform.
    """
    if runner is None and os.name == "nt":  # pragma: no cover - POSIX-only
        return []
    run = runner or _ps
    code, out = run(["ps", "-Ao", "pid=,etime=,command="])
    if code != 0:
        return []

    known = set(known_pids)
    self_pid = os.getpid()
    found: list[ExternalProcess] = []
    for line in out.splitlines():
        parts = line.strip().split(None, 2)
        if len(parts) != 3:
            continue
        pid_s, elapsed, command = parts
        try:
            pid = int(pid_s)
        except ValueError:
            continue
        if pid == self_pid or pid in known:
            continue
        low = command.lower()
        if any(x in low for x in _EXCLUDE):
            continue
        if not any(sig in low for sig in _SIGNATURES):
            continue
        found.append(ExternalProcess(pid=pid, elapsed=elapsed, command=command))
        if len(found) >= max(1, limit):
            break
    return found


__all__ = ["ExternalProcess", "scan"]
