"""End-to-end profiler tests over the wire (pymongo)."""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture
def server(tmp_path):
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        yield srv


@pytest.fixture
def client(server: SecantusDBServer):
    mc = MongoClient(server.uri, serverSelectionTimeoutMS=2000)
    try:
        yield mc
    finally:
        mc.close()


# ---- profile -1 / get state -----------------------------------------------


def test_profile_get_returns_defaults(client: MongoClient) -> None:
    out = client["app"].command("profile", -1)
    assert out["was"] == 0
    assert out["slowms"] == 100
    assert out["sampleRate"] == 1.0


# ---- profile 1 / slowms ---------------------------------------------------


def test_profile_set_then_get_round_trips(client: MongoClient) -> None:
    set_out = client["app"].command("profile", 2, slowms=50, sampleRate=0.75)
    # ``was`` reports prior level (0).
    assert set_out["was"] == 0
    get_out = client["app"].command("profile", -1)
    assert get_out["was"] == 2
    assert get_out["slowms"] == 50
    assert get_out["sampleRate"] == 0.75


def test_profile_invalid_level_rejected(client: MongoClient) -> None:
    with pytest.raises(OperationFailure):
        client["app"].command("profile", 7)


# ---- entries land in system.profile ---------------------------------------


def test_profile_level_2_records_every_op(client: MongoClient) -> None:
    db = client["lvl2"]
    db.command("profile", 2)
    db["c"].insert_one({"_id": 1, "x": 1})
    list(db["c"].find({"_id": 1}))
    # system.profile should have at least one insert + one query entry.
    entries = list(db["system.profile"].find())
    ops = {e["op"] for e in entries}
    assert "insert" in ops
    assert "query" in ops


def test_profile_level_1_only_records_slow(client: MongoClient) -> None:
    db = client["lvl1"]
    # slowms = 0 so every op qualifies; verifies the slowms gate fires.
    db.command("profile", 1, slowms=0)
    db["c"].insert_one({"_id": 1})
    entries = list(db["system.profile"].find())
    assert len(entries) >= 1


def test_profile_level_1_skips_fast_ops(client: MongoClient) -> None:
    db = client["lvl1_fast"]
    # slowms in the future — nothing local should ever take that long.
    db.command("profile", 1, slowms=10_000)
    db["c"].insert_one({"_id": 1})
    list(db["c"].find({"_id": 1}))
    # system.profile may have been auto-created but it should be empty
    # (or omitted entirely if no entry triggered creation).
    if "system.profile" in db.list_collection_names():
        assert db["system.profile"].count_documents({}) == 0


def test_profile_level_0_disables_recording(client: MongoClient) -> None:
    db = client["lvl0"]
    # First arm at level 2 to record at least one entry + create
    # system.profile, then turn off BEFORE counting (so the count itself
    # doesn't get profiled).
    db.command("profile", 2)
    db["c"].insert_one({"_id": 1})
    db.command("profile", 0)
    initial = db["system.profile"].count_documents({})
    assert initial >= 1

    db["c"].insert_one({"_id": 2})
    list(db["c"].find())
    assert db["system.profile"].count_documents({}) == initial


def test_profile_writes_dont_recurse(client: MongoClient) -> None:
    db = client["recurse"]
    db.command("profile", 2)
    db["c"].insert_one({"_id": 1})
    # Query system.profile — that read itself goes through profiling and
    # would recurse if the recursion guard weren't in place. The cap
    # would otherwise grow without bound.
    entries_before = db["system.profile"].count_documents({})
    list(db["system.profile"].find())
    entries_after = db["system.profile"].count_documents({})
    # Reads of system.profile shouldn't write to system.profile.
    assert entries_after == entries_before


def test_profile_collection_is_capped(client: MongoClient) -> None:
    db = client["capped_chk"]
    db.command("profile", 2)
    db["c"].insert_one({"_id": 1})
    info = next(c for c in db.list_collections() if c["name"] == "system.profile")
    assert info["options"].get("capped") is True
    assert info["options"].get("size") == 10 * 1024 * 1024


def test_profile_entry_shape_is_mongod_faithful(client: MongoClient) -> None:
    db = client["shape"]
    db.command("profile", 2)
    db["c"].insert_one({"_id": 1, "x": 1})
    entries = list(db["system.profile"].find())
    assert entries
    e = entries[0]
    for field in ("ts", "op", "ns", "command", "millis", "client", "ok"):
        assert field in e, f"missing {field} in profile entry: {e}"
    assert e["ns"].startswith("shape.")
    assert isinstance(e["millis"], int)
