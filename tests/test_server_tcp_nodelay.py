"""Both wire servers disable Nagle on accepted sockets.

Reply paths write small frames back-to-back (a reply then ReadyForQuery, one
batch item's result then the next); with Nagle on, the second write waits for
the peer's delayed ACK — ~40ms per round trip on Linux, invisible on macOS
loopback. pgjdbc's generated-keys batches (1000 single-row round trips per
test) measured 41.5s per test in CI against 0.2s locally from exactly this.
"""

from __future__ import annotations

import socket
import time

import secantus.server as mongo_server
import secantus.sql.pgserver as pg_server
from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage


def test_tune_helper_sets_nodelay_both_servers():
    for tune in (mongo_server._tune_client_socket, pg_server._tune_client_socket):
        srv = socket.socket()
        srv.bind(("127.0.0.1", 0))
        srv.listen(1)
        cli = socket.create_connection(srv.getsockname(), timeout=5)
        conn, _ = srv.accept()
        try:
            assert conn.getsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY) == 0
            tune(conn)
            assert conn.getsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY) != 0
        finally:
            conn.close()
            cli.close()
            srv.close()


def test_pg_server_accepted_connections_have_nodelay(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        with socket.create_connection((host, port), timeout=5):
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                with srv._conns_lock:
                    conns = list(srv._conns)
                if conns:
                    assert all(
                        c.getsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY) != 0 for c in conns
                    )
                    break
                time.sleep(0.02)
            else:
                raise AssertionError("server never registered the accepted connection")
    finally:
        srv.stop()
        st.close()
