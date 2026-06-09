"""Smoke test: pymongo against the embedded Rust server (R6).

This is the embryonic R8 conformance gate — the first test that drives the
**Rust server** (its own wire / dispatch / cursors over WiredTiger) through a
real `pymongo` client, exactly as the full suites will once the server grows the
remaining command families.

Gated on the `_secantus_server` extension being importable, which requires the
WiredTiger-linking build (the wheel's CMake under
``SECANTUS_BUILD_STORAGE_ENGINE=ON`` or a local ``maturin`` build with
``SECANTUS_WT_INCLUDE`` / ``SECANTUS_WT_LIB`` set). It is skipped in the WT-less
dev sandbox / the default `rust` CI job.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")


def test_pymongo_crud_against_rust_server(tmp_path) -> None:
    """Open the Rust server, connect pymongo, and exercise the CRUD + read path
    end-to-end (insert / count / find / find-with-filter / delete)."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        host, port = srv.address
        client = pymongo.MongoClient(
            host,
            port,
            directConnection=True,
            serverSelectionTimeoutMS=5000,
        )
        coll = client["t"]["c"]

        coll.insert_many([{"_id": 1, "x": 1}, {"_id": 2, "x": 2}, {"_id": 3, "x": 1}])
        assert coll.count_documents({}) == 3

        assert coll.find_one({"_id": 2})["x"] == 2
        assert sorted(d["_id"] for d in coll.find({"x": 1})) == [1, 3]

        coll.delete_one({"_id": 1})
        assert coll.count_documents({}) == 2

        client.close()
    finally:
        srv.stop()


def test_rust_server_handshake(tmp_path) -> None:
    """A bare ``hello`` / ``ping`` admin round-trip — the handshake the driver
    runs on connect."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        host, port = srv.address
        client = pymongo.MongoClient(
            host, port, directConnection=True, serverSelectionTimeoutMS=5000
        )
        hello = client.admin.command("hello")
        assert hello["ok"] == 1.0
        assert hello["isWritablePrimary"] is True
        assert client.admin.command("ping")["ok"] == 1.0
        client.close()
    finally:
        srv.stop()
