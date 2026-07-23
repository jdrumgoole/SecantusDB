"""Web-app side of job orchestration.

``JobRunner`` starts, cancels, and tails jobs — but every run goes through the
SAME jobkit entrypoint the CLI uses (``python -m secantus.jobkit <argv>``, which
is what the ``./inv`` wrapper runs too). So a job the UI starts and a job a
developer starts in a terminal are indistinguishable in the shared journal, and
the UI can attach to either by reading the journal and tailing the logfile.
"""

from __future__ import annotations

import contextlib
import os
import signal
import subprocess
import sys
import time
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
        # PIDs of jobkit children we spawned, so we can reap them (a detached
        # child that finishes or is cancelled would otherwise linger as a zombie
        # under the web app until reaped).
        self._spawned: set[int] = set()

    def _reap(self) -> None:
        for pid in list(self._spawned):
            try:
                reaped, _status = os.waitpid(pid, os.WNOHANG)
            except ChildProcessError:
                self._spawned.discard(pid)  # already reaped / not ours
                continue
            except OSError:
                continue
            if reaped:  # nonzero → the child was dead and is now reaped
                self._spawned.discard(pid)

    def start(self, argv: list[str]) -> Job:
        """Spawn a tracked job detached; return its journal row immediately.

        The row is created HERE (so the id is known synchronously — no
        pid-poll race, which was slow/flaky on Windows CI) and its id is passed
        to the jobkit child via ``SECANTUS_OPSBOARD_JOB_ID``; the child adopts
        it, records its own pid, tees the logfile, and finishes the row on exit.
        The job keeps running even if the web window closes — it's a detached
        process writing to the shared journal + logfile.
        """
        self._reap()  # opportunistically clear any finished detached children
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
        self._spawned.add(proc.pid)
        self.journal.set_host_pid(job_id, proc.pid)
        job = self.journal.get(job_id)
        assert job is not None
        return job

    def cancel(self, job_id: int, *, grace: float = 1.5) -> bool:
        """Tear down a running job and its whole process tree.

        jobkit made ``host_pid`` a process-group leader (``start_new_session``),
        so the group covers `uv`/`invoke`/`pytest`/`cargo` and any shells they
        spawned. We (1) SIGINT the group for a graceful stop, (2) after ``grace``
        seconds SIGTERM, then (3) SIGKILL both the group AND any descendant that
        escaped into its own session (e.g. a tool that called setsid). Works
        across a web reload since it drives off the journal's recorded pid, not a
        held Popen handle.
        """
        job = self.journal.get(job_id)
        if job is None or not job.running:
            return False
        pid = job.host_pid
        if pid <= 0:
            return False
        _terminate_tree(pid, grace=grace)
        self._reap()  # reap the killed child if it was one of ours
        return True

    def cancel_all(self) -> int:
        """Cancel every running job. Returns how many were signalled."""
        count = 0
        for job in self.journal.running():
            if self.cancel(job.id):
                count += 1
        self._reap()
        return count

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


# --------------------------------------------------------------------------- #
# Process-tree teardown (POSIX). Kills the process GROUP plus any descendant
# that escaped it, escalating INT → TERM → KILL. macOS/Linux dev tool.
# --------------------------------------------------------------------------- #


def _descendants(root: int) -> list[int]:
    """All PIDs under ``root`` (inclusive), via a single ``ps`` snapshot."""
    try:
        out = subprocess.run(
            ["ps", "-Ao", "pid=,ppid="],
            capture_output=True,
            text=True,
            timeout=5,
        ).stdout
    except (OSError, subprocess.SubprocessError):
        return [root]
    children: dict[int, list[int]] = {}
    for line in out.splitlines():
        parts = line.split()
        if len(parts) != 2:
            continue
        try:
            pid, ppid = int(parts[0]), int(parts[1])
        except ValueError:
            continue
        children.setdefault(ppid, []).append(pid)
    seen: list[int] = []
    stack = [root]
    while stack:
        pid = stack.pop()
        if pid in seen:
            continue
        seen.append(pid)
        stack.extend(children.get(pid, []))
    return seen


def _signal_all(pids: list[int], group_leader: int, sig: int) -> None:
    with contextlib.suppress(ProcessLookupError, PermissionError):
        os.killpg(group_leader, sig)  # the whole group in one shot
    for pid in pids:
        with contextlib.suppress(ProcessLookupError, PermissionError):
            os.kill(pid, sig)  # backstop for escapees


def _terminate_tree(pid: int, *, grace: float = 1.5) -> None:
    pids = _descendants(pid)
    _signal_all(pids, pid, signal.SIGINT)
    deadline = time.monotonic() + grace
    while time.monotonic() < deadline:
        if not any(_alive(p) for p in pids):
            return
        time.sleep(0.05)
    _signal_all(pids, pid, signal.SIGTERM)
    time.sleep(0.3)
    # Anything still alive gets SIGKILL, rescanning for freshly-spawned children.
    survivors = _descendants(pid)
    _signal_all(survivors, pid, signal.SIGKILL)


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


__all__ = ["JobRunner"]
