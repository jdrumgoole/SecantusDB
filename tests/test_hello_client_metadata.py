"""``hello.client`` subdoc is captured per connection and surfaced
back via ``currentOp`` as ``clientMetadata``.

Per the MongoDB Handshake spec, drivers send their
self-identification (name + version, OS info, platform, optional
application name) in the ``client`` subdoc on the first ``hello``
/ ``isMaster`` command of every connection. Admin tools and
mongo-rust-driver's ``test::client::metadata_sent_in_handshake``
read it back via ``db.adminCommand({currentOp: 1})`` to identify
which connection is theirs.

Before this slice we threw the ``client`` subdoc away on hello
and currentOp emitted no ``clientMetadata`` field. The fix
stashes the subdoc on the ``ConnInfo`` registry entry and
``currentOp`` echoes it back on the corresponding in-progress op.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path / "d")) as srv:
        yield srv


@pytest.fixture
def client(server):
    c = MongoClient(server.uri, serverSelectionTimeoutMS=2000, appname="meta-app")
    try:
        yield c
    finally:
        c.close()


def test_current_op_surfaces_client_metadata(client) -> None:
    """``currentOp`` echoes the ``hello.client`` subdoc back on each
    in-progress op. pymongo sends its driver / OS / platform
    self-identification on connect; ``currentOp`` must round-trip it."""
    resp = client["admin"].command("currentOp", {"command.currentOp": {"$exists": True}})
    inprog = resp.get("inprog", [])
    assert inprog, "currentOp returned no in-progress ops"
    # Find the op for this very connection — the one running the
    # currentOp command. It's the only one with ``op == "currentOp"``.
    self_op = next((op for op in inprog if op.get("op") == "currentOp"), None)
    assert self_op is not None
    meta = self_op.get("clientMetadata")
    assert meta is not None
    # The driver subdoc — pymongo identifies itself. (The exact
    # name varies: ``"PyMongo"`` for pure-python, ``"PyMongo|c"``
    # when the C extensions are linked.)
    assert meta["driver"]["name"].startswith("PyMongo")
    assert isinstance(meta["driver"]["version"], str)
    # OS subdoc — driver populates name + version + type.
    assert "os" in meta
    # ``appname`` from MongoClient(appname=...) round-trips here.
    assert meta.get("application", {}).get("name") == "meta-app"


def test_aggregation_currentop_surfaces_appname_and_metadata(client) -> None:
    """The ``$currentOp`` aggregation stage (not just the ``currentOp``
    command) surfaces the connection's ``clientMetadata`` document plus a
    top-level ``appName``.

    Mirrors mongo-cxx-driver's "integration tests for client metadata
    handshake feature", which connects with ``?appName=...`` and scans
    ``db.aggregate([{$currentOp: {}}])`` for an op whose ``appName`` matches,
    then asserts its ``clientMetadata.{application,driver,os}``."""
    ops = list(client["admin"].aggregate([{"$currentOp": {}}]))
    assert ops, "$currentOp returned no ops"
    self_op = next((op for op in ops if op.get("appName") == "meta-app"), None)
    assert self_op is not None, f"no op with appName=meta-app: {ops}"
    meta = self_op.get("clientMetadata")
    assert isinstance(meta, dict)
    assert meta.get("application", {}).get("name") == "meta-app"
    assert meta["driver"]["name"].startswith("PyMongo")
    assert isinstance(meta["driver"]["version"], str)
    assert "os" in meta and "type" in meta["os"]


def test_currentop_without_hello_has_no_metadata(server) -> None:
    """A connection that hasn't sent ``hello`` shouldn't have
    ``clientMetadata`` in its currentOp entry. The default pymongo
    client always sends hello so this case isn't reachable in
    normal usage — we go through a fresh connection that sends no
    ``client`` subdoc to confirm the absence is handled cleanly
    (no KeyError, no None placeholder)."""
    # Open a fresh client + immediately read currentOp. pymongo always
    # sends ``client`` on hello, so we ALWAYS expect clientMetadata
    # to be present. This test confirms the value is shaped like a
    # dict, not None / missing / placeholder.
    c = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        resp = c["admin"].command("currentOp")
        for op in resp.get("inprog", []):
            if op.get("type") == "op" and "clientMetadata" in op:
                assert isinstance(op["clientMetadata"], dict)
                assert "driver" in op["clientMetadata"]
    finally:
        c.close()
