"""Tests for the server-side TLS path.

When ``tls_cert_file`` + ``tls_key_file`` are passed to
``SecantusDBServer``, accepted sockets are TLS-wrapped before the
mongo wire protocol starts. pymongo connects over TLS with
``?tls=true&tlsCAFile=<ca>`` against the CA we used to sign the
server cert.

Without TLS the daemon must behave exactly as before — that's the
no-regression guarantee for the existing 1300+ tests that don't
pass cert files.
"""

from __future__ import annotations

import socket
import ssl
import time
from pathlib import Path

import pytest
import trustme
from pymongo import MongoClient

from secantus import SecantusDBServer


@pytest.fixture(scope="module")
def cert_authority() -> trustme.CA:
    """Per-module CA. trustme.CA() is cheap (~100ms) but creating one
    per test would dominate; the certs it signs are themselves
    per-test via ``issue_cert``."""
    return trustme.CA()


@pytest.fixture
def tls_files(tmp_path: Path, cert_authority: trustme.CA) -> tuple[Path, Path, Path]:
    """Write a fresh server cert + key + CA cert into ``tmp_path``
    and return their paths. The cert covers ``127.0.0.1`` so pymongo's
    hostname-verification path passes against a loopback connection."""
    server_cert = cert_authority.issue_cert("127.0.0.1")
    cert_path = tmp_path / "server.crt"
    key_path = tmp_path / "server.key"
    ca_path = tmp_path / "ca.crt"
    server_cert.cert_chain_pems[0].write_to_path(cert_path)
    server_cert.private_key_pem.write_to_path(key_path)
    cert_authority.cert_pem.write_to_path(ca_path)
    return cert_path, key_path, ca_path


def test_tls_round_trip_insert_find(tmp_path, tls_files) -> None:
    """End-to-end: pymongo connects with TLS, inserts a doc, reads it
    back. The default suite never hits this path because no fixture
    has been passing tls_* kwargs before now."""
    cert_path, key_path, ca_path = tls_files
    srv = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / "data"),
        tls_cert_file=str(cert_path),
        tls_key_file=str(key_path),
    )
    srv.start()
    try:
        # pymongo URI: ?tls=true is the modern flag; tlsCAFile points
        # at the CA we used so the server cert verifies.
        uri = f"mongodb://127.0.0.1:{srv.port}/?tls=true&tlsCAFile={ca_path}"
        client = MongoClient(uri, serverSelectionTimeoutMS=3000)
        try:
            client["tlsdb"]["coll"].insert_one({"_id": 1, "v": "encrypted-hi"})
            rows = list(client["tlsdb"]["coll"].find())
            assert rows == [{"_id": 1, "v": "encrypted-hi"}]
        finally:
            client.close()
    finally:
        srv.stop()


def test_tls_server_rejects_plaintext_client(tmp_path, tls_files) -> None:
    """A client that opens a raw TCP socket and sends MongoDB wire
    bytes (no TLS handshake) gets dropped — the handshake fails on
    the server side and the connection is closed. The server keeps
    serving everyone else."""
    cert_path, key_path, ca_path = tls_files
    srv = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / "data"),
        tls_cert_file=str(cert_path),
        tls_key_file=str(key_path),
    )
    srv.start()
    try:
        # Raw TCP. No TLS. Send a few junk bytes and read until close.
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(3.0)
        s.connect(("127.0.0.1", srv.port))
        s.sendall(b"GET / HTTP/1.0\r\n\r\n")  # arbitrary non-TLS payload
        try:
            data = s.recv(4096)
        except (ConnectionResetError, OSError):
            data = b""
        assert data == b"", f"non-TLS client should have been dropped, got {data!r}"
        s.close()
        # Server still serving normal TLS clients after the bad-handshake.
        uri = f"mongodb://127.0.0.1:{srv.port}/?tls=true&tlsCAFile={ca_path}"
        client = MongoClient(uri, serverSelectionTimeoutMS=3000)
        try:
            client.admin.command("ping")
        finally:
            client.close()
    finally:
        srv.stop()


def test_no_tls_args_keeps_plaintext_behavior(tmp_path) -> None:
    """The pre-TLS no-regression guarantee: a server constructed with
    no tls_* kwargs serves plaintext exactly as before."""
    srv = SecantusDBServer(port=0, storage_path=str(tmp_path / "data"))
    assert srv._ssl_context is None  # noqa: SLF001
    srv.start()
    try:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            client["d"]["c"].insert_one({"_id": 1})
            assert list(client["d"]["c"].find()) == [{"_id": 1}]
        finally:
            client.close()
    finally:
        srv.stop()


def test_partial_tls_args_raises(tmp_path, tls_files) -> None:
    """Either both cert+key or neither — half-configured TLS is
    almost certainly a deployment mistake and should fail at startup,
    not silently fall back to plaintext."""
    cert_path, _key_path, _ca_path = tls_files
    with pytest.raises(ValueError, match="both be set or both be None"):
        SecantusDBServer(
            port=0,
            storage_path=str(tmp_path / "data"),
            tls_cert_file=str(cert_path),
            # no tls_key_file
        )


def test_missing_cert_file_fails_at_startup(tmp_path) -> None:
    """A path that doesn't exist surfaces a clean exception during
    __init__ (not a runtime accept-loop crash)."""
    with pytest.raises((FileNotFoundError, ssl.SSLError, OSError)):
        SecantusDBServer(
            port=0,
            storage_path=str(tmp_path / "data"),
            tls_cert_file=str(tmp_path / "missing.crt"),
            tls_key_file=str(tmp_path / "missing.key"),
        )


def test_tls_handshake_failure_doesnt_consume_connection_slot(tmp_path, tls_files) -> None:
    """If a bad TLS handshake leaked the active-connections counter,
    enough failed handshakes would lock everyone out. Verify the
    counter is correctly decremented on handshake error."""
    cert_path, key_path, ca_path = tls_files
    srv = SecantusDBServer(
        port=0,
        storage_path=str(tmp_path / "data"),
        tls_cert_file=str(cert_path),
        tls_key_file=str(key_path),
    )
    srv.start()
    try:
        # Several non-TLS connections in a row — each should be
        # rejected and not pin a slot.
        for _ in range(5):
            s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
            s.settimeout(2.0)
            s.connect(("127.0.0.1", srv.port))
            try:
                s.sendall(b"\x00\x00\x00\x00")
                with contextlib_suppress(OSError):
                    s.recv(1)
            finally:
                s.close()
        # Let the accept-loop drain.
        time.sleep(0.1)
        assert srv._active_conns == 0, (  # noqa: SLF001
            f"active_conns leaked after failed handshakes: {srv._active_conns}"  # noqa: SLF001
        )
        # A legit TLS client still works.
        uri = f"mongodb://127.0.0.1:{srv.port}/?tls=true&tlsCAFile={ca_path}"
        client = MongoClient(uri, serverSelectionTimeoutMS=3000)
        try:
            client.admin.command("ping")
        finally:
            client.close()
    finally:
        srv.stop()


# Stdlib contextlib.suppress, imported locally to keep the test
# file's top-of-file import block focused on test machinery.
def contextlib_suppress(*excs):
    import contextlib

    return contextlib.suppress(*excs)
