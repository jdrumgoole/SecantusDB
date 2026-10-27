"""RM-219: `applyOps`, measured against mongod 8.2.11, and the oplog read in
`$natural: -1` order that a tool replaying the oplog relies on.
"""

from __future__ import annotations

import contextlib
import datetime
import re
from typing import Any

import pymongo
import pytest
from bson import Binary, Timestamp
from pymongo.errors import PyMongoError

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


UPDATES = [
    {"$set": {"x": 2}},
    {"$set": {"new": 1}},
    {"$set": {"a.b": 2}},
    {"$set": {"a.c.d": 5}},
    {"$set": {"q.r.s": 1}},
    {"$unset": {"a.b": ""}},
    {"$rename": {"x": "y"}},
    {"$push": {"arr": {"$each": [4, 5]}}},
    {"$push": {"arr": {"$each": [0], "$position": 0}}},
    {"$pop": {"arr": 1}},
    {"$pull": {"arr": 2}},
    {"$set": {"arr.5": 50}},
    {"$set": {"objs.0.k": 10}},
    {"$set": {"objs.$[].k": 0}},
    {"$set": {"x": 2, "a.b": 3, "arr.0": 7, "new": 1}, "$unset": {"s": ""}},
    {"$set": {"arr": [1, 2, 3, 4]}},
]


def _update_entries(client: Any) -> list:
    """The `o` each update leaves in `local.oplog.rs`."""
    out = []
    base = {
        "_id": 1,
        "x": 1,
        "s": "a",
        "a": {"b": 1, "c": {"d": 1}},
        "arr": [1, 2, 3],
        "objs": [{"k": 1}, {"k": 2}],
    }
    for update in UPDATES:
        client.drop_database("rm224")
        client.rm224.c.insert_one(base)
        start = client.local["oplog.rs"].find_one(sort=[("$natural", -1)])["ts"]
        client.rm224.c.update_one({"_id": 1}, update)
        entry = client.local["oplog.rs"].find_one(
            {"ts": {"$gt": start}, "ns": "rm224.c", "op": "u"}
        )
        out.append(entry["o"] if entry else None)
    client.drop_database("rm224")
    return out


@pytest.mark.backlog_fixed("RM-224")
def test_oplog_update_entries_are_mongods_diff(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 logs an update as its `$v: 2` diff: `u` / `i` / `d` by
    whether the field existed, `s<field>` into documents and arrays, a whole
    array where `$pop`, `$pull`, `$position` or a `$[]` that changed several
    elements rewrote it."""
    assert _update_entries(rust_mongo) == _update_entries(mongod_rs)


def _insert_groups(client: Any) -> list:
    """How batches of inserts are logged, outside a retryable write: one
    `applyOps` per group, or plain inserts."""
    host, port = client.address
    plain = pymongo.MongoClient(host, port, directConnection=True, retryWrites=False)
    db = plain.rm224
    out = []
    for docs, ordered in [
        ([{"_id": 1}], True),
        ([{"_id": i} for i in range(3)], True),
        ([{"_id": i} for i in range(501)], True),
        ([{"_id": i, "s": "x" * 100_000} for i in range(6)], True),
        ([{"_id": i, "s": "x" * 300_000} for i in range(3)], True),
        ([{"_id": 1}, {"_id": "pre"}, {"_id": 2}, {"_id": 3}], False),
        ([{"_id": 1}, {"_id": "pre"}, {"_id": 2}], True),
    ]:
        plain.drop_database("rm224")
        db.c.insert_one({"_id": "pre"})
        start = plain.local["oplog.rs"].find_one(sort=[("$natural", -1)])["ts"]
        # A batch with a duplicate fails; what it logged is the point.
        with contextlib.suppress(PyMongoError):
            db.c.insert_many(docs, ordered=ordered)
        out.append(
            [
                (e["op"], len(e["o"]["applyOps"]) if e["op"] == "c" else 1)
                for e in plain.local["oplog.rs"].find({"ts": {"$gt": start}, "op": {"$ne": "n"}})
                if e["ns"] in ("rm224.c", "admin.$cmd")
            ]
        )
    plain.drop_database("rm224")
    plain.close()
    return out


@pytest.mark.backlog_fixed("RM-224")
def test_oplog_insert_batches_are_mongods_apply_ops(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 logs a batch of inserts as `applyOps` entries of up to
    500 documents or 256 KiB, one document as a plain insert, and a batch
    with a failed document as single inserts."""
    assert _insert_groups(rust_mongo) == _insert_groups(mongod_rs)


def _retryable_writes(client: Any) -> list:
    """The field names, statement ids and `prevOpTime` terms of a few
    retryable writes' entries."""
    db = client.rm224
    client.drop_database("rm224")
    db.c.insert_many([{"_id": i} for i in range(3)])
    out = []
    for write in [
        lambda: db.c.insert_one({"_id": 10}),
        lambda: db.c.insert_many([{"_id": 11}, {"_id": 12}]),
        lambda: db.c.update_one({"_id": 1}, {"$set": {"y": 1}}),
        lambda: db.c.update_one({"_id": 77}, {"$set": {"y": 1}}, upsert=True),
        lambda: db.c.delete_one({"_id": 2}),
        lambda: db.c.update_many({}, {"$set": {"z": 1}}),
    ]:
        start = client.local["oplog.rs"].find_one(sort=[("$natural", -1)])["ts"]
        write()
        out.append(
            [
                (
                    list(e.keys()),
                    e.get("stmtId"),
                    e.get("prevOpTime", {}).get("t"),
                    [o.get("stmtId") for o in e["o"].get("applyOps", [])],
                )
                for e in client.local["oplog.rs"].find({"ts": {"$gt": start}, "op": {"$ne": "n"}})
                if e["ns"] in ("rm224.c", "admin.$cmd")
            ]
        )
    client.drop_database("rm224")
    return out


@pytest.mark.backlog_fixed("RM-224")
def test_oplog_retryable_writes_carry_their_session(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 stamps a retryable insert, update or delete with its
    `lsid`, `txnNumber`, `stmtId` and `prevOpTime` (an insert group with
    `multiOpType: 1` and a `stmtId` per insert); a multi-document update
    carries none."""
    assert _retryable_writes(rust_mongo) == _retryable_writes(mongod_rs)


def _index_build_entries(client: Any) -> list:
    db = client.rm224
    client.drop_database("rm224")
    db.c.insert_one({"_id": 1})
    start = client.local["oplog.rs"].find_one(sort=[("$natural", -1)])["ts"]
    db.c.create_index("x")
    entries = [
        (e["op"], e["ns"], next(iter(e["o"])))
        for e in client.local["oplog.rs"].find({"ts": {"$gt": start}, "op": {"$ne": "n"}})
        if e["ns"].startswith(("rm224.", "config.system.indexBuilds"))
    ]
    client.drop_database("rm224")
    return entries


@pytest.mark.backlog_fixed("RM-224")
def test_oplog_entries_have_mongods_shape(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 logs an index build over documents in two phases:
    `config.system.indexBuilds` rows around `startIndexBuild` and
    `commitIndexBuild` (the hybrid build, RM-187, since 2026-10-26)."""
    assert _index_build_entries(rust_mongo) == _index_build_entries(mongod_rs)


def _bulk_write_entries(client: Any) -> list:
    """The field names and statement ids of a retryable `bulkWrite`'s
    entries."""
    from pymongo import InsertOne, UpdateOne

    client.drop_database("rm224")
    client.rm224.c.insert_many([{"_id": i} for i in range(3)])
    start = client.local["oplog.rs"].find_one(sort=[("$natural", -1)])["ts"]
    client.bulk_write(
        [
            InsertOne({"_id": 10}, namespace="rm224.c"),
            UpdateOne({"_id": 1}, {"$set": {"y": 1}}, namespace="rm224.c"),
        ]
    )
    entries = [
        (sorted(e.keys()), e.get("stmtId"), [o.get("stmtId") for o in e["o"].get("applyOps", [])])
        for e in client.local["oplog.rs"].find({"ts": {"$gt": start}, "op": {"$ne": "n"}})
        if e["ns"] in ("rm224.c", "admin.$cmd")
    ]
    client.drop_database("rm224")
    return entries


@pytest.mark.backlog("RM-224")
def test_bulk_write_entries_carry_their_session(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 stamps a retryable `bulkWrite`'s entries with its
    session (`lsid`, `txnNumber`, `stmtId`, `prevOpTime`), as it does a
    retryable insert's or update's; this server writes them bare."""
    assert _bulk_write_entries(rust_mongo) == _bulk_write_entries(mongod_rs)


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
        # mongod writes entries of its own (config collections it creates
        # lazily), so only this test's namespace is read.
        for e in client.local["oplog.rs"].find(
            {"ts": {"$gt": start}, "ns": {"$regex": "^rm224\\."}}
        )
    ]
    client.drop_database("rm224")
    return entries


@pytest.mark.backlog_fixed("RM-224")
def test_oplog_ddl_and_delete_entries_have_mongods_shape(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11: a delete has no `o2`; a one-index `createIndexes` is
    the spec (`v`, `unique`, `hidden` first); `dropIndexes` keeps the spec in
    `o2`; a rename says `stayTemp: false`; a drop has `o2: {numRecords}`."""
    assert _ddl_entries(rust_mongo) == _ddl_entries(mongod_rs)


def _transaction_entries(client: Any) -> list:
    db = client.rm224
    client.drop_database("rm224")
    db.create_collection("c")
    start = client.local["oplog.rs"].find_one(sort=[("$natural", -1)])["ts"]
    with client.start_session() as s, s.start_transaction():
        db.c.insert_one({"_id": 1}, session=s)
        db.c.update_one({"_id": 1}, {"$set": {"x": 1}}, session=s)
        db.c.delete_one({"_id": 1}, session=s)
    entries = [
        (
            e["op"],
            e["ns"],
            sorted(e.keys()),
            [(o["op"], o["o"]) for o in e["o"].get("applyOps", [])],
        )
        for e in client.local["oplog.rs"].find({"ts": {"$gt": start}, "op": {"$ne": "n"}})
        if e["ns"] in ("rm224.c", "admin.$cmd")
    ]
    client.drop_database("rm224")
    return entries


@pytest.mark.backlog_fixed("RM-224")
def test_oplog_transaction_is_one_apply_ops(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 logs a committed transaction as one `applyOps` entry
    under its `lsid` / `txnNumber`, with `prevOpTime`."""
    assert _transaction_entries(rust_mongo) == _transaction_entries(mongod_rs)


def _delete_batches(client: Any) -> list:
    out = []
    for n, query in [(1, {}), (2, {}), (11, {}), (25, {"_id": {"$gte": 5}})]:
        client.drop_database("rm224")
        client.rm224.c.insert_many([{"_id": i} for i in range(n)])
        start = client.local["oplog.rs"].find_one(sort=[("$natural", -1)])["ts"]
        client.rm224.c.delete_many(query)
        out.append(
            [
                (
                    e["op"],
                    [o["o"]["_id"] for o in e["o"]["applyOps"]]
                    if e["op"] == "c"
                    else e["o"]["_id"],
                )
                for e in client.local["oplog.rs"].find({"ts": {"$gt": start}, "op": {"$ne": "n"}})
                if e["ns"] in ("rm224.c", "admin.$cmd")
            ]
        )
    client.drop_database("rm224")
    return out


@pytest.mark.backlog_fixed("RM-224")
def test_oplog_batched_delete_is_apply_ops(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 logs a multi-document delete in batches of ten, each
    one `applyOps` holding its deletes last-read first, a batch of one as a
    plain delete."""
    assert _delete_batches(rust_mongo) == _delete_batches(mongod_rs)


def _find_and_modify_retry(client: Any) -> list:
    """A retryable `findAndModify`'s entries and the image mongod keeps for
    it in `config.image_collection`."""
    from pymongo import ReturnDocument

    db = client.rm224
    client.drop_database("rm224")
    db.c.insert_many([{"_id": i, "x": i} for i in range(3)])
    out = []
    with client.start_session() as s:
        lid = s.session_id["id"]
        for write in [
            lambda: db.c.find_one_and_update({"_id": 1}, {"$set": {"y": 1}}, session=s),
            lambda: db.c.find_one_and_update(
                {"_id": 1}, {"$set": {"y": 2}}, return_document=ReturnDocument.AFTER, session=s
            ),
            lambda: db.c.find_one_and_delete({"_id": 2}, session=s),
            lambda: db.c.find_one_and_update(
                {"_id": 9}, {"$set": {"y": 1}}, upsert=True, session=s
            ),
        ]:
            start = client.local["oplog.rs"].find_one(sort=[("$natural", -1)])["ts"]
            write()
            entries = [
                (list(e.keys()), e.get("needsRetryImage"), e.get("stmtId"))
                for e in client.local["oplog.rs"].find({"ts": {"$gt": start}, "ns": "rm224.c"})
            ]
            image = client.config.image_collection.find_one({"_id.id": lid})
            out.append(
                (
                    entries,
                    image
                    and (
                        list(image.keys()),
                        type(image["txnNum"]).__name__,
                        image["imageKind"],
                        image["image"],
                    ),
                    client.local["oplog.rs"].count_documents(
                        {"ts": {"$gt": start}, "ns": "config.image_collection"}
                    ),
                )
            )
    client.drop_database("rm224")
    return out


@pytest.mark.backlog_fixed("RM-224")
def test_retryable_find_and_modify_names_its_image(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 stamps a retryable `findAndModify` with its session and
    `needsRetryImage` (none for an upsert's insert), and keeps the image it
    returned in `config.image_collection`, outside the oplog."""
    assert _find_and_modify_retry(rust_mongo) == _find_and_modify_retry(mongod_rs)
