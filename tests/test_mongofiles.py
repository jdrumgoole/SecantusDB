"""End-to-end mongofiles (GridFS) against an embedded SecantusDB.

mongofiles is the Go-driver GridFS CLI. GridFS lives client-side —
fs.files / fs.chunks plus a createIndexes call — so this exercises
ordinary inserts, queries, and index creation through a strict client,
plus interop with pymongo's gridfs implementation reading the same
buckets.

The tests skip gracefully if `mongofiles` isn't on PATH so they don't
break local runs without the MongoDB Database Tools installed (CI image
must install them explicitly).
"""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import gridfs
import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer

MONGOFILES = shutil.which("mongofiles")

pytestmark = pytest.mark.skipif(
    MONGOFILES is None,
    reason="mongofiles not on PATH (install MongoDB Database Tools)",
)

PAYLOAD = b"SecantusDB GridFS round-trip payload\n" * 64


def _run_mongofiles(uri: str, *args: str) -> str:
    assert MONGOFILES is not None  # narrowed by skipif
    result = subprocess.run(
        [MONGOFILES, f"--uri={uri}", "--db", "filestore", *args],
        check=True,
        capture_output=True,
        text=True,
        timeout=30,
    )
    return result.stdout


def test_put_then_pymongo_reads(tmp_path: Path) -> None:
    """mongofiles put → pymongo's gridfs returns identical bytes."""
    src = tmp_path / "payload.bin"
    src.write_bytes(PAYLOAD)

    wt_dir = tmp_path / "secantus-wt"
    wt_dir.mkdir()
    with SecantusDBServer(port=0, storage_path=str(wt_dir)) as server:
        _run_mongofiles(server.uri, "put", str(src))

        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            fs = gridfs.GridFS(client["filestore"])
            stored = fs.find_one({"filename": str(src)})
            assert stored is not None
            assert stored.read() == PAYLOAD
        finally:
            client.close()


def test_pymongo_writes_then_get(tmp_path: Path) -> None:
    """pymongo's gridfs writes → mongofiles get produces identical bytes."""
    wt_dir = tmp_path / "secantus-wt"
    wt_dir.mkdir()
    with SecantusDBServer(port=0, storage_path=str(wt_dir)) as server:
        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            fs = gridfs.GridFS(client["filestore"])
            fs.put(PAYLOAD, filename="from-pymongo.bin")
        finally:
            client.close()

        dest = tmp_path / "fetched.bin"
        _run_mongofiles(server.uri, "get", "from-pymongo.bin", "--local", str(dest))
        assert dest.read_bytes() == PAYLOAD


def test_list_and_delete(tmp_path: Path) -> None:
    """mongofiles list shows stored files; delete removes them."""
    src = tmp_path / "listed.bin"
    src.write_bytes(PAYLOAD)

    wt_dir = tmp_path / "secantus-wt"
    wt_dir.mkdir()
    with SecantusDBServer(port=0, storage_path=str(wt_dir)) as server:
        _run_mongofiles(server.uri, "put", str(src))

        listing = _run_mongofiles(server.uri, "list")
        assert str(src) in listing
        assert str(len(PAYLOAD)) in listing

        _run_mongofiles(server.uri, "delete", str(src))
        assert str(src) not in _run_mongofiles(server.uri, "list")

        client = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            assert client["filestore"]["fs.files"].count_documents({}) == 0
            assert client["filestore"]["fs.chunks"].count_documents({}) == 0
        finally:
            client.close()
