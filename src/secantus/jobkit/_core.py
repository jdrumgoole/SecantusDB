"""Self-contained, stdlib-only job runner + journal for the Ops Board.

This module is the shared execution + tracking core that BOTH the invoke
CLI (via the repo-root ``./inv`` wrapper) and the Ops Board web app spawn
through, so a terminal-started and a UI-started build are the same
journaled process.

**Import-light invariant.** This file must never ``import secantus`` (or use
intra-package relative imports): the ``secantus`` top-level package eagerly
imports the WiredTiger-linked server, which would break the "``./inv``
lint/fmt work in an unsynced worktree with no WT build" property. The
``./inv`` wrapper therefore loads THIS file directly by path (bypassing
``secantus/__init__.py``); keep it standalone so that load stays cheap and
build-free. The web app imports it normally as ``secantus.jobkit._core`` in a
synced env, where the heavy parent import is fine.

A job is one ``inv <task>`` invocation. Its whole terminal output (colours
included — the child runs under a pty) is teed to a per-job logfile that the
UI tails; its start/end/exit is recorded in a sqlite journal shared by every
process on the host.
"""

from __future__ import annotations

import contextlib
import json
import os
import select
import signal
import sqlite3
import subprocess
import sys
import threading
import time
from collections.abc import Iterable, Sequence
from dataclasses import dataclass
from pathlib import Path

# pty (and its termios dependency) is POSIX-only. On Windows the module import
# must still succeed — the runner falls back to a plain pipe (no colour, but
# fully functional). The Ops Board targets macOS/Linux; this just keeps the
# package importable and the tests collectable everywhere.
try:
    import pty as _pty
except ModuleNotFoundError:  # pragma: no cover - exercised on Windows CI only
    _pty = None  # type: ignore[assignment]

# ---------------------------------------------------------------------------
# Locations (overridable via env so tests never touch the real home dir).
# ---------------------------------------------------------------------------

_HOME_DIR = Path.home() / ".secantus"


def default_db_path() -> Path:
    override = os.environ.get("SECANTUS_OPSBOARD_DB")
    if override:
        return Path(override)
    return _HOME_DIR / "opsboard.db"


def default_log_dir() -> Path:
    override = os.environ.get("SECANTUS_OPSBOARD_LOGS")
    if override:
        return Path(override)
    return _HOME_DIR / "opsboard-logs"


# ---------------------------------------------------------------------------
# Target inference — display metadata mapping a task name to which server's
# cycle it belongs to. Best-effort; a miss lands in "other".
# ---------------------------------------------------------------------------

_PG_TASKS = {"validate-psycopg", "validate-slt"}
_PYTHON_TASKS = {
    "test",
    "test-one",
    "perf",
    "lint",
    "fmt",
    "py-gate",
    "py-ship",
    "docs",
    "release-prepare",
    "release-finalize",
    "release",
}


def infer_target(task: str, argv: Sequence[str] = ()) -> str:
    """Best-effort ``python`` / ``rust`` / ``pg`` / ``other`` classification."""
    task = task.lstrip("-")
    if task in _PG_TASKS:
        return "pg"
    if task.startswith("rust") or task in {"compare-servers"}:
        return "rust"
    # A gauge run explicitly targeting the Rust server.
    if "--server" in argv:
        idx = list(argv).index("--server")
        if idx + 1 < len(argv) and argv[idx + 1] == "rust":
            return "rust"
    if task.startswith("validate"):
        return "python"
    if task in _PYTHON_TASKS:
        return "python"
    return "other"


# ---------------------------------------------------------------------------
# Status vocabulary.
# ---------------------------------------------------------------------------

RUNNING = "running"
PASSED = "passed"
FAILED = "failed"
CANCELLED = "cancelled"


def status_for_exit(exit_code: int | None) -> str:
    if exit_code is None:
        return RUNNING
    if exit_code == 0:
        return PASSED
    # 130 = SIGINT, 143 = SIGTERM: a human/UI cancel, not a real failure.
    if exit_code in (130, 143, -signal.SIGINT, -signal.SIGTERM):
        return CANCELLED
    return FAILED


@dataclass(frozen=True)
class Job:
    id: int
    target: str
    task: str
    argv: list[str]
    worktree: str
    host_pid: int
    status: str
    exit_code: int | None
    started_at: float
    ended_at: float | None
    log_path: str | None

    @property
    def running(self) -> bool:
        return self.status == RUNNING

    @property
    def duration(self) -> float:
        end = self.ended_at if self.ended_at is not None else time.time()
        return max(0.0, end - self.started_at)


# ---------------------------------------------------------------------------
# Journal — a tiny sqlite table shared by every process on the host.
# ---------------------------------------------------------------------------

_SCHEMA = """
CREATE TABLE IF NOT EXISTS jobs (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    target     TEXT NOT NULL,
    task       TEXT NOT NULL,
    argv       TEXT NOT NULL,
    worktree   TEXT NOT NULL,
    host_pid   INTEGER NOT NULL,
    status     TEXT NOT NULL,
    exit_code  INTEGER,
    started_at REAL NOT NULL,
    ended_at   REAL,
    log_path   TEXT
);
CREATE INDEX IF NOT EXISTS jobs_id_desc ON jobs (id DESC);
"""


class Journal:
    """sqlite-backed job journal. Safe to open from many processes/threads.

    A fresh connection is opened per operation (cheap) so callers never share
    a handle across threads. WAL mode + a busy timeout let the ``./inv``
    writer and the web-app reader overlap without lock errors.
    """

    def __init__(self, path: str | Path | None = None) -> None:
        self.path = Path(path) if path is not None else default_db_path()
        self.path.parent.mkdir(parents=True, exist_ok=True)
        with self._connect() as conn:
            conn.execute("PRAGMA journal_mode=WAL")
            conn.executescript(_SCHEMA)

    def _connect(self) -> sqlite3.Connection:
        conn = sqlite3.connect(self.path, timeout=5.0)
        conn.row_factory = sqlite3.Row
        conn.execute("PRAGMA busy_timeout=5000")
        return conn

    # -- writes -----------------------------------------------------------

    def create(
        self,
        *,
        target: str,
        task: str,
        argv: Iterable[str],
        worktree: str,
        host_pid: int,
        started_at: float | None = None,
    ) -> int:
        argv_json = json.dumps(list(argv))
        started = started_at if started_at is not None else time.time()
        with self._connect() as conn:
            cur = conn.execute(
                "INSERT INTO jobs "
                "(target, task, argv, worktree, host_pid, status, started_at) "
                "VALUES (?, ?, ?, ?, ?, ?, ?)",
                (target, task, argv_json, worktree, host_pid, RUNNING, started),
            )
            return int(cur.lastrowid)

    def set_log_path(self, job_id: int, log_path: str) -> None:
        with self._connect() as conn:
            conn.execute("UPDATE jobs SET log_path = ? WHERE id = ?", (log_path, job_id))

    def set_host_pid(self, job_id: int, host_pid: int) -> None:
        with self._connect() as conn:
            conn.execute("UPDATE jobs SET host_pid = ? WHERE id = ?", (host_pid, job_id))

    def finish(
        self,
        job_id: int,
        exit_code: int | None,
        *,
        status: str | None = None,
        ended_at: float | None = None,
    ) -> None:
        end = ended_at if ended_at is not None else time.time()
        resolved = status if status is not None else status_for_exit(exit_code)
        with self._connect() as conn:
            conn.execute(
                "UPDATE jobs SET status = ?, exit_code = ?, ended_at = ? WHERE id = ?",
                (resolved, exit_code, end, job_id),
            )

    def reap_stale(self) -> int:
        """Mark ``running`` rows whose owning pid is gone as ``cancelled``.

        A job whose process was SIGKILL'd (or a machine that rebooted) never
        recorded its exit, so it would otherwise show ``running`` forever.
        """
        reaped = 0
        for job in self.running():
            if not _pid_alive(job.host_pid):
                # Process vanished without recording an exit (SIGKILL, reboot).
                self.finish(job.id, 137, status=CANCELLED)
                reaped += 1
        return reaped

    # -- reads ------------------------------------------------------------

    def get(self, job_id: int) -> Job | None:
        with self._connect() as conn:
            row = conn.execute("SELECT * FROM jobs WHERE id = ?", (job_id,)).fetchone()
        return _row_to_job(row) if row else None

    def list(
        self,
        *,
        limit: int = 50,
        before_id: int | None = None,
        include_running: bool = True,
    ) -> tuple[list[Job], int | None]:
        """Newest-first page of jobs.

        ``before_id`` is the exclusive upper-bound cursor (pass the returned
        ``next_cursor`` to fetch the following page). Returns
        ``(jobs, next_cursor)`` where ``next_cursor`` is ``None`` on the last
        page. Unbounded listing is never exposed — always paginate.

        ``include_running=False`` excludes in-flight jobs, so a "history" view
        can page through finished work without the currently-running rows
        drifting in and out of the page as they complete.
        """
        limit = max(1, min(int(limit), 500))
        where = "" if include_running else f" WHERE status != '{RUNNING}'"
        joiner = " AND" if where else " WHERE"
        with self._connect() as conn:
            if before_id is None:
                rows = conn.execute(
                    f"SELECT * FROM jobs{where} ORDER BY id DESC LIMIT ?", (limit,)
                ).fetchall()
            else:
                rows = conn.execute(
                    f"SELECT * FROM jobs{where}{joiner} id < ? ORDER BY id DESC LIMIT ?",
                    (before_id, limit),
                ).fetchall()
        jobs = [_row_to_job(r) for r in rows]
        next_cursor = jobs[-1].id if len(jobs) == limit else None
        return jobs, next_cursor

    def running(self) -> list[Job]:
        with self._connect() as conn:
            rows = conn.execute(
                "SELECT * FROM jobs WHERE status = ? ORDER BY id DESC", (RUNNING,)
            ).fetchall()
        return [_row_to_job(r) for r in rows]

    def completed_durations(self, argv: Iterable[str], *, limit: int = 20) -> list[float]:
        """Durations (seconds) of the most recent *successful* runs of ``argv``.

        Powers the Ops Board's "typical time" estimate from real history rather
        than a hardcoded guess. Only PASSED runs count — a failed run often
        aborts early and would skew the estimate low. Bounded by ``limit``.
        """
        limit = max(1, min(int(limit), 200))
        argv_json = json.dumps(list(argv))
        with self._connect() as conn:
            rows = conn.execute(
                "SELECT started_at, ended_at FROM jobs "
                "WHERE argv = ? AND status = ? AND ended_at IS NOT NULL "
                "ORDER BY id DESC LIMIT ?",
                (argv_json, PASSED, limit),
            ).fetchall()
        return [
            r["ended_at"] - r["started_at"]
            for r in rows
            if r["ended_at"] is not None and r["ended_at"] >= r["started_at"]
        ]


def _row_to_job(row: sqlite3.Row) -> Job:
    return Job(
        id=row["id"],
        target=row["target"],
        task=row["task"],
        argv=json.loads(row["argv"]),
        worktree=row["worktree"],
        host_pid=row["host_pid"],
        status=row["status"],
        exit_code=row["exit_code"],
        started_at=row["started_at"],
        ended_at=row["ended_at"],
        log_path=row["log_path"],
    )


def _pid_alive(pid: int) -> bool:
    if pid <= 0:
        return False
    if os.name == "nt":  # pragma: no cover - Windows-only path
        # os.kill(pid, 0) raises WinError 87 ("parameter is incorrect") on
        # Windows — signal 0 isn't valid there. Probe via OpenProcess instead.
        import ctypes

        process_query_limited_information = 0x1000
        still_active = 259
        kernel32 = ctypes.windll.kernel32
        handle = kernel32.OpenProcess(process_query_limited_information, False, pid)
        if not handle:
            return False
        try:
            code = ctypes.c_ulong()
            if not kernel32.GetExitCodeProcess(handle, ctypes.byref(code)):
                return True
            return code.value == still_active
        finally:
            kernel32.CloseHandle(handle)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


# ---------------------------------------------------------------------------
# The tracked runner.
# ---------------------------------------------------------------------------


# The canonical child: the same command ``./inv`` used before instrumentation
# (``uv run --no-sync --with invoke python -m invoke <task>``). Overridable for
# tests via SECANTUS_OPSBOARD_INVOKE (a shell-word string).
def _invoke_prefix() -> list[str]:
    override = os.environ.get("SECANTUS_OPSBOARD_INVOKE")
    if override:
        import shlex

        # posix=False on Windows so backslash path separators in the override
        # (e.g. C:\...\python.exe) survive the split.
        return shlex.split(override, posix=(os.name != "nt"))
    return ["uv", "run", "--no-sync", "--with", "invoke", "python", "-m", "invoke"]


def run_tracked(
    argv: Sequence[str],
    *,
    journal: Journal | None = None,
    worktree: str | None = None,
    echo: bool = True,
) -> int:
    """Run ``inv <argv>`` as a journaled job under a pty; return its exit code.

    Records a row in the shared journal, tees the child's whole terminal
    output to a per-job logfile, and updates the row on exit. This is the one
    entrypoint both the CLI wrapper and the web app funnel through.

    When ``SECANTUS_OPSBOARD_JOB_ID`` is set (the web app pre-creates the row so
    it knows the id synchronously — no flaky pid-poll race), this run adopts
    that row instead of creating a new one.
    """
    argv = list(argv)
    worktree = worktree or os.getcwd()
    task = argv[0] if argv else "?"
    target = infer_target(task, argv)
    journal = journal or Journal()

    log_dir = default_log_dir()
    log_dir.mkdir(parents=True, exist_ok=True)

    # Pop so a nested grandchild (the invoke task) never inherits it.
    preassigned = os.environ.pop("SECANTUS_OPSBOARD_JOB_ID", None)
    if preassigned is not None:
        job_id = int(preassigned)
        journal.set_host_pid(job_id, os.getpid())
    else:
        job_id = journal.create(
            target=target,
            task=task,
            argv=argv,
            worktree=worktree,
            host_pid=os.getpid(),
        )
    log_path = log_dir / f"job-{job_id}.log"
    journal.set_log_path(job_id, str(log_path))

    cmd = [*_invoke_prefix(), *argv]
    try:
        code = _run_child(cmd, cwd=worktree, log_path=log_path, echo=echo)
    except BaseException:
        journal.finish(job_id, 1)
        raise
    journal.finish(job_id, code)
    return code


def _run_child(cmd: Sequence[str], *, cwd: str, log_path: Path, echo: bool) -> int:
    """Run ``cmd``, teeing its output to ``log_path``. pty where available."""
    if _pty is not None:
        return _run_pty(cmd, cwd=cwd, log_path=log_path, echo=echo)
    return _run_pipe(cmd, cwd=cwd, log_path=log_path, echo=echo)


def _run_pipe(cmd: Sequence[str], *, cwd: str, log_path: Path, echo: bool) -> int:
    """No-pty fallback (Windows): capture via a pipe and tee to the logfile.

    Loses terminal colour but keeps the journal + live-tailable log intact.
    """
    with open(log_path, "wb") as logf:
        proc = subprocess.Popen(
            list(cmd),
            cwd=cwd,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            close_fds=True,
        )
        assert proc.stdout is not None
        for chunk in iter(lambda: proc.stdout.read(65536), b""):
            logf.write(chunk)
            logf.flush()
            if echo:
                sys.stdout.buffer.write(chunk)
                sys.stdout.buffer.flush()
        proc.wait()
    return proc.returncode


def _run_pty(cmd: Sequence[str], *, cwd: str, log_path: Path, echo: bool) -> int:
    """Run ``cmd`` with its stdout/stderr on a pty; tee output to ``log_path``.

    The pty makes child tools believe they're on a terminal (colour, progress
    bars), and mirrors everything to both the real stdout and the logfile the
    UI tails. SIGINT/SIGTERM are forwarded to the child's process group so a
    Ctrl-C (or a UI cancel that signals this wrapper) stops the whole tree.
    """
    master, slave = _pty.openpty()
    on_main = threading.current_thread() is threading.main_thread()
    old_handlers: dict[int, object] = {}

    with open(log_path, "wb") as logf:
        proc = subprocess.Popen(
            list(cmd),
            cwd=cwd,
            # The pty drives stdout/stderr ONLY (so child tools believe they're
            # on a terminal → colour/progress). stdin is INHERITED, never the
            # pty slave: giving a downstream `invoke` task a pty-slave stdin made
            # it think it owned a foreground tty and crash in os.tcgetpgrp()
            # ("Inappropriate ioctl for device") under CI. Inherited stdin is a
            # non-tty in CI (invoke skips the tty path) and the real terminal
            # for an interactive `./inv`.
            stdout=slave,
            stderr=slave,
            close_fds=True,
            start_new_session=True,  # own process group → killpg reaches the tree
        )
        # NOTE: the parent deliberately keeps `slave` OPEN for the whole read
        # loop (it is closed in the `finally` below). Closing it here — so the
        # child holds the only slave fd — makes the pty signal EOF the moment
        # the child exits, and on macOS/BSD that `read()` returns EIO and
        # *discards whatever is still buffered*. A child that finishes before
        # the parent reaches its first read therefore loses ALL of its output
        # (measured: 20/20 runs when the parent is descheduled by only 50ms,
        # 0/20 when it wins the race — which is why this only reddened macOS CI
        # under xdist load while Linux, which delivers buffered bytes before
        # EIO, stayed green). Holding the slave open means the pty never sees
        # "all writers gone", so nothing is dropped; the loop below instead
        # terminates on child-exit + drained-buffer.

        def _forward(signum: int, _frame: object) -> None:
            with contextlib.suppress(ProcessLookupError):
                os.killpg(proc.pid, signum)

        if on_main:
            for sig in (signal.SIGINT, signal.SIGTERM):
                old_handlers[sig] = signal.signal(sig, _forward)

        try:
            # Because the parent holds the slave open (above), `read()` would
            # block forever after the child exits rather than reporting EOF —
            # so poll for readability instead and stop once the child is gone
            # AND the pty buffer has gone quiet. Anything the child wrote before
            # exiting is already buffered, so a timed-out select with the child
            # reaped means everything has been drained.
            while True:
                readable, _, _ = select.select([master], [], [], 0.05)
                if readable:
                    try:
                        data = os.read(master, 65536)
                    except OSError:  # slave side genuinely gone
                        break
                    if not data:
                        break
                    logf.write(data)
                    logf.flush()
                    if echo:
                        sys.stdout.buffer.write(data)
                        sys.stdout.buffer.flush()
                    continue
                if proc.poll() is not None:
                    # The child is gone — but it may have written its output
                    # AND exited in the window between the select() above
                    # reporting "nothing readable" and this poll(). Those
                    # bytes are sitting in the pty buffer, and breaking here
                    # discards them: the job row finishes with exit 0 while
                    # its logfile is empty, which is the "(no output yet)"
                    # log box on a completed job that reddened macOS CI.
                    #
                    # A timed-out select does NOT imply the buffer is drained
                    # (the comment that used to say so was the bug). Drain
                    # with non-blocking selects until the pty is genuinely
                    # quiet, then leave. A fast child that writes once and
                    # exits is exactly the shape that loses the race.
                    while True:
                        readable, _, _ = select.select([master], [], [], 0)
                        if not readable:
                            break
                        try:
                            data = os.read(master, 65536)
                        except OSError:
                            break
                        if not data:
                            break
                        logf.write(data)
                        logf.flush()
                        if echo:
                            sys.stdout.buffer.write(data)
                            sys.stdout.buffer.flush()
                    break
        finally:
            if on_main:
                for sig, handler in old_handlers.items():
                    signal.signal(sig, handler)  # type: ignore[arg-type]
            os.close(slave)
            os.close(master)
            proc.wait()
    return proc.returncode


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
