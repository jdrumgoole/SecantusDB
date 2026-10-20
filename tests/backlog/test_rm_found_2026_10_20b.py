"""Gap tests for the Rust MongoDB server items found on 2026-10-20 by the
server audit (§7.086).

Every test asks mongod 8.2.11 live -- a standalone (`mongod`) or a one-member
replica set (`mongod_rs`) where the item needs sessions or transactions -- and
asserts the Rust server gives the same answer. Answers are reduced to what the
item is about (a value, or an error code) so that an unrelated difference
cannot make a test fail.
"""

from __future__ import annotations

import datetime
import socket
import struct
import threading
import time
import uuid
from collections.abc import Callable
from typing import Any

import bson
import pymongo
import pytest
from bson import Binary, Decimal128, Int64, Timestamp
from pymongo.errors import PyMongoError

from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

# ------------------------------------------------------------- helpers --


def answer(fn: Callable[[], Any]) -> tuple:
    """`("ok", value)`, `("err", code)` for a server error, or
    `("exc", type)` for anything else -- never raises."""
    try:
        return ("ok", fn())
    except PyMongoError as e:
        return ("err", getattr(e, "code", None))
    except Exception as e:  # noqa: BLE001 - a crash is an answer too
        return ("exc", type(e).__name__)


def direct(client: Any) -> Any:
    """A second client to the same server (a session of its own)."""
    host, port = client.address
    return pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=10_000)


def _db(client: Any, name: str) -> Any:
    client.drop_database(name)
    return client[name]


# ------------------------------------------------- RM-210 snapshot reads --


def _snapshot_at_cluster_time(client: Any) -> tuple:
    db = _db(client, "rm210a")
    db.c.insert_one({"_id": 1, "n": 0})
    t = db.command("ping")["operationTime"]
    db.c.update_one({"_id": 1}, {"$set": {"n": 9}})
    return answer(
        lambda: db.command({"find": "c", "readConcern": {"level": "snapshot", "atClusterTime": t}})[
            "cursor"
        ]["firstBatch"][0]["n"]
    )


@pytest.mark.backlog("RM-210")
def test_snapshot_read_at_cluster_time(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 answers a `readConcern: {level: "snapshot",
    atClusterTime: T}` find with the document as it was at T (n: 0)."""
    assert _snapshot_at_cluster_time(rust_mongo) == _snapshot_at_cluster_time(mongod_rs)


def _snapshot_session(client: Any) -> tuple:
    db = _db(client, "rm210b")
    db.c.insert_one({"_id": 1, "n": 0})
    other = direct(client)

    def run() -> tuple:
        with client.start_session(snapshot=True) as s:
            a = db.c.find_one({"_id": 1}, session=s)["n"]
            other.rm210b.c.update_one({"_id": 1}, {"$set": {"n": 7}})
            b = db.c.find_one({"_id": 1}, session=s)["n"]
            return a, b

    try:
        return answer(run)
    finally:
        other.close()


@pytest.mark.backlog("RM-210")
def test_snapshot_session_keeps_its_snapshot(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11: a `snapshot=True` session reads (0, 0) across another
    client's write between its two reads."""
    assert _snapshot_session(rust_mongo) == _snapshot_session(mongod_rs)


# --------------------------------------------- RM-211 aggregation memory --

_BIG = "x" * 1_000_000

MEMORY_CASES = {
    "$group allowDiskUse false": {
        "pipeline": [
            {"$group": {"_id": "$_id", "s": {"$push": "$s"}}},
            {"$project": {"_id": 1}},
        ],
        "allowDiskUse": False,
    },
    "$push into one group": {
        "pipeline": [{"$group": {"_id": None, "s": {"$push": "$s"}}}, {"$project": {"_id": 1}}],
    },
    "$addToSet into one group": {
        "pipeline": [
            {
                "$group": {
                    "_id": None,
                    "s": {"$addToSet": {"$concat": ["$s", {"$toString": "$_id"}]}},
                }
            },
            {"$project": {"_id": 1}},
        ],
    },
    "$setWindowFields unbounded $push": {
        "pipeline": [
            {
                "$setWindowFields": {
                    "sortBy": {"_id": 1},
                    "output": {
                        "w": {"$push": "$s", "window": {"documents": ["unbounded", "unbounded"]}}
                    },
                }
            },
            {"$project": {"_id": 1}},
        ],
        "allowDiskUse": False,
    },
    "$lookup of the whole collection": {
        "pipeline": [
            {"$limit": 1},
            {"$lookup": {"from": "big", "pipeline": [], "as": "j"}},
            {"$project": {"n": {"$size": "$j"}}},
        ],
    },
    "$facet over the whole collection": {
        "pipeline": [{"$facet": {"a": [{"$project": {"s": 1}}]}}, {"$project": {"_id": 1}}],
    },
}


def _memory_limits(client: Any) -> dict:
    db = _db(client, "rm211")
    for start in range(0, 110, 10):
        db.big.insert_many([{"_id": i, "s": _BIG} for i in range(start, start + 10)])
    out = {}
    for name, spec in MEMORY_CASES.items():
        cmd = {"aggregate": "big", "cursor": {}, **spec}
        got = answer(lambda cmd=cmd: len(db.command(cmd)["cursor"]["firstBatch"]))
        out[name] = got[1] if got[0] == "err" else got[0]
    client.drop_database("rm211")
    return out


@pytest.mark.backlog("RM-211")
def test_aggregation_memory_limits(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11, over 110 one-megabyte documents: 292 for `$group` with
    `allowDiskUse: false`, 146 for an array past 100 MB from `$push` /
    `$addToSet`, 5643011 for `$setWindowFields`, 4568 for a `$lookup`
    result and 4031700 for `$facet`."""
    assert _memory_limits(rust_mongo) == _memory_limits(mongod)


# --------------------------------------------- RM-212 date arithmetic --

DATE = datetime.datetime(2024, 3, 4, 5, 6, 7)

DATE_CASES = {
    "date - int": {"$subtract": ["$d", 1000]},
    "date - long": {"$subtract": ["$d", Int64(1000)]},
    "date - double": {"$subtract": ["$d", 1000.5]},
    "date - decimal": {"$subtract": ["$d", Decimal128("1000")]},
    "date - date": {"$subtract": ["$d", datetime.datetime(2024, 1, 1)]},
    "date + int": {"$add": ["$d", 1000]},
    "date alone": {"$add": ["$d"]},
    "empty $add": {"$add": []},
    "date + double + decimal": {"$add": ["$d", 1.5, Decimal128("2")]},
}


def _date_arithmetic(client: Any) -> dict:
    db = _db(client, "rm212")
    db.c.insert_one({"_id": 0, "d": DATE})
    out = {
        name: answer(lambda e=e: list(db.c.aggregate([{"$project": {"_id": 0, "v": e}}])))
        for name, e in DATE_CASES.items()
    }
    out["$expr in find"] = answer(
        lambda: db.c.count_documents({"$expr": {"$lt": [{"$subtract": ["$d", 86400000]}, "$d"]}})
    )
    out["pipeline update"] = answer(
        lambda: (
            db.c.update_one({"_id": 0}, [{"$set": {"e": {"$subtract": ["$d", 1]}}}]),
            db.c.find_one({"_id": 0})["e"],
        )[1]
    )
    return out


@pytest.mark.backlog_fixed("RM-212")
def test_date_arithmetic(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 computes `$add` / `$subtract` with a Date (a date
    minus a number or a date, a date plus numbers, `$add: []` is 0) in
    aggregation, in `$expr` and in a pipeline update."""
    assert _date_arithmetic(rust_mongo) == _date_arithmetic(mongod)


# --------------------------------------------- RM-213 expression gaps --

EXPRESSION_CASES = {
    "$unsetField": {"$unsetField": {"field": "x", "input": "$o"}},
    "$toUUID": {"$toUUID": "123e4567-e89b-12d3-a456-426614174000"},
    "$convert string to binData base64": {
        "$convert": {"input": "aGk=", "to": "binData", "format": "base64"}
    },
    "$convert to {type: binData}": {
        "$convert": {"input": 1, "to": {"type": "binData", "subtype": 0}}
    },
    "$dateToString of a Timestamp": {"$dateToString": {"date": Timestamp(1700000000, 1)}},
    "$substrCP non-numeric index": {"$substrCP": ["abc", "x", 1]},
    "$setField on a non-object": {"$setField": {"field": "a", "input": 5, "value": 1}},
    "$zip over a non-array": {"$zip": {"inputs": [1]}},
    "$map over a non-array": {"$map": {"input": 1, "in": 1}},
    "$getField on a non-object": {"$getField": {"field": "nope", "input": 5}},
    "$slice with a string count": {"$slice": ["$arr", "x"]},
    "$let with an invalid name": {"$let": {"vars": {"X": 1}, "in": 1}},
    "$arrayToObject mixing forms": {"$arrayToObject": [[{"k": "a", "v": 1}, ["b", 2]]]},
}


def _convert(**spec: Any) -> dict:
    return {"$convert": spec}


UUID = "123e4567-e89b-12d3-a456-426614174000"

# Measured against mongod 8.2.11 on 2026-10-20 while fixing RM-213: each
# format both ways, the subtype rules, `byteOrder`, and the refusals.
MORE_EXPRESSION_CASES = {
    "binData to string, hex": _convert(input="$b", to="string", format="hex"),
    "binData to string, utf8": _convert(input="$b", to="string", format="utf8"),
    "binData to string, base64url": _convert(
        input={"$literal": Binary(b"\xfb\xff", 0)}, to="string", format="base64url"
    ),
    "binData to string, no format": _convert(input="$b", to="string"),
    "binData to string, bad utf8": _convert(
        input={"$literal": Binary(b"\xff", 0)}, to="string", format="utf8"
    ),
    "$toString of a UUID": {"$toString": {"$toUUID": UUID}},
    "string to binData, hex": _convert(input="6869", to="binData", format="hex"),
    "string to binData, odd hex": _convert(input="686", to="binData", format="hex"),
    "string to binData, base64url": _convert(input="aGk", to="binData", format="base64url"),
    "string to binData, no format": _convert(input="aGk=", to="binData"),
    "string to binData, uuid at subtype 0": _convert(input=UUID, to="binData", format="uuid"),
    "bad base64, onError": _convert(input="!!", to="binData", format="base64", onError="E"),
    "bad subtype": _convert(input="aGk=", to={"type": "binData", "subtype": 300}, format="base64"),
    "int to binData, big-endian": _convert(input=258, to="binData", byteOrder="big"),
    "binData to int, signed": _convert(input={"$literal": Binary(b"\xff", 0)}, to="int"),
    "binData between subtypes": _convert(input={"$literal": Binary(b"hi", 5)}, to="binData"),
    "bad byteOrder": _convert(input=1, to="binData", byteOrder="Big"),
    "$toUUID of a bad string": {"$toUUID": "nope"},
    "$unsetField on a string": {"$unsetField": {"field": "x", "input": "abc"}},
    "$unsetField with a computed field": {
        "$unsetField": {"field": {"$concat": ["x"]}, "input": {"x": 1}}
    },
    "a dotted key in an object literal": {"$concatArrays": [[{"a.b": 1}]]},
    "$substrBytes, string index": {"$substrBytes": ["abc", "x", 1]},
    "$substrBytes, negative start": {"$substrBytes": ["abc", -1, 1]},
    "$substrBytes, mid-character": {"$substrBytes": ["éa", 0, 1]},
    "$zip over a string input": {"$zip": {"inputs": ["$o", [1]]}},
    "$zip with non-list inputs": {"$zip": {"inputs": "$arr"}},
    "$filter over an object": {"$filter": {"input": "$o", "cond": True}},
    "$map as an invalid name": {"$map": {"input": [1], "as": "X", "in": 1}},
    "$let with a dotted name": {"$let": {"vars": {"a.b": 1}, "in": 1}},
    "$arrayToObject, a short pair": {"$arrayToObject": [[["a"]]]},
    "$arrayToObject, no v": {"$arrayToObject": [[{"k": "a"}]]},
    "$slice with a string position": {"$slice": ["$arr", "x", 1]},
    "$dateToString, a numeric format": {
        "$dateToString": {"date": Timestamp(1700000000, 1), "format": 1}
    },
}


def _expressions(client: Any) -> dict:
    db = _db(client, "rm213")
    db.c.insert_one({"_id": 0, "arr": [1, 2, 3], "o": {"x": 1, "y": 2}, "b": Binary(b"hi", 0)})
    out = {}
    for name, e in {**EXPRESSION_CASES, **MORE_EXPRESSION_CASES}.items():
        got = answer(lambda e=e: list(db.c.aggregate([{"$project": {"_id": 0, "v": e}}])))
        # An error is compared by code; a value by the document it produced
        # (an absent `v` and `v: null` differ, which is the `$getField` case).
        out[name] = got
    return out


@pytest.mark.backlog_fixed("RM-213")
def test_expression_operators(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 computes `$unsetField`, `$toUUID` and the binData
    `$convert` forms, gives each malformed shape its own error code, and
    leaves `v` MISSING for `$getField` on a non-object."""
    assert _expressions(rust_mongo) == _expressions(mongod)


# ----------------------------------------------- RM-214 window functions --

WINDOW_CASES = {
    "$covariancePop": {"$covariancePop": ["$b", "$a"], "window": {"documents": [-1, 1]}},
    "$covarianceSamp": {"$covarianceSamp": ["$b", "$a"]},
    "$topN": {
        "$topN": {"n": 1, "sortBy": {"b": -1}, "output": "$b"},
        "window": {"documents": [-2, 0]},
    },
    "$bottomN": {"$bottomN": {"n": 2, "sortBy": {"b": 1}, "output": "$b"}},
    "$linearFill sorted by a date": {"$linearFill": "$nb"},
    "$derivative with a unit": {
        "$derivative": {"input": "$b", "unit": "day"},
        "window": {"range": [-1, 0], "unit": "day"},
    },
    "$integral with a unit": {
        "$integral": {"input": "$b", "unit": "day"},
        "window": {"range": ["unbounded", "current"], "unit": "day"},
    },
}


def _windows(client: Any) -> dict:
    db = _db(client, "rm214")
    db.c.insert_many(
        [
            {
                "_id": i,
                "a": i % 4,
                "b": i * 7 % 11,
                "nb": i if i % 2 == 0 else None,
                "t": datetime.datetime(2024, 1, 1) + datetime.timedelta(hours=17 * i),
            }
            for i in range(8)
        ]
    )
    return {
        name: answer(
            lambda w=w: [
                d.get("w", "<missing>")
                for d in db.c.aggregate(
                    [
                        {"$setWindowFields": {"sortBy": {"t": 1}, "output": {"w": w}}},
                        {"$sort": {"_id": 1}},
                    ]
                )
            ]
        )
        for name, w in WINDOW_CASES.items()
    }


@pytest.mark.backlog_fixed("RM-214")
def test_window_functions(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 runs `$covariancePop` / `$covarianceSamp` and the
    `$topN` / `$bottomN` window accumulators, `$linearFill` over a date
    sort, and gives `$derivative` / `$integral` with a unit to the last
    digit."""
    assert _windows(rust_mongo) == _windows(mongod)


# -------------------------------------------------- RM-215 stages unknown --

STAGE_CASES = {
    "$planCacheStats": lambda c: (
        c.rm215.c.insert_one({"a": 1}),
        list(c.rm215.c.aggregate([{"$planCacheStats": {}}]))[:0],
    )[1],
    "$queryStats": lambda c: list(c.admin.aggregate([{"$queryStats": {}}]))[:0],
    "$listSampledQueries": lambda c: list(c.admin.aggregate([{"$listSampledQueries": {}}]))[:0],
}


def _stages(client: Any) -> dict:
    client.drop_database("rm215")
    return {name: answer(lambda f=f: f(client)) for name, f in STAGE_CASES.items()}


@pytest.mark.backlog("RM-215")
def test_diagnostic_stages(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 (one-member replica set) runs `$planCacheStats`,
    `$queryStats` and `$listSampledQueries`; the Rust server answers 40324
    (unknown stage) for each."""
    assert _stages(rust_mongo) == _stages(mongod_rs)


# ------------------------------------------------ RM-216 operation registry --


def _idle_cursors(client: Any) -> tuple:
    db = _db(client, "rm216")
    db.c.insert_many([{"_id": i} for i in range(10)])
    other = direct(client)
    try:
        cur = other.rm216.c.find(batch_size=2)
        next(cur)
        return answer(
            lambda: len(
                client.admin.command(
                    "aggregate",
                    1,
                    pipeline=[
                        {"$currentOp": {"idleCursors": True}},
                        {"$match": {"type": "idleCursor", "ns": "rm216.c"}},
                    ],
                    cursor={},
                )["cursor"]["firstBatch"]
            )
        )
    finally:
        other.close()


@pytest.mark.backlog("RM-216")
def test_current_op_lists_idle_cursors(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 lists another client's open cursor under
    `$currentOp: {idleCursors: true}` (1 entry)."""
    assert _idle_cursors(rust_mongo) == _idle_cursors(mongod)


def _running_op(client: Any) -> tuple:
    """Whether `currentOp` lists another client's waiting getMore, and how
    that getMore ends once `killOp` is sent for it."""
    db = _db(client, "rm216b")
    db.create_collection("cap", capped=True, size=100_000)
    db.cap.insert_one({"x": 1})
    other = direct(client)
    ended: dict = {}

    def tail() -> None:
        cur = other.rm216b.cap.find(cursor_type=pymongo.CursorType.TAILABLE_AWAIT)
        cur.max_await_time_ms(6000)
        next(cur)
        ended["how"] = answer(cur.next)[0:2]  # blocks in a getMore for up to 6 s

    t = threading.Thread(target=tail, daemon=True)
    t.start()
    time.sleep(1.5)
    try:
        ops = client.admin.command({"currentOp": 1, "ns": "rm216b.cap"})["inprog"]
        getmores = [o for o in ops if o.get("op") == "getmore"]
        for o in getmores:
            answer(lambda o=o: client.admin.command("killOp", op=o["opid"]))
        t.join(10)
        how = ended.get("how", ("exc", "no result"))
        return (len(getmores), how[1] if how[0] == "err" else how[0])
    finally:
        other.close()


@pytest.mark.backlog("RM-216")
def test_current_op_sees_a_running_operation(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `currentOp` lists another client's getMore waiting on
    a tailable awaitData cursor (1 entry, op "getmore"), and `killOp` on its
    opid ends it with 11601 Interrupted."""
    assert _running_op(rust_mongo) == _running_op(mongod)


# --------------------------------------------- RM-217 killing sessions --


def _kill_by_pattern(client: Any) -> tuple:
    db = _db(client, "rm217")
    db.c.insert_many([{"_id": i} for i in range(5)])
    f = direct(client)
    try:
        s1, s2 = f.start_session(), f.start_session()
        s1.start_transaction()
        s2.start_transaction()
        f.rm217.c.update_one({"_id": 1}, {"$set": {"x": 1}}, session=s1)
        f.rm217.c.update_one({"_id": 2}, {"$set": {"x": 1}}, session=s2)
        users = client.admin.command("connectionStatus")["authInfo"]["authenticatedUsers"]
        assert not users, "the comparison assumes no authentication"
        import hashlib

        lsid = dict(s1.session_id)
        lsid["uid"] = Binary(hashlib.sha256(b"").digest())
        client.admin.command("killAllSessionsByPattern", [{"lsid": lsid}])
        out = []
        for s in (s1, s2):
            got = answer(s.commit_transaction)
            out.append("committed" if got[0] == "ok" else got[1])
        return tuple(out)
    finally:
        f.close()


@pytest.mark.backlog_fixed("RM-217")
def test_kill_sessions_by_pattern_kills_only_the_match(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11: `killAllSessionsByPattern` naming s1's lsid aborts
    s1's transaction (251 on commit) and leaves s2's to commit."""
    assert _kill_by_pattern(rust_mongo) == _kill_by_pattern(mongod_rs)


def _kill_closes_cursor(client: Any, command: str) -> tuple:
    db = _db(client, "rm217b")
    db.c.insert_many([{"_id": i} for i in range(10)])
    f = direct(client)
    try:
        s = f.start_session()
        cur = f.rm217b.c.find(batch_size=2, session=s)
        next(cur)
        client.admin.command(command, [])
        return answer(lambda: len(list(cur)))
    finally:
        f.close()


@pytest.mark.backlog_fixed("RM-217")
def test_kill_all_sessions_closes_cursors(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11: after `killAllSessions: []` or
    `killAllSessionsByPattern: []` a session's open cursor is gone (43
    CursorNotFound on its next getMore)."""
    for command in ("killAllSessions", "killAllSessionsByPattern"):
        assert _kill_closes_cursor(rust_mongo, command) == _kill_closes_cursor(mongod_rs, command)


# --------------------------------------------------- RM-218 index limit --


def _index_limit(client: Any) -> tuple:
    db = _db(client, "rm218")
    db.c.insert_one({"_id": 0})
    got: tuple = ("ok", None)
    for i in range(64):
        got = answer(lambda i=i: db.c.create_index(f"f{i}"))
    single = got if got[0] != "ok" else ("ok", len(db.c.index_information()))
    # An index that already exists is no new one, even on a full collection.
    again = answer(lambda: db.c.create_index("f0"))
    # A request that fills the collection itself fails as a whole, and one
    # that creates the collection takes it back.
    specs = [{"key": {f"g{i}": 1}, "name": f"g{i}"} for i in range(64)]
    db.create_collection("d")
    batch = answer(lambda: db.command("createIndexes", "d", indexes=specs))
    new = answer(lambda: db.command("createIndexes", "e", indexes=specs))
    return (
        single,
        again,
        batch,
        len(db.d.index_information()),
        new,
        "e" in db.list_collection_names(),
    )


@pytest.mark.backlog_fixed("RM-218")
def test_index_count_limit(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 allows 64 indexes on a collection, `_id_` included:
    the 64th `createIndex` beside `_id_` fails with 67 CannotCreateIndex,
    and a request that would cross the limit builds none of its indexes."""
    assert _index_limit(rust_mongo) == _index_limit(mongod)


# ---------------------------------------------- RM-219 missing commands --

COMMANDS: dict[str, Callable[[Any], Any]] = {
    "applyOps": lambda c: c.admin.command(
        "applyOps", [{"op": "i", "ns": "rm219.a", "o": {"_id": uuid.uuid4().hex}}]
    ),
    "filemd5": lambda c: c.rm219.command("filemd5", 1, root="fs"),
    "getDiagnosticData": lambda c: c.admin.command("getDiagnosticData"),
    "setUserWriteBlockMode": lambda c: c.admin.command(
        {"setUserWriteBlockMode": 1, "global": False}
    ),
    "setQuerySettings": lambda c: c.admin.command(
        {
            "setQuerySettings": {"find": "a", "filter": {"x": 1}, "$db": "rm219"},
            "settings": {"reject": True},
        }
    ),
    "removeQuerySettings": lambda c: c.admin.command(
        {"removeQuerySettings": {"find": "a", "filter": {"x": 1}, "$db": "rm219"}}
    ),
    "setClusterParameter": lambda c: c.admin.command(
        {
            "setClusterParameter": {
                "changeStreamOptions": {"preAndPostImages": {"expireAfterSeconds": "off"}}
            }
        }
    ),
    "dropConnections": lambda c: c.admin.command({"dropConnections": 1, "hostAndPort": []}),
    "invalidateUserCache": lambda c: c.admin.command("invalidateUserCache"),
    "autoCompact": lambda c: c.admin.command({"autoCompact": False}),
    "rotateCertificates": lambda c: c.admin.command({"rotateCertificates": 1}),
    "releaseMemory": lambda c: c.rm219.command({"releaseMemory": []}),
    "dbCheck": lambda c: c.rm219.command({"dbCheck": 1}),
    "validateDBMetadata": lambda c: c.rm219.command(
        {"validateDBMetadata": 1, "apiParameters": {"version": "1", "strict": True}}
    ),
    "replSetGetRBID": lambda c: c.admin.command({"replSetGetRBID": 1}),
    "replSetFreeze": lambda c: c.admin.command({"replSetFreeze": 0}),
    "replSetMaintenance": lambda c: c.admin.command({"replSetMaintenance": False}),
}


def _not_found(client: Any) -> list:
    client.drop_database("rm219")
    return sorted(
        name for name, f in COMMANDS.items() if answer(lambda f=f: f(client)) == ("err", 59)
    )


@pytest.mark.backlog("RM-219")
def test_commands_mongod_runs_are_commands_here(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """None of these is 59 CommandNotFound on mongod 8.2.11 (one-member
    replica set); each answers something of its own, success or a
    command-specific error."""
    assert _not_found(rust_mongo) == _not_found(mongod_rs)


def _short_admin_replies(client: Any) -> dict:
    db = client["rm219b"]
    client.drop_database("rm219b")
    db.r.insert_many([{"_id": i} for i in range(10)])
    cursor = db.command("find", "r", batchSize=2)["cursor"]["id"]
    db.fs.chunks.create_index([("files_id", 1), ("n", 1)], unique=True)
    db.fs.chunks.insert_many(
        [
            {"files_id": 1, "n": 1, "data": Binary(b"def")},
            {"files_id": 1, "n": 0, "data": Binary(b"abc")},
            {"files_id": 3, "n": 1, "data": Binary(b"x")},
        ]
    )
    db.gx.chunks.insert_one({"files_id": "a", "n": 0, "data": Binary(b"q")})

    def reply(fn: Callable[[], Any]) -> Any:
        got = answer(fn)
        if got[0] != "ok":
            return got
        return {k: v for k, v in got[1].items() if k not in ("$clusterTime", "operationTime")}

    released = reply(lambda: db.command({"releaseMemory": [cursor]}))
    return {
        "invalidateUserCache": reply(lambda: client.admin.command("invalidateUserCache")),
        "replSetGetRBID": reply(lambda: client.admin.command("replSetGetRBID")),
        "replSetFreeze": reply(lambda: client.admin.command({"replSetFreeze": 0})),
        "replSetMaintenance": reply(lambda: client.admin.command({"replSetMaintenance": True})),
        "dropConnections": reply(
            lambda: client.admin.command({"dropConnections": 1, "hostAndPort": ["h:1"]})
        ),
        "dropConnections, no hosts": reply(lambda: client.admin.command("dropConnections")),
        "releaseMemory": (
            released["cursorsReleased"] == [cursor] if isinstance(released, dict) else released
        ),
        "releaseMemory, unknown": reply(lambda: db.command({"releaseMemory": [Int64(12345)]})),
        "releaseMemory, int": reply(lambda: db.command({"releaseMemory": [5]})),
        "getMore after releaseMemory": answer(
            lambda: len(db.command("getMore", cursor, collection="r")["cursor"]["nextBatch"])
        ),
        "filemd5": reply(lambda: db.command("filemd5", 1)),
        "filemd5, no file": reply(lambda: db.command("filemd5", 2)),
        "filemd5, out of order": reply(lambda: db.command("filemd5", 3)),
        "filemd5, no index": reply(lambda: db.command("filemd5", "a", root="gx")),
        "filemd5, bad root": reply(lambda: db.command("filemd5", 1, root=5)),
    }


@pytest.mark.backlog_fixed("RM-219")
def test_short_admin_commands_answer_as_mongod(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """The RM-219 commands with a short answer on one node, against mongod
    8.2.11: a primary refuses `replSetFreeze` / `replSetMaintenance` (95),
    `releaseMemory` lists the cursors it let go (which still answer
    `getMore`), and `filemd5` hashes the chunks in order through an index."""
    assert _short_admin_replies(rust_mongo) == _short_admin_replies(mongod_rs)


# ------------------------------------------------------ RM-220 fsync lock --


def _fsync_lock(client: Any) -> tuple:
    got = answer(lambda: client.admin.command("fsync", lock=True)["lockCount"])
    answer(lambda: client.admin.command("fsyncUnlock"))
    return got


@pytest.mark.backlog_fixed("RM-220")
def test_fsync_lock(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 takes `fsync: 1, lock: true` (lockCount 1), and
    `fsyncUnlock` releases it."""
    assert _fsync_lock(rust_mongo) == _fsync_lock(mongod)


# ------------------------------------------------ RM-221 OP_MSG validation --


def _raw_op_msg(client: Any, flags: int, trailer: bytes = b"") -> str:
    """Send one OP_MSG `ping` and report whether the server answered or
    closed the connection."""
    body = bson.encode({"ping": 1, "$db": "admin"})
    payload = struct.pack("<I", flags) + b"\x00" + body + trailer
    msg = struct.pack("<iiii", 16 + len(payload), 1, 0, 2013) + payload
    with socket.create_connection(client.address, timeout=5) as s:
        s.sendall(msg)
        try:
            head = s.recv(16)
        except (ConnectionResetError, TimeoutError):
            return "closed"
        return "replied" if head else "closed"


@pytest.mark.backlog_fixed("RM-221")
def test_op_msg_bad_checksum_is_refused(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 closes the connection on an OP_MSG whose CRC-32C
    checksum (flag bit 0) is wrong."""
    trailer = b"\xde\xad\xbe\xef"
    assert _raw_op_msg(rust_mongo, 1, trailer) == _raw_op_msg(mongod, 1, trailer)


@pytest.mark.backlog_fixed("RM-221")
def test_op_msg_unknown_required_flag_is_refused(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 closes the connection on an OP_MSG with an unknown
    REQUIRED flag bit (bits 0-15; bit 3 here)."""
    assert _raw_op_msg(rust_mongo, 1 << 3) == _raw_op_msg(mongod, 1 << 3)


# ---------------------------------------------------- RM-222 invalid UTF-8 --


def _insert_invalid_utf8(client: Any) -> Any:
    client.drop_database("rm222")
    element = b"\x02s\x00\x03\x00\x00\x00\xff\xfe\x00"  # string "s" = 0xff 0xfe
    doc = struct.pack("<i", 4 + len(element) + 1) + element + b"\x00"
    body = bson.encode({"insert": "u", "$db": "rm222"})
    section = b"documents\x00" + doc
    payload = (
        struct.pack("<I", 0)
        + b"\x00"
        + body
        + b"\x01"
        + struct.pack("<i", 4 + len(section))
        + section
    )
    with socket.create_connection(client.address, timeout=5) as s:
        s.sendall(struct.pack("<iiii", 16 + len(payload), 1, 0, 2013) + payload)
        head = s.recv(16)
        if not head:
            return "closed"
        length = struct.unpack("<i", head[:4])[0]
        data = b""
        while len(data) < length - 16:
            data += s.recv(length - 16 - len(data))
    reply = bson.decode(data[5:])
    return (reply.get("ok"), reply.get("n"), reply.get("code"))


@pytest.mark.backlog("RM-222")
def test_insert_string_with_invalid_utf8(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 stores a document whose string holds bytes that are
    not UTF-8 (ok 1, n 1)."""
    assert _insert_invalid_utf8(rust_mongo) == _insert_invalid_utf8(mongod)


# ------------------------------------------- RM-223 oversized update text --


def _grow_past_limit(client: Any) -> tuple:
    db = _db(client, "rm223")
    db.c.insert_one({"_id": 1, "a": "x" * (9 * 1024 * 1024)})
    try:
        db.c.update_one({"_id": 1}, {"$set": {"b": "x" * (9 * 1024 * 1024)}})
        return ("ok",)
    except PyMongoError as e:
        return (e.code, (e.details or {}).get("errmsg"))


@pytest.mark.backlog_fixed("RM-223")
def test_update_past_16mb_message(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 refuses an update that grows a document past 16 MB
    with 10334 (as here) and the message "Plan executor error during update :: caused by ::
    Resulting document after update is larger than 16777216"."""
    assert _grow_past_limit(rust_mongo) == _grow_past_limit(mongod)
