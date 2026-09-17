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


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


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

    # A tiny wrapper records the exit code, which the caller cannot waitpid()
    # for once this launcher process has gone away.
    quoted = " ".join(_shell_quote(part) for part in args.command)
    wrapper = f'{quoted}; printf "%s" "$?" > {_shell_quote(str(exit_file.resolve()))}'

    with log.open("wb") as handle:
        proc = subprocess.Popen(
            ["/bin/sh", "-c", wrapper],
            cwd=str(args.cwd.resolve()),
            stdout=handle,
            stderr=subprocess.STDOUT,
            stdin=subprocess.DEVNULL,
            start_new_session=True,  # the whole point: our own process group
        )

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


def _shell_quote(part: str) -> str:
    if part and all(c.isalnum() or c in "-_./=:" for c in part):
        return part
    return "'" + part.replace("'", "'\\''") + "'"


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


def cmd_stop(args: argparse.Namespace) -> int:
    state = _read_state(args.state_dir, args.name)
    pid = int(state["pid"])  # type: ignore[arg-type]
    if not _alive(pid):
        print(f"{args.name} is not running")
        return 0
    os.killpg(os.getpgid(pid), signal.SIGTERM)
    for _ in range(50):
        if not _alive(pid):
            break
        time.sleep(0.2)
    else:
        os.killpg(os.getpgid(pid), signal.SIGKILL)
    print(f"stopped {args.name} (pid {pid})")
    return 0


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
