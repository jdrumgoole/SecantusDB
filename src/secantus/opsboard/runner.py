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
from pathlib import Path

from secantus.jobkit import Job, Journal, infer_target


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

    def start(self, argv: list[str]) -> Job:
        """Spawn a tracked job detached; return its journal row immediately.

        The row is created HERE (so the id is known synchronously — no
        pid-poll race, which was slow/flaky on Windows CI) and its id is passed
        to the jobkit child via ``SECANTUS_OPSBOARD_JOB_ID``; the child adopts
        it, records its own pid, tees the logfile, and finishes the row on exit.
        The job keeps running even if the web window closes — it's a detached
        process writing to the shared journal + logfile.
        """
        task = argv[0] if argv else "?"
        job_id = self.journal.create(
            target=infer_target(task, argv),
            task=task,
            argv=argv,
            worktree=self.repo_root,
            host_pid=os.getpid(),  # placeholder (alive); child overwrites it
        )
        env = os.environ.copy()
        env["SECANTUS_OPSBOARD_JOB_ID"] = str(job_id)
        proc = subprocess.Popen(
            [self.python, "-m", "secantus.jobkit", *argv],
            cwd=self.repo_root,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,  # detached; survives window close (POSIX)
        )
        self.journal.set_host_pid(job_id, proc.pid)
        job = self.journal.get(job_id)
        assert job is not None
        return job

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
