"""Tests for the in-process embedded SecantusDB server controller."""

from __future__ import annotations

from pymongo import MongoClient

from secantus.admin.embedded import EmbeddedServer


def test_start_returns_uri_pymongo_can_use(tmp_path) -> None:
    embedded = EmbeddedServer(default_storage_path=tmp_path)
    try:
        uri = embedded.start()
        assert uri.startswith("mongodb://127.0.0.1:")
        # The URI works for a pymongo round-trip — proves the in-process
        # server is actually listening and speaking the wire protocol.
        mc = MongoClient(uri, serverSelectionTimeoutMS=2000)
        try:
            mc.admin.command("ping")
        finally:
            mc.close()
    finally:
        embedded.stop()


def test_start_is_idempotent(tmp_path) -> None:
    embedded = EmbeddedServer(default_storage_path=tmp_path)
    try:
        first = embedded.start()
        second = embedded.start()
        assert first == second
    finally:
        embedded.stop()


def test_status_reflects_lifecycle(tmp_path) -> None:
    embedded = EmbeddedServer(default_storage_path=tmp_path)
    assert embedded.status()["running"] is False
    try:
        embedded.start()
        s = embedded.status()
        assert s["running"] is True
        assert s["uri"].startswith("mongodb://127.0.0.1:")
        assert s["storage_path"] == str(tmp_path)
    finally:
        embedded.stop()
    assert embedded.status()["running"] is False


def test_stop_when_not_running_is_safe(tmp_path) -> None:
    embedded = EmbeddedServer(default_storage_path=tmp_path)
    embedded.stop()  # must not raise
    assert embedded.status()["running"] is False


def test_start_with_explicit_storage_path(tmp_path) -> None:
    other = tmp_path / "other-storage"
    embedded = EmbeddedServer(default_storage_path=tmp_path / "default")
    try:
        embedded.start(storage_path=other)
        assert embedded.status()["storage_path"] == str(other)
        assert other.is_dir()
    finally:
        embedded.stop()
