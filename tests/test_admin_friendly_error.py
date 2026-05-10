"""Unit tests for ``secantus.admin.client.friendly_error``."""

from __future__ import annotations

from secantus.admin.client import friendly_error


def test_strips_topology_description() -> None:
    raw = (
        "127.0.0.1:1: [Errno 61] Connection refused (configured timeouts: "
        "socketTimeoutMS: 20000.0ms, connectTimeoutMS: 20000.0ms), Timeout: "
        "30s, Topology Description: <TopologyDescription id: 67ab1234, "
        "topology_type: Single, servers: [<ServerDescription ('127.0.0.1', 1) "
        "server_type: Unknown, rtt: None, error=AutoReconnect('...')>]>"
    )
    out = friendly_error(Exception(raw))
    assert out == "127.0.0.1:1: [Errno 61] Connection refused"


def test_strips_full_error_dict() -> None:
    raw = (
        "E11000 duplicate key error in index name_1: _id=1, full error: {'ok': 0.0, 'code': 11000}"
    )
    out = friendly_error(Exception(raw))
    assert out == "E11000 duplicate key error in index name_1: _id=1"


def test_keeps_first_line_only() -> None:
    out = friendly_error(Exception("first line\nsecond line\nthird"))
    assert out == "first line"


def test_falls_back_to_class_name_for_empty_message() -> None:
    out = friendly_error(RuntimeError(""))
    assert out == "RuntimeError"


def test_passes_short_clean_messages_through() -> None:
    assert friendly_error(Exception("not authorized")) == "not authorized"
