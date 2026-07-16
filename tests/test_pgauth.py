"""Unit tests for the PostgreSQL SCRAM exchange (``secantus.sql.pgauth``).

These exercise ``ScramExchange`` directly (no socket, no ``Storage``): the SCRAM
math is pure, so a malformed client message must surface as the typed
``PGAuthError`` rather than a bare ``ValueError`` leaking out of the parse. The
full wire handshake is covered in ``test_pgserver_auth.py``.
"""

from __future__ import annotations

import pytest

from secantus.sql.pgauth import PGAuthError, ScramExchange, mock_credentials


@pytest.mark.parametrize("bad", [b"n,", b"y,", b"n"])
def test_server_first_rejects_truncated_gs2_header(bad):
    # A truncated gs2 header ("n," has no bare message) must raise the typed
    # PGAuthError, not an unpack ValueError. (§I21)
    exch = ScramExchange(mock_credentials())
    with pytest.raises(PGAuthError):
        exch.server_first(bad)


def test_server_first_rejects_missing_client_nonce():
    exch = ScramExchange(mock_credentials())
    with pytest.raises(PGAuthError):
        exch.server_first(b"n,,x=1")  # well-formed header, no r= nonce


def test_server_first_accepts_well_formed_client_first():
    exch = ScramExchange(mock_credentials())
    reply = exch.server_first(b"n,,n=,r=abc123")
    # server-first echoes the client nonce as a prefix of the combined nonce.
    assert reply.startswith(b"r=abc123")
    assert b",s=" in reply and b",i=" in reply
