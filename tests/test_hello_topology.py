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


def test_hello_process_id_is_stable_across_calls(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
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
