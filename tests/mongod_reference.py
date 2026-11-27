"""The reference mongod: start a throwaway one, stop it, and say so loudly
when it died on its own.

Moved out of `tests/test_mongod_differential.py` (2026-10-21) so the Rust
MongoDB server's backlog tests can start their reference without importing
that file, which imports the deprecated Python server (`secantus`). The
differential file re-imports every name from here.
"""

from __future__ import annotations

import contextlib
import os
import re
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from collections.abc import Iterator, Mapping, Sequence
from pathlib import Path

import pytest
from pymongo import MongoClient

MONGOD = shutil.which("mongod")
# `python_tasks._PROBE_TMP_PREFIX`; tests/test_clean_pytest_tmp.py pins the match.
MONGOD_TMP_PREFIX = "secantus-probe-"


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


#: Keep a spawned oracle out of the user's face on Windows.
#:
#: A console executable started with no creation flags allocates its own
#: console when the parent has none, so under ``-n auto`` every xdist worker's
#: mongod popped a terminal window on the desktop -- dozens of them flashing
#: through a suite run. ``CREATE_NO_WINDOW`` keeps the child headless; its
#: output is already going to a file, so nothing is lost. No-op off Windows.
_NO_CONSOLE: dict[str, int] = (
    {"creationflags": subprocess.CREATE_NO_WINDOW} if sys.platform == "win32" else {}
)


def _mongod_log(dbpath: str) -> Path:
    """Where this mongod's captured output lives, inside its own dbpath."""
    return Path(dbpath) / "mongod.log"


def _stop_mongod(proc: subprocess.Popen, dbpath: str) -> None:
    """Shut the oracle down, and make an unexpected death LOUD.

    ``proc.terminate()`` on an already-dead process is a no-op and
    ``wait()``'s status was previously discarded, so a mongod that aborted
    mid-module was indistinguishable from one we stopped ourselves -- while
    every comparison made after it died was against nothing at all. If the
    process exited on its own, keep the log (the dbpath is about to be
    deleted) and raise with its tail.
    """
    died_on_its_own = proc.poll() is not None
    returncode = proc.poll()
    if not died_on_its_own:
        proc.terminate()
        with contextlib.suppress(subprocess.TimeoutExpired):
            proc.wait(timeout=30)
        shutil.rmtree(dbpath, ignore_errors=True)
        return

    log = _mongod_log(dbpath)
    tail = ""
    kept: Path | None = None
    if log.exists():
        with contextlib.suppress(OSError):
            tail = log.read_text(encoding="utf-8", errors="replace")[-2000:]
        with contextlib.suppress(OSError):
            kept = Path(tempfile.gettempdir()) / f"mongod-crash-{os.getpid()}-{proc.pid}.log"
            shutil.copyfile(log, kept)
    shutil.rmtree(dbpath, ignore_errors=True)
    raise RuntimeError(
        f"the reference mongod exited on its own (returncode={returncode}) -- "
        "it was not stopped by this harness, so any comparison made after it "
        "died measured nothing. This is a crash of the oracle, not a test "
        f"failure.\nlog kept at: {kept or '(none captured)'}\n"
        f"--- last of mongod's output ---\n{tail or '(empty)'}"
    )


class _LostPortRace(RuntimeError):
    """Another process bound the port between `_free_port()` and our mongod."""


def _start_mongod(
    env: Mapping[str, str] | None = None,
    extra_args: Sequence[str] = (),
) -> tuple[subprocess.Popen, str, str]:
    """A throwaway standalone mongod of OUR OWN: ``(proc, uri, dbpath)``.

    `_free_port()` has to close its probe socket before mongod binds, so
    another xdist worker can take the port first; ours then exits, and
    `_assert_owns_port` refuses to go on. That refusal used to be the end of
    it: thirty errors in one CI lane on 2026-10-12, for a race that says
    nothing about either server. The loser is reaped and another port is
    tried. Four lost in a row is still an error, and never a comparison
    against somebody else's mongod.
    """
    lost: _LostPortRace | None = None
    for _ in range(4):
        try:
            return _start_mongod_once(env, extra_args)
        except _LostPortRace as exc:
            lost = exc
    assert lost is not None
    raise lost


def _start_mongod_once(
    env: Mapping[str, str] | None = None,
    extra_args: Sequence[str] = (),
) -> tuple[subprocess.Popen, str, str]:
    """Spawn a throwaway standalone mongod; return ``(proc, uri, dbpath)``.

    Shared by the module fixture below, by the per-zone local-time cases at
    the end of this file (identical readiness handling, a DIFFERENT process
    environment), and by the ``--noscripting`` fixture (same again, different
    ARGV) -- ``extra_args`` is appended to the command line for that last one.
    The caller owns the teardown.
    """
    # Named like a probe store, with this process's pid, so the sweep at the
    # next session's start (`python_tasks._sweep_stale_probe_tmp`) reaps it
    # if this process dies without its teardown. The old
    # `differential-mongod-` name carried no owner and was never reaped: one
    # such dbpath held a 427 GB mongod.log (2026-11-06).
    tmp = tempfile.mkdtemp(prefix=f"{MONGOD_TMP_PREFIX}{os.getpid()}-mongod-")
    port = _free_port()
    # Keep the oracle's own diagnostics. These used to go to DEVNULL, which
    # made a mongod crash unreadable after the fact: one really did abort on
    # this box (2026-09-22, an application-defined 0xE0000001 -- mongod's own
    # fatal-assertion path), and the only surviving artefact was a minidump
    # with no log beside it. A reference server that dies mid-module
    # invalidates every comparison after it, so its last words are worth a
    # file handle.
    log = subprocess.DEVNULL
    with contextlib.suppress(OSError):
        log = open(_mongod_log(tmp), "wb")  # noqa: SIM115 - closed with the proc
    proc = subprocess.Popen(
        [MONGOD, "--port", str(port), "--dbpath", tmp, "--quiet", *extra_args],
        stdout=log,
        stderr=subprocess.STDOUT,
        env=None if env is None else dict(env),
        **_NO_CONSOLE,
    )
    if log is not subprocess.DEVNULL:
        log.close()  # the child holds its own descriptor
    uri = f"mongodb://127.0.0.1:{port}/"
    # Everything from here to the `return` runs under `_reap_on_failure`: the
    # caller only gets a `finally` to clean up with once it HOLDS the handle, so
    # any exit before the return -- the skip below, the race guard's raise, a
    # KeyboardInterrupt -- would otherwise leave a live mongod and its dbpath
    # behind. `pytest.skip` raises `Skipped`, which is a `BaseException` and not
    # an `Exception`, so the guard has to catch the wider one.
    with _reap_on_failure(proc, tmp):
        if not _wait_owned(proc, port, 30):
            pytest.skip("mongod did not become ready")
        _assert_owns_port(proc, port)
    return proc, uri, tmp


@contextlib.contextmanager
def _reap_on_failure(proc: subprocess.Popen, dbpath: str) -> Iterator[None]:
    """Terminate `proc` and drop `dbpath` if the body doesn't complete."""
    try:
        yield
    except BaseException:
        proc.terminate()
        with contextlib.suppress(Exception):
            proc.wait(timeout=30)
        shutil.rmtree(dbpath, ignore_errors=True)
        raise


def _answering_pid(port: int) -> int | None:
    """The pid of the mongod that answers on `port`, or `None` when nothing
    does. `serverStatus` reports it whichever role the member has.

    `directConnection` matters for the replica-set fixture: an UNINITIATED
    member fails server selection through normal discovery, so the readiness
    probe timed out and the whole step-down comparison skipped with "mongod
    did not become ready" rather than running."""
    client = MongoClient(
        f"mongodb://127.0.0.1:{port}/", serverSelectionTimeoutMS=500, directConnection=True
    )
    try:
        status = client.admin.command(
            {"serverStatus": 1, "repl": 0, "metrics": 0, "locks": 0, "wiredTiger": 0}
        )
        return int(status["pid"])
    except Exception:  # noqa: BLE001 - nothing answering yet
        return None
    finally:
        client.close()


def _wait_owned(proc: subprocess.Popen, port: int, timeout: float) -> bool:
    """Wait until the mongod answering on `port` is `proc` itself.

    Returns `True` once it is, and `False` when `proc` exited with nothing
    answering (it refused to start: its own log says why) or `timeout`
    passed with nothing answering. Raises `_LostPortRace` when a DIFFERENT
    mongod answers: `_free_port()` closes its probe socket before `proc`
    binds, and mongod opens WiredTiger BEFORE it binds, so another worker's
    mongod can take the port while `proc` is still starting. `proc` has not
    exited yet at that point, so "it is still alive" proves nothing; the pid
    the server reports is the identity (2026-11-27: 108 setup errors, one
    xdist worker's `replSetInitiate` reaching a standalone mongod).
    """
    deadline = time.monotonic() + timeout
    foreign: int | None = None
    while time.monotonic() < deadline:
        pid = _answering_pid(port)
        if pid == proc.pid:
            return True
        if pid is not None:
            foreign = pid
        if proc.poll() is not None:
            # Dead. A server still answering on the port took it from us;
            # nothing answering means mongod itself refused to start.
            foreign = _answering_pid(port)
            if foreign is not None and foreign != proc.pid:
                raise _LostPortRace(
                    f"mongod on port {port} exited during startup and pid {foreign} "
                    "holds the port: a ping would have reached a DIFFERENT server, "
                    "so this gate would have compared against a mongod it does not own"
                )
            return False
        time.sleep(0.25)
    if foreign is not None:
        raise _LostPortRace(
            f"pid {foreign} answers on port {port} and mongod pid {proc.pid} never did"
        )
    return False


def _assert_owns_port(proc: subprocess.Popen, port: int) -> None:
    # A ping that succeeds is NOT proof it reached the mongod spawned above.
    # `_free_port()` closes its probe socket before this child binds, so
    # under `-n auto` another worker can take the port; ours then exits
    # "address already in use" while the ping lands on THEIRS. This gate
    # would go on measuring a server it does not own -- and, being a
    # differential gate, would report agreement it never actually tested.
    # "Ours has not exited" is not an answer either (it is still opening
    # WiredTiger when the other one answers), so the identity is checked
    # outright: the pid the answering server reports must be `proc`'s.
    # (The same race broke `pg-oracle` on 2026-09-09; the `secantusd-pg`
    # harness fixed it properly by binding :0 and reading the port back,
    # which mongod gives us no way to do.)
    pid = _answering_pid(port)
    if proc.poll() is not None or pid != proc.pid:
        raise _LostPortRace(
            f"mongod pid {proc.pid} on port {port} is not the server answering there "
            f"(pid {pid}); a ping that succeeded reached a DIFFERENT server, so this "
            "gate would have compared against a mongod it does not own"
        )


def _host_port(uri: str) -> tuple[str, int]:
    from pymongo.uri_parser import parse_uri

    return parse_uri(uri)["nodelist"][0]


# mongod renders a wrong-type error's expected-type list in an order that
# changes between patch releases (8.2.1, 8.2.11 and 8.2.12 all differ, and
# 8.2.12 orders two fields of one command differently). It is a set; compare
# it as one. Everything else in the message is still compared exactly.
_TYPE_LIST = re.compile(r"(expected types \\?')\[([^\]]*)\]")


def sort_type_lists(message: str) -> str:
    """`message` with each "expected types '[...]'" list sorted."""

    def repl(m: re.Match) -> str:
        items = sorted(part.strip() for part in m.group(2).split(",") if part.strip())
        return f"{m.group(1)}[{', '.join(items)}]"

    return _TYPE_LIST.sub(repl, message)
