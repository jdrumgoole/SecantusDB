"""A transaction older than a collection's last catalog change (RM-320),
against a live mongod 8.2.

mongod keeps a minimum valid snapshot on each collection, moved on by every
change to its catalog entry, and a transaction whose snapshot predates it
fails a write there before the write runs: 112 WriteConflict "Unable to
write to collection '<ns>' due to catalog changes; please retry the
operation", labelled TransientTransactionError, and the transaction is
aborted. A read is unaffected, and so is a write to another collection.
Each case takes the transaction's snapshot on another collection, makes the
change from outside it, then writes, on both servers.

The servers run with test commands on, so a case can hold a hybrid index
build part-way (`hangAfterStartingIndexBuild`, `hangIndexBuildBeforeCommit`).
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

import pymongo
import pytest

from tests.mongod_live.test_index_builds import _Build, mongod_fp, rust_fp  # noqa: F401

DB = "txn_catalog"
OTHER_DB = "txn_catalog_x"


def _seed(client: Any) -> Any:
    client.drop_database(DB)
    client.drop_database(OTHER_DB)
    db = client[DB]
    db.c.insert_many([{"_id": i, "a": i} for i in range(5)])
    db.c.create_index("b", name="b_1")
    db.d.insert_one({"_id": 1})
    db.r.insert_one({"_id": 1})
    db.other.insert_one({"_id": 1})
    client[OTHER_DB].c.insert_one({"_id": 1})
    return db


def _outcome(fn: Callable[[], Any]) -> tuple:
    try:
        fn()
        return ("ok",)
    except pymongo.errors.OperationFailure as e:
        d = e.details or {}
        return ("err", e.code, sorted(d.get("errorLabels", [])), d.get("errmsg"))


# mongod 8.2.11's answer to the write, measured 2026-11-08.
_EXPECTED_CONFLICT = (
    "err",
    112,
    ["TransientTransactionError"],
    "Caused by :: Unable to write to collection 'txn_catalog.c' due to catalog changes; please "
    "retry the operation :: Please retry your operation or multi-document transaction.",
)

Change = Callable[[Any, Any], Any]
Write = Callable[[Any, Any, Any], Any]

_CHANGES: dict[str, Change] = {
    "createIndex": lambda c, db: db.c.create_index("a", name="a_1"),
    "createIndex existing": lambda c, db: db.c.create_index("b", name="b_1"),
    "createIndex elsewhere": lambda c, db: db.d.create_index("a", name="a_1"),
    "createIndex other db": lambda c, db: c[OTHER_DB].c.create_index("a", name="a_1"),
    "dropIndex": lambda c, db: db.c.drop_index("b_1"),
    "collMod validator": lambda c, db: db.command("collMod", "c", validator={"a": {"$gte": 0}}),
    "collMod hidden": lambda c, db: db.command(
        "collMod", "c", index={"name": "b_1", "hidden": True}
    ),
    "drop": lambda c, db: db.c.drop(),
    # Of the other database: the transaction holds this one's lock.
    "dropDatabase": lambda c, db: c.drop_database(OTHER_DB),
    "rename away": lambda c, db: db.c.rename("moved"),
    "rename over": lambda c, db: db.r.rename("c", dropTarget=True),
    "plain insert": lambda c, db: db.c.insert_one({"_id": 50}),
}

_WRITES: dict[str, Write] = {
    "insert": lambda c, db, s: db.c.insert_one({"_id": 100, "a": 100}, session=s),
    "update": lambda c, db, s: db.c.update_one({"_id": 1}, {"$set": {"z": 1}}, session=s),
    "update no match": lambda c, db, s: db.c.update_one(
        {"_id": 999}, {"$set": {"z": 1}}, session=s
    ),
    "upsert": lambda c, db, s: db.c.update_one(
        {"_id": 77}, {"$set": {"z": 1}}, upsert=True, session=s
    ),
    "delete": lambda c, db, s: db.c.delete_one({"_id": 2}, session=s),
    "findAndModify": lambda c, db, s: db.c.find_one_and_update(
        {"_id": 1}, {"$set": {"z": 1}}, session=s
    ),
    "read": lambda c, db, s: list(db.c.find({}, session=s)),
    "insert other db": lambda c, db, s: c[OTHER_DB].c.insert_one({"_id": 100}, session=s),
}

_CASES = [
    ("createIndex", "insert"),
    ("createIndex", "update"),
    ("createIndex", "update no match"),
    ("createIndex", "upsert"),
    ("createIndex", "delete"),
    ("createIndex", "findAndModify"),
    ("createIndex", "read"),
    ("createIndex existing", "insert"),
    ("createIndex elsewhere", "insert"),
    ("createIndex other db", "insert"),
    ("dropIndex", "insert"),
    ("collMod validator", "insert"),
    ("collMod hidden", "insert"),
    ("drop", "insert"),
    ("dropDatabase", "insert other db"),
    ("rename away", "insert"),
    ("rename over", "insert"),
    ("plain insert", "update"),
]


def _run(client: Any, change: Callable[[Any, Any], Any], write: Write) -> tuple:
    db = _seed(client)
    with client.start_session() as s:
        s.start_transaction()
        # The snapshot is taken here, on another collection.
        db.other.find_one({}, session=s)
        held = change(client, db)
        out = _outcome(lambda: write(client, db, s))
        if out == ("ok",):
            out += _outcome(s.commit_transaction)
        else:
            # The failed write ended the transaction.
            out += _outcome(s.commit_transaction)[:2]
        if isinstance(held, _Build):
            held.finish()
    client.drop_database(DB)
    client.drop_database(OTHER_DB)
    return out


@pytest.mark.parametrize(("change", "write"), _CASES, ids=[f"{c}->{w}" for c, w in _CASES])
def test_a_write_after_a_catalog_change(
    change: str,
    write: str,
    mongod_fp: Any,  # noqa: F811
    rust_fp: Any,  # noqa: F811
) -> None:
    theirs = _run(mongod_fp, _CHANGES[change], _WRITES[write])
    assert _run(rust_fp, _CHANGES[change], _WRITES[write]) == theirs


@pytest.mark.parametrize("pause", ["hangAfterStartingIndexBuild", "hangIndexBuildBeforeCommit"])
def test_a_write_while_an_index_builds(
    pause: str,
    mongod_fp: Any,  # noqa: F811
    rust_fp: Any,  # noqa: F811
) -> None:
    """An index build in progress has already changed the catalog: the
    unfinished index entered it when the build started."""

    def build(client: Any, db: Any) -> _Build:
        return _Build(client, DB, [{"key": {"z": 1}, "name": "z_1"}], pause)

    write = _WRITES["insert"]
    theirs = _run(mongod_fp, build, write)
    assert theirs == (*_EXPECTED_CONFLICT, "err", 251), theirs
    assert _run(rust_fp, build, write) == theirs


def test_mongod_refuses_a_write_past_a_new_index(mongod_fp: Any) -> None:  # noqa: F811
    """Pins the reference's answer, so the comparisons cannot pass with both
    servers agreeing on something else."""
    theirs = _run(mongod_fp, _CHANGES["createIndex"], _WRITES["insert"])
    assert theirs == (*_EXPECTED_CONFLICT, "err", 251), theirs


def _bulk_write(client: Any, ops: list, ns: list) -> tuple:
    db = _seed(client)
    db.e.insert_one({"_id": 1})
    with client.start_session() as s:
        s.start_transaction()
        db.other.find_one({}, session=s)
        db.c.create_index("a", name="a_1")
        db.e.create_index("a", name="a_1")
        out = _outcome(
            lambda: client.admin.command(
                {"bulkWrite": 1, "ops": ops, "nsInfo": [{"ns": f"{DB}.{n}"} for n in ns]},
                session=s,
            )
        )
        out += _outcome(s.commit_transaction)[:2]
    client.drop_database(DB)
    client.drop_database(OTHER_DB)
    return out


@pytest.mark.parametrize(
    ("ops", "ns"),
    [
        # The first op's namespace is the one named, not nsInfo's first.
        (
            [{"insert": 1, "document": {"_id": 50}}, {"insert": 0, "document": {"_id": 51}}],
            ["e", "c"],
        ),
        # An op on an unchanged collection first: still the whole command.
        (
            [{"insert": 0, "document": {"_id": 50}}, {"insert": 1, "document": {"_id": 51}}],
            ["d", "c"],
        ),
        ([{"update": 0, "filter": {"_id": 999}, "updateMods": {"$set": {"z": 1}}}], ["c"]),
        ([{"insert": 0, "document": {"_id": 50}}], ["d"]),
    ],
    ids=["op order", "unchanged first", "update no match", "unchanged only"],
)
def test_a_bulk_write_after_a_catalog_change(
    ops: list,
    ns: list,
    mongod_fp: Any,  # noqa: F811
    rust_fp: Any,  # noqa: F811
) -> None:
    theirs = _bulk_write(mongod_fp, ops, ns)
    assert _bulk_write(rust_fp, ops, ns) == theirs


def _write_after_killed_build(client: Any) -> tuple:
    db = _seed(client)
    build = _Build(client, DB, [{"key": {"z": 1}, "name": "z_1"}], "hangAfterStartingIndexBuild")
    with client.start_session() as s:
        s.start_transaction()
        db.other.find_one({}, session=s)
        # The build started before the snapshot; its abort comes after it.
        client.admin.command("killOp", op=build.ops()[0]["opid"])
        build.finish()
        out = _outcome(lambda: db.c.insert_one({"_id": 100}, session=s))
        out += _outcome(s.commit_transaction)[:2]
    indexes = sorted(i["name"] for i in db.c.list_indexes())
    client.drop_database(DB)
    client.drop_database(OTHER_DB)
    return out, indexes


def test_a_write_after_an_index_build_aborts(
    mongod_fp: Any,  # noqa: F811
    rust_fp: Any,  # noqa: F811
) -> None:
    """An aborted build takes its unfinished index out of the catalog, which
    is a change like any other."""
    theirs = _write_after_killed_build(mongod_fp)
    assert theirs == ((*_EXPECTED_CONFLICT, "err", 251), ["_id_", "b_1"]), theirs
    assert _write_after_killed_build(rust_fp) == theirs
