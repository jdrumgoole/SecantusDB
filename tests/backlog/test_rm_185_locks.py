"""RM-185: the locks each command takes, as `serverStatus.locks` counts
them (`crates/secantus-commands/src/lock_manager.rs`, `statement_rounds`),
against mongod 8.2.11."""

from __future__ import annotations

import contextlib
import itertools
from collections.abc import Callable
from typing import Any

import pytest

# Internal resources mongod's background threads lock; this server has none.
_INTERNAL = {"Metadata", "oplog", "MultiDocumentTransactionsBarrier"}


def _flat(client: Any) -> dict:
    locks = client.admin.command("serverStatus").get("locks", {})
    return {
        (res, mode): n
        for res, v in locks.items()
        if res not in _INTERNAL
        for mode, n in v.get("acquireCount", {}).items()
    }


def _quiet(fn: Callable[[], Any]) -> None:
    # An error answer is part of what is measured.
    with contextlib.suppress(Exception):
        fn()


def _deltas(client: Any, cases: list) -> list:
    out = []
    for label, fn in cases:
        before = _flat(client)
        _quiet(fn)
        after = _flat(client)
        keys = set(before) | set(after)
        out.append(
            (
                label,
                sorted(
                    (k, after.get(k, 0) - before.get(k, 0))
                    for k in keys
                    if after.get(k, 0) != before.get(k, 0)
                ),
            )
        )
    return out


def _crud(client: Any) -> list:
    client.drop_database("rm185")
    db = client.rm185
    db.c.insert_many([{"_id": i, "x": i} for i in range(10)])
    n = itertools.count()
    return _deltas(
        client,
        [
            ("insert_one", lambda: db.c.insert_one({})),
            ("insert_many", lambda: db.c.insert_many([{}, {}])),
            ("insert creating a collection", lambda: db[f"n{next(n)}"].insert_one({})),
            ("find", lambda: db.c.find_one({"_id": 1})),
            ("find and getMore", lambda: list(db.c.find().batch_size(3))),
            ("update", lambda: db.c.update_one({"_id": 1}, {"$set": {"a": 1}})),
            (
                "update of two statements",
                lambda: db.command(
                    "update",
                    "c",
                    updates=[
                        {"q": {"_id": 1}, "u": {"$set": {"a": 2}}},
                        {"q": {"_id": 2}, "u": {"$set": {"a": 2}}},
                    ],
                ),
            ),
            (
                "delete of two statements",
                lambda: db.command(
                    "delete",
                    "c",
                    deletes=[{"q": {"_id": 3}, "limit": 1}, {"q": {"_id": 4}, "limit": 1}],
                ),
            ),
            ("findAndModify", lambda: db.c.find_one_and_update({"_id": 5}, {"$set": {"y": 1}})),
            ("aggregate", lambda: list(db.c.aggregate([{"$match": {}}]))),
            ("count", lambda: db.command("count", "c")),
            ("distinct", lambda: db.c.distinct("x")),
            ("listCollections", lambda: db.list_collection_names()),
            ("listIndexes", lambda: list(db.c.list_indexes())),
            ("create", lambda: db.command("create", "k1")),
            ("create of one that exists", lambda: db.command("create", "k1")),
            ("drop", lambda: db.command("drop", "k1")),
            ("drop of one that is not there", lambda: db.command("drop", "k9")),
            ("ping", lambda: db.command("ping")),
            ("killCursors", lambda: db.command("killCursors", "c", cursors=[])),
            ("dropDatabase", lambda: client.drop_database("rm185b")),
        ],
    )


@pytest.mark.backlog_fixed("RM-185")
def test_lock_counts_per_command(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 counts one IS on the global lock for a read, a round of
    IX on the state transition, global, database and collection locks per
    write statement (three for an insert that creates its collection), and
    the collection in X for a drop."""
    assert _crud(rust_mongo) == _crud(mongod)


def _builds(client: Any) -> list:
    client.drop_database("rm185c")
    db = client.rm185c
    db.c.insert_many([{"_id": i, "x": i} for i in range(10)])
    return _deltas(
        client,
        [
            ("createIndexes", lambda: db.c.create_index("x")),
            ("dropIndexes", lambda: db.c.drop_index("x_1")),
            ("$out", lambda: list(db.c.aggregate([{"$match": {}}, {"$out": "o"}]))),
            ("dbStats", lambda: db.command("dbStats")),
        ],
    )


@pytest.mark.backlog("RM-185")
def test_lock_counts_of_index_builds_out_and_db_stats(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's counts for an index build (its phases take the
    collection in X and IX several times), `dropIndexes`, an `$out` and
    `dbStats` (IS on each collection)."""
    assert _builds(rust_mongo) == _builds(mongod)
