from __future__ import annotations

import signal
import sys

import bson
import pytest

from secantus.storage import Storage


@pytest.fixture
def storage(tmp_path) -> Storage:
    return Storage(str(tmp_path))


def test_insert_assigns_object_id(storage: Storage) -> None:
    inserted, errors = storage.insert("db", "c", [{"x": 1}])
    assert inserted == 1
    assert errors == []
    docs = storage.find_matching("db", "c", {})
    assert len(docs) == 1
    assert isinstance(docs[0]["_id"], bson.ObjectId)
    assert docs[0]["x"] == 1


def test_insert_respects_provided_id(storage: Storage) -> None:
    storage.insert("db", "c", [{"_id": "abc", "x": 1}])
    docs = storage.find_matching("db", "c", {"_id": "abc"})
    assert docs == [{"_id": "abc", "x": 1}]


def test_duplicate_id_ordered_stops(storage: Storage) -> None:
    inserted, errors = storage.insert("db", "c", [{"_id": 1}, {"_id": 1}, {"_id": 2}], ordered=True)
    assert inserted == 1
    assert len(errors) == 1
    assert storage.count_matching("db", "c", {}) == 1


def test_duplicate_id_unordered_continues(storage: Storage) -> None:
    inserted, errors = storage.insert(
        "db", "c", [{"_id": 1}, {"_id": 1}, {"_id": 2}], ordered=False
    )
    assert inserted == 2
    assert len(errors) == 1
    assert storage.count_matching("db", "c", {}) == 2


def test_update_modifies_matching(storage: Storage) -> None:
    storage.insert("db", "c", [{"_id": 1, "n": 1}, {"_id": 2, "n": 1}])
    result = storage.update_matching("db", "c", {"n": 1}, {"$inc": {"n": 10}}, multi=True)
    assert result["matched"] == 2
    assert result["modified"] == 2
    docs = sorted(storage.find_matching("db", "c", {}), key=lambda d: d["_id"])
    assert [d["n"] for d in docs] == [11, 11]


def test_update_single_when_multi_false(storage: Storage) -> None:
    storage.insert("db", "c", [{"_id": 1, "n": 1}, {"_id": 2, "n": 1}])
    result = storage.update_matching("db", "c", {"n": 1}, {"$set": {"n": 5}}, multi=False)
    assert result["matched"] == 1


def test_upsert_creates_when_no_match(storage: Storage) -> None:
    result = storage.update_matching("db", "c", {"k": "abc"}, {"$set": {"v": 9}}, upsert=True)
    assert result["matched"] == 0
    assert result["upserted_id"] is not None
    docs = storage.find_matching("db", "c", {})
    assert docs[0]["k"] == "abc"
    assert docs[0]["v"] == 9


def test_delete_with_limit(storage: Storage) -> None:
    storage.insert("db", "c", [{"_id": i, "tag": "x"} for i in range(5)])
    deleted = storage.delete_matching("db", "c", {"tag": "x"}, limit=2)
    assert deleted == 2
    assert storage.count_matching("db", "c", {}) == 3


def test_drop_collection(storage: Storage) -> None:
    storage.insert("db", "c", [{"x": 1}])
    assert storage.drop_collection("db", "c") is True
    assert storage.find_matching("db", "c", {}) == []
    assert storage.drop_collection("db", "c") is False


def test_list_collections_and_databases(storage: Storage) -> None:
    storage.insert("db1", "c1", [{"x": 1}])
    storage.insert("db1", "c2", [{"x": 1}])
    storage.insert("db2", "c1", [{"x": 1}])
    assert storage.list_collections("db1") == ["c1", "c2"]
    # ``local`` is synthesised when the oplog is enabled (mongod always
    # exposes it); filter it out so this test focuses on user databases.
    user_dbs = [db for db in storage.list_databases() if db != "local"]
    assert user_dbs == ["db1", "db2"]


def test_databases_are_isolated(storage: Storage) -> None:
    storage.insert("db1", "c", [{"_id": 1, "x": "a"}])
    storage.insert("db2", "c", [{"_id": 1, "x": "b"}])
    d1 = storage.find_matching("db1", "c", {})
    d2 = storage.find_matching("db2", "c", {})
    assert d1 == [{"_id": 1, "x": "a"}]
    assert d2 == [{"_id": 1, "x": "b"}]


def test_numeric_id_bridge_int_vs_float(storage: Storage) -> None:
    inserted, _ = storage.insert("db", "c", [{"_id": 1, "x": "int"}], ordered=True)
    assert inserted == 1
    inserted2, errors = storage.insert("db", "c", [{"_id": 1.0, "x": "float"}], ordered=True)
    assert inserted2 == 0
    assert len(errors) == 1


def test_numeric_id_bridge_decimal128(storage: Storage) -> None:
    from bson import Decimal128

    storage.insert("db", "c", [{"_id": 5}])
    _, errors = storage.insert("db", "c", [{"_id": Decimal128("5")}])
    assert len(errors) == 1


def test_numeric_id_bridge_distinct_values_still_ok(storage: Storage) -> None:
    inserted, _ = storage.insert("db", "c", [{"_id": 1}, {"_id": 1.5}, {"_id": 2}])
    assert inserted == 3


def test_bool_id_not_treated_as_numeric(storage: Storage) -> None:
    inserted, _ = storage.insert("db", "c", [{"_id": True}, {"_id": 1}])
    assert inserted == 2


def test_rename_collection_moves_docs_and_indexes(storage: Storage) -> None:
    storage.insert("db", "src", [{"_id": 1, "x": 1}, {"_id": 2, "x": 2}])
    storage.create_index("db", "src", "x_1", {"x": 1}, {})
    ok, err = storage.rename_collection("db", "src", "db", "dst")
    assert ok and err is None
    assert storage.find_matching("db", "src", {}) == []
    docs = sorted(storage.find_matching("db", "dst", {}), key=lambda d: d["_id"])
    assert [d["_id"] for d in docs] == [1, 2]
    names = [i["name"] for i in storage.list_indexes("db", "dst")]
    assert "x_1" in names


def test_rename_collection_missing_source(storage: Storage) -> None:
    ok, err = storage.rename_collection("db", "missing", "db", "dst")
    assert not ok
    assert err is not None and "does not exist" in err


def test_rename_collection_target_exists_without_drop(storage: Storage) -> None:
    storage.insert("db", "src", [{"_id": 1}])
    storage.insert("db", "dst", [{"_id": 99}])
    ok, err = storage.rename_collection("db", "src", "db", "dst")
    assert not ok
    assert err is not None and "exists" in err


def test_rename_collection_drop_target(storage: Storage) -> None:
    storage.insert("db", "src", [{"_id": 1, "from": "src"}])
    storage.insert("db", "dst", [{"_id": 99, "from": "dst"}])
    ok, err = storage.rename_collection("db", "src", "db", "dst", drop_target=True)
    assert ok and err is None
    docs = storage.find_matching("db", "dst", {})
    assert docs == [{"_id": 1, "from": "src"}]


def test_rename_collection_across_databases(storage: Storage) -> None:
    storage.insert("dba", "c", [{"_id": 1}])
    ok, _ = storage.rename_collection("dba", "c", "dbb", "c2")
    assert ok
    assert storage.find_matching("dba", "c", {}) == []
    assert storage.find_matching("dbb", "c2", {}) == [{"_id": 1}]


def test_sort_cross_type_order(storage: Storage) -> None:
    from bson import ObjectId

    storage.insert(
        "db",
        "c",
        [
            {"_id": 1, "v": "string"},
            {"_id": 2, "v": 5},
            {"_id": 3, "v": True},
            {"_id": 4, "v": None},
            {"_id": 5, "v": [1, 2]},
            {"_id": 6, "v": {"x": 1}},
            {"_id": 7, "v": ObjectId()},
        ],
    )
    out = storage.find_matching("db", "c", {}, sort={"v": 1})
    ids = [d["_id"] for d in out]
    # MongoDB order: null < numbers < string < object < array < ObjectId < bool
    pos = {i: ids.index(i) for i in range(1, 8)}
    assert pos[4] < pos[2]  # null < num
    assert pos[2] < pos[1]  # num < string
    assert pos[1] < pos[6]  # string < object
    assert pos[6] < pos[5]  # object < array
    assert pos[5] < pos[7]  # array < ObjectId
    assert pos[7] < pos[3]  # ObjectId < bool


def test_storage_persists_across_reopen(tmp_path) -> None:
    """Close the WT connection and reopen at the same path; data survives.

    The whole point of on-disk storage is that data persists across
    process restarts. This test exercises that end-to-end: write
    documents, an index, and an oplog entry; close; reopen the same
    directory; verify the data is exactly what we wrote.

    Adding a new field, table, or option to a future SecantusDB release
    should keep this test passing — that's the load-bearing format
    compatibility we promise to users.
    """
    s1 = Storage(str(tmp_path))
    try:
        s1.insert(
            "winelog",
            "bottles",
            [
                {"_id": 1, "name": "Pommard 2018", "year": 2018},
                {"_id": 2, "name": "Brunello 2015", "year": 2015},
            ],
        )
        s1.create_index("winelog", "bottles", "year_1", {"year": 1}, {})
    finally:
        s1.close()

    s2 = Storage(str(tmp_path))
    try:
        docs = sorted(
            s2.find_matching("winelog", "bottles", {}),
            key=lambda d: d["_id"],
        )
        assert [d["_id"] for d in docs] == [1, 2]
        assert docs[0]["name"] == "Pommard 2018"
        assert docs[1]["year"] == 2015

        # The user-created index is back, alongside the implicit _id_.
        names = {ix["name"] for ix in s2.list_indexes("winelog", "bottles")}
        assert "year_1" in names
        assert "_id_" in names

        # The index actually serves a query (the entries table survived,
        # not just the index metadata).
        plan = s2.explain_plan("winelog", "bottles", {"year": 2018})
        assert plan["kind"] == "IXSCAN"
        assert plan["index_name"] == "year_1"
    finally:
        s2.close()


def test_secantusdb_server_persists_across_restart(tmp_path) -> None:
    """End-to-end: server restart on the same on-disk path keeps data.

    Mirrors the test_storage_persists_across_reopen check but goes
    through the full wire stack — pymongo writes, server restart,
    pymongo reads — which is the path real users exercise.
    """
    from pymongo import MongoClient

    from secantus import SecantusDBServer

    docs = [
        {"_id": 1, "x": "first"},
        {"_id": 2, "x": "second"},
    ]

    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        mc = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            mc["persist_db"]["c"].insert_many(docs)
            mc["persist_db"]["c"].create_index([("x", 1)])
        finally:
            mc.close()

    # Reopen the server on the same path (different bound port).
    with SecantusDBServer(port=0, storage_path=str(tmp_path)) as srv:
        mc = MongoClient(srv.uri, serverSelectionTimeoutMS=2000)
        try:
            stored = sorted(
                mc["persist_db"]["c"].find(),
                key=lambda d: d["_id"],
            )
            assert stored == docs
            names = {ix["name"] for ix in mc["persist_db"]["c"].list_indexes()}
            assert names >= {"_id_", "x_1"}
        finally:
            mc.close()


def test_id_equality_uses_index_not_collscan(storage: Storage) -> None:
    """`{_id: value}` is a primary-key point lookup, reported as IXSCAN.

    The documents table is keyed by encode_value(_id), so an _id-equality
    filter must not fall back to a COLLSCAN. Regression for the bug where
    the virtual `_id_` index was invisible to the planner.
    """
    storage.insert("db", "c", [{"_id": i, "x": i * 10} for i in range(50)])

    for filt in ({"_id": 7}, {"_id": {"$eq": 7}}, {"_id": {"$in": [7, 3, 1]}}):
        plan = storage.explain_plan("db", "c", filt)
        assert plan["kind"] == "IXSCAN", filt
        assert plan["index_name"] == "_id_"
        assert plan["key_pattern"] == {"_id": 1}

    assert storage.find_matching("db", "c", {"_id": 7}) == [{"_id": 7, "x": 70}]
    assert storage.find_matching("db", "c", {"_id": {"$eq": 7}}) == [{"_id": 7, "x": 70}]
    # $in comes back in ascending _id order, missing ids dropped.
    assert storage.find_matching("db", "c", {"_id": {"$in": [7, 3, 99, 1]}}) == [
        {"_id": 1, "x": 10},
        {"_id": 3, "x": 30},
        {"_id": 7, "x": 70},
    ]
    assert storage.find_matching("db", "c", {"_id": {"$in": []}}) == []
    assert storage.find_matching("db", "c", {"_id": 12345}) == []


def test_id_non_point_lookups_stay_collscan(storage: Storage) -> None:
    """Range / regex / compound / subdocument _id filters are not point lookups.

    They must keep their existing (correct) routing rather than being
    mis-served by the equality fast path.
    """
    from bson import Regex

    storage.insert("db", "c", [{"_id": i, "x": i} for i in range(10)])

    # Range operator on _id: COLLSCAN (no _id entries table to range-scan).
    plan = storage.explain_plan("db", "c", {"_id": {"$gt": 5}})
    assert plan["kind"] == "COLLSCAN"
    assert storage.count_matching("db", "c", {"_id": {"$gt": 5}}) == 4

    # Multi-field filter that happens to include _id: not a single-field
    # point lookup, so it stays COLLSCAN — but still returns correctly.
    plan = storage.explain_plan("db", "c", {"_id": 4, "x": 4})
    assert plan["kind"] == "COLLSCAN"
    assert storage.find_matching("db", "c", {"_id": 4, "x": 4}) == [{"_id": 4, "x": 4}]

    # A regex _id is a pattern match, never an equality point lookup
    # (pymongo delivers regex query values as bson.Regex).
    storage.insert("db", "c2", [{"_id": "alpha"}, {"_id": "beta"}])
    plan = storage.explain_plan("db", "c2", {"_id": Regex("^al")})
    assert plan["kind"] == "COLLSCAN"
    assert storage.find_matching("db", "c2", {"_id": Regex("^al")}) == [{"_id": "alpha"}]


def test_id_point_lookup_cross_numeric_collision(storage: Storage) -> None:
    """An int _id is found by a float / Decimal128 equality (and vice versa).

    The fast path encodes the query value with the same `encode_value`
    used as the primary key, so the documented cross-numeric collision
    (1 == 1.0 == Decimal128("1")) holds for point lookups too.
    """
    from bson import Decimal128

    storage.insert("db", "c", [{"_id": 1, "v": "a"}])
    assert storage.find_matching("db", "c", {"_id": 1.0}) == [{"_id": 1, "v": "a"}]
    assert storage.find_matching("db", "c", {"_id": Decimal128("1")}) == [{"_id": 1, "v": "a"}]
    assert storage.explain_plan("db", "c", {"_id": 1.0})["kind"] == "IXSCAN"


def test_checkpoint_persists_inserts(tmp_path) -> None:
    """Forcing a checkpoint flushes pending writes; subsequent close+reopen sees them."""
    storage = Storage(str(tmp_path))
    storage.insert("db", "c", [{"_id": 1, "x": 1}])
    storage.checkpoint()
    storage.close()

    reopened = Storage(str(tmp_path))
    try:
        results = reopened.find_matching("db", "c", {})
        assert results == [{"_id": 1, "x": 1}]
    finally:
        reopened.close()


def test_checkpoint_after_close_is_safe(tmp_path) -> None:
    storage = Storage(str(tmp_path))
    storage.close()
    storage.checkpoint()  # no-op; must not raise.


@pytest.mark.skipif(
    sys.platform == "win32",
    reason="signal.SIGKILL doesn't exist on Windows; the test simulates a "
    "hard crash by self-SIGKILL which has no portable equivalent there.",
)
def test_inserts_survive_simulated_crash(tmp_path) -> None:
    """Writes persist across SIGKILL — i.e. no checkpoint, no clean close.

    Regression for the chaos-monkey finding (``bench/chaos.py``): with
    WT logging disabled (the old default), every uncommitted write
    between checkpoints was lost on SIGKILL. ``bench/chaos.py`` over a
    3-minute, 17-kill run reported 432,881 acked / 1 persisted. With
    logging enabled, the same workload now persists ~99.98% of acks.

    This test exercises the same code path in miniature: a worker
    subprocess writes ``N`` documents, then SIGKILLs itself before any
    explicit checkpoint or clean close. The parent reopens the same
    storage path and expects all ``N`` writes back.

    Subprocess-based because Python can't safely simulate SIGKILL
    in-process — the connection has to actually exit without WT
    getting the chance to flush.
    """
    import subprocess
    import sys
    import textwrap

    n_writes = 1000
    storage_dir = tmp_path / "wt"

    worker_script = textwrap.dedent(
        f"""
        import os, signal
        from secantus.storage import Storage
        s = Storage({str(storage_dir)!r})
        s.insert("db", "c", [{{"_id": i, "n": i}} for i in range(1, {n_writes + 1})])
        # Simulate hard kill: don't close, don't checkpoint, don't even
        # let the interpreter atexit run. SIGKILL the process from itself
        # so WT cannot flush.
        os.kill(os.getpid(), signal.SIGKILL)
        """
    )

    # The kill happens before the subprocess exits cleanly, so we expect
    # a non-zero return / -SIGKILL signal indication.
    result = subprocess.run([sys.executable, "-c", worker_script], capture_output=True, timeout=30)
    assert result.returncode == -signal.SIGKILL, (
        f"worker did not SIGKILL itself; rc={result.returncode}, stderr={result.stderr!r}"
    )

    # Reopen the same storage path. With logging enabled, recovery
    # replays the journal and all 1000 docs come back.
    s2 = Storage(str(storage_dir))
    try:
        docs = s2.find_matching("db", "c", {})
        ns = sorted(d["n"] for d in docs)
        assert ns == list(range(1, n_writes + 1)), (
            f"expected {n_writes} docs back after SIGKILL+reopen, "
            f"got {len(docs)}; missing {set(range(1, n_writes + 1)) - set(ns)}"
        )
    finally:
        s2.close()


def test_profile_defaults_are_off(tmp_path) -> None:
    s = Storage(str(tmp_path))
    try:
        out = s.get_profile("admin")
        assert out == {"level": 0, "slowms": 100, "sampleRate": 1.0}
    finally:
        s.close()


def test_profile_set_then_get_round_trips(tmp_path) -> None:
    s = Storage(str(tmp_path))
    try:
        s.set_profile("admin", level=2, slowms=50, sample_rate=0.5)
        out = s.get_profile("admin")
        assert out == {"level": 2, "slowms": 50, "sampleRate": 0.5}
        # Other dbs untouched.
        assert s.get_profile("other") == {"level": 0, "slowms": 100, "sampleRate": 1.0}
    finally:
        s.close()


def test_profile_set_validates(tmp_path) -> None:
    s = Storage(str(tmp_path))
    try:
        with pytest.raises(ValueError):
            s.set_profile("admin", level=3)
        with pytest.raises(ValueError):
            s.set_profile("admin", level=1, slowms=-1)
        with pytest.raises(ValueError):
            s.set_profile("admin", level=1, sample_rate=2.0)
    finally:
        s.close()


def test_ensure_profile_collection_is_capped(tmp_path) -> None:
    s = Storage(str(tmp_path))
    try:
        s.ensure_profile_collection("appdb")
        opts = s.get_collection_options("appdb", "system.profile")
        assert opts.get("capped") is True
        assert opts.get("size") == 10 * 1024 * 1024
        # Idempotent.
        s.ensure_profile_collection("appdb")
        assert s.get_collection_options("appdb", "system.profile").get("capped") is True
    finally:
        s.close()


# --- local.oplog.rs synthetic view ----------------------------------------


def test_oplog_rs_appears_in_list_collections(tmp_path) -> None:
    s = Storage(str(tmp_path / "wt"))
    try:
        # ``local`` is listed even when no user collection lives there,
        # because mongod always exposes the local database when the
        # oplog is enabled.
        assert "local" in s.list_databases()
        # ``oplog.rs`` is synthesised in ``local`` regardless of whether
        # any other collection in ``local`` exists.
        assert "oplog.rs" in s.list_collections("local")
    finally:
        s.close()


def test_oplog_rs_get_collection_options_reports_capped(tmp_path) -> None:
    s = Storage(str(tmp_path / "wt"))
    try:
        opts = s.get_collection_options("local", "oplog.rs")
        assert opts == {
            "capped": True,
            "size": s.oplog_max_entries * 16 * 1024,
            "max": s.oplog_max_entries,
        }
    finally:
        s.close()


def test_oplog_rs_find_returns_decoded_oplog_entries(tmp_path) -> None:
    s = Storage(str(tmp_path / "wt"))
    try:
        s.insert("appdb", "things", [{"_id": 1, "v": "alpha"}])
        s.insert("appdb", "things", [{"_id": 2, "v": "beta"}])
        rows = s.find_matching("local", "oplog.rs", {})
        # Two inserts → two ``op: "i"`` entries on appdb.things, in order.
        i_rows = [r for r in rows if r.get("op") == "i"]
        assert len(i_rows) == 2
        assert i_rows[0]["ns"] == "appdb.things"
        assert i_rows[0]["o"]["_id"] == 1
        assert i_rows[1]["o"]["_id"] == 2
        # Walked seq-order, which equals ts-order — first ts <= second.
        assert i_rows[0]["ts"] <= i_rows[1]["ts"]
    finally:
        s.close()


def test_oplog_rs_find_honours_filter_skip_limit(tmp_path) -> None:
    s = Storage(str(tmp_path / "wt"))
    try:
        s.insert("appdb", "c", [{"_id": i} for i in range(5)])
        i_rows = s.find_matching("local", "oplog.rs", {"op": "i"})
        assert len(i_rows) == 5
        page = s.find_matching("local", "oplog.rs", {"op": "i"}, skip=1, limit=2)
        assert len(page) == 2
        assert page[0]["o"]["_id"] == 1
        assert page[1]["o"]["_id"] == 2
    finally:
        s.close()


def test_oplog_rs_count_matching(tmp_path) -> None:
    s = Storage(str(tmp_path / "wt"))
    try:
        s.insert("appdb", "c", [{"_id": i} for i in range(3)])
        assert s.count_matching("local", "oplog.rs", {"op": "i"}) == 3
        assert s.count_matching("local", "oplog.rs", {"op": "u"}) == 0
    finally:
        s.close()


def test_oplog_rs_projection_strips_fields(tmp_path) -> None:
    s = Storage(str(tmp_path / "wt"))
    try:
        s.insert("appdb", "c", [{"_id": 1}])
        rows = s.find_matching(
            "local", "oplog.rs", {"op": "i"}, projection={"_id": 0, "op": 1, "ns": 1}
        )
        assert rows == [{"op": "i", "ns": "appdb.c"}]
    finally:
        s.close()


def test_oplog_rs_sort_descending_reverses_order(tmp_path) -> None:
    s = Storage(str(tmp_path / "wt"))
    try:
        s.insert("appdb", "c", [{"_id": 1}])
        s.insert("appdb", "c", [{"_id": 2}])
        rows = s.find_matching("local", "oplog.rs", {"op": "i"}, sort={"ts": -1})
        assert rows[0]["o"]["_id"] == 2
        assert rows[1]["o"]["_id"] == 1
    finally:
        s.close()


def test_oplog_rs_disabled_when_oplog_off(tmp_path) -> None:
    s = Storage(str(tmp_path / "wt"), enable_oplog=False)
    try:
        assert "oplog.rs" not in s.list_collections("local")
        assert "local" not in s.list_databases()
    finally:
        s.close()


# --- WT-checkpoint backup -------------------------------------------------


def test_create_archive_round_trips_inserts(tmp_path) -> None:
    """Checkpoint+tar an on-disk WT home, extract elsewhere, verify docs survive."""
    import tarfile

    src = tmp_path / "src"
    archive = tmp_path / "backup.tar.gz"
    dst = tmp_path / "dst"

    s = Storage(str(src))
    try:
        s.insert("appdb", "things", [{"_id": 1, "v": "alpha"}, {"_id": 2, "v": "beta"}])
        result = s.create_archive(str(archive))
        assert result["path"] == str(archive)
        assert int(result["sizeBytes"]) > 0
        assert archive.exists()
    finally:
        s.close()

    dst.mkdir()
    with tarfile.open(archive, "r:gz") as tar:
        tar.extractall(dst, filter="data")
    s2 = Storage(str(dst))
    try:
        rows = sorted(s2.find_matching("appdb", "things"), key=lambda d: d["_id"])
        assert [r["v"] for r in rows] == ["alpha", "beta"]
    finally:
        s2.close()


def test_create_archive_in_memory_refuses(tmp_path) -> None:
    import pytest

    s = Storage()  # :memory: default
    try:
        with pytest.raises(RuntimeError, match="in-memory"):
            s.create_archive(str(tmp_path / "x.tar.gz"))
    finally:
        s.close()


def test_create_archive_creates_parent_directory(tmp_path) -> None:
    s = Storage(str(tmp_path / "src"))
    try:
        s.insert("appdb", "c", [{"_id": 1}])
        out = tmp_path / "nested" / "dirs" / "backup.tar.gz"
        result = s.create_archive(str(out))
        assert out.exists()
        assert result["sizeBytes"] > 0
    finally:
        s.close()


def test_drop_collection_sees_writes_from_other_threads(tmp_path) -> None:
    """drop_collection must see rows committed by other threads.

    Companion to the snapshot-refresh fix in the mutating scanners
    (drop_collection / drop_database / rename_collection / drop_index /
    drop_all_indexes): a pinned read snapshot on the dropping thread made
    the prefix-scan miss other threads' rows, surfacing in the pymongo
    gauge as drop-then-reinsert E11000. The full pin needs server-layer
    state and is covered by the gauge (test_cursor.py: TestCursor +
    TestRawBatchCommandCursor); this pins the storage-level contract."""
    import threading

    s = Storage(str(tmp_path))
    try:
        # Pin this thread's snapshot: a FAILED duplicate insert leaves the
        # overwrite=False doc-table cursor positioned on the conflicting
        # row, and no later operation on this thread resets that cursor
        # variant — the session's read snapshot stays pinned from here on.
        # (pymongo's TestCursor runs duplicate-insert tests, which is how
        # the gauge's shared-client thread got pinned in the wild.)
        s.insert("db", "pin", [{"_id": 1}])
        _, dup_errors = s.insert("db", "pin", [{"_id": 1}], ordered=False)
        assert dup_errors and dup_errors[0]["code"] == 11000

        # Another thread commits docs this thread's snapshot predates.
        def writer() -> None:
            s.insert("db", "c", [{"_id": i} for i in range(3)])

        t = threading.Thread(target=writer)
        t.start()
        t.join()

        # The drop on the pinned thread must still see and delete them.
        s.drop_collection("db", "c")

        survivors: list[dict] = []

        def reader() -> None:
            survivors.extend(s.find_matching("db", "c", {}))

        t2 = threading.Thread(target=reader)
        t2.start()
        t2.join()
        assert survivors == []

        # And a re-insert of the same ids must not collide.
        inserted, errors = s.insert("db", "c", [{"_id": i} for i in range(3)])
        assert errors == []
        assert inserted == 3
    finally:
        s.close()


def test_timeseries_allows_duplicate_ids(tmp_path) -> None:
    """Timeseries collections don't enforce _id uniqueness (mongod buckets
    measurements by time; _id is not a key). Pins the doc-key suffix
    scheme: duplicates coexist, find/count see both, _id-filter reads work
    (fast path gated off), delete removes both rows."""
    import datetime as dt

    s = Storage(str(tmp_path))
    try:
        s.create_collection("ts", "m")
        s.set_collection_options("ts", "m", timeseries={"timeField": "time"})
        t0 = dt.datetime(2019, 3, 18, 22, 53, 50)
        inserted, errors = s.insert(
            "ts", "m", [{"_id": 1, "time": t0}, {"_id": 1, "time": t0.replace(second=51)}]
        )
        assert (inserted, errors) == (2, [])
        docs = list(s.find_matching("ts", "m", {}))
        assert len(docs) == 2
        assert [d["_id"] for d in docs] == [1, 1]
        by_id = list(s.find_matching("ts", "m", {"_id": 1}))
        assert len(by_id) == 2
        assert s.delete_matching("ts", "m", {"_id": 1}) == 2
        assert list(s.find_matching("ts", "m", {})) == []
    finally:
        s.close()
