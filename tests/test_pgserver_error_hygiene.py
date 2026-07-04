"""Wire-level error hygiene: an unexpected internal error must not leak the raw
Python exception text to the client (security review 2026-07-04 §I17).

A curated ``SQLError`` still surfaces its real SQLSTATE + message (that's the
user-facing contract), but an *unexpected* exception in the query path is logged
server-side and answered with a generic ``XX000 internal error`` — never the raw
``str(exc)``, which could disclose internal paths, types, or data values. Driven
over a real socket against the real WT-backed ``Storage`` (never ``FakeStorage``).
"""

from __future__ import annotations

import socket

import pytest

from secantus.sql import pgserver as pgserver_mod
from secantus.sql import pgwire
from secantus.sql.pgserver import SecantusPGServer
from secantus.storage import Storage

_SECRET = "SECRET-/etc/passwd-internal-detail-42"


def _read_until_ready(sock) -> list[pgwire.Message]:
    msgs: list[pgwire.Message] = []
    while True:
        m = pgwire.read_message(sock)
        msgs.append(m)
        if m.type == "Z":
            return msgs


@pytest.fixture
def server(tmp_path):
    storage = Storage(str(tmp_path))
    srv = SecantusPGServer(port=0, storage=storage)  # trust mode
    srv.start()
    try:
        yield srv
    finally:
        srv.stop()
        storage.close()


def _connect_and_ready(server) -> socket.socket:
    host, port = server.address
    s = socket.create_connection((host, port), timeout=5)
    s.sendall(pgwire.build_startup_message({"user": "joe", "database": "db"}))
    _read_until_ready(s)  # AuthenticationOk + ParameterStatus* + ReadyForQuery
    return s


def test_unexpected_internal_error_is_generic_on_the_wire(server, monkeypatch):
    def boom(*_a, **_k):
        raise RuntimeError(_SECRET)

    monkeypatch.setattr(pgserver_mod, "run_sql", boom)

    s = _connect_and_ready(server)
    try:
        s.sendall(pgwire.build_query("SELECT 1"))
        msgs = _read_until_ready(s)
        err = next(m for m in msgs if m.type == "E")
        fields = pgwire.parse_error_response(err.payload)
        assert fields["C"] == "XX000"
        assert fields["M"] == "internal error"
        # The raw exception text must not appear in any field of the reply.
        assert not any(_SECRET in v for v in fields.values())
        # The connection survives — a ReadyForQuery followed the error.
        assert msgs[-1].type == "Z"
    finally:
        s.close()


def test_sql_error_still_surfaces_its_real_message(server):
    # A curated SQLError is user-facing and must keep its SQLSTATE + message —
    # the generic-message rule applies only to *unexpected* exceptions.
    s = _connect_and_ready(server)
    try:
        s.sendall(pgwire.build_query("SELECT * FROM does_not_exist"))
        msgs = _read_until_ready(s)
        err = next(m for m in msgs if m.type == "E")
        fields = pgwire.parse_error_response(err.payload)
        assert fields["C"] == "42P01"  # undefined_table
        assert "does_not_exist" in fields["M"]
    finally:
        s.close()
