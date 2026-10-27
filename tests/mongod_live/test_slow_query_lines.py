"""`Slow query` lines and profile entries against a live mongod (RM-023).

Every operation is logged as slow and profiled on both servers, and each
line and entry is compared with mongod's field for field: which fields it
has and in what order, the counts it reports -- keys and documents
examined, the index keys written, what was matched, modified, inserted,
deleted and returned -- and what it names (the plan, its cache key, the
engine, the read and write concern, the kind of collection, the error).
Times and sizes cannot be equal, so they are checked for their type and
for being plausible. The counts are this server's own, made as the
operation ran.

Left out of the comparison, each an open backlog item with a gap test of
its own: `queryShapeHash` (RM-358), `numInterruptChecks` (RM-360), `queues`
(RM-355), a profile entry's `execStats` (RM-359), `resolvedViews`
(RM-362), `totalOplogSlotDurationMicros` (RM-361), and a write's `locks`
and `flowControl` (RM-357). The `storage` section is compared for its
shape only: mongod's always has `storageExecutionMicros` (RM-355), and what
each engine reads from disk is its own.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

import pytest
from pymongo.read_concern import ReadConcern
from pymongo.write_concern import WriteConcern

from tests.backlog.opdebug import keys, logged, seed

#: Fields this server does not write yet, each an open item (above).
UNMODELLED = frozenset(
    {
        "queryShapeHash",
        "numInterruptChecks",
        "queues",
        "execStats",
        "resolvedViews",
        "totalOplogSlotDurationMicros",
        "storage",
    }
)

#: Fields compared exactly.
EXACT = (
    "type",
    "op",
    "isFromUserConnection",
    "collectionType",
    "planSummary",
    "keysExamined",
    "docsExamined",
    "hasSortStage",
    "nMatched",
    "nBatches",
    "nModified",
    "ninserted",
    "ndeleted",
    "nUpserted",
    "cursorExhausted",
    "keysInserted",
    "keysDeleted",
    "numYields",
    "numYield",
    "nreturned",
    "planCacheShapeHash",
    "queryHash",
    "planCacheKey",
    "queryFramework",
    "ok",
    "errName",
    "errCode",
    "readConcern",
    "writeConcern",
    "protocol",
)

#: Fields that hold a time or a size: an integer, and not negative.
TIMED = (
    "planningTimeMicros",
    "reslen",
    "responseLength",
    "workingMillis",
    "durationMillis",
    "millis",
    "waitForWriteConcernDurationMillis",
    "waitForWriteConcernDuration",
    "cpuNanos",
)

#: What a `storage` section may hold here.
STORAGE = {
    "data": {"bytesRead", "bytesWritten", "timeReadingMicros", "timeWritingMicros", "txnBytesDirty"},
    "timeWaitingMicros": {"cacheMicros", "schemaLock", "handleLock"},
}


def _wrote(doc: dict) -> bool:
    counts = doc.get("locks", {}).get("Global", {}).get("acquireCount", {})
    return "w" in counts or "W" in counts


def _compare(label: str, ours: dict, theirs: dict) -> list[str]:
    problems = []
    # A write's locks and flow control are RM-357.
    leave_out = set(UNMODELLED)
    if _wrote(theirs):
        leave_out |= {"locks", "flowControl"}
    a, b = keys(ours, frozenset(leave_out)), keys(theirs, frozenset(leave_out))
    if a != b:
        problems.append(f"{label}: fields\n    ours:   {a}\n    mongod: {b}")
    for field in EXACT:
        if ours.get(field) != theirs.get(field):
            problems.append(f"{label}: {field}: ours {ours.get(field)!r}, mongod {theirs.get(field)!r}")
    if "locks" not in leave_out and ours.get("locks") != theirs.get("locks"):
        problems.append(f"{label}: locks: ours {ours.get('locks')}, mongod {theirs.get('locks')}")
    for field in TIMED:
        if field in ours and not (isinstance(ours[field], int) and ours[field] >= 0):
            problems.append(f"{label}: {field} is {ours[field]!r}")
    storage = ours.get("storage")
    if storage is not None:
        for section, values in storage.items():
            allowed = STORAGE.get(section, set())
            for name, n in values.items():
                if name not in allowed or not (isinstance(n, int) and n > 0):
                    problems.append(f"{label}: storage.{section}.{name} = {n!r}")
    return problems


def _scenario(rust_mongo: Any, mongod: Any, op: Callable[[Any], Any]) -> None:
    ours = logged(rust_mongo, False, seed, op)
    theirs = logged(mongod, True, seed, op)
    problems = []
    (our_lines, our_entries), (their_lines, their_entries) = ours, theirs
    shape = lambda lines: [(c, a.get("type"), next(iter(a.get("command", {})), "")) for c, a in lines]  # noqa: E731
    if shape(our_lines) != shape(their_lines):
        problems.append(f"lines: ours {shape(our_lines)}, mongod {shape(their_lines)}")
    else:
        for i, ((_, a), (_, b)) in enumerate(zip(our_lines, their_lines)):
            problems += _compare(f"line {i}", a, b)
    entry_shape = lambda es: [(e.get("op"), next(iter(e.get("command", {})), "")) for e in es]  # noqa: E731
    if entry_shape(our_entries) != entry_shape(their_entries):
        problems.append(f"entries: ours {entry_shape(our_entries)}, mongod {entry_shape(their_entries)}")
    else:
        for i, (a, b) in enumerate(zip(our_entries, their_entries)):
            problems += _compare(f"entry {i}", a, b)
    assert not problems, "\n" + "\n".join(problems)


SCENARIOS: dict[str, Callable[[Any], Any]] = {
    "find through an index": lambda d: list(d.c.find({"x": {"$gt": 1}})),
    "find of the collection": lambda d: list(d.c.find({"z": 1})),
    "find in batches": lambda d: list(d.c.find({"x": {"$gte": 2}}).batch_size(3)),
    "find with a blocking sort": lambda d: list(d.c.find({"z": 0}).sort("y", 1).batch_size(2)),
    "find with a sort and a limit": lambda d: list(d.c.find({"z": 0}).sort("y", -1).limit(2)),
    "find by _id": lambda d: d.c.find_one({"_id": 3}),
    "find by an _id that is not there": lambda d: d.c.find_one({"_id": 99}),
    "find with $or": lambda d: list(d.c.find({"$or": [{"x": 1}, {"x": 5}]})),
    "find with a limit": lambda d: list(d.c.find({"x": {"$gte": 3}}).limit(1)),
    "find of nothing": lambda d: list(d.c.find({"x": 999})),
    "find of a collection not there": lambda d: list(d.nope.find({"x": 1})),
    "find under a read concern": lambda d: list(
        d.get_collection("c", read_concern=ReadConcern("majority")).find({"x": 1})
    ),
    "find that fails": lambda d: d.command("find", "c", filter={"$foo": 1}),
    "killCursors": lambda d: (lambda cur: (next(cur), cur.close()))(d.c.find().batch_size(2)),
    "aggregate with a $group": lambda d: list(
        d.c.aggregate([{"$match": {"x": {"$gte": 4}}}, {"$group": {"_id": "$y", "n": {"$sum": 1}}}])
    ),
    "aggregate in batches": lambda d: list(d.c.aggregate([{"$match": {"x": {"$gte": 1}}}], batchSize=2)),
    "aggregate with a $sort": lambda d: list(d.c.aggregate([{"$sort": {"y": 1}}, {"$limit": 3}])),
    "aggregate of $documents": lambda d: list(d.aggregate([{"$documents": [{"a": 1}]}])),
    "count_documents": lambda d: d.c.count_documents({"z": 1}),
    "count through an index": lambda d: d.command("count", "c", query={"x": {"$gt": 3}}),
    "count of the collection": lambda d: d.command("count", "c", query={"z": 1}),
    "count with skip and limit": lambda d: d.command(
        "count", "c", query={"x": {"$gt": 3}}, skip=2, limit=3
    ),
    "count of nothing": lambda d: d.command("count", "c", query={"x": 99}),
    "distinct of the collection": lambda d: d.c.distinct("z"),
    "distinct from an index": lambda d: d.c.distinct("x"),
    "distinct with a query": lambda d: d.c.distinct("y", {"x": {"$lt": 7}}),
    "insert": lambda d: d.c.insert_one({"_id": 100, "x": 100}),
    "insert of a multikey document": lambda d: (
        d.c.create_index([("y", 1), ("z", 1)]),
        d.c.insert_one({"x": [1, 2, 3], "y": 1, "z": [4, 5]}),
    ),
    "insert under w: 1": lambda d: d.get_collection("c", write_concern=WriteConcern(w=1)).insert_one(
        {"_id": 500}
    ),
    "insert of a duplicate": lambda d: d.c.insert_one({"_id": 1}),
    "update_one": lambda d: d.c.update_one({"x": {"$gte": 5}}, {"$set": {"x": 55}}),
    "update_many": lambda d: d.c.update_many({"z": 1}, {"$inc": {"w": 1}}),
    "update of nothing": lambda d: d.c.update_one({"x": 999}, {"$set": {"w": 1}}),
    "upsert": lambda d: d.c.update_one({"x": 999}, {"$set": {"y": 9}}, upsert=True),
    "replace by _id": lambda d: d.c.replace_one({"_id": 2}, {"x": 2, "r": 1}),
    "update making a key multikey": lambda d: d.c.update_one({"_id": 3}, {"$set": {"x": [1, 2, 2, 3]}}),
    "update of two statements": lambda d: d.command(
        "update",
        "c",
        updates=[{"q": {"x": 1}, "u": {"$set": {"k": 1}}}, {"q": {"x": 2}, "u": {"$set": {"k": 1}}}],
    ),
    "update that fails": lambda d: d.c.update_one({"x": 1}, {"$set": {"_id": 99}}),
    "delete_one": lambda d: d.c.delete_one({"x": {"$gte": 8}}),
    "delete_many": lambda d: d.c.delete_many({"z": 0}),
    "delete by _id": lambda d: d.c.delete_one({"_id": 4}),
    "findAndModify": lambda d: d.c.find_one_and_update({"x": 3}, {"$set": {"q": 1}}),
    "findAndModify with a sort": lambda d: d.c.find_one_and_update(
        {"z": 1}, {"$set": {"q": 1}}, sort=[("y", -1)]
    ),
    "findAndModify remove": lambda d: d.c.find_one_and_delete({"x": 4}),
    "findAndModify upsert": lambda d: d.c.find_one_and_update(
        {"x": 999}, {"$set": {"q": 1}}, upsert=True
    ),
}


@pytest.mark.parametrize("label", list(SCENARIOS))
def test_lines_and_entries_are_mongods(rust_mongo: Any, mongod: Any, label: str) -> None:
    """The line and profile entry of each operation are mongod 8.2's, with
    the counts the operation made as it ran."""
    _scenario(rust_mongo, mongod, SCENARIOS[label])
