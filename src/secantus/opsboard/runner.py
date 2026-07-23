"""Web-app side of job orchestration.

``JobRunner`` starts, cancels, and tails jobs — but every run goes through the
SAME jobkit entrypoint the CLI uses (``python -m secantus.jobkit <argv>``, which
is what the ``./inv`` wrapper runs too). So a job the UI starts and a job a
developer starts in a terminal are indistinguishable in the shared journal, and
the UI can attach to either by reading the journal and tailing the logfile.
"""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import time
from pathlib import Path

from secantus.jobkit import Job, Journal


class JobRunner:
    def __init__(
        self,
        *,
        repo_root: str | Path,
        journal: Journal | None = None,
        python: str | None = None,
    ) -> None:
        self.repo_root = str(repo_root)
        self.journal = journal or Journal()
        # The interpreter that runs `-m secantus.jobkit`. Defaults to the one
        # hosting the web app (same env → secantus + uv resolvable).
        self.python = python or sys.executable

    def start(self, argv: list[str], *, wait_for_id: float = 5.0) -> Job:
        """Spawn a tracked job detached; return its journal row once visible.

        The child (jobkit) creates the journal row and owns the logfile, so we
        poll the journal for the row whose ``host_pid`` is the child's pid. The
        job keeps running even if the web window closes — it's a detached
        process writing to the shared journal + logfile.
        """
        proc = subprocess.Popen(
            [self.python, "-m", "secantus.jobkit", *argv],
            cwd=self.repo_root,
            env=os.environ.copy(),
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,  # detached; survives window close
        )
        deadline = time.monotonic() + wait_for_id
        while time.monotonic() < deadline:
            for job in self.journal.running():
                if job.host_pid == proc.pid:
                    return job
            if proc.poll() is not None:
                break  # child exited before we saw the row
            time.sleep(0.02)
        # Fall back to whatever the child recorded (it may already be finished
        # for a very fast/failed task).
        for job in reversed(self.journal.list(limit=20)[0]):
            if job.host_pid == proc.pid:
                return job
        raise RuntimeError(f"job for pid {proc.pid} never appeared in the journal")

    def cancel(self, job_id: int) -> bool:
        """Signal a running job's process group (SIGINT, escalating to TERM).

        Uses the journal's recorded ``host_pid`` — which jobkit made a process
        group leader (``start_new_session``) — so cancel works even across a web
        reload where we no longer hold the Popen handle.
        """
        job = self.journal.get(job_id)
        if job is None or not job.running:
            return False
        try:
            os.killpg(job.host_pid, signal.SIGINT)
        except (ProcessLookupError, PermissionError):
            return False
        return True

    def tail(self, job_id: int, offset: int = 0) -> tuple[str, int, bool]:
        """Return ``(new_text, new_offset, done)`` for a job's logfile.

        ``done`` is True once the job has left the running state — the UI then
        stops polling. Robust to a not-yet-created logfile (returns empty).
        """
        job = self.journal.get(job_id)
        if job is None:
            return "", offset, True
        done = not job.running
        if not job.log_path:
            return "", offset, done
        path = Path(job.log_path)
        if not path.exists():
            return "", offset, done
        with path.open("rb") as fh:
            fh.seek(offset)
            chunk = fh.read()
            new_offset = fh.tell()
        return chunk.decode("utf-8", "replace"), new_offset, done


__all__ = ["JobRunner"]
