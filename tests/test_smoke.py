from __future__ import annotations

import socket

import secantus
from secantus import SecantusDBServer


def test_version_string_set() -> None:
    assert secantus.__version__


def test_server_binds_ephemeral_port(wt_home) -> None:
    with SecantusDBServer(host="127.0.0.1", port=0, storage_path=wt_home) as server:
        host, port = server.address
        assert host == "127.0.0.1"
        assert port > 0
        with socket.create_connection((host, port), timeout=1.0):
            pass


def test_uri_property_uses_bound_address(wt_home) -> None:
    with SecantusDBServer(host="127.0.0.1", port=0, storage_path=wt_home) as server:
        host, port = server.address
        assert server.uri == f"mongodb://{host}:{port}/"


def test_native_extensions_in_the_wheel_can_actually_be_imported() -> None:
    """A compiled extension that SHIPS must load, not merely have been built.

    cibuildwheel ran this file against every wheel, and every wheel built
    green — while `_secantus_server.abi3.so` could not be imported on Linux at
    all:

        ImportError: cannot allocate memory in static TLS block

    Nothing caught it, because the smoke tests imported `secantus` and never
    the Rust extensions. Building a shared library and being able to `dlopen`
    it are different questions; only an import asks the second one, and that
    question is exactly what a user's `pip install` asks first.

    A wheel that legitimately omits an extension (the storage-engine build is
    conditional) skips it. A wheel that CONTAINS one and cannot import it
    fails.
    """
    import importlib
    import importlib.util

    for name in ("_secantus_core", "_secantus_server"):
        try:
            spec = importlib.util.find_spec(name)
        except ImportError as exc:  # a broken spec is itself the failure
            raise AssertionError(f"{name} is present but unimportable: {exc}") from exc
        if spec is None:
            continue  # not built into this wheel; nothing to assert
        importlib.import_module(name)
