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
from pymongo_validation.plugin import _start_server, per_worker_servers
from tasks import _gauge_parallel_flags


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
@pytest.mark.skipif(
    importlib.util.find_spec("_secantus_server") is not None,
    reason="only meaningful without the _secantus_server extension installed",
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


def test_per_worker_servers_is_off_by_default(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("SECANTUS_GAUGE_PER_WORKER", raising=False)
    assert per_worker_servers() is False


@pytest.mark.parametrize("value", ["", "0"])
def test_per_worker_servers_off_for_empty_and_zero(
    monkeypatch: pytest.MonkeyPatch, value: str
) -> None:
    monkeypatch.setenv("SECANTUS_GAUGE_PER_WORKER", value)
    assert per_worker_servers() is False


@pytest.mark.parametrize("value", ["1", "yes", "true"])
def test_per_worker_servers_on_for_truthy_values(
    monkeypatch: pytest.MonkeyPatch, value: str
) -> None:
    monkeypatch.setenv("SECANTUS_GAUGE_PER_WORKER", value)
    assert per_worker_servers() is True


def test_serial_gauge_flags_are_unchanged() -> None:
    # The published number is measured with --jobs 1; that invocation must
    # stay byte-identical to the pre-parallel one (no per-worker env, -n1,
    # no --dist override).
    assert _gauge_parallel_flags(1) == ("", "-n1")


def test_parallel_gauge_flags_distribute_whole_files() -> None:
    env, flags = _gauge_parallel_flags(4)
    assert env == "SECANTUS_GAUGE_PER_WORKER=1 "
    # loadfile is load-bearing: it keeps upstream's within-file ordering,
    # which the vendored suites depend on.
    assert flags == "-n4 --dist loadfile"


def test_zero_jobs_is_rejected() -> None:
    with pytest.raises(SystemExit):
        _gauge_parallel_flags(0)


def test_two_embedded_servers_do_not_share_state(tmp_path: Path) -> None:
    # The premise of --jobs N: workers can reuse the same database and
    # collection names because each has its own server and WT store.
    a, a_host, a_port = _start_server("python", str(tmp_path / "a"))
    b, b_host, b_port = _start_server("python", str(tmp_path / "b"))
    try:
        assert (a_host, a_port) != (b_host, b_port)
        ca: MongoClient = MongoClient(a_host, a_port, serverSelectionTimeoutMS=5000)
        cb: MongoClient = MongoClient(b_host, b_port, serverSelectionTimeoutMS=5000)
        try:
            ca.pymongo_test.coll.insert_one({"_id": 1, "worker": "a"})
            assert cb.pymongo_test.coll.find_one({"_id": 1}) is None
            assert "pymongo_test" not in cb.list_database_names()
        finally:
            ca.close()
            cb.close()
    finally:
        a.stop()
        b.stop()
