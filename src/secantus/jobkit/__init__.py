"""Shared job runner + journal for the SecantusDB Ops Board.

Both the invoke CLI (via the repo-root ``./inv`` wrapper) and the Ops Board
web app spawn builds through this one runner, so terminal-started and
UI-started jobs are the same journaled process. See ``_core`` for the
import-light invariant that keeps ``./inv`` build-free in unsynced worktrees.
"""

from __future__ import annotations

from secantus.jobkit._core import (
    CANCELLED,
    FAILED,
    PASSED,
    RUNNING,
    Job,
    Journal,
    default_db_path,
    default_log_dir,
    infer_target,
    run_tracked,
    status_for_exit,
)

__all__ = [
    "Journal",
    "Job",
    "run_tracked",
    "infer_target",
    "status_for_exit",
    "default_db_path",
    "default_log_dir",
    "RUNNING",
    "PASSED",
    "FAILED",
    "CANCELLED",
]
