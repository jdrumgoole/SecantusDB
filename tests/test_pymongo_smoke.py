from __future__ import annotations

import bson
import pytest
from pymongo import MongoClient
from pymongo.errors import PyMongoError

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        yield srv


@pytest.fixture
def client(server: SecantusDBServer):
    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield mc
    finally:
        mc.close()


def test_admin_ping_returns_ok(client: MongoClient) -> None:
    reply = client.admin.command("ping")
    assert reply.get("ok") == 1.0


def test_admin_hello_advertises_writable_primary(client: MongoClient) -> None:
    reply = client.admin.command("hello")
    assert reply.get("ok") == 1.0
    assert reply.get("isWritablePrimary") is True
    assert reply.get("maxWireVersion", 0) >= 6
    assert reply.get("maxBsonObjectSize", 0) >= 16 * 1024 * 1024


def test_server_info_returns_version(client: MongoClient) -> None:
    info = client.server_info()
    assert info.get("ok") == 1.0
    assert "version" in info
    assert isinstance(info["version"], str)


def test_hello_emits_int64_for_topology_counter_and_connection_id(
    client: MongoClient,
) -> None:
    """Regression for the mongo-go-driver compatibility break.

    The official Go driver (mongodump/mongorestore + everything else built
    on mongo-go-driver) hard-fails the handshake with "expected 'counter'
    to be an int64 but it's a BSON 32-bit integer" when these fields are
    encoded as int32. pymongo is permissive here so the bug only surfaces
    against Go clients. Decode the raw BSON and assert the type tags.
    """
    raw = client.admin.command("hello", codec_options=bson.CodecOptions(document_class=dict))
    assert isinstance(raw["topologyVersion"]["counter"], bson.Int64)
    assert isinstance(raw["connectionId"], bson.Int64)


def test_unknown_command_does_not_crash_connection(client: MongoClient) -> None:
    with pytest.raises(PyMongoError):
        client.admin.command("definitelyNotARealCommand")
    reply = client.admin.command("ping")
    assert reply.get("ok") == 1.0
