"""The server binds whatever address family the host implies, not just IPv4.

`SecantusDBServer.start` hardcoded `socket.AF_INET`, which made the server
IPv4-only: `host="::1"` did not merely fail to serve IPv6 clients, it raised a
bare `gaierror` at bind ("nodename nor servname provided") that gave no hint the
family was the problem. Nothing tested a non-IPv4 host, so it went unnoticed.

Found while triaging the mongo-c-driver gauge's `/Client/ipv6` failure. That test
is *not* fixed by this — it hardcodes `mongodb://[::1]/`, i.e. the default port
27017, and ignores `MONGOC_TEST_URI` entirely, so it can only pass against a
server the harness happens to have put on `[::1]:27017`. The IPv4-only limitation
this pins is a real one on its own merits.
"""

from __future__ import annotations

import socket

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer


def _uri(host: str, port: int) -> str:
    """IPv6 literals need brackets in a MongoDB URI."""
    return f"mongodb://[{host}]:{port}/" if ":" in host else f"mongodb://{host}:{port}/"


def _has_ipv6_loopback() -> bool:
    try:
        with socket.socket(socket.AF_INET6, socket.SOCK_STREAM) as s:
            s.bind(("::1", 0))
        return True
    except OSError:
        return False


@pytest.mark.parametrize("host", ["127.0.0.1", "::1"])
def test_server_serves_over_both_address_families(host: str, tmp_path) -> None:
    """A client must complete a real round-trip over IPv4 and over IPv6."""
    if host == "::1" and not _has_ipv6_loopback():
        pytest.skip("no IPv6 loopback on this host")

    with SecantusDBServer(host=host, port=0, storage_path=str(tmp_path / "wt")) as srv:
        client = MongoClient(
            _uri(srv.host, srv.port), serverSelectionTimeoutMS=5000, directConnection=True
        )
        try:
            assert client.admin.command("ping")["ok"] == 1.0
            # A ping only proves the handshake; write and read back so the whole
            # wire path is exercised on this family.
            client.test.probe.insert_one({"_id": 1, "family": host})
            assert client.test.probe.find_one({"_id": 1})["family"] == host
        finally:
            client.close()


def test_bound_host_is_reported_back(tmp_path) -> None:
    """`srv.host` reflects what was actually bound, so callers can build a URI."""
    with SecantusDBServer(host="127.0.0.1", port=0, storage_path=str(tmp_path / "wt")) as srv:
        assert srv.host == "127.0.0.1"
        assert srv.port > 0
