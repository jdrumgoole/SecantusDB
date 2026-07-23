"""Unified activity feed: local jobs and GitHub CI runs, side by side.

Builds are visible from two places — the local jobkit journal and GitHub
Actions — and until now which one you were looking at was implied only by which
page you were on. This merges them into one time-ordered feed where every entry
carries an explicit ``origin`` (``local`` or ``github``), so "did this run on my
machine or on CI?" is answered by the row, not by context.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timezone

LOCAL = "local"
GITHUB = "github"


@dataclass(frozen=True)
class Activity:
    origin: str  # LOCAL | GITHUB
    label: str  # invoke task, or workflow name
    state: str  # running | passed | failed | cancelled
    when: float  # epoch seconds, for ordering
    when_text: str  # short human timestamp
    detail: str  # target/worktree for local; branch · event for CI
    url: str  # job detail path, or the GitHub run URL

    @property
    def is_local(self) -> bool:
        return self.origin == LOCAL


def _iso_to_epoch(value: str) -> float:
    """GitHub's ISO-8601 (Z-suffixed) → epoch seconds; 0.0 when unparseable."""
    if not value:
        return 0.0
    try:
        dt = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return 0.0
    if dt.tzinfo is None:
        dt = dt.replace(tzinfo=timezone.utc)
    return dt.timestamp()


def _short_time(epoch: float) -> str:
    if not epoch:
        return ""
    return datetime.fromtimestamp(epoch, tz=timezone.utc).strftime("%m-%d %H:%M")


# GitHub's status/conclusion pair → the same vocabulary local jobs use, so the
# feed can colour both with one set of pills.
def _ci_state(status: str, conclusion: str) -> str:
    if status != "completed":
        return "running"
    return {
        "success": "passed",
        "failure": "failed",
        "timed_out": "failed",
        "startup_failure": "failed",
    }.get(conclusion, "cancelled")


def from_jobs(jobs) -> list[Activity]:  # noqa: ANN001 - jobkit.Job sequence
    out = []
    for job in jobs:
        out.append(
            Activity(
                origin=LOCAL,
                label=job.task,
                state=job.status,
                when=job.started_at,
                when_text=_short_time(job.started_at),
                detail=job.target,
                url=f"/jobs/{job.id}",
            )
        )
    return out


def from_runs(runs) -> list[Activity]:  # noqa: ANN001 - github.WorkflowRun sequence
    out = []
    for run in runs:
        epoch = _iso_to_epoch(run.created_at)
        out.append(
            Activity(
                origin=GITHUB,
                label=run.name,
                state=_ci_state(run.status, run.conclusion),
                when=epoch,
                when_text=_short_time(epoch),
                detail=f"{run.branch} · {run.event}" if run.branch else run.event,
                url=run.url,
            )
        )
    return out


def merge(jobs, runs, *, limit: int = 20) -> list[Activity]:  # noqa: ANN001
    """Newest-first feed of both origins, bounded by ``limit``."""
    combined = from_jobs(jobs) + from_runs(runs)
    combined.sort(key=lambda a: a.when, reverse=True)
    return combined[: max(1, limit)]


__all__ = ["Activity", "merge", "from_jobs", "from_runs", "LOCAL", "GITHUB"]
