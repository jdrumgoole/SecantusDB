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
    tmp = tempfile.mkdtemp(prefix="differential-mongod-")
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
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            try:
                # `directConnection` matters for the replica-set fixture: an
                # UNINITIATED member fails server selection through normal
                # discovery, so the readiness probe timed out and the whole
                # step-down comparison skipped with "mongod did not become
                # ready" rather than running.
                MongoClient(uri, serverSelectionTimeoutMS=500, directConnection=True).admin.command(
                    "ping"
                )
                break
            except Exception:  # noqa: BLE001 - polling for readiness
                time.sleep(0.25)
        else:
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


def _assert_owns_port(proc: subprocess.Popen, port: int) -> None:
    # A ping that succeeds is NOT proof it reached the mongod spawned above.
    # `_free_port()` closes its probe socket before this child binds, so
    # under `-n auto` another worker can take the port; ours then exits
    # "address already in use" while the ping lands on THEIRS. This gate
    # would go on measuring a server it does not own -- and, being a
    # differential gate, would report agreement it never actually tested.
    # A child that lost the race is long gone by the time a ping succeeds,
    # so its exit is the reliable tell. Fail loudly rather than measure the
    # wrong server. (The same race broke `pg-oracle` on 2026-09-09; the
    # `secantusd-pg` harness fixed it properly by binding :0 and reading the
    # port back, which mongod gives us no way to do.)
    if proc.poll() is not None:
        raise _LostPortRace(
            f"mongod on port {port} exited during startup; the ping that "
            "succeeded reached a DIFFERENT server, so this gate would have "
            "compared against a mongod it does not own"
        )


def _host_port(uri: str) -> tuple[str, int]:
    from pymongo.uri_parser import parse_uri

    return parse_uri(uri)["nodelist"][0]
