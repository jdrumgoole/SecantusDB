"""RM-219: `applyOps`, measured against mongod 8.2.11, and the oplog read in
`$natural: -1` order that a tool replaying the oplog relies on.
"""

from __future__ import annotations

import datetime
import re
from typing import Any

import pytest
from bson import Binary, Timestamp

from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture


def _diff(d: dict, i: int) -> dict:
    return {"op": "u", "ns": "rm219a.c", "o": {"$v": 2, "diff": d}, "o2": {"_id": i}}


def _ins(i: Any, ns: str = "rm219a.c") -> dict:
    return {"op": "i", "ns": ns, "o": {"_id": i}}


CASES: list[tuple[str, Any, dict]] = [
    ("insert", [_ins(2)], {}),
    ("insert of an _id that exists", [{"op": "i", "ns": "rm219a.c", "o": {"_id": 1, "y": 5}}], {}),
    ("insert without _id", [{"op": "i", "ns": "rm219a.c", "o": {"x": 7}}], {}),
    ("diff u", [_diff({"u": {"x": 10}}, 1)], {}),
    ("diff i", [_diff({"i": {"z": 3}}, 1)], {}),
    ("diff d", [_diff({"d": {"x": False}}, 1)], {}),
    ("diff sub-document", [_diff({"sa": {"u": {"b": 2}}}, 1)], {}),
    ("diff array", [_diff({"sarr": {"a": True, "l": 2, "u0": 9}}, 1)], {}),
    ("diff array, l after an index", [_diff({"sarr": {"a": True, "u0": 9, "l": 2}}, 1)], {}),
    ("diff array, index past the length", [_diff({"sarr": {"a": True, "l": 2, "u4": 7}}, 1)], {}),
    ("diff array, padded", [_diff({"sarr": {"a": True, "u5": 7}}, 1)], {}),
    ("diff array, bad entry", [_diff({"sarr": {"a": True, "x0": 1}}, 1)], {}),
    ("diff bad section", [_diff({"zz": {"x": 1}}, 1)], {}),
    (
        "replacement",
        [{"op": "u", "ns": "rm219a.c", "o": {"_id": 1, "x": 11}, "o2": {"_id": 1}}],
        {},
    ),
    (
        "replacement changing _id",
        [{"op": "u", "ns": "rm219a.c", "o": {"_id": 2}, "o2": {"_id": 1}}],
        {},
    ),
    ("update of a missing document", [_diff({"u": {"x": 10}}, 99)], {}),
    ("upsert", [dict(_diff({"u": {"x": 10}}, 98), b=True)], {}),
    ("update without o2", [{"op": "u", "ns": "rm219a.c", "o": {"$v": 2, "diff": {}}}], {}),
    (
        "update with $set",
        [{"op": "u", "ns": "rm219a.c", "o": {"$set": {"x": 1}}, "o2": {"_id": 1}}],
        {},
    ),
    ("delete", [{"op": "d", "ns": "rm219a.c", "o": {"_id": 1}}], {}),
    ("delete of a missing document", [{"op": "d", "ns": "rm219a.c", "o": {"_id": 5}}], {}),
    ("delete without _id", [{"op": "d", "ns": "rm219a.c", "o": {"x": 1}}], {}),
    ("noop", [{"op": "n", "ns": "", "o": {"msg": "x"}}], {}),
    ("one fails, the rest go on", [_ins(3), _diff({"u": {"x": 1}}, 99), _ins(4)], {}),
    ("one raises, the rest do not run", [_ins(3), _ins(1, "rm219a.nope"), _ins(4)], {}),
    ("create", [{"op": "c", "ns": "rm219a.$cmd", "o": {"create": "d"}}], {}),
    ("create of one that exists", [{"op": "c", "ns": "rm219a.$cmd", "o": {"create": "c"}}], {}),
    ("drop of one that is missing", [{"op": "c", "ns": "rm219a.$cmd", "o": {"drop": "zz"}}], {}),
    (
        "createIndexes",
        [
            {
                "op": "c",
                "ns": "rm219a.$cmd",
                "o": {"createIndexes": "c", "v": 2, "key": {"q": 1}, "name": "q_1"},
            }
        ],
        {},
    ),
    (
        "dropIndexes",
        [{"op": "c", "ns": "rm219a.$cmd", "o": {"dropIndexes": "c", "index": "x_1"}}],
        {},
    ),
    (
        "rename",
        [
            {
                "op": "c",
                "ns": "rm219a.$cmd",
                "o": {"renameCollection": "rm219a.c", "to": "rm219a.c2"},
            }
        ],
        {},
    ),
    (
        "rename onto one that exists",
        [
            {
                "op": "c",
                "ns": "rm219a.$cmd",
                "o": {"renameCollection": "rm219a.c", "to": "rm219a.cap"},
            }
        ],
        {},
    ),
    ("commitTransaction", [{"op": "c", "ns": "admin.$cmd", "o": {"commitTransaction": 1}}], {}),
    ("nested applyOps", [{"op": "c", "ns": "admin.$cmd", "o": {"applyOps": [_ins(40)]}}], {}),
    ("unknown command", [_ins(5), {"op": "c", "ns": "rm219a.$cmd", "o": {"bogus": 1}}], {}),
    ("missing collection", [_ins(1, "rm219a.newc")], {}),
    ("view", [_ins(1, "rm219a.v")], {}),
    ("validator", [{"op": "i", "ns": "rm219a.val", "o": {"_id": 1, "x": -1}}], {}),
    (
        "validator bypassed",
        [{"op": "i", "ns": "rm219a.val", "o": {"_id": 1, "x": -1}}],
        {"bypassDocumentValidation": True},
    ),
    ("ui of another collection", [dict(_ins(31), ui=Binary(b"0" * 16, 4))], {}),
    ("unknown field", [dict(_ins(32), zz=1)], {}),
    ("bad op", [_ins(5), {"op": "z", "ns": "rm219a.c", "o": {}}], {}),
    ("op not a string", [{"op": 1, "ns": "rm219a.c", "o": {"_id": 33}}], {}),
    ("ns not a string", [_ins(5), {"op": "i", "ns": 5, "o": {}}], {}),
    ("ns not a namespace", [{"op": "i", "ns": "c", "o": {"_id": 33}}], {}),
    ("o not an object", [{"op": "i", "ns": "rm219a.c", "o": 5}], {}),
    ("o missing", [{"op": "i", "ns": "rm219a.c"}], {}),
    (
        "ts, t, v and wall",
        [dict(_ins(30), ts=Timestamp(1, 1), t=1, v=2, wall=datetime.datetime(2020, 1, 1))],
        {},
    ),
    ("entry not an object", [5], {}),
    ("not an array", 5, {}),
    ("empty", [], {}),
    ("bad application mode", [_ins(37)], {"oplogApplicationMode": "x"}),
]

TMP = re.compile(r"tmp\w{5}\.rename")


def _apply_ops(client: Any) -> list:
    admin, db = client.admin, client.rm219a
    out = []
    for _label, ops, options in CASES:
        client.drop_database("rm219a")
        db.c.insert_one({"_id": 1, "x": 1, "a": {"b": 1}, "arr": [1, 2, 3]})
        db.c.create_index("x")
        db.command("create", "v", viewOn="c", pipeline=[])
        db.command("create", "cap", capped=True, size=4096)
        db.command("create", "val", validator={"x": {"$gt": 0}})
        reply = admin.command("applyOps", ops, check=False, **options)
        reply = {
            k: v
            for k, v in reply.items()
            if k not in ("$clusterTime", "operationTime", "electionId", "opTime")
        }
        docs = sorted(repr(d) for d in db.c.find())
        colls = sorted(TMP.sub("tmp.rename", n) for n in db.list_collection_names())
        out.append((reply, docs, colls))
    client.drop_database("rm219a")
    return out


@pytest.mark.backlog_fixed("RM-219")
def test_apply_ops_as_mongod(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """Each entry applied, refused or failed as mongod 8.2.11 does, with its
    reply (`applied`, `results`, the error that stopped the run) and what
    the collection holds after."""
    got, want = _apply_ops(rust_mongo), _apply_ops(mongod_rs)
    for (label, _, _), g, w in zip(CASES, got, want, strict=True):
        assert g == w, label


def _newest_entry_is_the_last_write(client: Any) -> bool:
    # mongod writes entries of its own (periodic no-ops, session upkeep), so
    # the read is narrowed to this test's namespace.
    client.rm219a.n.insert_one({"_id": "older"})
    client.rm219a.n.insert_one({"_id": "newest"})
    newest = client.local["oplog.rs"].find_one({"ns": "rm219a.n"}, sort=[("$natural", -1)])
    client.drop_database("rm219a")
    return newest["o"] == {"_id": "newest"}


@pytest.mark.backlog_fixed("RM-219")
def test_oplog_natural_descending_reads_the_newest_entry(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """`find().sort({$natural: -1})` on `local.oplog.rs` answers the newest
    entry on mongod 8.2.11 (what `mongodump --oplog` asks for); it answered
    the oldest here."""
    assert _newest_entry_is_the_last_write(mongod_rs)
    assert _newest_entry_is_the_last_write(rust_mongo)


# ------------------------------------------------- RM-224 oplog entry shapes --


def _oplog_entries(client: Any) -> list:
    """The `local.oplog.rs` entries a few ordinary writes leave, without the
    fields that differ run to run."""
    db = client.rm224
    client.drop_database("rm224")
    db.c.insert_one({"_id": 1, "x": 1, "a": {"b": 1}})
    start = client.local["oplog.rs"].find_one(sort=[("$natural", -1)])["ts"]
    db.c.update_one({"_id": 1}, {"$set": {"x": 2, "a.b": 2, "y": 1}})
    db.c.delete_one({"_id": 1})
    db.c.drop()
    entries = [
        {k: e[k] for k in ("op", "ns", "o", "o2") if k in e}
        for e in client.local["oplog.rs"].find({"ts": {"$gt": start}, "op": {"$ne": "n"}})
    ]
    client.drop_database("rm224")
    return entries


@pytest.mark.backlog("RM-224")
def test_oplog_entries_have_mongods_shape(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 logs an update as its `$v: 2` diff (`u` / `i` / `s<field>`
    sections), a delete with no `o2`, and a drop with `o2: {numRecords}`."""
    assert _oplog_entries(rust_mongo) == _oplog_entries(mongod_rs)


def _ddl_entries(client: Any) -> list:
    """The entries single-document writes and DDL leave (an index built on
    an empty collection, which mongod logs as one `createIndexes`)."""
    db = client.rm224
    client.drop_database("rm224")
    start = client.local["oplog.rs"].find_one(sort=[("$natural", -1)])["ts"]
    db.create_collection("c")
    db.c.create_index([("y", -1)], name="yy", unique=True, sparse=True, hidden=True)
    db.c.insert_one({"_id": 1})
    db.c.insert_one({"_id": 2})
    db.c.delete_one({"_id": 1})
    db.c.drop_index("yy")
    client.admin.command("renameCollection", "rm224.c", to="rm224.d")
    db.d.drop()
    entries = [
        {k: e[k] for k in ("op", "ns", "o", "o2") if k in e}
        for e in client.local["oplog.rs"].find({"ts": {"$gt": start}, "op": {"$ne": "n"}})
    ]
    client.drop_database("rm224")
    return entries


@pytest.mark.backlog_fixed("RM-224")
def test_oplog_ddl_and_delete_entries_have_mongods_shape(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11: a delete has no `o2`; a one-index `createIndexes` is
    the spec (`v`, `unique`, `hidden` first); `dropIndexes` keeps the spec in
    `o2`; a rename says `stayTemp: false`; a drop has `o2: {numRecords}`."""
    assert _ddl_entries(rust_mongo) == _ddl_entries(mongod_rs)
