"""Pytest plugin that points pymongo's vendored test suite at an embedded
SecantusDB server — the pure-Python server by default, or the Rust server
(R8) when ``SECANTUS_GAUGE_SERVER=rust``.

Mechanism
---------
pymongo's test bootstrap (`vendor/pymongo-tests/test/helpers_shared.py`)
reads `DB_IP` (default `"localhost"`) and `DB_PORT` (default `27017`) at
import time. We start an embedded server in `pytest_configure`
— before pytest collects any tests, therefore before pymongo's helpers
get imported — and write the bound host/port into the env vars.

Server selection (``SECANTUS_GAUGE_SERVER``, default ``python``):

* ``python`` — the original pure-Python ``SecantusDBServer`` (the headline
  "MongoDB compatibility" gauge; unchanged behaviour).
* ``rust`` — the Rust server via the ``_secantus_server`` embedded handle
  (the R8 conformance gate). Needs the WiredTiger-linking extension built
  (``maturin build`` in ``crates/secantus-server-py``, or the wheel's
  ``SECANTUS_BUILD_STORAGE_ENGINE=ON`` CMake path) and importable in the
  gauge interpreter. Imported lazily so python-mode runs don't need it
  (and rust-mode runs don't need the ``secantus`` package).

The server uses an **on-disk** WiredTiger store rooted in a fresh
`tempfile.mkdtemp()` so the conformance run exercises the real
persistence path (schema, journal, close-and-reopen). The tempdir is
removed in `pytest_unconfigure`, leaving nothing behind. Matches the
project policy in `CLAUDE.md` → Tooling: the default suite runs against
real on-disk WiredTiger so the schema and persistence paths are
continuously exercised. Only `tests/test_perf_regression.py` keeps
`:memory:`, where stable in-memory baselines are the point.
"""

from __future__ import annotations

import logging
import os
import shutil
import tempfile
from typing import Any

_logger = logging.getLogger(__name__)

# Module-level state so pytest_unconfigure can stop the same server we
# started. Either a SecantusDBServer or a _secantus_server.RustServer —
# both expose address/stop, which is all we use after startup.
_server: Any | None = None
_storage_dir: str | None = None


def _start_server(mode: str, storage_dir: str) -> tuple[Any, str, int]:
    if mode == "rust":
        # Lazy: only rust-mode runs need the WT-linking extension.
        import _secantus_server

        server = _secantus_server.RustServer(
            storage_path=storage_dir,
            port=0,
            host="127.0.0.1",
            replica_set_name="secantus",
        )
        host, port = server.address
        return server, host, port
    if mode == "python":
        from secantus import SecantusDBServer

        server = SecantusDBServer(host="127.0.0.1", port=0, storage_path=storage_dir)
        server.start()
        host, port = server.address
        return server, host, port
    raise ValueError(f"SECANTUS_GAUGE_SERVER={mode!r} not recognised (expected 'python' or 'rust')")


def pytest_configure(config: Any) -> None:
    global _server, _storage_dir
    mode = os.environ.get("SECANTUS_GAUGE_SERVER", "python")
    _storage_dir = tempfile.mkdtemp(prefix=f"secantus-pymongo-gauge-{mode}-")
    _server, host, port = _start_server(mode, _storage_dir)
    os.environ["DB_IP"] = host
    os.environ["DB_PORT"] = str(port)
    # Some pymongo tests (e.g. test_index_management's search-index suite)
    # read DB_USER/DB_PASSWORD directly rather than via helpers_shared's
    # `.get(..., default)`. We don't have auth, so any value lets the test
    # reach the auth probe and self-skip cleanly.
    os.environ.setdefault("DB_USER", "user")
    os.environ.setdefault("DB_PASSWORD", "password")
    _logger.info(
        "pymongo-validation: embedded SecantusDB (%s server) on %s:%d (storage=%s)",
        mode,
        host,
        port,
        _storage_dir,
    )


def pytest_unconfigure(config: Any) -> None:
    global _server, _storage_dir
    if _server is not None:
        _server.stop()
        _server = None
    if _storage_dir is not None:
        shutil.rmtree(_storage_dir, ignore_errors=True)
        _storage_dir = None
