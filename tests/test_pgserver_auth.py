"""P4 tests: SCRAM-SHA-256 authentication and TLS.

A pure-Python SCRAM client (RFC 5802) authenticates against a real
``SecantusPGServer`` configured with ``require_auth`` + users, and a TLS test
negotiates an encrypted channel via an ephemeral ``trustme`` CA. psql/psycopg
need libpq (absent here), so the wire exchange is exercised directly.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import os
import socket
import ssl
import struct

import pytest
import trustme

from secantus.auth import saslprep
from secantus.sql import pgwire
from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage


def _read_until_ready(sock) -> list[pgwire.Message]:
    msgs: list[pgwire.Message] = []
    while True:
        m = pgwire.read_message(sock)
        msgs.append(m)
        if m.type == "Z":
            return msgs


def scram_authenticate(sock, user: str, password: str) -> pgwire.Message:
    """Run the client side of SCRAM-SHA-256. Returns the final server message."""
    cnonce = base64.b64encode(os.urandom(18)).decode()
    client_first_bare = f"n=,r={cnonce}"
    sock.sendall(
        pgwire.build_sasl_initial_response("SCRAM-SHA-256", ("n,," + client_first_bare).encode())
    )
    cont = pgwire.read_message(sock)
    subtype, data = pgwire.parse_authentication(cont.payload)
    assert subtype == 11, "expected AuthenticationSASLContinue"
    server_first = data.decode()
    attrs = dict(f.split("=", 1) for f in server_first.split(","))
    salted = hashlib.pbkdf2_hmac(
        "sha256", saslprep(password).encode(), base64.b64decode(attrs["s"]), int(attrs["i"])
    )
    client_key = hmac.new(salted, b"Client Key", hashlib.sha256).digest()
    stored_key = hashlib.sha256(client_key).digest()
    without_proof = f"c=biws,r={attrs['r']}"
    auth_message = f"{client_first_bare},{server_first},{without_proof}"
    client_sig = hmac.new(stored_key, auth_message.encode(), hashlib.sha256).digest()
    proof = bytes(a ^ b for a, b in zip(client_key, client_sig, strict=True))
    sock.sendall(
        pgwire.build_sasl_response(f"{without_proof},p={base64.b64encode(proof).decode()}".encode())
    )
    return pgwire.read_message(sock)  # 'R'(12) on success, 'E' on failure


def _startup(sock, user="joe", database="db"):
    sock.sendall(pgwire.build_startup_message({"user": user, "database": database}))


# --------------------------------------------------------------------------- #


@pytest.fixture
def auth_server(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st, require_auth=True, users={"joe": "s3cret"})
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()
        st.close()


def test_scram_auth_success_then_query(auth_server):
    host, port = auth_server.address
    s = socket.create_connection((host, port), timeout=5)
    try:
        _startup(s)
        req = pgwire.read_message(s)
        assert pgwire.parse_authentication(req.payload)[0] == 10  # AuthenticationSASL
        final = scram_authenticate(s, "joe", "s3cret")
        assert pgwire.parse_authentication(final.payload)[0] == 12  # SASLFinal (verified)
        msgs = _read_until_ready(s)  # AuthenticationOk + ParameterStatus* + ReadyForQuery
        assert any(m.type == "R" for m in msgs)
        # The authenticated connection runs queries normally.
        s.sendall(pgwire.build_query("SELECT 1"))
        out = [m.type for m in _read_until_ready(s)]
        assert out == ["T", "D", "C", "Z"]
    finally:
        s.close()


def test_scram_wrong_password_rejected(auth_server):
    host, port = auth_server.address
    s = socket.create_connection((host, port), timeout=5)
    try:
        _startup(s)
        pgwire.read_message(s)  # AuthenticationSASL
        resp = scram_authenticate(s, "joe", "wrong-password")
        assert resp.type == "E"
        assert pgwire.parse_error_response(resp.payload)["C"] == "28P01"
    finally:
        s.close()


def test_scram_unknown_user_rejected(auth_server):
    host, port = auth_server.address
    s = socket.create_connection((host, port), timeout=5)
    try:
        _startup(s, user="nobody")
        pgwire.read_message(s)  # AuthenticationSASL
        resp = scram_authenticate(s, "nobody", "whatever")
        assert resp.type == "E"
        assert pgwire.parse_error_response(resp.payload)["C"] == "28P01"
    finally:
        s.close()


def test_no_auth_required_stays_trust(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)  # require_auth defaults False
    srv.start()
    try:
        host, port = srv.address
        s = socket.create_connection((host, port), timeout=5)
        _startup(s)
        # First reply is AuthenticationOk (subtype 0), not a SASL challenge.
        first = pgwire.read_message(s)
        assert pgwire.parse_authentication(first.payload)[0] == 0
        s.close()
    finally:
        srv.stop()
        st.close()


@pytest.fixture
def tls_server(tmp_path):
    ca = trustme.CA()
    cert = ca.issue_cert("127.0.0.1")
    cert_file = tmp_path / "server.pem"
    key_file = tmp_path / "server.key"
    cert.cert_chain_pems[0].write_to_path(cert_file)
    cert.private_key_pem.write_to_path(key_file)
    st = Storage(str(tmp_path / "wt"))
    srv = SecantusPGServer(
        port=0,
        storage=st,
        tls_cert_file=str(cert_file),
        tls_key_file=str(key_file),
    )
    srv.start()
    try:
        yield srv, ca
    finally:
        srv.stop()
        st.close()


def test_tls_request_accepted_and_query_over_tls(tls_server):
    srv, ca = tls_server
    host, port = srv.address
    raw = socket.create_connection((host, port), timeout=5)
    try:
        # SSLRequest -> server answers 'S', then we wrap the socket.
        raw.sendall(struct.pack("!ii", 8, pgwire.SSL_REQUEST_CODE))
        assert raw.recv(1) == b"S"
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ca.configure_trust(ctx)
        tls = ctx.wrap_socket(raw, server_hostname="127.0.0.1")
        _startup(tls)
        first = pgwire.read_message(tls)
        assert pgwire.parse_authentication(first.payload)[0] == 0  # trust auth over TLS
        _read_until_ready(tls)
        tls.sendall(pgwire.build_query("SELECT 42"))
        msgs = _read_until_ready(tls)
        data = [pgwire.parse_data_row(m.payload) for m in msgs if m.type == "D"]
        assert data == [[b"42"]]
        tls.close()
    finally:
        raw.close()


def test_tls_declined_when_not_configured(tmp_path):
    st = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=st)
    srv.start()
    try:
        host, port = srv.address
        s = socket.create_connection((host, port), timeout=5)
        s.sendall(struct.pack("!ii", 8, pgwire.SSL_REQUEST_CODE))
        assert s.recv(1) == b"N"  # declined; client may continue in plaintext
        s.close()
    finally:
        srv.stop()
        st.close()


# --------------------------------------------------------------------------- #
# RBAC authorization over the wire (#193). The server is started with
# ``require_auth`` + per-user role bindings, so each statement is gated by
# ``secantus.rbac.check_privilege`` against the authenticated user's roles.
# --------------------------------------------------------------------------- #


def _authenticated_socket(server, user, password, database="db"):
    """Connect, run SCRAM to completion, and drain to ReadyForQuery."""
    host, port = server.address
    s = socket.create_connection((host, port), timeout=5)
    _startup(s, user=user, database=database)
    pgwire.read_message(s)  # AuthenticationSASL
    final = scram_authenticate(s, user, password)
    assert pgwire.parse_authentication(final.payload)[0] == 12  # verified
    _read_until_ready(s)
    return s


@pytest.fixture
def authz_server(tmp_path):
    # Shared storage seeded with a table + row via the embedded (unrestricted)
    # API before the server starts; the wire clients are then gated by roles.
    from secantus.sql import run_sql
    from secantus.sql.session import Session

    storage = Storage(str(tmp_path))
    seed = Session(database="db")
    run_sql(storage, "db", "CREATE TABLE t (id bigint primary key, n int)", session=seed)
    run_sql(storage, "db", "INSERT INTO t (id, n) VALUES (1, 10)", session=seed)

    srv = SecantusPGServer(
        port=0,
        storage=storage,
        require_auth=True,
        users={"reader": "r-pw", "writer": "w-pw"},
        user_roles={
            "reader": [{"role": "read", "db": "db"}],
            "writer": [{"role": "readWrite", "db": "db"}],
        },
    )
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()
        storage.close()


def test_authz_read_role_selects_but_cannot_write(authz_server):
    s = _authenticated_socket(authz_server, "reader", "r-pw")
    try:
        s.sendall(pgwire.build_query("SELECT n FROM t"))
        assert [m.type for m in _read_until_ready(s)] == ["T", "D", "C", "Z"]
        # An INSERT is denied with SQLSTATE 42501, and the connection survives.
        s.sendall(pgwire.build_query("INSERT INTO t (id, n) VALUES (2, 20)"))
        msgs = _read_until_ready(s)
        err = next(m for m in msgs if m.type == "E")
        assert pgwire.parse_error_response(err.payload)["C"] == "42501"
        s.sendall(pgwire.build_query("SELECT n FROM t"))
        assert any(m.type == "D" for m in _read_until_ready(s))
    finally:
        s.close()


def test_authz_readwrite_role_can_insert(authz_server):
    s = _authenticated_socket(authz_server, "writer", "w-pw")
    try:
        s.sendall(pgwire.build_query("INSERT INTO t (id, n) VALUES (3, 30)"))
        assert [m.type for m in _read_until_ready(s)] == ["C", "Z"]
    finally:
        s.close()
