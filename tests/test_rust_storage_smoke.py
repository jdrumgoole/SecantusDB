"""Smoke test for the Rust storage extension (`_secantus_storage`, Phase 4).

Skipped unless the WiredTiger-linking extension is importable — it isn't part of
the default install/CI yet (shipping it across the wheel matrix is Phase 4's
go/no-go gate). Build + run it with ``invoke rust-storage-py``.
"""

from __future__ import annotations

import bson
import pytest
from bson import ObjectId

ss = pytest.importorskip(
    "_secantus_storage",
    reason="Rust storage extension not built (see `invoke rust-storage-py`)",
)


def _wrap_id(v):
    return bson.encode({"v": v})


def test_rust_storage_crud_roundtrip(tmp_path):
    st = ss.RustStorage(str(tmp_path))

    # insert: explicit ids of several BSON types + an auto-assigned ObjectId
    st.insert_one("app", "c", bson.encode({"v": "noid"}))
    st.insert_one("app", "c", bson.encode({"_id": 1, "v": "one"}))
    st.insert_one("app", "c", bson.encode({"_id": "x", "v": "ex"}))
    oid = ObjectId()
    st.insert_one("app", "c", bson.encode({"_id": oid, "v": "obj"}))

    # duplicate _id is rejected
    with pytest.raises(KeyError):
        st.insert_one("app", "c", bson.encode({"_id": 1, "v": "dup"}))

    # find by _id (across types) + miss
    assert bson.decode(st.find_by_id("app", "c", _wrap_id(1)))["v"] == "one"
    assert bson.decode(st.find_by_id("app", "c", _wrap_id(oid)))["v"] == "obj"
    assert st.find_by_id("app", "c", _wrap_id(999)) is None

    # scan is in cross-type natural order: number < string < ObjectId
    ids = [bson.decode(b)["_id"] for b in st.scan_collection("app", "c")]
    assert ids[0] == 1
    assert ids[1] == "x"
    assert all(isinstance(i, ObjectId) for i in ids[2:])

    # replace (preserves _id) + delete
    assert st.replace_by_id("app", "c", _wrap_id(1), bson.encode({"v": "REPLACED"}))
    replaced = bson.decode(st.find_by_id("app", "c", _wrap_id(1)))
    assert replaced["v"] == "REPLACED" and replaced["_id"] == 1
    assert st.delete_by_id("app", "c", _wrap_id(1)) is True
    assert st.delete_by_id("app", "c", _wrap_id(1)) is False

    # registry
    assert st.collection_exists("app", "c") is True
    assert st.list_collections("app") == ["c"]

    del st  # close before tmp_path cleanup


def test_dbs_and_collections_isolated(tmp_path):
    st = ss.RustStorage(str(tmp_path))
    st.insert_one("db1", "a", bson.encode({"_id": 1}))
    st.insert_one("db1", "b", bson.encode({"_id": 1}))
    st.insert_one("db2", "a", bson.encode({"_id": 1}))

    assert sorted(st.list_collections("db1")) == ["a", "b"]
    assert st.list_collections("db2") == ["a"]
    # same _id in different namespaces does not collide
    assert len(st.scan_collection("db1", "a")) == 1
    assert len(st.scan_collection("db2", "a")) == 1
    del st


def test_query_write_index_surface(tmp_path):
    """The expanded PyO3 surface: query / update / delete / count + indexes."""
    st = ss.RustStorage(str(tmp_path))
    for i in range(1, 6):
        st.insert_one("app", "c", bson.encode({"_id": i, "n": i, "grp": i % 2}))

    # find_matching: filter routed through matches()
    got = [bson.decode(b)["_id"] for b in st.find_matching("app", "c", bson.encode({"grp": 1}))]
    assert sorted(got) == [1, 3, 5]

    # count_matching
    assert st.count_matching("app", "c", bson.encode({})) == 5
    assert st.count_matching("app", "c", bson.encode({"n": {"$gt": 3}})) == 2

    # create an index, then find_matching_with sort + explain reports IXSCAN
    assert st.create_index("app", "c", "n_1", bson.encode({"n": 1}), bson.encode({})) is True
    ordered = [
        bson.decode(b)["n"]
        for b in st.find_matching_with("app", "c", bson.encode({}), bson.encode({"n": -1}))
    ]
    assert ordered == [5, 4, 3, 2, 1]
    plan = bson.decode(st.explain_plan("app", "c", bson.encode({"n": 3})))
    assert plan["kind"] == "IXSCAN" and plan["index_name"] == "n_1"

    # update_matching: operator update, multi
    out = bson.decode(
        st.update_matching(
            "app", "c", bson.encode({"grp": 0}), bson.encode({"$set": {"hit": True}}), True, False
        )
    )
    assert out["matched"] == 2 and out["modified"] == 2 and "upserted_id" not in out

    # upsert path surfaces upserted_id
    out = bson.decode(
        st.update_matching(
            "app", "c", bson.encode({"k": "new"}), bson.encode({"$set": {"v": 1}}), False, True
        )
    )
    assert "upserted_id" in out

    # delete_matching with a limit
    assert st.delete_matching("app", "c", bson.encode({"grp": 1}), 1) == 1

    # index registry + sizes
    names = {bson.decode(b)["name"] for b in st.list_indexes("app", "c")}
    assert {"_id_", "n_1"} <= names
    sizes = bson.decode(st.index_sizes("app", "c"))
    assert sizes["n_1"] > 0
    del st


def test_lifecycle_and_oplog_surface(tmp_path):
    """The expanded PyO3 surface: lifecycle, options, oplog reads."""
    st = ss.RustStorage(str(tmp_path))

    # create_collection emits an op:"c" create entry
    floor = st.oplog_tail_seq()
    assert st.create_collection("app", "c") is True
    st.insert_one("app", "c", bson.encode({"_id": 1, "x": 1}))
    cmds = [bson.decode(b) for _seq, b in st.read_oplog(floor + 1, 100)]
    assert any(e["op"] == "c" and e["o"].get("create") == "c" for e in cmds)
    assert any(e["op"] == "i" for e in cmds)

    # options round-trip; uuid present as raw bytes
    opts = bson.decode(st.get_collection_options("app", "c"))
    assert isinstance(opts.get("uuid"), bytes) and len(opts["uuid"]) == 16
    assert st.collection_is_capped("app", "c") is False

    # rename moves the data
    ok, msg = st.rename_collection("app", "c", "app", "d", False)
    assert ok is True and msg is None
    assert st.collection_exists("app", "c") is False
    assert len(st.scan_collection("app", "d")) == 1

    # list_databases includes synthetic local (oplog on by default)
    assert "local" in st.list_databases()

    # cluster time advances
    t1 = st.current_cluster_time()
    t2 = st.current_cluster_time()
    assert t2 > t1
    del st


def test_engine_fallback_exception_exported():
    """The EngineFallback exception is exported for the engine-selection adapter."""
    assert issubclass(ss.EngineFallback, Exception)


def test_wait_for_oplog_surface(tmp_path):
    """The tailable-wait primitive: timeout, already-advanced, and threaded wake."""
    import threading
    import time

    st = ss.RustStorage(str(tmp_path))
    tail = st.oplog_tail_seq()

    # Idle: returns ~after the timeout with the tail unchanged.
    t0 = time.monotonic()
    assert st.wait_for_oplog(tail, 200) == tail
    assert time.monotonic() - t0 >= 0.15

    # A write advances the tail; a wait against the old tail returns at once.
    st.insert_one("app", "c", bson.encode({"_id": 1}))
    assert st.wait_for_oplog(tail, 5000) > tail

    # Threaded wake: a blocked waiter is released by a concurrent insert. This
    # also proves the GIL is released while blocking (the insert thread runs).
    captured = st.oplog_tail_seq()
    result = {}

    def wait(st=st, captured=captured, result=result):
        result["tail"] = st.wait_for_oplog(captured, 10_000)

    w = threading.Thread(target=wait)
    w.start()
    time.sleep(0.15)
    st.insert_one("app", "c", bson.encode({"_id": 2}))
    w.join(timeout=5)
    assert not w.is_alive()
    assert result["tail"] > captured
    del st


def test_users_roles_profile_surface(tmp_path):
    """The auth + profiling surface: user/role record CRUD + per-db profile."""
    st = ss.RustStorage(str(tmp_path))

    # user records are opaque BSON blobs, stored verbatim
    assert st.add_user("admin", "alice", bson.encode({"user": "alice", "roles": ["read"]}), False)
    assert not st.add_user("admin", "alice", bson.encode({"user": "x"}), False)  # dup, no replace
    assert bson.decode(st.get_user("admin", "alice"))["user"] == "alice"
    assert st.add_user("app", "bob", bson.encode({"user": "bob"}), False)
    assert len(st.list_users()) == 2
    assert len(st.list_users("admin")) == 1
    assert st.drop_user("admin", "alice") and not st.drop_user("admin", "alice")

    # roles live in a separate table
    assert st.add_role("admin", "auditor", bson.encode({"role": "auditor"}), False)
    assert bson.decode(st.get_role("admin", "auditor"))["role"] == "auditor"
    assert len(st.list_roles()) == 1

    # per-db profile settings: defaults, round-trip, validation
    p = bson.decode(st.get_profile("app"))
    assert p == {"level": 0, "slowms": 100, "sampleRate": 1.0}
    st.set_profile("app", 1, 0, 0.0)
    assert bson.decode(st.get_profile("app")) == {"level": 1, "slowms": 0, "sampleRate": 0.0}
    with pytest.raises(ValueError):
        st.set_profile("app", 3, 100, 1.0)

    # system.profile is created capped
    st.ensure_profile_collection("app", 4096)
    assert st.collection_is_capped("app", "system.profile")
    del st


def test_batch_insert_surface(tmp_path):
    """Batch insert: ordered/unordered, write-errors, auto-_id."""
    st = ss.RustStorage(str(tmp_path))
    # all succeed; missing _id auto-assigned
    inserted, errors = st.insert("app", "c", [bson.encode({"x": 1}), bson.encode({"x": 2})])
    assert inserted == 2 and errors == []
    # unordered continues past a duplicate _id, collecting one write-error
    st.insert_one("app", "c", bson.encode({"_id": 7}))
    inserted, errors = st.insert(
        "app",
        "c",
        [bson.encode({"_id": 7}), bson.encode({"_id": 8})],
        ordered=False,
    )
    assert inserted == 1
    assert len(errors) == 1
    err = bson.decode(errors[0])
    assert err["index"] == 0 and err["code"] == 11000
    del st
