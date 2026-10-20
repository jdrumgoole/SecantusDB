"""RM-185: the locks each command takes, as `serverStatus.locks` counts
them (`crates/secantus-commands/src/lock_manager.rs`, `statement_rounds`),
against mongod 8.2.11."""

from __future__ import annotations

import contextlib
import itertools
import uuid
from collections.abc import Callable
from typing import Any

import pymongo
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


def _fresh(prefix: str) -> str:
    """A database name never used before: mongod takes an extra global IS
    building an index on a namespace a drop has just freed, so a reused
    name measures that, not the build."""
    return f"{prefix}_{uuid.uuid4().hex[:8]}"


# mongod's index build runs on a thread of its own, and the command takes
# the global IS each time it looks at the build: more the longer the build
# takes (measured 1 to 4 on mongod 8.2.11 for one shape). Every other count
# of a build is fixed, so the global IS is compared as a floor: mongod takes
# at least as many as this server.
_BUILD_POLLS = ("Global", "r")


def _same_counts(got: list, want: list) -> None:
    assert [label for label, _ in got] == [label for label, _ in want]
    for (label, ours), (_, theirs) in zip(got, want, strict=True):
        ours, theirs = dict(ours), dict(theirs)
        if label.startswith("createIndex"):
            assert ours.get(_BUILD_POLLS, 0) >= 1, label
            assert theirs.get(_BUILD_POLLS, 0) >= ours.get(_BUILD_POLLS, 0), label
            ours.pop(_BUILD_POLLS, None)
            theirs.pop(_BUILD_POLLS, None)
        assert (label, ours) == (label, theirs)


def _reference(measure: Callable[[Any], list], client: Any) -> list:
    """mongod's counts for each case: the least of five runs. Its
    background threads (the TTL monitor, the session refresh) take the
    global lock too, and one that fires inside a case's window can only
    ADD to that case's count. Three runs were not enough under a loaded
    machine: on 2026-10-20, with four test workers, a background lock landed
    in all three windows of one case."""
    runs = [measure(client) for _ in range(5)]
    out = []
    for cases in zip(*runs, strict=True):
        label = cases[0][0]
        counts = [dict(c[1]) for c in cases]
        keys = set().union(*counts)
        least = {k: min(c.get(k, 0) for c in counts) for k in keys}
        out.append((label, sorted((k, n) for k, n in least.items() if n)))
    return out


def _crud(client: Any) -> list:
    db = client[_fresh("rm185")]
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
            ("dropDatabase", lambda: client.drop_database(_fresh("rm185b"))),
        ],
    )


@pytest.mark.backlog_fixed("RM-185")
def test_lock_counts_per_command(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 counts one IS on the global lock for a read, a round of
    IX on the state transition, global, database and collection locks per
    write statement (three for an insert that creates its collection), and
    the collection in X for a drop."""
    assert _crud(rust_mongo) == _reference(_crud, mongod)


def _builds(client: Any) -> list:
    db = client[_fresh("rm185c")]
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


@pytest.mark.backlog_fixed("RM-185")
def test_lock_counts_of_index_builds_out_and_db_stats(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's counts for an index build (its phases take the
    collection in X and IX several times), `dropIndexes`, an `$out` and
    `dbStats` (IS on each collection)."""
    _same_counts(_builds(rust_mongo), _reference(_builds, mongod))


def _variations(client: Any) -> list:
    db = client[_fresh("rm185d")]
    db.c.insert_many([{"x": i, "y": i} for i in range(5)])
    db.e.insert_one({})
    db.e.delete_many({})
    cases = [
        ("createIndex", lambda: db.c.create_index("x")),
        ("createIndex of one that exists", lambda: db.c.create_index("x")),
        ("createIndex on an empty collection", lambda: db.e.create_index("x")),
        (
            "createIndexes of two",
            lambda: db.c.create_indexes(
                [pymongo.IndexModel("y"), pymongo.IndexModel([("x", 1), ("y", 1)])]
            ),
        ),
        ("createIndex creating the collection", lambda: db.newc.create_index("x")),
        ("dropIndexes of all", lambda: db.c.drop_indexes()),
        ("dropIndex of one that is not there", lambda: db.c.drop_index("nope_1")),
        ("$out replacing its target", lambda: list(db.c.aggregate([{"$out": "o1"}]))),
        ("$out again", lambda: list(db.c.aggregate([{"$out": "o1"}]))),
    ]
    for n in (1, 3):
        name = _fresh(f"rm185s{n}")
        for i in range(n):
            client[name][f"c{i}"].insert_one({})
        cases.append((f"dbStats over {n}", lambda name=name: client[name].command("dbStats")))
    cases.append(("dbStats of no database", lambda: client[_fresh("rm185none")].command("dbStats")))
    return _deltas(client, cases)


@pytest.mark.backlog_fixed("RM-185")
def test_lock_counts_of_index_build_and_out_shapes(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: an index build takes the collection in X three times
    and IX five (once each on an empty collection, a write round when every
    index is there already, two when it creates the collection); `$out`
    replaces an existing target with one more X round; `dbStats` takes IS
    on each collection."""
    _same_counts(_variations(rust_mongo), _reference(_variations, mongod))


def _merge(client: Any) -> list:
    db = client[_fresh("rm185m")]
    db.c.insert_many([{"_id": i} for i in range(5)])
    pipeline = [{"$merge": {"into": "m"}}]
    return _deltas(
        client,
        [
            ("$merge creating its target", lambda: list(db.c.aggregate(pipeline))),
            ("$merge into it again", lambda: list(db.c.aggregate(pipeline))),
        ],
    )


@pytest.mark.backlog_fixed("RM-185")
def test_lock_counts_of_merge(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$merge` takes the global IS twice and a write round
    for each document it writes (two more when it creates the target)."""
    assert _merge(rust_mongo) == _reference(_merge, mongod)


def _merge_modes(client: Any) -> list:
    cases = []
    for n in (0, 1, 3):
        db = client[_fresh("rm185n")]
        if n:
            db.c.insert_many([{"_id": i} for i in range(n)])
        else:
            db.create_collection("c")
        db.t.insert_one({"_id": 0})
        cases.append(
            (f"$merge of {n}", lambda db=db: list(db.c.aggregate([{"$merge": {"into": "t"}}])))
        )
    for matched, not_matched in [
        ("replace", "insert"),
        ("keepExisting", "insert"),
        ("merge", "discard"),
        ([{"$set": {"z": 1}}], "insert"),
    ]:
        db = client[_fresh("rm185w")]
        db.c.insert_many([{"_id": i} for i in range(4)])
        db.t.insert_one({"_id": 0})
        spec = {"into": "t", "whenMatched": matched, "whenNotMatched": not_matched}
        cases.append(
            (
                f"$merge {matched} / {not_matched}",
                lambda db=db, spec=spec: list(db.c.aggregate([{"$merge": spec}])),
            )
        )
    db = client[_fresh("rm185f")]
    db.c.insert_many([{"_id": i} for i in range(4)])
    db.t.insert_one({"_id": 99})
    cases.append(
        (
            "$merge fail with no duplicate",
            lambda: list(db.c.aggregate([{"$merge": {"into": "t", "whenMatched": "fail"}}])),
        )
    )
    return _deltas(client, cases)


@pytest.mark.backlog_fixed("RM-185")
def test_lock_counts_of_merge_modes(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$merge`: the global IS twice and a write round per
    document, whatever `whenMatched` / `whenNotMatched` say; `whenMatched:
    "fail"` one round for a batch that inserts cleanly."""
    assert _merge_modes(rust_mongo) == _reference(_merge_modes, mongod)


def _merge_fail_duplicates(client: Any) -> list:
    db = client[_fresh("rm185d2")]
    db.c.insert_many([{"_id": i} for i in range(4)])
    db.t.insert_many([{"_id": 0}, {"_id": 2}])
    spec = {"into": "t", "whenMatched": "fail"}
    return _deltas(
        client,
        [("$merge fail with two duplicates", lambda: list(db.c.aggregate([{"$merge": spec}])))],
    )


@pytest.mark.backlog("RM-185")
def test_lock_counts_of_merge_fail_with_duplicates(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `whenMatched: "fail"` retries a batch that met a
    duplicate, taking more write rounds (4 for 4 documents with two
    duplicates, 3 with one); this server takes the batch's one."""
    assert _merge_fail_duplicates(rust_mongo) == _reference(_merge_fail_duplicates, mongod)
