"""``scripts/detached_run.py`` must outlive a kill of the launching shell.

A supervisor that reaps the shell it spawned signals the whole process
group, which is what killed three multi-minute suite runs in one session.
The helper exists so the real work sits in its OWN process group; the test
that matters is therefore the one that signals the launcher's group and
asserts the child is still there.
"""

from __future__ import annotations

import importlib.util
import json
import os
import signal
import subprocess
import sys
import time
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parent.parent / "scripts" / "detached_run.py"


def _run(state_dir: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--state-dir", str(state_dir), *args],
        capture_output=True,
        text=True,
    )


def _wait_for(predicate, timeout: float = 20.0, interval: float = 0.2) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return False


def _alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX")
def test_child_runs_in_its_own_process_group(tmp_path: Path) -> None:
    state = tmp_path / "runs"
    out = _run(state, "start", "--name", "pg", "--", "sleep", "5")
    assert out.returncode == 0, out.stderr
    pid = json.loads((state / "pg.json").read_text())["pid"]
    try:
        assert os.getpgid(pid) != os.getpgid(os.getpid())
        assert os.getpgid(pid) == pid, "the child should lead its own group"
    finally:
        _run(state, "stop", "--name", "pg")


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX")
def test_child_survives_a_kill_of_the_launching_group(tmp_path: Path) -> None:
    """The regression this script exists for."""
    state = tmp_path / "runs"
    marker = tmp_path / "finished"
    out = _run(
        state,
        "start",
        "--name",
        "survivor",
        "--",
        "/bin/sh",
        "-c",
        f"sleep 3; echo done > {marker}",
    )
    assert out.returncode == 0, out.stderr
    pid = json.loads((state / "survivor.json").read_text())["pid"]

    # Signal OUR whole process group, the way a reaped shell is killed.
    # The child is in a different group, so it must be untouched.
    os.killpg(os.getpgid(os.getpid()), signal.SIGCONT)
    os.kill(pid, 0)  # raises if the signal reached it

    assert _wait_for(marker.exists), "detached child did not finish"
    assert marker.read_text().strip() == "done"
    assert _wait_for(lambda: not _alive(pid))
    status = _run(state, "status", "--name", "survivor")
    assert "finished rc=0" in status.stdout, status.stdout


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX")
def test_failure_exit_code_is_recorded(tmp_path: Path) -> None:
    state = tmp_path / "runs"
    assert _run(state, "start", "--name", "boom", "--", "/bin/sh", "-c", "exit 7").returncode == 0
    wait = _run(state, "wait", "--name", "boom", "--timeout", "20", "--interval", "0.2")
    assert wait.returncode == 1, wait.stdout
    assert "rc=7" in wait.stdout


@pytest.mark.skipif(os.name != "posix", reason="process groups are POSIX")
def test_a_second_start_under_a_live_name_is_refused(tmp_path: Path) -> None:
    state = tmp_path / "runs"
    assert _run(state, "start", "--name", "dup", "--", "sleep", "5").returncode == 0
    try:
        again = _run(state, "start", "--name", "dup", "--", "sleep", "5")
        assert again.returncode != 0
        assert "still running" in again.stderr
    finally:
        _run(state, "stop", "--name", "dup")


@pytest.mark.skipif(os.name != "posix", reason="process inspection is POSIX")
def test_no_shell_is_interposed_before_the_command(tmp_path: Path) -> None:
    """The run's parent must be python, never `/bin/sh`.

    macOS attributes a permission decision to the process tree, and an
    intervening `sh -c` loses the association: with one in the middle, every
    child's connection to a Postgres.app server timed out, so 825 differential
    tests SKIPPED behind a green run instead of failing (measured 2026-09-17).
    Exit-code capture is therefore done by a python supervisor, and this test
    is what stops a future 'simplification' putting the shell back.
    """
    state = tmp_path / "runs"
    out = tmp_path / "parent.txt"
    script = tmp_path / "who.py"
    script.write_text(
        "import os, pathlib, sys\n"
        f"pathlib.Path({str(out)!r}).write_text(open(f'/proc/{{os.getppid()}}/comm').read()"
        " if os.path.exists(f'/proc/{os.getppid()}/comm') else"
        " __import__('subprocess').run(['ps','-o','comm=','-p',str(os.getppid())],"
        " capture_output=True, text=True).stdout)\n"
    )
    assert _run(state, "start", "--name", "who", "--", sys.executable, str(script)).returncode == 0
    assert _wait_for(out.exists), "child never ran"
    parent = out.read_text().strip()
    assert Path(parent).name != "sh", f"a shell was interposed: {parent!r}"
    assert "python" in parent.lower(), f"unexpected supervisor: {parent!r}"


# --- Every platform. The tests above are POSIX-only by nature (process
# groups), which is how the helper shipped broken on Windows: `_alive` used
# `os.kill(pid, 0)` (WinError 87 there), `stop` used `os.killpg`, and a bare
# `python` resolved to the base interpreter instead of the venv's. None of
# that had a test that could run where it failed.


def _run_env(state_dir: Path, env: dict[str, str], *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--state-dir", str(state_dir), *args],
        capture_output=True,
        text=True,
        env=env,
    )


def _venv_first_env() -> dict[str, str]:
    """PATH with this interpreter's directory first, the way `uv run` sets it."""
    here = str(Path(sys.executable).parent)
    return {**os.environ, "PATH": here + os.pathsep + os.environ.get("PATH", "")}


def test_exit_code_is_recorded_on_every_platform(tmp_path: Path) -> None:
    state = tmp_path / "runs"
    env = _venv_first_env()
    start = _run_env(
        state, env, "start", "--name", "rc", "--", sys.executable, "-c", "raise SystemExit(7)"
    )
    assert start.returncode == 0, start.stderr
    wait = _run_env(state, env, "wait", "--name", "rc", "--timeout", "60", "--interval", "0.2")
    assert "rc=7" in wait.stdout, (wait.stdout, wait.stderr)
    assert wait.returncode == 1


def test_a_bare_python_is_the_venv_interpreter(tmp_path: Path) -> None:
    """`-- python -m pytest` must run the venv's python, found through PATH.

    On Windows a bare name is looked up in the PARENT's executable directory
    first, and under a venv that parent is the base interpreter -- so the run
    failed at once with "No module named pytest" (2026-09-18)."""
    state = tmp_path / "runs"
    env = _venv_first_env()
    code = "import sys; print('PREFIX=' + sys.prefix)"
    assert _run_env(state, env, "start", "--name", "py", "--", "python", "-c", code).returncode == 0
    _run_env(state, env, "wait", "--name", "py", "--timeout", "60", "--interval", "0.2")
    log = (state / "py.log").read_text()
    assert f"PREFIX={sys.prefix}" in log, log


def test_stop_ends_a_running_command(tmp_path: Path) -> None:
    state = tmp_path / "runs"
    env = _venv_first_env()
    sleeper = [sys.executable, "-c", "import time; time.sleep(120)"]
    assert _run_env(state, env, "start", "--name", "nap", "--", *sleeper).returncode == 0
    running = _run_env(state, env, "status", "--name", "nap")
    assert "running" in running.stdout, (running.stdout, running.stderr)
    stop = _run_env(state, env, "stop", "--name", "nap")
    assert stop.returncode == 0, (stop.stdout, stop.stderr)
    # `stop` records the end itself, so one `status` is enough: no waiting.
    # (This used to poll for 90s and still fail on Windows CI. The run WAS
    # stopped; `status` was crashing on an exit file the killed supervisor left
    # empty, which the stdout-only failure message could not show.)
    status = _run_env(state, env, "status", "--name", "nap")
    assert "finished" in status.stdout, (stop.stdout, stop.stderr, status.stdout, status.stderr)
    assert json.loads((state / "nap.json").read_text())["exit_code"] is not None


@pytest.mark.parametrize("left_behind", ["", "12", "not a number", None])
def test_status_survives_whatever_a_killed_supervisor_left(
    tmp_path: Path, left_behind: str | None
) -> None:
    """A supervisor killed while recording its child's exit leaves the exit
    file empty, partial or absent. `status` must still say the run finished:
    an empty file made it raise `ValueError` on every call, so a stopped run
    read as never finishing (Windows CI, 2026-10-07)."""
    state = tmp_path / "runs"
    state.mkdir()
    dead = subprocess.Popen([sys.executable, "-c", "pass"])
    dead.wait()
    (state / "nap.json").write_text(
        json.dumps(
            {"name": "nap", "pid": dead.pid, "log": str(state / "nap.log"), "exit_code": None}
        )
    )
    if left_behind is not None:
        (state / "nap.exit").write_text(left_behind)
    status = _run(state, "status", "--name", "nap")
    want = "rc=12" if left_behind == "12" else "rc=-1"
    assert f"finished {want}" in status.stdout, (status.stdout, status.stderr)
    # ...and the answer is recorded, so the next reader does not redo this.
    assert json.loads((state / "nap.json").read_text())["exit_code"] is not None


def test_a_kill_cannot_leave_a_half_written_file(tmp_path: Path) -> None:
    """Files are written beside their name and renamed into place, so the name
    holds the old content or the new one and never an empty file."""
    spec = importlib.util.spec_from_file_location("detached_run", SCRIPT)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    target = tmp_path / "nap.exit"
    target.write_text("old")
    real_replace = os.replace
    seen: list[str] = []

    def replace(src: object, dst: object) -> None:
        seen.append(target.read_text())  # what a reader sees mid-write
        real_replace(src, dst)  # type: ignore[arg-type]

    module.os.replace = replace
    try:
        module._write_atomic(target, "new")
    finally:
        module.os.replace = real_replace
    assert seen == ["old"]
    assert target.read_text() == "new"
    assert list(tmp_path.iterdir()) == [target]


def test_the_state_file_records_the_exit_without_a_status_call(tmp_path: Path) -> None:
    """A finished run's ``<name>.json`` says so by itself. It used to keep
    ``exit_code: null`` until something called ``status`` / ``wait``, so a
    reader of the file waited on a run that had ended hours before
    (2026-09-28)."""
    state = tmp_path / "runs"
    out = _run(state, "start", "--name", "quick", "--", sys.executable, "-c", "pass")
    assert out.returncode == 0, out.stderr
    deadline = time.monotonic() + 60
    recorded = None
    while time.monotonic() < deadline:
        recorded = json.loads((state / "quick.json").read_text()).get("exit_code")
        if recorded is not None:
            break
        time.sleep(0.2)
    assert recorded == 0
