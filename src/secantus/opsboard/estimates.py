"""Time estimates for a task, preferring real history over a declared guess.

Every run is journaled with its duration, so once a task has been run on this
machine we can quote a **median of past successful runs** — which reflects this
hardware, this repo size, and warm caches. Until then we fall back to the
registry's declared estimate, clearly labelled as rough.

The distinction matters and is surfaced in the UI: a measured median is real
data, a declared estimate is a guess.
"""

from __future__ import annotations

from dataclasses import dataclass
from statistics import median

# Below this many samples we still quote the observation, but say how few runs
# it's based on rather than calling it a typical/median figure.
_CONFIDENT_SAMPLES = 3


def format_duration(seconds: float) -> str:
    """Human duration: '45s', '3m 20s', '1h 05m'."""
    total = int(round(seconds))
    if total < 60:
        return f"{total}s"
    if total < 3600:
        m, s = divmod(total, 60)
        return f"{m}m {s:02d}s"
    h, rem = divmod(total, 3600)
    m = rem // 60
    return f"{h}h {m:02d}m"


@dataclass(frozen=True)
class Estimate:
    text: str  # e.g. "4m 12s"  (or "unknown")
    source: str  # "measured" | "rough" | "unknown"
    samples: int = 0

    @property
    def qualifier(self) -> str:
        """How much to trust the number — shown next to it in the UI."""
        if self.source == "measured":
            if self.samples >= _CONFIDENT_SAMPLES:
                return f"median of the last {self.samples} successful runs here"
            noun = "run" if self.samples == 1 else "runs"
            return f"based on only {self.samples} previous {noun} here"
        if self.source == "rough":
            return "rough estimate — no successful runs recorded here yet"
        return "no estimate available"


def estimate_for(durations: list[float] | None, declared_seconds: int = 0) -> Estimate:
    """Build an :class:`Estimate` from observed durations + a declared fallback."""
    if durations:
        observed = median(durations)
        return Estimate(format_duration(observed), "measured", len(durations))
    if declared_seconds > 0:
        return Estimate(format_duration(declared_seconds), "rough", 0)
    return Estimate("unknown", "unknown", 0)


__all__ = ["Estimate", "estimate_for", "format_duration"]
