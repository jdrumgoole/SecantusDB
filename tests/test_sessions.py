"""Tests for ``secantus.sessions.SessionRegistry`` plus the
end-to-end behaviour of ``startSession`` / ``endSessions`` /
``refreshSessions`` driven via pymongo.

The unit tests pin the pure registry semantics (idle pruning,
implicit refresh, byte-key handling); the integration tests
exercise the wire shape drivers actually see.
"""

from __future__ import annotations

import pytest
from bson import Binary
from pymongo import MongoClient

from secantus import SecantusDBServer
from secantus.sessions import DEFAULT_IDLE_TTL_SECONDS, SessionRegistry


class TestSessionRegistryUnit:
    def test_register_and_is_known(self) -> None:
        reg = SessionRegistry()
        lsid = b"\x01" * 16
        assert not reg.is_known(lsid)
        reg.register(lsid)
        assert reg.is_known(lsid)
        assert len(reg) == 1

    def test_register_rejects_non_16_byte_lsid(self) -> None:
        reg = SessionRegistry()
        with pytest.raises(ValueError):
            reg.register(b"too-short")
        with pytest.raises(ValueError):
            reg.register("not-bytes")  # type: ignore[arg-type]

    def test_unregister_removes_session(self) -> None:
        reg = SessionRegistry()
        lsid = b"\x02" * 16
        reg.register(lsid)
        reg.unregister(lsid)
        assert not reg.is_known(lsid)
        # Unregister of an unknown lsid is a silent no-op.
        reg.unregister(lsid)

    def test_refresh_is_implicit_register(self) -> None:
        """``refreshSessions`` semantics: bump a known session's TTL,
        or implicitly create it if absent (matches mongod)."""
        reg = SessionRegistry()
        lsid = b"\x03" * 16
        reg.refresh(lsid)
        assert reg.is_known(lsid)

    def test_idle_prune_removes_stale_sessions(self) -> None:
        """Sessions older than ``idle_ttl_seconds`` are evicted on the
        next ``register`` call (opportunistic) or on explicit
        ``prune_idle()``."""
        clock = [1000.0]
        reg = SessionRegistry(idle_ttl_seconds=60, time_func=lambda: clock[0])
        old_lsid = b"\xaa" * 16
        new_lsid = b"\xbb" * 16
        reg.register(old_lsid)
        clock[0] = 1100  # +100s — past TTL
        reg.register(new_lsid)  # triggers prune of old_lsid
        assert not reg.is_known(old_lsid)
        assert reg.is_known(new_lsid)

    def test_explicit_prune_returns_count(self) -> None:
        clock = [0.0]
        reg = SessionRegistry(idle_ttl_seconds=10, time_func=lambda: clock[0])
        for i in range(3):
            reg.register(bytes([i]) * 16)
        clock[0] = 100
        # All three are now stale.
        assert reg.prune_idle() == 3
        assert len(reg) == 0

    def test_default_ttl_matches_hello_advertise(self) -> None:
        # ``hello`` advertises ``logicalSessionTimeoutMinutes: 30``;
        # the registry's default TTL must agree or session pruning
        # races driver expectations.
        assert DEFAULT_IDLE_TTL_SECONDS == 30 * 60


class TestSessionCommandsViaWire:
    @pytest.fixture
    def server(self, tmp_path):
        with SecantusDBServer(port=0, storage_path=str(tmp_path / "wt")) as srv:
            yield srv

    @pytest.fixture
    def client(self, server: SecantusDBServer):
        mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
        try:
            yield mc
        finally:
            mc.close()

    def test_start_session_registers_real_id(
        self, server: SecantusDBServer, client: MongoClient
    ) -> None:
        out = client.admin.command("startSession")
        assert out["ok"] == 1.0
        # Wire shape: ``{id: BinData(4, <uuid>), timeoutMinutes: 30}``.
        wrapped = out["id"]
        assert isinstance(wrapped, dict)
        binary = wrapped["id"]
        assert isinstance(binary, Binary)
        assert binary.subtype == 4
        lsid_bytes = bytes(binary)
        assert len(lsid_bytes) == 16
        assert server.sessions.is_known(lsid_bytes)
        assert out["timeoutMinutes"] == 30

    def test_end_sessions_drops_listed_ids(
        self, server: SecantusDBServer, client: MongoClient
    ) -> None:
        started = client.admin.command("startSession")
        lsid = bytes(started["id"]["id"])
        assert server.sessions.is_known(lsid)
        client.admin.command("endSessions", [started["id"]])
        assert not server.sessions.is_known(lsid)

    def test_refresh_sessions_implicit_create(
        self, server: SecantusDBServer, client: MongoClient
    ) -> None:
        """``refreshSessions`` against an unknown lsid registers it —
        matches mongod's behaviour for drivers that send a refresh
        before any explicit ``startSession``."""
        fake = {"id": Binary(b"\x42" * 16, 4)}
        assert not server.sessions.is_known(b"\x42" * 16)
        client.admin.command("refreshSessions", [fake])
        assert server.sessions.is_known(b"\x42" * 16)

    def test_implicit_session_registration_on_lsid_command(
        self, server: SecantusDBServer, client: MongoClient
    ) -> None:
        """Drivers that don't call ``startSession`` still get implicit
        registration — pymongo always attaches an ``lsid`` to commands
        once it has minted one internally, and the registry tracks
        them so ``logicalSessionTimeoutMinutes`` is meaningful."""
        # Issue any command; pymongo will attach an lsid for it.
        client["sessions_implicit_db"]["c"].insert_one({"_id": 1})
        # At least one session should now be tracked — the one pymongo
        # auto-attached.
        assert len(server.sessions) >= 1
