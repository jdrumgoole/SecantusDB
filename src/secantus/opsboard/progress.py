"""Best-effort progress extraction from a job's log stream.

Task subprocesses expose no progress API, so progress is *derived* from the log:

- **Phase stepper** — explicit ``==> [k/N] label`` step markers (the gate and
  release tasks emit these). The last marker seen fixes the current phase k of N.
- **Determinate bar** — the pytest ``[ NN% ]`` marker drives a within-phase (or
  overall, when there are no phases) percentage. Covers ``test`` / ``perf`` /
  the gauges.
- **Indeterminate** — when neither signal is present, the UI shows an animated
  bar + spinner while the job runs.

All heuristic and forgiving: a task with none of these still gets a spinner,
elapsed timer, and status.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field

_MARKER_RE = re.compile(r"==>\s*\[(\d+)/(\d+)\]\s*(.*)")
# pytest / pytest-xdist progress: "... PASSED [ 42%]"
_PCT_RE = re.compile(r"\[\s*(\d+)\s*%\]")

# Phase states.
DONE = "done"
ACTIVE = "active"
PENDING = "pending"
FAILED = "failed"


@dataclass(frozen=True)
class Phase:
    index: int
    label: str
    state: str  # DONE | ACTIVE | PENDING | FAILED


@dataclass
class Progress:
    total_phases: int  # N from markers / known labels (0 if none)
    current_phase: int  # k (0 before the first marker)
    phases: list[Phase] = field(default_factory=list)
    percent: int | None = None  # last pytest % seen (within the active phase)
    overall: int | None = None  # 0..100 estimate, or None → indeterminate
    determinate: bool = False

    @property
    def has_phases(self) -> bool:
        return self.total_phases > 0


def parse_progress(
    log_text: str,
    *,
    known_labels: list[str] | None = None,
    done: bool = False,
    passed: bool = False,
) -> Progress:
    """Derive a :class:`Progress` snapshot from ``log_text``.

    ``known_labels`` (from the registry) pre-populates the phase names so the
    stepper shows real labels — even for phases not yet reached. ``done`` /
    ``passed`` come from the job's final status.
    """
    markers = _MARKER_RE.findall(log_text)
    percents = _PCT_RE.findall(log_text)
    percent = int(percents[-1]) if percents else None
    current = int(markers[-1][0]) if markers else 0
    marker_labels = {int(k): label.strip() for k, _n, label in markers}

    # Markers (once they appear) are authoritative for the phase COUNT — a task
    # may run fewer steps than the declared labels (e.g. gate --no-perf). Before
    # any marker, fall back to the declared label count so the stepper shows all
    # phases (pending) with real names up front.
    if markers:
        total = int(markers[-1][1])
    elif known_labels:
        total = len(known_labels)
    else:
        total = 0

    def label_for(i: int) -> str:
        if known_labels and i - 1 < len(known_labels):
            return known_labels[i - 1]
        return marker_labels.get(i, "…")

    def state_for(i: int) -> str:
        if done and passed:
            return DONE
        if i < current:
            return DONE
        if i == current:
            return FAILED if done else ACTIVE
        return PENDING

    phases = [Phase(i, label_for(i), state_for(i)) for i in range(1, total + 1)]

    if total and current:
        frac = (percent / 100.0) if percent is not None else 0.0
        overall: int | None = round(((current - 1 + frac) / total) * 100)
        determinate = True
    elif percent is not None:
        overall = percent
        determinate = True
    else:
        overall = None
        determinate = False

    if done and passed:
        overall = 100
        determinate = True
    if overall is not None:
        overall = max(0, min(100, overall))

    return Progress(
        total_phases=total,
        current_phase=current,
        phases=phases,
        percent=percent,
        overall=overall,
        determinate=determinate,
    )


__all__ = ["Progress", "Phase", "parse_progress", "DONE", "ACTIVE", "PENDING", "FAILED"]
