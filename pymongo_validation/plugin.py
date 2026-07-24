"""Pytest plugin that points pymongo's vendored test suite at an embedded
SecantusDB server — the pure-Python server by default, or the Rust server
(R8) when ``SECANTUS_GAUGE_SERVER=rust``.

Mechanism — and why the hook choice is load-bearing
---------------------------------------------------
pymongo's test bootstrap (`vendor/pymongo-tests/test/helpers_shared.py`)
reads `DB_IP` (default `"localhost"`) and `DB_PORT` (default `27017`)
**at import time**, and `vendor/pymongo-tests/test/conftest.py` triggers
that import (``from test import pytest_conf, setup, teardown`` →
``test/__init__.py`` → ``helpers_shared``). Initial conftests are
imported during pytest startup, BEFORE ``pytest_configure`` runs — so
the server start and env-var write live in
``pytest_load_initial_conftests``, the one hook that fires before any
conftest import (command-line ``-p`` plugins are loaded early enough to
see it).

An earlier version of this plugin did the setup in ``pytest_configure``.
That was too late: pymongo's helpers had already captured
``localhost:27017``, so every test silently targeted whatever listened
on the developer's real 27017 (a real ``mongod``!) and CI — where
nothing listens there — mass-skipped 1100+ tests. ``pytest_configure``
now carries a tripwire instead: if the imported helpers hold anything
other than the embedded server's address, the run aborts rather than
measuring the wrong server.

Parallel mode (``SECANTUS_GAUGE_PER_WORKER``, set by ``invoke validate
--jobs N`` for N > 1)
----------------------------------------------------------------------
The gauge is serial by default because pymongo's tests share database
and collection names — two of them running concurrently against ONE
server trample each other. That is a property of the shared server, not
of the tests: give every xdist worker **its own** embedded SecantusDB
(its own WT store, its own port) and the collision disappears, because
no two workers can see each other's databases.

With ``SECANTUS_GAUGE_PER_WORKER=1`` each worker starts a server in this
same pre-conftest hook and overwrites the inherited DB_IP/DB_PORT with
its own address, so pymongo's helpers freeze the *worker's* server. The
controller still starts one (idle) server so its ``pytest_configure``
tripwire — including the serverStatus identity probe — runs exactly as
in serial mode; every process therefore verifies its own target.

The task pairs this with ``--dist loadfile`` so a whole test file stays
on one worker: upstream's within-file ordering (shared fixtures,
collections created by one test and read by the next — the same reason
``-p no:randomly`` is passed) is preserved. Coverage is identical; only
the file→process assignment changes. One behavioural difference worth
knowing: if pytest-timeout kills a worker, its restart gets a *fresh*
server, so server-side state from the killed worker's earlier tests is
gone (in serial mode the controller's server outlived the restart).

Server selection (``SECANTUS_GAUGE_SERVER``, default ``python``):

* ``python`` — the original pure-Python ``SecantusDBServer`` (the headline
  "MongoDB compatibility" gauge).
* ``rust`` — the Rust server via the ``_secantus_server`` embedded handle
  (the R8 conformance gate). Needs the WT-linking extension built
  (``SKBUILD_CMAKE_DEFINE=SECANTUS_BUILD_STORAGE_ENGINE=ON uv sync``)
  and importable in the gauge interpreter. Imported lazily so
  python-mode runs don't need it.

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
import sys
import tempfile
from typing import Any

import pytest

_logger = logging.getLogger(__name__)

# Module-level state so pytest_unconfigure can stop the same server we
# started. Either a SecantusDBServer or a _secantus_server.RustServer —
# both expose address/stop, which is all we use after startup.
_server: Any | None = None
_storage_dir: str | None = None
_address: tuple[str, int] | None = None


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


def per_worker_servers() -> bool:
    """True when each xdist worker runs against its own embedded server.

    Set by ``invoke validate --jobs N`` (N > 1). See the module docstring.
    """
    return os.environ.get("SECANTUS_GAUGE_PER_WORKER", "") not in ("", "0")


def pytest_load_initial_conftests(early_config: Any, parser: Any, args: Any) -> None:
    # MUST run before pymongo's conftest is imported — see module docstring.
    global _server, _storage_dir, _address
    worker = os.environ.get("PYTEST_XDIST_WORKER")
    if worker and not per_worker_servers():
        # Serial gauge (-n1): the CONTROLLER owns the one server and the
        # single worker inherits DB_IP/DB_PORT through the environment.
        # Starting another server here would waste a WT store and lose
        # server-side state on every worker restart.
        return
    mode = os.environ.get("SECANTUS_GAUGE_SERVER", "python")
    suffix = f"-{worker}" if worker else ""
    _storage_dir = tempfile.mkdtemp(prefix=f"secantus-pymongo-gauge-{mode}{suffix}-")
    _server, host, port = _start_server(mode, _storage_dir)
    _address = (host, port)
    os.environ["DB_IP"] = host
    os.environ["DB_PORT"] = str(port)
    # Some pymongo tests (e.g. test_index_management's search-index suite)
    # read DB_USER/DB_PASSWORD directly rather than via helpers_shared's
    # `.get(..., default)`. We don't have auth, so any value lets the test
    # reach the auth probe and self-skip cleanly.
    os.environ.setdefault("DB_USER", "user")
    os.environ.setdefault("DB_PASSWORD", "password")
    _logger.info(
        "pymongo-validation: embedded SecantusDB (%s server%s) on %s:%d (storage=%s)",
        mode,
        f", worker {worker}" if worker else "",
        host,
        port,
        _storage_dir,
    )


def pytest_configure(config: Any) -> None:
    # Tripwire: by now pymongo's initial conftest has imported
    # test.helpers_shared, which froze DB_IP/DB_PORT into module globals.
    # If they don't hold OUR server's address, the suite would silently
    # measure some other server (e.g. a real mongod on localhost:27017).
    # Abort instead — a dead gauge must never produce a number.
    helpers = sys.modules.get("test.helpers_shared")
    if helpers is None or _address is None:
        return
    captured = (getattr(helpers, "host", None), getattr(helpers, "port", None))
    if captured != _address:
        raise pytest.UsageError(
            f"pymongo's test helpers captured {captured!r}, but the embedded "
            f"SecantusDB is on {_address!r}. The helpers were imported before "
            "this plugin set DB_IP/DB_PORT — the gauge would measure the "
            "wrong server. Refusing to run."
        )
    # Belt and braces: ask the server AT THE ADDRESS THE TESTS WILL USE to
    # identify itself. SecantusDB's serverStatus carries a `secantus`
    # subdocument that real mongod never has, so even an address-plumbing
    # bug that the check above misses cannot put a real MongoDB behind
    # the gauge. (Both servers report it: {server: "python"|"rust"}.)
    from pymongo import MongoClient

    cap_host, cap_port = captured
    client: MongoClient = MongoClient(cap_host, cap_port, serverSelectionTimeoutMS=10_000)
    try:
        status = client.admin.command("serverStatus")
    finally:
        client.close()
    marker = status.get("secantus")
    if not isinstance(marker, dict) or "server" not in marker:
        raise pytest.UsageError(
            f"the server at {captured!r} did not identify itself as SecantusDB "
            f"(serverStatus has no 'secantus' marker — process={status.get('process')!r}, "
            f"version={status.get('version')!r}). The gauge would measure a "
            "foreign server, probably a real mongod. Refusing to run."
        )
    _logger.info("pymongo-validation: target verified — secantus %s server", marker["server"])


def pytest_unconfigure(config: Any) -> None:
    global _server, _storage_dir, _address
    if _server is not None:
        _server.stop()
        _server = None
    _address = None
    if _storage_dir is not None:
        shutil.rmtree(_storage_dir, ignore_errors=True)
        _storage_dir = None
