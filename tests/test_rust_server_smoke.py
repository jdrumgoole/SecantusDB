"""Smoke test: pymongo against the embedded Rust server (R6).

This is the embryonic R8 conformance gate — the first test that drives the
**Rust server** (its own wire / dispatch / cursors over WiredTiger) through a
real `pymongo` client, exactly as the full suites will once the server grows the
remaining command families.

Scoped to the commands the Rust dispatch currently implements: handshake
(`hello` / `ping`), `insert`, `find` (+ `getMore` / `killCursors`), `delete`,
and `aggregate` (incl. `count_documents`, which pymongo routes through an
aggregation pipeline). **Not yet exercised** (deferred): storage-backed
aggregation stages (`$lookup` / `$out` / `$merge`), change streams, etc.

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


def _client(srv):
    host, port = srv.address
    return pymongo.MongoClient(
        host,
        port,
        directConnection=True,
        serverSelectionTimeoutMS=5000,
    )


def test_pymongo_crud_against_rust_server(tmp_path) -> None:
    """Insert / find / find-with-filter / count_documents / delete end-to-end."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]

        coll.insert_many([{"_id": 1, "x": 1}, {"_id": 2, "x": 2}, {"_id": 3, "x": 1}])
        assert len(list(coll.find({}))) == 3
        assert coll.find_one({"_id": 2})["x"] == 2
        assert sorted(d["_id"] for d in coll.find({"x": 1})) == [1, 3]
        # count_documents routes through an aggregation pipeline.
        assert coll.count_documents({}) == 3
        assert coll.count_documents({"x": 1}) == 2

        coll.delete_one({"_id": 1})
        assert sorted(d["_id"] for d in coll.find({})) == [2, 3]
    finally:
        srv.stop()


def test_aggregate_pipeline_against_rust_server(tmp_path) -> None:
    """A direct aggregation pipeline ($match → $group) via pymongo."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        coll = _client(srv)["t"]["c"]
        coll.insert_many(
            [
                {"_id": 1, "g": "a", "v": 10},
                {"_id": 2, "g": "a", "v": 20},
                {"_id": 3, "g": "b", "v": 5},
            ]
        )
        result = list(
            coll.aggregate(
                [
                    {"$match": {"g": "a"}},
                    {"$group": {"_id": "$g", "total": {"$sum": "$v"}}},
                ]
            )
        )
        assert result == [{"_id": "a", "total": 30}]
    finally:
        srv.stop()


def test_rust_server_handshake(tmp_path) -> None:
    """A bare ``hello`` / ``ping`` admin round-trip — the handshake the driver
    runs on connect."""
    srv = _server.RustServer(str(tmp_path / "wt"), 0)
    try:
        admin = _client(srv).admin
        hello = admin.command("hello")
        assert hello["ok"] == 1.0
        assert hello["isWritablePrimary"] is True
        assert admin.command("ping")["ok"] == 1.0
    finally:
        srv.stop()
