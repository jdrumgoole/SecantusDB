from __future__ import annotations

import socket

import secantus
from secantus import SecantusDBServer


def test_version_string_set() -> None:
    assert secantus.__version__


def test_server_binds_ephemeral_port(tmp_path) -> None:
    with SecantusDBServer(host="127.0.0.1", port=0, storage_path=str(tmp_path)) as server:
        host, port = server.address
        assert host == "127.0.0.1"
        assert port > 0
        with socket.create_connection((host, port), timeout=1.0):
            pass


def test_uri_property_uses_bound_address(tmp_path) -> None:
    with SecantusDBServer(host="127.0.0.1", port=0, storage_path=str(tmp_path)) as server:
        host, port = server.address
        assert server.uri == f"mongodb://{host}:{port}/"
