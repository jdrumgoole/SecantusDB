"""Start and stop the bundled servers: in this process, or as child processes.

``MongoServer()`` runs the MongoDB server inside the Python process by
default, through the ``secantusdb._secantus_server`` extension.
``MongoServer(embedded=False)`` and ``PostgresServer()`` run the bundled
binaries as child processes.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import weakref
from collections import deque
from pathlib import Path
from types import TracebackType
from typing import Any

_BIN_DIR = Path(__file__).resolve().parent / "bin"

# Both binaries print one line naming the address they bound, e.g.
# "secantusd-mdb listening on 127.0.0.1:53211". Port 0 asks for an ephemeral
# port, so this line is the only way to learn which one was picked.
_LISTENING = re.compile(r"listening on (\S+):(\d+)")


class ServerStartError(RuntimeError):
    """The server could not start: it exited, never said it was listening,
    or (in-process) the extension is missing or refused to open the store."""


class ForkedServerError(RuntimeError):
    """An in-process server was used from a forked child process.

    ``fork()`` copies the server's Python object into the child, but none of
    its threads, so only the parent can stop it. The parent's server keeps
    serving, and the child can still connect to it through ``uri``.
    """


# The in-process servers that are running, so a forked child can disown them
# (see `_disown_in_child`). Weak: an unreferenced server is not kept alive.
_RUNNING_IN_PROCESS: weakref.WeakSet[MongoServer] = weakref.WeakSet()


def _disown_in_child() -> None:
    for server in list(_RUNNING_IN_PROCESS):
        server._disown_after_fork()
    _RUNNING_IN_PROCESS.clear()


if hasattr(os, "register_at_fork"):
    os.register_at_fork(after_in_child=_disown_in_child)


def binary_path(name: str) -> Path:
    """The path of a bundled binary: ``"secantusd-mdb"`` or ``"secantusd-pg"``."""
    if name not in ("secantusd-mdb", "secantusd-pg"):
        raise ValueError(f"unknown SecantusDB binary {name!r}")
    path = _BIN_DIR / name
    if not path.is_file():
        raise FileNotFoundError(
            f"{path} is missing: this SecantusDB install has no {name} binary for {sys.platform}"
        )
    return path


class _Server:
    _binary = ""

    def __init__(
        self,
        storage_path: str | os.PathLike[str] | None = None,
        *,
        port: int = 0,
        startup_timeout: float = 60.0,
    ) -> None:
        self._requested_storage = Path(storage_path) if storage_path is not None else None
        self._requested_port = port
        self._startup_timeout = startup_timeout
        self._proc: subprocess.Popen[str] | None = None
        self._tempdir: str | None = None
        self._output: deque[str] = deque(maxlen=200)
        self._ready = threading.Event()
        self._reader: threading.Thread | None = None
        self.storage_path: Path | None = None
        self.host = "127.0.0.1"
        self.port = 0

    def _args(self, storage: Path) -> list[str]:
        raise NotImplementedError

    def start(self) -> _Server:
        if self._proc is not None:
            raise RuntimeError("server already started")
        if self._requested_storage is None:
            self._tempdir = tempfile.mkdtemp(prefix="secantusdb-")
            self.storage_path = Path(self._tempdir)
        else:
            self.storage_path = self._requested_storage
            self.storage_path.mkdir(parents=True, exist_ok=True)
        self._proc = subprocess.Popen(
            [str(binary_path(self._binary)), *self._args(self.storage_path)],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            errors="replace",
        )
        # Drain the pipe for the server's whole life, not just until the
        # banner: a pipe nobody reads fills at 64 KiB and the server blocks.
        self._reader = threading.Thread(target=self._drain, daemon=True)
        self._reader.start()
        if not self._ready.wait(self._startup_timeout) or not self.port:
            self.stop()
            raise ServerStartError(
                f"{self._binary} did not start within {self._startup_timeout}s:\n"
                + "".join(self._output)
            )
        return self

    def _drain(self) -> None:
        assert self._proc is not None and self._proc.stdout is not None
        for line in self._proc.stdout:
            self._output.append(line)
            if not self._ready.is_set():
                match = _LISTENING.search(line)
                if match:
                    self.host, self.port = match.group(1), int(match.group(2))
                    self._ready.set()
        # EOF: the server exited. Wake a start() still waiting for the banner.
        self._ready.set()

    @property
    def output(self) -> str:
        """The server's most recent output lines, for diagnosing a failure."""
        return "".join(self._output)

    def stop(self) -> None:
        proc, self._proc = self._proc, None
        if proc is not None and proc.poll() is None:
            proc.terminate()
            try:
                proc.wait(timeout=30)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=10)
        if self._reader is not None:
            self._reader.join(timeout=5)
            self._reader = None
        if self._tempdir is not None:
            shutil.rmtree(self._tempdir, ignore_errors=True)
            self._tempdir = None

    def __enter__(self) -> _Server:
        return self.start()

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        self.stop()


class MongoServer(_Server):
    """The MongoDB-wire server, ``secantusd-mdb``.

    With no ``storage_path`` the data lives in a temporary directory that
    is removed on ``stop()``. ``port=0`` (the default) picks a free port.

    ``embedded=True`` (the default) runs the server inside this process:
    its threads serve a real loopback TCP port, and a driver connects to
    ``uri`` as it would to ``mongod``. A crash in the server (a WiredTiger
    panic, an abort) then ends this process too. ``embedded=False`` runs the
    bundled ``secantusd-mdb`` binary as a child process instead, which
    isolates the two. The server behaves the same either way: a single-node
    ``secantus`` replica set (change streams and transactions work), test
    commands off, TTL documents expired every 60 seconds.

    In-process, the server does not survive ``fork()`` into the child: a
    forked child can still connect to the parent's server through ``uri``,
    and leaving the parent's ``with`` block there does nothing, but
    ``stop()`` in the child raises :class:`ForkedServerError`. Use the
    ``spawn`` start method for ``multiprocessing`` (the default on macOS),
    or start the server after forking.
    """

    _binary = "secantusd-mdb"

    def __init__(
        self,
        storage_path: str | os.PathLike[str] | None = None,
        *,
        port: int = 0,
        startup_timeout: float = 60.0,
        embedded: bool = True,
    ) -> None:
        super().__init__(storage_path, port=port, startup_timeout=startup_timeout)
        self.embedded = embedded
        self._handle: Any = None
        self._forked = False

    def _args(self, storage: Path) -> list[str]:
        return ["--port", str(self._requested_port), "--storage-path", str(storage)]

    @property
    def uri(self) -> str:
        """A connection string for pymongo or any MongoDB driver."""
        return f"mongodb://{self.host}:{self.port}/"

    @property
    def output(self) -> str:
        """The server's most recent output lines, for diagnosing a failure.

        Always empty in-process: the server then writes its messages to this
        process's stderr.
        """
        return "" if self.embedded else super().output

    def start(self) -> MongoServer:
        if not self.embedded:
            super().start()
            return self
        if self._forked:
            raise ForkedServerError(
                "this MongoServer was started in the parent process; a forked child cannot use it"
            )
        if self._handle is not None:
            raise RuntimeError("server already started")
        try:
            from secantusdb import _secantus_server
        except ImportError as exc:
            raise ServerStartError(
                "this SecantusDB install has no in-process server for Python "
                f"{sys.version.split()[0]} on {sys.platform} (the "
                "secantusdb._secantus_server extension is not installed); pass "
                "embedded=False to run secantusd-mdb as a child process"
            ) from exc
        if self._requested_storage is None:
            self._tempdir = tempfile.mkdtemp(prefix="secantusdb-")
            self.storage_path = Path(self._tempdir)
        else:
            self.storage_path = self._requested_storage
            self.storage_path.mkdir(parents=True, exist_ok=True)
        try:
            # The daemon's defaults, so the two modes behave the same: the
            # single-node `secantus` replica set, mongod's test commands off,
            # the TTL monitor on at mongod's 60 seconds.
            self._handle = _secantus_server.RustServer(
                str(self.storage_path),
                port=self._requested_port,
                replica_set_name="secantus",
                enable_test_commands=False,
                ttl_sweep_seconds=60.0,
            )
        except Exception as exc:
            self._remove_tempdir()
            raise ServerStartError(f"{self._binary} did not start in-process: {exc}") from exc
        self.host, self.port = self._handle.address
        _RUNNING_IN_PROCESS.add(self)
        return self

    def stop(self) -> None:
        if not self.embedded:
            super().stop()
            return
        if self._forked:
            raise ForkedServerError(
                "this MongoServer was started in the parent process; only the parent can stop it"
            )
        handle, self._handle = self._handle, None
        _RUNNING_IN_PROCESS.discard(self)
        if handle is not None:
            try:
                handle.stop()
            except RuntimeError as exc:
                # WiredTiger's final checkpoint failed. The directory is kept,
                # temporary or not: the store is the evidence.
                self._tempdir = None
                raise RuntimeError(
                    f"{self._binary} at {self.storage_path} did not stop cleanly: {exc}"
                ) from exc
        self._remove_tempdir()

    def _remove_tempdir(self) -> None:
        if self._tempdir is not None:
            shutil.rmtree(self._tempdir, ignore_errors=True)
            self._tempdir = None

    def _disown_after_fork(self) -> None:
        """In a forked child: forget the parent's server without stopping it."""
        handle, self._handle = self._handle, None
        if handle is not None:
            handle._abandon_after_fork()
        # The temporary directory is the parent's to remove.
        self._tempdir = None
        self._forked = True

    def __enter__(self) -> MongoServer:
        return self.start()

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        # A forked child leaving the parent's `with` block: the server is the
        # parent's, so there is nothing to stop. Only an explicit stop() in
        # the child raises.
        if self._forked:
            return
        self.stop()


class PostgresServer(_Server):
    """The PostgreSQL-wire server, ``secantusd-pg`` (beta).

    With no ``storage_path`` the data lives in a temporary directory that
    is removed on ``stop()``. ``port=0`` (the default) picks a free port.
    """

    _binary = "secantusd-pg"

    def _args(self, storage: Path) -> list[str]:
        return [str(storage), f"127.0.0.1:{self._requested_port}"]

    @property
    def dsn(self) -> str:
        """A connection string for psycopg or any PostgreSQL driver."""
        return f"postgresql://postgres@{self.host}:{self.port}/postgres"
