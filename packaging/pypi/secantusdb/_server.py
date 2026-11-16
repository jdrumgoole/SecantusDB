"""Start and stop the bundled server binaries as child processes."""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
from collections import deque
from pathlib import Path
from types import TracebackType

_BIN_DIR = Path(__file__).resolve().parent / "bin"

# Both binaries print one line naming the address they bound, e.g.
# "secantusd-rs listening on 127.0.0.1:53211". Port 0 asks for an ephemeral
# port, so this line is the only way to learn which one was picked.
_LISTENING = re.compile(r"listening on (\S+):(\d+)")


class ServerStartError(RuntimeError):
    """The server exited, or never said it was listening, during startup."""


def binary_path(name: str) -> Path:
    """The path of a bundled binary: ``"secantusd-rs"`` or ``"secantusd-pg"``."""
    if name not in ("secantusd-rs", "secantusd-pg"):
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
    """The MongoDB-wire server, ``secantusd-rs``.

    With no ``storage_path`` the data lives in a temporary directory that
    is removed on ``stop()``. ``port=0`` (the default) picks a free port.
    """

    _binary = "secantusd-rs"

    def _args(self, storage: Path) -> list[str]:
        return ["--port", str(self._requested_port), "--storage-path", str(storage)]

    @property
    def uri(self) -> str:
        """A connection string for pymongo or any MongoDB driver."""
        return f"mongodb://{self.host}:{self.port}/"


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
