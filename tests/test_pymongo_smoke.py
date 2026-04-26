from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import PyMongoError

from fongodb import FongoDBServer


@pytest.fixture
def server():
    with FongoDBServer(port=0) as srv:
        yield srv


@pytest.fixture
def client(server: FongoDBServer):
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


def test_unknown_command_does_not_crash_connection(client: MongoClient) -> None:
    with pytest.raises(PyMongoError):
        client.admin.command("definitelyNotARealCommand")
    reply = client.admin.command("ping")
    assert reply.get("ok") == 1.0
