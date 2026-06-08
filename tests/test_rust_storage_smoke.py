"""Smoke test for the Rust storage extension (`_secantus_storage`, Phase 4).

Skipped unless the WiredTiger-linking extension is importable — it isn't part of
the default install/CI yet (shipping it across the wheel matrix is Phase 4's
go/no-go gate). Build + run it with ``invoke rust-storage-py``.
"""

from __future__ import annotations

import tempfile

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
