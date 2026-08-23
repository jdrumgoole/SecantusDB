from __future__ import annotations

import signal
import sys
import threading

import bson
import pytest

from secantus.storage import Storage, _frame_doc_value, _unframe_doc_value


@pytest.fixture
def storage(tmp_path):
    # close() in teardown is load-bearing: a never-closed Storage abandons its
    # WiredTiger connection (~2.5 MB + ~17 fds each), so a worker running many
    # of these leaks memory / fds until it dies "not properly terminated" with
    # no output. See tasks/backlog.md #275.
    s = Storage(str(tmp_path))
    try:
        yield s
    finally:
        s.close()


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


def test_create_index_name_conflicts(storage: Storage) -> None:
    from secantus.storage import IndexKeySpecsConflict, IndexOptionsConflict

    storage.create_collection("db", "c")
    assert storage.create_index("db", "c", "idx", {"a": 1}, {}) is True
    # Same name, different key spec -> IndexKeySpecsConflict.
    with pytest.raises(IndexKeySpecsConflict):
        storage.create_index("db", "c", "idx", {"a": -1}, {})
    # Same name, same key, different options -> IndexOptionsConflict.
    with pytest.raises(IndexOptionsConflict):
        storage.create_index("db", "c", "idx", {"a": 1}, {"unique": True})
    # Identical re-create -> no-op (False), no exception.
    assert storage.create_index("db", "c", "idx", {"a": 1}, {}) is False


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
    # MongoDB cross-type order for SCALARS:
    #   null < numbers < string < object < ObjectId < bool
    #
    # An ARRAY does not take a type slot of its own. mongod sorts an array-valued
    # field by its minimum element (ascending), so `[1, 2]` sorts as the number 1
    # and lands among the numbers — before string and object, not after them.
    # This previously asserted `object < array`, the whole-array model we used to
    # implement; mongod 6.0.16 on these exact seven documents returns
    # [4, 5, 2, 1, 6, 7, 3].
    pos = {i: ids.index(i) for i in range(1, 8)}
    assert pos[4] < pos[2]  # null < num
    assert pos[2] < pos[1]  # num < string
    assert pos[1] < pos[6]  # string < object
    assert pos[6] < pos[7]  # object < ObjectId
    assert pos[7] < pos[3]  # ObjectId < bool
    assert pos[5] < pos[2]  # array [1,2] -> min 1 -> before the scalar 5
    assert ids == [4, 5, 2, 1, 6, 7, 3], "must match mongod exactly"


def test_reopen_clamps_seq_counters_past_stale_meta(tmp_path) -> None:
    """A stale persisted meta row must never lower the recovered counters.

    ``_emit_oplog`` deliberately stops re-persisting the oplog-meta row on
    every write (it WT-rollbacks under concurrent writers). The row is only
    refreshed by ``current_cluster_time`` (every ``hello``), ``prune_oplog``,
    and ``close`` — so a checkpoint taken by ``backupArchive`` between two
    refreshes captures a ``next_seq`` / ``next_nat_seq`` that lags the actual
    oplog / natural-order tables. If recovery *trusts* that stale value it
    re-mints an already-used seq: a duplicate oplog seq, or a natural-order
    seq collision that overwrites a live doc's nat entry and corrupts
    capped-collection FIFO eviction after restore. Recovery must clamp each
    counter UP to what the tables contain.
    """
    s1 = Storage(str(tmp_path))
    try:
        s1.insert("db", "c", [{"_id": i} for i in range(6)])
        real_nat = s1._next_nat_seq
        real_seq = s1._next_seq
        assert real_nat > 1  # the inserts minted nat seqs
        # Simulate a hello/current_cluster_time that ran *before* the inserts:
        # it would have persisted these low counters, and the insert path
        # never refreshed them. Force that exact on-disk state, checkpoint,
        # and leave the in-memory counters stale so close() re-persists them.
        s1._next_nat_seq = 1
        s1._next_seq = 1
        s1._persist_oplog_meta()
        s1.checkpoint()
    finally:
        s1.close()

    s2 = Storage(str(tmp_path))
    try:
        # The stale meta (next_*=1) must be overridden by the table scans.
        assert s2._next_nat_seq >= real_nat
        assert s2._next_seq >= real_seq
        # A fresh insert mints a nat seq strictly above every existing one,
        # so the natural-order index stays collision-free.
        before = s2._next_nat_seq
        s2.insert("db", "c", [{"_id": 99}])
        assert s2._next_nat_seq == before + 1
    finally:
        s2.close()


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
        # Same capability probe as ``Storage.extract_backup_archive`` — see the
        # fuller note there. ``filter="data"`` becomes Python 3.14's default, so
        # passing it explicitly pins today's behaviour to tomorrow's; the
        # ``hasattr`` guard keeps 3.10.11 (pre-backport) working.
        if hasattr(tarfile, "data_filter"):
            tar.extractall(dst, filter="data")
        else:
            tar.extractall(dst)
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


def test_close_logs_teardown_errors_instead_of_swallowing(tmp_path, caplog) -> None:
    # A failure during the close() teardown (checkpoint, session/conn
    # close, oplog-meta persist) must be logged, never silently
    # discarded — a checkpoint error on the final flush is a durability
    # signal (issue #138). close() still completes idempotently.
    import logging

    s = Storage(str(tmp_path))
    s.insert("db", "c", [{"x": 1}])

    def _boom() -> None:
        raise RuntimeError("simulated checkpoint failure")

    s._persist_oplog_meta = _boom  # type: ignore[method-assign]

    with caplog.at_level(logging.ERROR, logger="secantus.storage.close"):
        s.close()  # must not raise

    assert s._closed is True
    matching = [
        r
        for r in caplog.records
        if r.name == "secantus.storage.close" and r.levelno >= logging.ERROR
    ]
    assert matching, "close() teardown error was swallowed without logging"
    assert any("simulated checkpoint failure" in (r.exc_text or "") for r in matching) or any(
        "oplog meta" in r.getMessage() for r in matching
    )


def test_durable_resolution(tmp_path, monkeypatch) -> None:
    """`durable` precedence: FORCE_DURABLE > explicit arg > FAST-default > durable."""
    monkeypatch.delenv("SECANTUS_FORCE_DURABLE", raising=False)
    monkeypatch.setenv("SECANTUS_TEST_FAST_STORAGE", "1")
    # Explicit args win over the fast-test default.
    s = Storage(str(tmp_path / "a"), durable=True)
    assert s._durable is True
    s.close()
    s = Storage(str(tmp_path / "b"), durable=False)
    assert s._durable is False
    s.close()
    # Unset arg + FAST env -> fast (non-durable).
    s = Storage(str(tmp_path / "c"))
    assert s._durable is False
    s.close()
    # No env at all -> the shipped/production default is durable.
    monkeypatch.delenv("SECANTUS_TEST_FAST_STORAGE", raising=False)
    s = Storage(str(tmp_path / "d"))
    assert s._durable is True
    s.close()
    # FORCE_DURABLE wins over everything, even an explicit durable=False.
    monkeypatch.setenv("SECANTUS_FORCE_DURABLE", "1")
    monkeypatch.setenv("SECANTUS_TEST_FAST_STORAGE", "1")
    s = Storage(str(tmp_path / "e"), durable=False)
    assert s._durable is True
    s.close()


def test_fast_storage_round_trips(tmp_path) -> None:
    """durable=False (journal on, close-checkpoint skipped) still creates tables
    on disk and round-trips a document within the session."""
    s = Storage(str(tmp_path / "fast"), durable=False)
    try:
        s.insert("db", "c", [{"_id": 1, "x": "hi"}])
        assert s.find_matching("db", "c", {"_id": 1}) == [{"_id": 1, "x": "hi"}]
    finally:
        s.close()


def _ddl_crud_round(storage: Storage, db: str, coll: str, writers: int, per_writer: int) -> None:
    """One race round: N fresh writer threads vs a concurrent index build.

    Each round lives in its own function so the thread closures capture
    parameters, never a loop variable.
    """
    storage.insert(db, coll, [{"_id": k, "x": k % 5} for k in range(20)])
    barrier = threading.Barrier(writers + 1)
    errors: list[BaseException] = []

    def writer(i: int) -> None:
        try:
            barrier.wait()
            base = 1000 + i * per_writer
            for k in range(per_writer):
                storage.insert(db, coll, [{"_id": base + k, "x": (base + k) % 5}])
        except BaseException as exc:  # noqa: BLE001 - surfaced after join
            errors.append(exc)

    def builder() -> None:
        try:
            barrier.wait()
            storage.create_index(db, coll, "x_1", {"x": 1})
        except BaseException as exc:  # noqa: BLE001 - surfaced after join
            errors.append(exc)

    threads = [threading.Thread(target=builder)]
    threads += [threading.Thread(target=writer, args=(i,)) for i in range(writers)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=60)
        assert not t.is_alive(), (
            "thread did not finish within 60s — deadlock between the index build "
            "and a concurrent write (check the _coll_lock/_lock ordering)"
        )
    assert not errors, f"worker failures: {errors!r}"

    total = 20 + writers * per_writer
    assert storage.count_matching(db, coll, {}) == total
    # Every doc must be reachable THROUGH THE INDEX, not merely by a scan.
    plan = storage.explain_plan(db, coll, {"x": 3}, hint="x_1")
    assert plan.get("kind") == "IXSCAN", f"expected IXSCAN, got {plan}"
    via_index = {d["_id"] for d in storage.find_matching(db, coll, {"x": 3}, hint="x_1")}
    via_scan = {d["_id"] for d in storage.find_matching(db, coll, {}) if d["x"] == 3}
    assert via_index == via_scan, f"index lost {sorted(via_scan - via_index)}"


def test_ddl_and_crud_never_deadlock_and_index_stays_complete(storage: Storage) -> None:
    """Concurrent index DDL and writes must neither deadlock nor lose an entry.

    Both paths can hold ``_coll_lock`` AND ``_lock``. The canonical order is
    ``_coll_lock`` → ``_lock``; acquiring them the other way round is an AB-BA
    deadlock. Each writer runs on a FRESH thread so it takes ``_coll_lock`` and
    then hits ``_lock`` inside ``_session``'s first-use path — the exact order-B
    acquisition — while ``create_index`` runs concurrently on the same
    collection.

    Guards two regressions at once: a hang (inverted lock order) fails the join
    timeout loudly instead of wedging the suite, and a lost index entry (DDL
    interleaving with an in-flight write) fails the index-vs-scan comparison.
    """
    for r in range(4):
        _ddl_crud_round(storage, "app", f"c{r}", writers=4, per_writer=25)


def _drop_ddl_crud_round(
    storage: Storage, db: str, coll: str, writers: int, per_writer: int
) -> None:
    """One race round: N fresh writer threads vs a concurrent dropIndex on the
    same collection — the drop-direction twin of `_ddl_crud_round` (#635)."""
    storage.insert(db, coll, [{"_id": k, "x": k % 5} for k in range(20)])
    storage.create_index(db, coll, "x_1", {"x": 1})
    barrier = threading.Barrier(writers + 1)
    errors: list[BaseException] = []

    def writer(i: int) -> None:
        try:
            barrier.wait()
            base = 1000 + i * per_writer
            for k in range(per_writer):
                storage.insert(db, coll, [{"_id": base + k, "x": (base + k) % 5}])
        except BaseException as exc:  # noqa: BLE001 - surfaced after join
            errors.append(exc)

    def dropper() -> None:
        try:
            barrier.wait()
            storage.drop_index(db, coll, "x_1")
        except BaseException as exc:  # noqa: BLE001 - surfaced after join
            errors.append(exc)

    threads = [threading.Thread(target=dropper)]
    threads += [threading.Thread(target=writer, args=(i,)) for i in range(writers)]
    for t in threads:
        t.start()
    for t in threads:
        t.join(timeout=60)
        assert not t.is_alive(), (
            "thread did not finish within 60s — deadlock between dropIndex and a "
            "concurrent write (check the _coll_lock/_lock ordering)"
        )
    assert not errors, f"worker failures: {errors!r}"

    # Every doc is still reachable by a plain scan (the drop removed the index,
    # not data), and no index-entry rows survive for the dropped index.
    total = 20 + writers * per_writer
    assert storage.count_matching(db, coll, {}) == total
    from secantus.storage import _IDX_ENTRIES_TABLE

    survivors = storage._collect_prefix(_IDX_ENTRIES_TABLE, (db, coll, "x_1"))
    assert survivors == [], f"dropIndex left {len(survivors)} orphaned entry rows"


def test_drop_index_and_crud_never_deadlock_or_orphan_entries(storage: Storage) -> None:
    """Concurrent dropIndex + writes must neither deadlock nor leave the dropped
    index's entry rows behind. Guards the #635 drop-direction race: before
    `drop_index`/`drop_all_indexes` took `_coll_lock`, a write interleaving the
    entry-table snapshot-then-delete could survive as an orphaned entry."""
    for r in range(4):
        _drop_ddl_crud_round(storage, "app", f"d{r}", writers=4, per_writer=25)


def test_bare_emit_failure_does_not_freeze_the_visible_tail(storage: Storage) -> None:
    """#714: if a bare (autocommit) oplog emit's write loop raises, the minted
    seq range must still leave `_oplog_in_flight` — otherwise
    `oplog_visible_tail_seq` clamps at that seq forever and change streams
    server-wide freeze. A DDL write is the bare path; we force its emit to throw
    and assert the tail recovers."""
    storage.insert("app", "c", [{"_id": 1}])
    tail_before = storage.oplog_visible_tail_seq()
    assert not storage._oplog_in_flight

    # Force the next oplog cursor write to raise, mid bare-path emit.
    real_cursor = storage._cursor
    from secantus.storage import _OPLOG_TABLE

    class _Boom(Exception):
        pass

    def _boom_cursor(table, *a, **k):
        cur = real_cursor(table, *a, **k)
        if table == _OPLOG_TABLE:

            class _Wrap:
                def __setitem__(self, *_):
                    raise _Boom("injected oplog write failure")

                def __getattr__(self, n):
                    return getattr(cur, n)

            return _Wrap()
        return cur

    storage._cursor = _boom_cursor
    try:
        with pytest.raises(_Boom):
            # create_collection goes through the bare `_emit_oplog` path.
            storage.create_collection("app", "fresh")
    finally:
        storage._cursor = real_cursor

    # The minted range was released despite the failure — nothing pinned.
    assert not storage._oplog_in_flight, "a failed bare emit leaked an in-flight range"
    # A subsequent successful write advances the tail past where it froze.
    storage.insert("app", "c", [{"_id": 2}])
    assert storage.oplog_visible_tail_seq() > tail_before


def test_frame_doc_value_layout_and_roundtrip() -> None:
    """The doc-table value frame is ``[u32-LE id_key_len][id_key][blob]`` — byte
    for byte what the Rust server writes (RecordId step 4a), so a store written by
    one server reads on the other. Pins the exact bytes, not just the round-trip."""
    framed = _frame_doc_value(b"ID", b"blob")
    assert framed == b"\x02\x00\x00\x00ID blob".replace(b" ", b"")  # \x02000 + ID + blob
    assert framed == bytes([2, 0, 0, 0]) + b"ID" + b"blob"
    assert _unframe_doc_value(framed) == (b"ID", b"blob")
    # An id_key can itself contain NULs / the frame separator bytes — the length
    # prefix makes the split exact regardless.
    idk = b"\x00\x00\x01\xff"
    blob = b"\x00\x00payload\x00"
    assert _unframe_doc_value(_frame_doc_value(idk, blob)) == (idk, blob)
    # Empty blob is legal; empty id_key too.
    assert _unframe_doc_value(_frame_doc_value(b"k", b"")) == (b"k", b"")
    assert _unframe_doc_value(_frame_doc_value(b"", b"v")) == (b"", b"v")


def test_unframe_doc_value_rejects_malformed() -> None:
    """A value shorter than the 4-byte header, or whose declared id_key length
    overruns the buffer, is a corrupt frame — raise, never silently mis-split."""
    import pytest as _pytest

    with _pytest.raises(ValueError):
        _unframe_doc_value(b"\x01\x02")  # < 4 bytes
    with _pytest.raises(ValueError):
        _unframe_doc_value(bytes([9, 0, 0, 0]) + b"xy")  # declares 9, has 2


def test_doc_table_is_keyed_by_recordid_with_framed_values(storage: Storage) -> None:
    """The doc row is ``(db, coll, RecordId) -> [u32-LE len][id_key][blob]``, and
    the only ordering row written alongside it is the reverse ``_id`` index.

    Pins the 4->3 write-amp cut of RecordId step 4a: the forward
    ``secantus_natural`` row (seq -> id_key) is gone. Reads the raw WT tables so
    a regression in the on-disk layout — the thing that has to stay byte-identical
    to the Rust server — fails here rather than silently.
    """
    from secantus.storage import _NAT_SEQ_TABLE, _NAT_TABLE, _doc_table_for, _id_key

    storage.insert("db", "c", [{"_id": 7, "x": "hello"}])
    id_key = _id_key(7)

    rows = storage._collect_prefix(_doc_table_for("db", "c"), ("db", "c"))
    assert len(rows) == 1
    (key, value) = rows[0]
    recordid = key[2]
    assert isinstance(recordid, int) and recordid > 0
    assert _unframe_doc_value(bytes(value)) == (id_key, bson.encode({"_id": 7, "x": "hello"}))

    # The reverse _id index maps id_key -> that same RecordId...
    assert storage._doc_recordid("db", "c", id_key) == recordid
    assert [k for k, _ in storage._collect_prefix(_NAT_SEQ_TABLE, ("db", "c"))] == [
        ("db", "c", id_key)
    ]
    # ...and the forward natural table is not written at all any more.
    assert storage._collect_prefix(_NAT_TABLE, ("db", "c")) == []


def test_recordids_are_monotonic_across_reopen(tmp_path) -> None:
    """RecordIds recovered on reopen stay strictly greater than every doc-table
    key already on disk — ``_scan_max_nat_seq`` now scans the doc shards (the
    forward natural table it used to read is gone)."""
    from secantus.storage import _doc_table_for

    s = Storage(str(tmp_path))
    try:
        s.insert("db", "c", [{"_id": i} for i in range(5)])
        before = max(k[2] for k, _ in s._collect_prefix(_doc_table_for("db", "c"), ("db", "c")))
    finally:
        s.close()
    s2 = Storage(str(tmp_path))
    try:
        assert s2._scan_max_nat_seq() == before
        s2.insert("db", "c", [{"_id": 99}])
        after = [k[2] for k, _ in s2._collect_prefix(_doc_table_for("db", "c"), ("db", "c"))]
        assert max(after) > before
        assert len(set(after)) == len(after), "RecordIds must be unique"
        # Insertion order survives the reopen: the new doc scans last.
        assert [bson.decode(b)["_id"] for _r, _k, b in s2._scan_docs("db", "c")] == [
            0,
            1,
            2,
            3,
            4,
            99,
        ]
    finally:
        s2.close()


def test_open_refuses_a_pre_recordid_doc_format(tmp_path) -> None:
    """A store whose doc shards are keyed ``SSu`` was written before the RecordId
    change. There is no in-place migration (decision on record) — opening it must
    fail loudly rather than mis-read ``SSq`` cursor ops against an ``SSu`` btree.
    """
    import wiredtiger as wt

    from secantus.storage import IncompatibleStorageFormatError, _doc_shard_name

    home = str(tmp_path)
    conn = wt.wiredtiger_open(home, "create,log=(enabled=true)")
    try:
        sess = conn.open_session()
        # Pre-change format: (db, coll, id_key) -> raw blob.
        sess.create(_doc_shard_name(0), "key_format=SSu,value_format=u")
        sess.close()
    finally:
        conn.close()

    with pytest.raises(IncompatibleStorageFormatError) as exc:
        Storage(home)
    assert "SSu" in str(exc.value) and "SSq" in str(exc.value)
    # The refusal must not leave the WT home locked by a half-open connection.
    with pytest.raises(IncompatibleStorageFormatError):
        Storage(home)


def test_large_batch_insert_survives_a_small_cache(tmp_path) -> None:
    # One wire message can carry ~48MB of documents. Pre-chunking, insert()
    # wrote the whole batch in ONE statement transaction whose unevictable
    # dirty content (doc rows + full-doc oplog entries + index entries)
    # livelocked WiredTiger once it neared the cache's dirty-stall fraction —
    # the mongo-rust-driver ``large_insert`` weekly-CI wedge, reproduced
    # locally at 35k x 1.2KB docs vs the 1G default cache. Chunked (<=1000
    # docs / <=4MB per transaction), the same batch's dirty footprint stays
    # bounded and this passes quickly even against a deliberately tiny cache.
    # A regression wedges (pytest-timeout is the alarm).
    filler = "x" * 1100
    docs = [{"_id": i, "pad": filler} for i in range(35000)]
    s = Storage(str(tmp_path), cache_size="128M")
    try:
        inserted, errors = s.insert("app", "c", docs)
        assert inserted == 35000
        assert errors == []
        assert len(s.find_matching("app", "c", {"_id": 17321})) == 1
    finally:
        s.close()


def test_ordered_insert_stops_across_chunk_boundaries(tmp_path) -> None:
    # The ordered contract must hold across the chunked transactions: an
    # error in chunk N stops the batch — later chunks never run — and the
    # error's ``index`` is the position in the CLIENT batch, not the chunk.
    docs = [{"_id": i} for i in range(1500)]
    docs[1200]["_id"] = 3  # duplicate of an earlier doc, lands in chunk 2
    s = Storage(str(tmp_path))
    try:
        inserted, errors = s.insert("app", "c", docs, ordered=True)
        assert inserted == 1200
        assert len(errors) == 1
        assert errors[0]["index"] == 1200
        assert errors[0]["code"] == 11000
        # Nothing after the ordered stop landed.
        assert s.find_matching("app", "c", {"_id": 1499}) == []
        assert len(s.find_matching("app", "c", {"_id": 1199})) == 1
    finally:
        s.close()


def test_unordered_insert_reports_errors_across_chunks(tmp_path) -> None:
    docs = [{"_id": i} for i in range(1500)]
    docs[1200]["_id"] = 3
    s = Storage(str(tmp_path))
    try:
        inserted, errors = s.insert("app", "c", docs, ordered=False)
        assert inserted == 1499
        assert [e["index"] for e in errors] == [1200]
        assert len(s.find_matching("app", "c", {"_id": 1499})) == 1
    finally:
        s.close()


def test_update_and_delete_many_survive_a_small_cache(tmp_path) -> None:
    # updateMany / deleteMany over a large matched set used to run as ONE
    # statement transaction — unbounded unevictable dirty content, the same
    # livelock class the chunked inserts closed. Chunked (twin of the Rust
    # driver), a whole-collection rewrite and delete stay bounded against a
    # deliberately small cache. A regression wedges (pytest-timeout alarms).
    filler = "x" * 1100
    s = Storage(str(tmp_path), cache_size="128M")
    try:
        docs = [{"_id": i, "pad": filler, "x": 1} for i in range(35000)]
        inserted, errors = s.insert("app", "c", docs)
        assert inserted == 35000 and errors == []
        out = s.update_matching("app", "c", {"x": 1}, {"$set": {"x": 2}}, multi=True)
        assert out["matched"] == 35000
        assert out["modified"] == 35000
        assert len(s.find_matching("app", "c", {"x": 2})) == 35000
        deleted = s.delete_matching("app", "c", {"x": 2})
        assert deleted == 35000
        assert s.find_matching("app", "c", {}) == []
    finally:
        s.close()


def test_multi_update_inc_applies_exactly_once_across_chunks(tmp_path) -> None:
    # The RecordId list is partitioned across chunk transactions and a
    # conflict retries only its own rolled-back chunk — $inc must apply
    # exactly once per doc even when the update spans multiple chunks.
    s = Storage(str(tmp_path))
    try:
        s.insert("app", "c", [{"_id": i, "n": 0} for i in range(2500)])
        out = s.update_matching("app", "c", {}, {"$inc": {"n": 1}}, multi=True)
        assert out["matched"] == 2500
        assert out["modified"] == 2500
        assert len(s.find_matching("app", "c", {"n": 1})) == 2500
        out = s.update_matching("app", "c", {"n": 1}, {"$inc": {"n": 1}}, multi=True)
        assert out["modified"] == 2500
        assert len(s.find_matching("app", "c", {"n": 2})) == 2500
    finally:
        s.close()


def test_bounded_write_paths_unchanged_by_chunking(tmp_path) -> None:
    s = Storage(str(tmp_path))
    try:
        s.insert("app", "c", [{"_id": i, "x": 1} for i in range(10)])
        # Single-doc update keeps post-image capture.
        out = s.update_matching(
            "app", "c", {"x": 1}, {"$set": {"x": 9}}, multi=False, return_post_images=True
        )
        assert (out["matched"], out["modified"]) == (1, 1)
        assert out["post_images"][0]["x"] == 9
        # deleteOne stays bounded-path.
        assert s.delete_matching("app", "c", {"x": 1}, limit=1) == 1
        assert len(s.find_matching("app", "c", {})) == 9
        # Upsert through the chunked route's zero-match delegation.
        out = s.update_matching("app", "c", {"x": 777}, {"$set": {"y": 1}}, multi=True, upsert=True)
        assert out["matched"] == 0
        assert out["did_upsert"] is True
        assert out["upserted_id"] is not None
    finally:
        s.close()


def test_pending_drop_tombstone_recovered_at_open(tmp_path) -> None:
    """A drop tombstone left by the Rust server's chunked drop crashing
    mid-purge (registry row gone, doc/index rows orphaned) is finished at the
    next open — the orphans must not resurface inside a re-created collection.
    The layouts are byte-identical cross-server, so the Python server must
    honour a Rust-written tombstone."""
    from secantus.storage import _COLL_TABLE, _TOMB_TABLE, _doc_table_for

    s1 = Storage(str(tmp_path))
    try:
        s1.insert("app", "c", [{"_id": i, "x": i} for i in range(50)])
        s1.create_index("app", "c", "x_1", {"x": 1}, {})
        with s1._lock:
            # Forge the crash-left state: phase 1's effects (registry row
            # removed, tombstone written) without the phase-2 purge.
            s1._delete_keys(_COLL_TABLE, [("app", "c")])
            c = s1._cursor(_TOMB_TABLE)
            c.set_key("app", "c")
            c.set_value(b"")
            c.insert()
            c.reset()
    finally:
        s1.close()

    s2 = Storage(str(tmp_path))
    try:
        # Recovery purged the orphans and cleared the tombstone; a re-created
        # collection sees only its own rows.
        assert s2._collect_prefix(_TOMB_TABLE, ()) == []
        assert s2._collect_prefix(_doc_table_for("app", "c"), ("app", "c")) == []
        s2.insert("app", "c", [{"_id": 100}])
        assert s2.count_matching("app", "c", {}) == 1
    finally:
        s2.close()
