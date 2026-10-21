"""Gap tests for the shared Rust storage items found on 2026-10-20 by the
storage audit (§7.085, measured against mongod 8.2.11).

Both start a `secantusd-rs` of their own: one is killed mid-life, the other
needs a small WiredTiger cache."""

from __future__ import annotations

import contextlib
import os
import signal
import subprocess
from collections.abc import Iterator
from typing import Any

import pymongo
import pytest
from pymongo.write_concern import WriteConcern

from tests.backlog.conftest import _RS_BANNER, RS_BINARY, drain_output


@contextlib.contextmanager
def rust_server(store: Any, *extra: str, socket_timeout_ms: int | None = None) -> Iterator[Any]:
    """`(proc, client)` for a `secantusd-rs` over `store`."""
    assert RS_BINARY.exists(), f"{RS_BINARY} is not built"
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", str(store), *extra],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        assert proc.stdout is not None
        m = _RS_BANNER.search(proc.stdout.readline())
        assert m, "secantusd-rs did not start"
        drain_output(proc)
        client = pymongo.MongoClient(
            host=m.group(1),
            port=int(m.group(2)),
            serverSelectionTimeoutMS=10_000,
            socketTimeoutMS=socket_timeout_ms,
        )
        yield proc, client
        client.close()
    finally:
        if proc.poll() is None:
            proc.kill()
        proc.wait(timeout=20)


@pytest.mark.backlog_fixed("RM-186")
def test_j_true_write_survives_sigkill(tmp_path: Any) -> None:
    """mongod 8.2.11 kept all 50 of 50 inserts acknowledged with
    `{w: 1, j: true}` across a SIGKILL and restart, in each of five runs: `j`
    waits for the journal to be synced. The Rust server lost 216 of 250."""
    store = tmp_path / "store"
    with rust_server(store) as (proc, client):
        coll = client.t.get_collection("c", write_concern=WriteConcern(w=1, j=True))
        for i in range(50):
            coll.insert_one({"_id": i})
        proc.send_signal(signal.SIGKILL)
        proc.wait(timeout=20)
    with rust_server(store) as (_, client):
        kept = client.t.c.count_documents({})
    assert kept == 50


@pytest.mark.backlog_fixed("RM-187")
def test_index_build_completes_under_a_small_cache(tmp_path: Any) -> None:
    """mongod 8.2.11 builds an index over 1,000,000 300-byte documents at
    its minimum 0.25 GB cache. The Rust server, at `--cache-size 8M`, retries
    a build over 5,000 of them forever."""
    with rust_server(tmp_path / "store", "--cache-size", "8M", socket_timeout_ms=20_000) as (
        _,
        client,
    ):
        coll = client.t.c
        pad = os.urandom(150).hex()
        # The documents go in on a client with no socket timeout: the 20 s
        # one is for the build, and an insert this slow (CI's runner) timed
        # out, was retried by pymongo while still running, and hit RM-230.
        with pymongo.MongoClient(*client.address, directConnection=True) as loader:
            loader.t.c.insert_many([{"_id": i, "s": f"{i:09d}{pad}"} for i in range(5000)])
        try:
            answer: tuple = ("ok", coll.create_index("s"))
        except pymongo.errors.PyMongoError as e:
            answer = ("err", type(e).__name__)
    assert answer == ("ok", "s_1")
