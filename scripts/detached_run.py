#!/usr/bin/env python3
"""Run a long command detached from this shell's process group.

Claude Code (and any supervisor that reaps a backgrounded shell) kills the
whole process group it spawned, which takes a multi-minute pytest / gauge run
with it -- observed three times in one session, twice at ~90% completion. A
child started with ``start_new_session=True`` gets its own session and process
group, so reaping the launching shell leaves it running.

Start a run, then poll it:

    uv run python scripts/detached_run.py start --name suite --cwd . -- \
        python -m pytest -q
    uv run python scripts/detached_run.py status --name suite
    uv run python scripts/detached_run.py wait --name suite --timeout 3600
    uv run python scripts/detached_run.py stop --name suite

State for ``<name>`` lives in ``--state-dir`` (default ``.detached-runs``):
``<name>.log`` (merged stdout/stderr), ``<name>.json`` (pid, argv, cwd, start
time, and the exit code once it finishes).
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

DEFAULT_STATE_DIR = Path(".detached-runs")


def _state_path(state_dir: Path, name: str) -> Path:
    return state_dir / f"{name}.json"


def _log_path(state_dir: Path, name: str) -> Path:
    return state_dir / f"{name}.log"


def _read_state(state_dir: Path, name: str) -> dict[str, object]:
    path = _state_path(state_dir, name)
    if not path.exists():
        raise SystemExit(f"no run named {name!r} under {state_dir}")
    return json.loads(path.read_text())


def _write_state(state_dir: Path, name: str, state: dict[str, object]) -> None:
    _state_path(state_dir, name).write_text(json.dumps(state, indent=2) + "\n")


_WINDOWS = os.name == "nt"


def _alive(pid: int) -> bool:
    if _WINDOWS:
        return _alive_windows(pid)
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def _alive_windows(pid: int) -> bool:
    """`os.kill(pid, 0)` is not a liveness probe on Windows -- it raises
    ``OSError [WinError 87]`` for a live process -- so ask the kernel."""
    import ctypes
    from ctypes import wintypes

    process_query_limited_information = 0x1000
    still_active = 259
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.OpenProcess.restype = wintypes.HANDLE
    handle = kernel32.OpenProcess(process_query_limited_information, False, pid)
    if not handle:
        return False
    try:
        code = wintypes.DWORD()
        if not kernel32.GetExitCodeProcess(handle, ctypes.byref(code)):
            return False
        return code.value == still_active
    finally:
        kernel32.CloseHandle(handle)


def _detach_kwargs() -> dict[str, object]:
    """How to start the supervisor outside the launcher's reach.

    POSIX: a new session, so a process-GROUP kill misses it. Windows has no
    process groups in that sense (`start_new_session` is ignored there): a new
    process group plus DETACHED_PROCESS leaves the console, and breaking away
    from the launcher's job object is what survives a job-wide kill -- when
    the job permits it, which the caller learns by trying.
    """
    if not _WINDOWS:
        return {"start_new_session": True}
    flags = subprocess.CREATE_NEW_PROCESS_GROUP | subprocess.DETACHED_PROCESS
    return {"creationflags": flags | subprocess.CREATE_BREAKAWAY_FROM_JOB, "_fallback": flags}


def _resolve_command(argv: list[str]) -> list[str]:
    """``argv`` with its program resolved through PATH.

    On Windows, CreateProcess looks in the directory of the PARENT's own
    executable before PATH -- and under a venv the parent is the base
    interpreter the venv launcher re-executes, so a bare `python` ran the
    interpreter WITHOUT the venv's packages ("No module named pytest").
    `shutil.which` searches PATH, which `uv run` puts the venv at the front of.
    """
    if not argv:
        return argv
    found = shutil.which(argv[0])
    return [found, *argv[1:]] if found else argv


def _reap(state_dir: Path, name: str, state: dict[str, object]) -> dict[str, object]:
    """Record the exit code once the child is gone, then return the state."""
    if state.get("exit_code") is not None:
        return state
    pid = int(state["pid"])  # type: ignore[arg-type]
    if _alive(pid):
        return state
    # The child is not ours to waitpid() -- it was reparented when its
    # launcher exited -- so the exit code comes from the wrapper's own file.
    code_file = state_dir / f"{name}.exit"
    state["exit_code"] = int(code_file.read_text().strip()) if code_file.exists() else -1
    state["finished_at"] = datetime.now(timezone.utc).isoformat()
    _write_state(state_dir, name, state)
    return state


def cmd_start(args: argparse.Namespace) -> int:
    if not args.command:
        raise SystemExit("give the command after `--`")
    state_dir: Path = args.state_dir
    state_dir.mkdir(parents=True, exist_ok=True)

    existing = _state_path(state_dir, args.name)
    if existing.exists():
        prior = _reap(state_dir, args.name, json.loads(existing.read_text()))
        if prior.get("exit_code") is None:
            raise SystemExit(
                f"{args.name!r} is still running (pid {prior['pid']}); "
                "stop it or pick another --name"
            )

    log = _log_path(state_dir, args.name)
    exit_file = state_dir / f"{args.name}.exit"
    exit_file.unlink(missing_ok=True)

    # The supervisor is a PYTHON process, never `sh -c`. An intervening shell
    # breaks macOS's permission association for the whole subtree: with
    # `/bin/sh` in the middle, every child's connection to a Postgres.app
    # server times out, which silently skipped 825 differential tests behind a
    # green run. Measured 2026-09-17 -- direct exec connects, `sh -c` does not.
    popen_args = [
        sys.executable,
        str(Path(__file__).resolve()),
        "_supervise",
        str(exit_file),
        *_resolve_command(args.command),
    ]
    detach = _detach_kwargs()
    fallback = detach.pop("_fallback", None)
    common: dict[str, object] = {
        "cwd": str(args.cwd.resolve()),
        "stdout": subprocess.DEVNULL,
        "stderr": subprocess.DEVNULL,
        "stdin": subprocess.DEVNULL,
        "env": {**os.environ, "_DETACHED_RUN_LOG": str(log.resolve())},
    }
    try:
        proc = subprocess.Popen(popen_args, **common, **detach)  # type: ignore[call-overload]
    except OSError:
        if fallback is None:
            raise
        # The launcher's job forbids breakaway; detach as far as it allows.
        proc = subprocess.Popen(popen_args, **common, creationflags=fallback)  # type: ignore[call-overload]

    _write_state(
        state_dir,
        args.name,
        {
            "name": args.name,
            "pid": proc.pid,
            "argv": args.command,
            "cwd": str(args.cwd.resolve()),
            "log": str(log.resolve()),
            "started_at": datetime.now(timezone.utc).isoformat(),
            "exit_code": None,
        },
    )
    print(f"started {args.name!r} pid {proc.pid}; log {log}")
    return 0


def cmd_status(args: argparse.Namespace) -> int:
    state = _reap(args.state_dir, args.name, _read_state(args.state_dir, args.name))
    code = state.get("exit_code")
    running = code is None
    print(
        f"{state['name']}: {'running' if running else f'finished rc={code}'} (pid {state['pid']})"
    )
    if args.tail:
        log = Path(str(state["log"]))
        if log.exists():
            lines = log.read_text(errors="replace").splitlines()
            for line in lines[-args.tail :]:
                print(line)
    return 0 if running or code == 0 else 1


def cmd_wait(args: argparse.Namespace) -> int:
    deadline = time.monotonic() + args.timeout
    while True:
        state = _reap(args.state_dir, args.name, _read_state(args.state_dir, args.name))
        code = state.get("exit_code")
        if code is not None:
            print(f"{state['name']} finished rc={code}")
            return 0 if code == 0 else 1
        if time.monotonic() >= deadline:
            print(f"{state['name']} still running after {args.timeout}s")
            return 2
        time.sleep(args.interval)


def _taskkill(pid: int) -> str:
    """Windows has no process group to signal, so end the supervisor and its
    whole tree -- and REPORT what happened.

    The output used to go to DEVNULL with `check=False`, so a kill that failed
    left no trace at all and `stop` still claimed success. A stop that does not
    stop anything is exactly the kind of thing that has to be loud.
    """
    done = subprocess.run(
        ["taskkill", "/T", "/F", "/PID", str(pid)],
        capture_output=True,
        text=True,
        check=False,
    )
    if done.returncode == 0:
        return ""
    detail = (done.stderr or done.stdout or "").strip().replace("\n", " ")
    return f"taskkill exited {done.returncode}: {detail}"


def cmd_stop(args: argparse.Namespace) -> int:
    state = _read_state(args.state_dir, args.name)
    pid = int(state["pid"])  # type: ignore[arg-type]
    if not _alive(pid):
        print(f"{args.name} is not running")
        return 0
    trouble = ""
    if _WINDOWS:
        trouble = _taskkill(pid)
    else:
        os.killpg(os.getpgid(pid), signal.SIGTERM)
    for _ in range(50):
        if not _alive(pid):
            break
        time.sleep(0.2)
    else:
        # Still there after ten seconds: escalate, then look again. POSIX has
        # SIGKILL; Windows has only a second forced kill, which at least
        # distinguishes "the first one was lost" from "this will not die".
        if _WINDOWS:
            trouble = _taskkill(pid) or trouble
        else:
            os.killpg(os.getpgid(pid), signal.SIGKILL)
        for _ in range(25):
            if not _alive(pid):
                break
            time.sleep(0.2)
        else:
            # Do NOT claim to have stopped it. A caller that believes this and
            # starts a replacement gets two of whatever it was running.
            note = f" ({trouble})" if trouble else ""
            print(f"{args.name} (pid {pid}) did not stop{note}")
            return 1
    if trouble:
        # It died, but the first kill reported something -- say so rather than
        # leave a failed command silently behind a success.
        print(f"stopped {args.name} (pid {pid}) after a retry ({trouble})")
        return 0
    print(f"stopped {args.name} (pid {pid})")
    return 0


def _supervise(argv: list[str]) -> int:
    """Run the real command and record its exit code.

    Exists so the launcher can exit while something still knows the child's
    fate: once a process is reparented, the caller can no longer waitpid() it.
    A Python supervisor rather than a shell one -- see the note in cmd_start.
    """
    exit_file = Path(argv[0])
    log_path = os.environ.get("_DETACHED_RUN_LOG")
    with open(log_path, "wb") if log_path else open(os.devnull, "wb") as handle:
        proc = subprocess.Popen(
            argv[1:], stdout=handle, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL
        )
        code = proc.wait()
    exit_file.write_text(str(code))
    return code


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--state-dir", type=Path, default=DEFAULT_STATE_DIR, help="where run state lives"
    )
    sub = parser.add_subparsers(dest="action", required=True)

    start = sub.add_parser("start", help="launch a detached command")
    start.add_argument("--name", required=True)
    start.add_argument("--cwd", type=Path, default=Path("."))
    start.add_argument("command", nargs=argparse.REMAINDER)
    start.set_defaults(func=cmd_start)

    status = sub.add_parser("status", help="is it still running?")
    status.add_argument("--name", required=True)
    status.add_argument("--tail", type=int, default=0, help="also print the last N log lines")
    status.set_defaults(func=cmd_status)

    wait = sub.add_parser("wait", help="block until it finishes")
    wait.add_argument("--name", required=True)
    wait.add_argument("--timeout", type=float, default=3600.0)
    wait.add_argument("--interval", type=float, default=10.0)
    wait.set_defaults(func=cmd_wait)

    stop = sub.add_parser("stop", help="terminate the run")
    stop.add_argument("--name", required=True)
    stop.set_defaults(func=cmd_stop)

    raw = list(sys.argv[1:] if argv is None else argv)
    if raw and raw[0] == "_supervise":
        return _supervise(raw[1:])
    args = parser.parse_args(argv)
    if args.action == "start" and args.command and args.command[0] == "--":
        args.command = args.command[1:]
    try:
        return int(args.func(args))
    except KeyboardInterrupt:
        print("interrupted", file=sys.stderr)
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
