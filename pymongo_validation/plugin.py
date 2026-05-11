"""Pytest plugin that points pymongo's vendored test suite at an embedded
SecantusDBServer.

Mechanism
---------
pymongo's test bootstrap (`vendor/pymongo-tests/test/helpers_shared.py`)
reads `DB_IP` (default `"localhost"`) and `DB_PORT` (default `27017`) at
import time. We start an embedded SecantusDBServer in `pytest_configure`
— before pytest collects any tests, therefore before pymongo's helpers
get imported — and write the bound host/port into the env vars.

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

from secantus import SecantusDBServer

_logger = logging.getLogger(__name__)

# Module-level state so pytest_unconfigure can stop the same server we started.
_server: SecantusDBServer | None = None
_storage_dir: str | None = None


def pytest_configure(config: Any) -> None:
    global _server, _storage_dir
    _storage_dir = tempfile.mkdtemp(prefix="secantus-pymongo-gauge-")
    _server = SecantusDBServer(host="127.0.0.1", port=0, storage_path=_storage_dir)
    _server.start()
    host, port = _server.address
    os.environ["DB_IP"] = host
    os.environ["DB_PORT"] = str(port)
    # Some pymongo tests (e.g. test_index_management's search-index suite)
    # read DB_USER/DB_PASSWORD directly rather than via helpers_shared's
    # `.get(..., default)`. We don't have auth, so any value lets the test
    # reach the auth probe and self-skip cleanly.
    os.environ.setdefault("DB_USER", "user")
    os.environ.setdefault("DB_PASSWORD", "password")
    _logger.info(
        "pymongo-validation: embedded SecantusDB on %s:%d (storage=%s)",
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
