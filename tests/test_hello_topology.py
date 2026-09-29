"""``hello``'s ``topologyVersion.processId`` must be stable across calls.

The SDAM spec treats a *changed* ``processId`` as a server restart, which makes
drivers invalidate and clear the connection pool (close + reconnect). Minting a
fresh ObjectId per hello caused a spurious pool-clear on nearly every monitoring
heartbeat — surfaced by the Java driver's connection-pool-logging and
client-metadata event-count tests. Pin it once per process.
"""

from __future__ import annotations

from pymongo import MongoClient

from secantus import SecantusDBServer


def test_hello_process_id_is_stable_across_calls(wt_home):
    with SecantusDBServer(port=0, storage_path=wt_home) as srv:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=2000, directConnection=True)
        try:
            h1 = client.admin.command("hello")
            h2 = client.admin.command("hello")
            # A second client (fresh connection) must see the same processId —
            # it identifies the server *process*, not the connection.
            other = MongoClient(srv.uri, serverSelectionTimeoutMS=2000, directConnection=True)
            try:
                h3 = other.admin.command("hello")
            finally:
                other.close()
        finally:
            client.close()

    pid1 = h1["topologyVersion"]["processId"]
    assert pid1 == h2["topologyVersion"]["processId"]
    assert pid1 == h3["topologyVersion"]["processId"]
    # counter stays 0 (topology never changes on a single-node surrogate).
    assert h1["topologyVersion"]["counter"] == 0


def test_hello_ok_is_echoed_only_when_the_client_asks(wt_home):
    """``helloOk: true`` decides which command a driver uses, for good.

    A driver puts ``helloOk: true`` in its handshake to ask whether this server
    understands the modern ``hello``; the echo says yes, and the driver then
    speaks ``hello`` for the life of the connection. Without it the driver
    concludes the server predates ``hello`` and falls back to the legacy
    ``isMaster`` on EVERY connection — verified on the wire against
    mongo-go-driver's SDAM monitor, which sent us ``isMaster`` where it sent
    mongod ``hello``.

    Both directions matter: mongod echoes only when asked (measured 8.2.11,
    2026-09-29), so echoing unconditionally would be its own divergence.
    """
    with SecantusDBServer(port=0, storage_path=wt_home) as srv:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=2000, directConnection=True)
        try:
            for command in ("hello", "isMaster"):
                asked = client.admin.command({command: 1, "helloOk": True})
                assert asked.get("helloOk") is True, command
                silent = client.admin.command({command: 1})
                assert "helloOk" not in silent, command
        finally:
            client.close()
