"""Unit tests for pymongo_validation.plugin's gauge-server selection (R8).

The full gauge runs are `invoke validate` / `invoke validate --server rust`;
these tests pin the mode-dispatch contract of `_start_server` without
running any vendored pymongo tests.
"""

from __future__ import annotations

import importlib.util
from pathlib import Path

import pytest
from pymongo import MongoClient
from pymongo_validation.plugin import _start_server


def test_python_mode_starts_a_reachable_server(tmp_path: Path) -> None:
    server, host, port = _start_server("python", str(tmp_path / "store"))
    try:
        client: MongoClient = MongoClient(host, port, serverSelectionTimeoutMS=5000)
        assert client.admin.command("ping")["ok"] == 1.0
        client.close()
    finally:
        server.stop()


def test_unknown_mode_is_rejected(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="SECANTUS_GAUGE_SERVER"):
        _start_server("mongod", str(tmp_path / "store"))


@pytest.mark.skipif(
    importlib.util.find_spec("_secantus_server") is not None,
    reason="_secantus_server is installed; the lazy-import contract is moot",
)
def test_rust_mode_requires_the_extension(tmp_path: Path) -> None:
    # Rust mode imports _secantus_server lazily so python-mode gauge runs
    # don't need the WT-linking extension. Without it, the failure must be
    # the honest ImportError, not something swallowed.
    with pytest.raises(ImportError):
        _start_server("rust", str(tmp_path / "store"))


@pytest.mark.skipif(
    importlib.util.find_spec("_secantus_server") is None,
    reason="needs the WT-linking _secantus_server extension (storage-engine build)",
)
def test_rust_mode_starts_a_reachable_server(tmp_path: Path) -> None:
    server, host, port = _start_server("rust", str(tmp_path / "store"))
    try:
        client: MongoClient = MongoClient(host, port, serverSelectionTimeoutMS=5000)
        hello = client.admin.command("hello")
        assert hello["ok"] == 1.0
        # The gauge needs the replica-set advertisement so pymongo accepts
        # change-stream topology, matching the python-mode server.
        assert hello.get("setName") == "secantus"
        client.close()
    finally:
        server.stop()
