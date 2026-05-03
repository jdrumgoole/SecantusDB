"""Pytest plugin that points pymongo's vendored test suite at an embedded
SecantusDBServer.

Mechanism
---------
pymongo's test bootstrap (`vendor/pymongo-tests/test/helpers_shared.py`)
reads `DB_IP` (default `"localhost"`) and `DB_PORT` (default `27017`) at
import time. We start an embedded SecantusDBServer in `pytest_configure`
— before pytest collects any tests, therefore before pymongo's helpers
get imported — and write the bound host/port into the env vars.

The server uses an in-memory WiredTiger store so the validation run
leaves nothing behind on disk and successive runs start from a clean
slate.
"""

from __future__ import annotations

import logging
import os
from typing import Any

from secantus import SecantusDBServer

_logger = logging.getLogger(__name__)

# Module-level state so pytest_unconfigure can stop the same server we started.
_server: SecantusDBServer | None = None


def pytest_configure(config: Any) -> None:
    global _server
    _server = SecantusDBServer(host="127.0.0.1", port=0, storage_path=":memory:")
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
    _logger.info("pymongo-validation: embedded SecantusDB on %s:%d", host, port)


def pytest_unconfigure(config: Any) -> None:
    global _server
    if _server is not None:
        _server.stop()
        _server = None
