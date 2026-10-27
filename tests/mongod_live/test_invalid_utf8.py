"""Strings whose bytes are not UTF-8, on the Rust server and on mongod 8.2
(RM-222).

mongod keeps a BSON string as the bytes it was sent: it stores, returns,
compares, sorts, indexes and groups a string that is not valid UTF-8, and its
operators read such a string byte by byte. Each test here runs the same
operations on both servers and compares the answers exactly.

pymongo encodes a Python `str` as strict UTF-8, so the documents, filters and
stages are built here as raw BSON (`raw`), with every `str` written by
`surrogateescape`: `"a\\udcffb"` is the three bytes `61 ff 62`. Replies are
read back the same way.
"""

from __future__ import annotations

import struct
import time
from typing import Any

import bson
import pytest
from bson.codec_options import CodecOptions
from bson.raw_bson import RawBSONDocument
from pymongo.errors import BulkWriteError, OperationFailure

from tests.mongod_live.conftest import mongo_answer, same

SURROGATE = CodecOptions(unicode_decode_error_handler="surrogateescape")
RAW = CodecOptions(document_class=RawBSONDocument)

#: Strings that are not UTF-8, one per fault PCRE2 names, and a few with
#: valid text around the fault.
SHAPES = [
    "\udcff\udcfe",
    "a\udc80",
    "\udcc3",
    "\udce2",
    "\udcc3x",
    "\udce2\udc82x",
    "\udcc0\udcaf",
    "\udce0\udc80\udc80",
    "\udced\udca0\udc80",
    "\udcf0\udc80\udc80\udc80",
    "\udcf4\udc90\udc80\udc80",
    "\udcf8\udc88\udc80\udc80\udc80",
    "\udcfc\udc84\udc80\udc80\udc80\udc80",
    "\udc80abc",
    "ab\udce2\udc82",
    "é\udcffé",
    "  \udcff  ",
    "A\udcffz",
]


def _text(s: str) -> bytes:
    return s.encode("utf-8", "surrogateescape")


def _value(v: Any) -> tuple[int, bytes]:
    if isinstance(v, bool):
        return 0x08, b"\x01" if v else b"\x00"
    if isinstance(v, int):
        if -(2**31) <= v < 2**31:
            return 0x10, struct.pack("<i", v)
        return 0x12, struct.pack("<q", v)
    if isinstance(v, float):
        return 0x01, struct.pack("<d", v)
    if v is None:
        return 0x0A, b""
    if isinstance(v, str):
        b = _text(v)
        return 0x02, struct.pack("<i", len(b) + 1) + b + b"\x00"
    if isinstance(v, dict):
        return 0x03, _doc(v)
    if isinstance(v, (list, tuple)):
        return 0x04, _doc({str(i): x for i, x in enumerate(v)})
    encoded = bson.encode({"x": v})
    return encoded[4], encoded[7:-1]


def _doc(d: dict) -> bytes:
    body = b""
    for k, v in d.items():
        t, payload = _value(v)
        body += bytes([t]) + _text(k) + b"\x00" + payload
    return struct.pack("<i", len(body) + 5) + body + b"\x00"


def raw(d: dict) -> RawBSONDocument:
    """`d` as raw BSON, every `str` (keys included) by `surrogateescape`."""
    return RawBSONDocument(_doc(d))


def q(flt: dict) -> dict:
    """A filter for pymongo's `find`, which reads a raw filter's top-level
    values (and so decodes them strictly): the raw document under `$and`."""
    return {"$and": [raw(flt)]}


def _db(client: Any, name: str = "rm222") -> Any:
    return client.get_database(name, codec_options=SURROGATE)


def insert(coll: Any, docs: list[dict]) -> Any:
    """Insert raw documents with the `insert` command; its `writeErrors`, or
    None. (pymongo's own insert reads the documents back, strictly.)"""
    reply = coll.database.command(
        "insert", coll.name, documents=[raw(d) for d in docs], codec_options=SURROGATE
    )
    return reply.get("writeErrors")


def find(coll: Any, flt: dict, projection: dict | None = None) -> list:
    """The `find` command over raw filter and projection, first batch."""
    extra = {} if projection is None else {"projection": raw(projection)}
    reply = coll.database.command(
        "find", coll.name, filter=raw(flt), codec_options=SURROGATE, **extra
    )
    return reply["cursor"]["firstBatch"]


def _fresh(client: Any, docs: list[dict], name: str = "c") -> Any:
    db = _db(client)
    db.drop_collection(name)
    db[name].insert_many([raw(d) for d in docs])
    return db[name]


DOCS = [{"_id": i, "s": s} for i, s in enumerate(SHAPES)] + [
    {"_id": 100, "s": "a"},
    {"_id": 101, "s": "b"},
    {"_id": 102, "s": "é"},
    {"_id": 103, "s": "\U0010ffff"},
    {"_id": 104, "s": 5},
    {"_id": 105, "s": {"x": 1}},
    {"_id": 106, "s": ["\udcc3", "q"]},
    {"_id": 107, "n": {"t": "\udcff"}, "l": ["\udcfe", "ok"]},
]


def test_documents_come_back_byte_for_byte(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 stores the strings and returns their bytes."""
    stored = []
    for client in (rust_mongo, mongod):
        _fresh(client, DOCS)
        coll = client.get_database("rm222", codec_options=RAW).c
        stored.append([d.raw for d in coll.find({}, sort=[("_id", 1)])])
    assert stored[0] == stored[1]
    assert stored[0] == [raw(d).raw for d in sorted(DOCS, key=lambda d: d["_id"])]


def test_an_id_that_is_not_utf8(rust_mongo: Any, mongod: Any) -> None:
    def run(client: Any) -> tuple:
        db = _db(client)
        db.drop_collection("ids")
        first = insert(db.ids, [{"_id": "\udcff\udcfe", "v": 1}])
        found = find(db.ids, {"_id": "\udcff\udcfe"})
        dup = insert(db.ids, [{"_id": "\udcff\udcfe"}])
        return first, found, dup

    same(run(rust_mongo), run(mongod))


FILTERS = [
    {"s": "\udcff\udcfe"},
    {"s": {"$in": ["a\udc80", "zz", "\udcc3"]}},
    {"s": {"$gt": "z"}},
    {"s": {"$lt": "\udcff\udcfe"}},
    {"s": {"$gte": "a\udc80", "$lte": "\udcff\udcfe"}},
    {"s": {"$ne": "\udcc3"}},
    {"s": {"$type": "string"}},
    {"s": {"$type": "object"}},
    {"s": {"$all": ["\udcc3"]}},
    {"n.t": "\udcff"},
    {"l": "\udcfe"},
    {"$expr": {"$gt": ["$s", "zz"]}},
]


@pytest.mark.parametrize("hint", [None, "s_1", "s_-1"])
@pytest.mark.parametrize("flt", FILTERS, ids=[repr(f) for f in FILTERS])
def test_queries_match_by_bytes(rust_mongo: Any, mongod: Any, flt: dict, hint: Any) -> None:
    """Equality, `$in` and ranges compare the bytes; `$type` calls the value a
    string; an index answers what a collection scan does."""

    def run(client: Any) -> Any:
        coll = _fresh(client, DOCS)
        coll.database.command(
            "createIndexes",
            "c",
            indexes=[{"key": {"s": 1}, "name": "s_1"}, {"key": {"s": -1}, "name": "s_-1"}],
        )
        kwargs = {"hint": hint} if hint and "n.t" not in flt and "l" not in flt else {}
        return mongo_answer(
            lambda: sorted(d["_id"] for d in coll.find(q(flt), {"_id": 1}, **kwargs))
        )

    same(run(rust_mongo), run(mongod))


@pytest.mark.parametrize("direction", [1, -1])
def test_sort_orders_by_bytes(rust_mongo: Any, mongod: Any, direction: int) -> None:
    def run(client: Any) -> list:
        coll = _fresh(client, [d for d in DOCS if d["_id"] != 106])
        plain = [d["_id"] for d in coll.find({}, {"_id": 1}, sort=[("s", direction), ("_id", 1)])]
        coll.create_index([("s", 1)])
        hinted = [
            d["_id"]
            for d in coll.find({}, {"_id": 1}, sort=[("s", direction), ("_id", 1)], hint="s_1")
        ]
        return [plain, hinted]

    same(run(rust_mongo), run(mongod))


def test_a_duplicate_key_is_reported_in_hex(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: `dup key: { s: "0xfffe" }`, `keyValue: {s: "fffe"}`,
    `hexEncoded: [true]`."""

    def run(client: Any) -> Any:
        coll = _fresh(client, [{"_id": 1, "s": "\udcff\udcfe", "t": "x"}])
        coll.create_index([("s", 1)], unique=True)
        coll.create_index([("t", 1), ("s", 1)], unique=True, name="ts")
        out = []
        for doc in [{"_id": 2, "s": "\udcff\udcfe"}, {"_id": 3, "s": "\udcff\udcfe", "t": "x"}]:
            out.append(insert(coll, [doc]))
        return out

    same(run(rust_mongo), run(mongod))


EXPRESSIONS = [
    ("strLenCP", {"$strLenCP": "$s"}),
    ("strLenBytes", {"$strLenBytes": "$s"}),
    ("toUpper", {"$toUpper": "$s"}),
    ("toLower", {"$toLower": "$s"}),
    ("indexOfCP", {"$indexOfCP": ["$s", "b"]}),
    ("indexOfCP x", {"$indexOfCP": ["$s", "x"]}),
    ("indexOfCP from 1", {"$indexOfCP": ["$s", "c", 1]}),
    ("indexOfBytes", {"$indexOfBytes": ["$s", "b"]}),
    ("substrCP", {"$substrCP": ["$s", 0, 1]}),
    ("substrCP long", {"$substrCP": ["$s", 0, 5]}),
    ("substrBytes", {"$substrBytes": ["$s", 0, 1]}),
    ("substrBytes from 1", {"$substrBytes": ["$s", 1, 2]}),
    ("trim", {"$trim": {"input": "$s"}}),
    ("trim chars", {"$trim": {"input": "$s", "chars": "\udcff "}}),
    ("ltrim bad chars", {"$ltrim": {"input": "abc", "chars": "\udc80"}}),
    ("ltrim short chars", {"$ltrim": {"input": "abc", "chars": "a\udce2"}}),
    ("split", {"$split": ["$s", "\x80"]}),
    ("split by a byte", {"$split": ["$s", "\udc80"]}),
    ("regexMatch", {"$regexMatch": {"input": "$s", "regex": "a"}}),
    ("regexMatch pattern", {"$regexMatch": {"input": "abc", "regex": "$s"}}),
    ("regexFind null", {"$regexFind": {"input": "$s", "regex": None}}),
    ("strcasecmp", {"$strcasecmp": ["$s", "a\udcffZ"]}),
    ("cmp", {"$cmp": ["$s", "é"]}),
    ("eq", {"$eq": ["$s", "\udcff\udcfe"]}),
    ("concat", {"$concat": ["<", "$s", ">"]}),
    ("replaceAll", {"$replaceAll": {"input": "$s", "find": "b", "replacement": "c"}}),
    ("replaceOne", {"$replaceOne": {"input": "$s", "find": "\udcff", "replacement": "c"}}),
    ("toHashedIndexKey", {"$toHashedIndexKey": "$s"}),
    ("max", {"$max": ["$s", "z"]}),
    ("type", {"$type": "$s"}),
    ("toString", {"$toString": "$s"}),
    ("toBool", {"$toBool": "$s"}),
    ("binarySize", {"$binarySize": "$s"}),
    ("convert with onError", {"$convert": {"input": "$s", "to": "int", "onError": "E"}}),
    ("literal", {"$literal": "\udcff\udcfe"}),
    ("setUnion", {"$setUnion": [["$s", "\udcff\udcfe"], ["$s"]]}),
]


def _per_shape(client: Any, stage: str, expr: dict) -> list:
    # Matched on a field other than `_id`: an `_id` equality is mongod's
    # express path, which keeps a following `$group` out of its slot-based
    # engine.
    coll = _fresh(client, [{"_id": i, "i": i, "s": s} for i, s in enumerate(SHAPES)])
    out = []
    for i in range(len(SHAPES)):
        if stage == "$group":
            spec = {"$group": {"_id": None, "r": {"$first": expr}}}
        else:
            spec = {"$project": {"_id": 0, "r": expr}}
        pipeline = [raw({"$match": {"i": i}}), raw(spec)]
        out.append(mongo_answer(lambda p=pipeline: [d.get("r") for d in coll.aggregate(p)]))
    return out


@pytest.mark.parametrize("expr", [e for _, e in EXPRESSIONS], ids=[n for n, _ in EXPRESSIONS])
def test_string_operators(rust_mongo: Any, mongod: Any, expr: dict) -> None:
    """Each operator over each shape, as mongod's classic engine answers:
    byte by byte, code points counted at every byte that is not a
    continuation byte, PCRE2's own verdict on what is not UTF-8."""
    same(_per_shape(rust_mongo, "$project", expr), _per_shape(mongod, "$project", expr))


GROUP_EXPRESSIONS = [
    ("toUpper", {"$toUpper": "$s"}),
    ("strLenCP", {"$strLenCP": "$s"}),
    ("strLenBytes", {"$strLenBytes": "$s"}),
    ("indexOfCP", {"$indexOfCP": ["$s", "b"]}),
    ("indexOfBytes", {"$indexOfBytes": ["$s", "b"]}),
    ("substrCP", {"$substrCP": ["$s", 0, 1]}),
    ("substrBytes", {"$substrBytes": ["$s", 0, 1]}),
    ("concat", {"$concat": ["$s", "!"]}),
    ("trim", {"$trim": {"input": "$s", "chars": "a"}}),
    ("regexMatch", {"$regexMatch": {"input": "$s", "regex": "a"}}),
    ("split", {"$split": ["$s", "b"]}),
    ("strcasecmp", {"$strcasecmp": ["$s", "A"]}),
    ("replaceOne", {"$replaceOne": {"input": "$s", "find": "b", "replacement": "c"}}),
    ("toString", {"$toString": "$s"}),
]


@pytest.mark.parametrize(
    "expr", [e for _, e in GROUP_EXPRESSIONS], ids=[n for n, _ in GROUP_EXPRESSIONS]
)
def test_string_operators_in_group(rust_mongo: Any, mongod: Any, expr: dict) -> None:
    """The same operators inside `$group`, which mongod runs in its slot-based
    engine, with that engine's codes (5075307, 5073403, 5155604/5)."""
    same(_per_shape(rust_mongo, "$group", expr), _per_shape(mongod, "$group", expr))


@pytest.mark.parametrize("pattern", ["a", ".", *SHAPES])
def test_regex_queries(rust_mongo: Any, mongod: Any, pattern: str) -> None:
    """A subject that is not UTF-8 never matches; a pattern that is not
    UTF-8 is 51091 with PCRE2's verdict."""

    def run(client: Any) -> Any:
        coll = _fresh(
            client, [{"_id": i, "s": s} for i, s in enumerate(SHAPES)] + [{"_id": 99, "s": "abc"}]
        )
        return mongo_answer(
            lambda: sorted(d["_id"] for d in coll.find(q({"s": {"$regex": pattern}}), {"_id": 1}))
        )

    same(run(rust_mongo), run(mongod))


def test_grouping_and_distinct(rust_mongo: Any, mongod: Any) -> None:
    def run(client: Any) -> list:
        docs = [{"_id": i, "s": s} for i, s in enumerate(SHAPES[:4])]
        docs += [{"_id": 10, "s": "\udcff\udcfe"}, {"_id": 11, "s": "zz"}]
        coll = _fresh(client, docs)
        grouped = list(
            coll.aggregate([{"$group": {"_id": "$s", "n": {"$sum": 1}}}, {"$sort": {"_id": 1}}])
        )
        acc = list(
            coll.aggregate(
                [
                    {"$sort": {"_id": 1}},
                    {
                        "$group": {
                            "_id": None,
                            "mx": {"$max": "$s"},
                            "mn": {"$min": "$s"},
                            "f": {"$first": "$s"},
                        }
                    },
                ]
            )
        )
        distinct = sorted(coll.distinct("s"), key=lambda s: _text(s))
        counted = coll.count_documents(q({"s": "\udcff\udcfe"}))
        return [grouped, acc, distinct, counted]

    same(run(rust_mongo), run(mongod))


def test_json_schema_counts_code_points_as_mongod_does(rust_mongo: Any, mongod: Any) -> None:
    def run(client: Any) -> list:
        db = _db(client)
        out = []
        for schema in [
            {"properties": {"s": {"type": "string", "minLength": 2, "maxLength": 2}}},
            {"properties": {"s": {"pattern": "^a"}}},
        ]:
            db.drop_collection("v")
            db.create_collection("v", validator={"$jsonSchema": schema})
            for i, s in enumerate(["\udcff\udcfe", "\udcff", "\udcff\udcff\udcff", "a\udcffb"]):
                try:
                    db.v.insert_many([raw({"_id": i, "s": s})])
                    out.append("ok")
                except BulkWriteError as e:
                    err = e.details["writeErrors"][0]
                    out.append([err["code"], err.get("errInfo")])
        return out

    same(run(rust_mongo), run(mongod))


def test_updates_keep_the_bytes(rust_mongo: Any, mongod: Any) -> None:
    def run(client: Any) -> list:
        coll = _fresh(client, [{"_id": 1, "s": "x", "a": []}])
        out = []
        for u in [
            {"$set": {"s": "\udcff\udcfe"}, "$push": {"a": "a\udcffb"}},
            {"$pull": {"a": "a\udcffb"}},
            {"$addToSet": {"a": "\udcff\udcfe"}},
            {"$addToSet": {"a": "\udcff\udcfe"}},
            {"$rename": {"s": "r\udcff"}},
        ]:
            r = coll.update_one({"_id": 1}, raw(u))
            out.append([r.matched_count, r.modified_count])
        coll.update_one(
            {"_id": 1},
            [raw({"$set": {"p": {"$concat": [{"$arrayElemAt": ["$a", 0]}, "!"]}}})],
        )
        out.append(mongo_answer(lambda: coll.find_one({"_id": 1})))
        out.append(
            mongo_answer(
                lambda: coll.find_one_and_update(
                    raw({"r\udcff": "\udcff\udcfe"}),
                    raw({"$set": {"u": "\udcc3"}}),
                    return_document=True,
                )
            )
        )
        coll.update_one(raw({"k": "\udcff\udcfe"}), {"$set": {"z": 1}}, upsert=True)
        out.append(coll.find_one(q({"k": "\udcff\udcfe"}), {"_id": 0}))
        return out

    same(run(rust_mongo), run(mongod))


def test_field_names_that_are_not_utf8(rust_mongo: Any, mongod: Any) -> None:
    """mongod keeps a name's bytes too, and paths reach the field."""

    def run(client: Any) -> list:
        coll = _fresh(client, [{"_id": 1, "k\udcff": 1, "n": {"x\udcc3": 2}}])
        out = [
            find(coll, {}, {"k\udcff": 1}),
            list(coll.find(q({"n.x\udcc3": 2}))),
            find(coll, {}, {"n.x\udcc3": 1}),
        ]
        for stage in [
            {"$project": {"v": {"$getField": {"field": "k\udcff", "input": "$$ROOT"}}}},
            {"$project": {"v": "$n.x\udcc3"}},
            {"$replaceWith": {"$setField": {"field": "z\udcfe", "input": "$$ROOT", "value": 5}}},
            {"$project": {"o": {"$objectToArray": "$$ROOT"}}},
            {"$project": {"o": {"$arrayToObject": [[["q\udcff", 1]]]}}},
            {"$addFields": {"w\udcfe": "$k\udcff"}},
        ]:
            out.append(mongo_answer(lambda s=stage: list(coll.aggregate([raw(s)]))))
        coll.update_one({"_id": 1}, raw({"$inc": {"k\udcff": 1}}))
        coll.database.command(
            "createIndexes", "c", indexes=[raw({"key": {"k\udcff": 1}, "name": "ki"})]
        )
        out.append(list(coll.find(q({"k\udcff": 2}), hint="ki")))
        return out

    same(run(rust_mongo), run(mongod))


def test_a_nul_in_a_name_from_a_string_is_refused(rust_mongo: Any, mongod: Any) -> None:
    """A name held escaped (NUL + hex) must not be forgeable from a string:
    mongod refuses a NUL in `$setField` (9534700), a field path (16411,
    16419 for `$unwind`'s), `$count` (40159), `$unset` (9527900), the
    stages that name an output field and `$rename` (9867600)."""

    def run(client: Any) -> list:
        coll = _fresh(client, [{"_id": 1, "a": 1}])
        out = []
        for stage in [
            {"$replaceWith": {"$setField": {"field": "a\x00ff", "input": "$$ROOT", "value": 5}}},
            {"$project": {"v": {"$getField": {"field": "a\x00ff", "input": "$$ROOT"}}}},
            {"$count": "a\x00ff"},
            {"$lookup": {"from": "c", "localField": "_id", "foreignField": "_id", "as": "a\x00ff"}},
            {"$unwind": {"path": "$a", "includeArrayIndex": "a\x00ff"}},
            {"$unwind": "$a\x00ff"},
            {"$group": {"_id": "$a\x00ff"}},
            {"$sortByCount": "$a\x00ff"},
            {"$densify": {"field": "a\x00ff", "range": {"step": 1, "bounds": "full"}}},
            {"$fill": {"output": {"a": {"value": 1}}, "partitionByFields": ["a\x00ff"]}},
            {"$unset": "a\x00ff"},
        ]:
            out.append(mongo_answer(lambda s=stage: list(coll.aggregate([s]))))
        try:
            coll.update_one({"_id": 1}, {"$rename": {"a": "b\x00ff"}})
            out.append("ok")
        except OperationFailure as e:
            out.append([e.code, (e.details or {}).get("errmsg")])
        out.append(list(coll.find({})))
        return out

    same(run(rust_mongo), run(mongod))


def test_out_merge_and_lookup(rust_mongo: Any, mongod: Any) -> None:
    def run(client: Any) -> list:
        coll = _fresh(client, [{"_id": 1, "s": "\udcff\udcfe"}])
        _fresh(client, [{"_id": 10, "t": "\udcff\udcfe"}, {"_id": 11, "t": "a\udcffb"}], name="d")
        db = coll.database
        db.drop_collection("o")
        list(coll.aggregate([{"$out": "o"}]))
        list(coll.aggregate([{"$merge": {"into": "o2", "on": "_id"}}]))
        joined = list(
            coll.aggregate(
                [{"$lookup": {"from": "d", "localField": "s", "foreignField": "t", "as": "j"}}]
            )
        )
        return [list(db.o.find()), list(db.o2.find()), joined]

    same(run(rust_mongo), run(mongod))


def test_explain_bounds_show_the_bytes(rust_mongo: Any, mongod: Any) -> None:
    def run(client: Any) -> list:
        coll = _fresh(client, [{"_id": 1, "s": "a"}])
        coll.create_index([("s", 1)])
        out = []
        for flt in [
            {"s": "\udcff\udcfe"},
            {"s": {"$lt": "\udcff\udcfe"}},
            {"s": {"$gte": "a\udcffb", "$lte": "\udcff\udcfe"}},
            {"s": {"$in": ["a\udcffb", "\udcc3"]}},
            {"s": {"$ne": "a\udcffb"}},
        ]:
            plan = coll.database.command(
                "explain",
                {"find": "c", "filter": raw(flt), "hint": "s_1"},
                verbosity="queryPlanner",
                codec_options=SURROGATE,
            )["queryPlanner"]["winningPlan"]
            while "inputStage" in plan:
                plan = plan["inputStage"]
            out.append(plan.get("indexBounds"))
        return out

    same(run(rust_mongo), run(mongod))


def _raw_command(client: Any, db: str, cmd: dict) -> dict:
    """One command over raw OP_MSG, every `str` (the database and collection
    names included) by `surrogateescape`; the reply without the fields that
    differ by run."""
    import socket

    body = _doc({**cmd, "$db": db})
    payload = struct.pack("<I", 0) + b"\x00" + body
    with socket.create_connection(client.address, timeout=30) as s:
        s.sendall(struct.pack("<iiii", 16 + len(payload), 1, 0, 2013) + payload)
        head = b""
        while len(head) < 16:
            chunk = s.recv(16 - len(head))
            assert chunk, "the server closed the connection"
            head += chunk
        length = struct.unpack("<i", head[:4])[0]
        data = b""
        while len(data) < length - 16:
            data += s.recv(length - 16 - len(data))
    reply = bson.decode(data[5:], codec_options=SURROGATE)
    for k in ("$clusterTime", "operationTime", "electionId", "opTime"):
        reply.pop(k, None)
    if isinstance(reply.get("cursor"), dict):
        reply["cursor"].pop("id", None)
    return reply


C = "c\udcff"
D = "d\udcfe"


def test_names_that_are_not_utf8(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 creates, uses, lists, renames and drops a collection or
    database whose name is not UTF-8; replies, error messages and the oplog
    carry the name's bytes; a NUL in a name is still refused."""

    def run(client: Any) -> list:
        db = "rm222names"
        _raw_command(client, db, {"dropDatabase": 1})
        _raw_command(client, D, {"dropDatabase": 1})
        out: list = []
        say = out.append
        say(_raw_command(client, db, {"create": C}))
        say(_raw_command(client, db, {"insert": C, "documents": [{"_id": 1, "s": "x"}]}))
        say(_raw_command(client, db, {"find": C, "filter": {}}))
        say(_raw_command(client, db, {"count": C}))
        listed = _raw_command(client, db, {"listCollections": 1, "nameOnly": True})
        say(sorted(_text(x["name"]) for x in listed["cursor"]["firstBatch"]))
        filtered = _raw_command(client, db, {"listCollections": 1, "filter": {"name": C}})
        say([x["name"] for x in filtered["cursor"]["firstBatch"]])
        say(_raw_command(client, db, {"listIndexes": C}))
        say(
            _raw_command(
                client,
                db,
                {"createIndexes": C, "indexes": [{"key": {"s": 1}, "name": "s_1", "unique": True}]},
            )
        )
        say(_raw_command(client, db, {"insert": C, "documents": [{"_id": 2, "s": "x"}]}))
        stats = _raw_command(client, db, {"collStats": C})
        say([stats.get("ns"), stats.get("count")])
        say(
            _raw_command(
                client,
                db,
                {
                    "aggregate": C,
                    "pipeline": [{"$project": {"r": {"$add": ["$s", 1]}}}],
                    "cursor": {},
                },
            )
        )
        plan = _raw_command(
            client, db, {"explain": {"find": C, "filter": {}}, "verbosity": "queryPlanner"}
        )
        say(plan["queryPlanner"]["namespace"])
        say(
            _raw_command(
                client,
                db,
                {
                    "aggregate": C,
                    "pipeline": [
                        {
                            "$lookup": {
                                "from": C,
                                "localField": "_id",
                                "foreignField": "_id",
                                "as": "j",
                            }
                        }
                    ],
                    "cursor": {},
                },
            )
        )
        say(_raw_command(client, "admin", {"renameCollection": f"{db}.{C}", "to": f"{db}.r\udcfd"}))
        listed = _raw_command(client, db, {"listCollections": 1, "nameOnly": True})
        say(sorted(_text(x["name"]) for x in listed["cursor"]["firstBatch"]))
        entries = _raw_command(
            client,
            "local",
            {"find": "oplog.rs", "filter": {"op": "c", "o.renameCollection": f"{db}.{C}"}},
        )
        say([[e["ns"], e["o"]] for e in entries["cursor"]["firstBatch"]])
        say(_raw_command(client, db, {"insert": "a\x00b", "documents": [{}]}))
        say(_raw_command(client, db, {"create": "x$\udcff"}))
        say(_raw_command(client, D, {"insert": "t", "documents": [{"_id": 1}]}))
        say(_raw_command(client, D, {"find": "t", "filter": {}}))
        dbs = _raw_command(client, "admin", {"listDatabases": 1, "nameOnly": True})
        say(_text(D) in [_text(x["name"]) for x in dbs["databases"]])
        say(_raw_command(client, D, {"insert": "t", "documents": [{"_id": 1}]}))
        say(_raw_command(client, db, {"drop": "r\udcfd"}))
        say(_raw_command(client, D, {"dropDatabase": 1}))
        dbs = _raw_command(client, "admin", {"listDatabases": 1, "nameOnly": True})
        say(_text(D) in [_text(x["name"]) for x in dbs["databases"]])
        return out

    same(run(rust_mongo), run(mongod_rs))


def test_change_streams_on_names_that_are_not_utf8(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 refuses a stream on such a collection or database with
    PCRE2's verdict on its namespace regex (51091), and a database stream
    shows no event for such a collection."""

    def run(client: Any) -> list:
        db = "rm222streams"
        _raw_command(client, db, {"dropDatabase": 1})
        _raw_command(client, db, {"create": "plain"})
        out = []
        for coll in ["r\udcfd", "q\udcc3"]:
            r = _raw_command(
                client, db, {"aggregate": coll, "pipeline": [{"$changeStream": {}}], "cursor": {}}
            )
            out.append([r.get("code"), r.get("errmsg")])
        r = _raw_command(
            client, D, {"aggregate": 1, "pipeline": [{"$changeStream": {}}], "cursor": {}}
        )
        out.append([r.get("code"), r.get("errmsg")])
        coll = client.get_database(db, codec_options=SURROGATE).plain
        with coll.database.watch() as stream:
            _raw_command(client, db, {"insert": "r\udcfd", "documents": [{"_id": 9}]})
            coll.insert_one({"_id": 10})
            events = []
            deadline = time.monotonic() + 10
            while len(events) < 1 and time.monotonic() < deadline:
                ev = stream.try_next()
                if ev is not None:
                    events.append([ev["operationType"], ev["ns"], ev["documentKey"]])
        out.append(events)
        return out

    same(run(rust_mongo), run(mongod_rs))


@pytest.mark.parametrize(
    "expr",
    [
        {"$toInt": "$s"},
        {"$toDouble": "$s"},
        {"$toObjectId": "$s"},
        {"$toDate": "$s"},
        {"$dateFromString": {"dateString": "$s"}},
    ],
    ids=["toInt", "toDouble", "toObjectId", "toDate", "dateFromString"],
)
def test_error_messages_quote_the_bytes(rust_mongo: Any, mongod: Any, expr: dict) -> None:
    """mongod 8.2.11 quotes the string's own bytes in the message, and counts
    an ObjectId's length in bytes."""
    same(_per_shape(rust_mongo, "$project", expr), _per_shape(mongod, "$project", expr))


OTHER_TEXT = {
    "javascript": b"\x0dv\x00\x02\x00\x00\x00\xff\x00",
    "symbol": b"\x0ev\x00\x02\x00\x00\x00\xff\x00",
    "regex": b"\x0bv\x00\xff\x00i\x00",
    "dbPointer": b"\x0cv\x00\x04\x00\x00\x00d.\xff\x00" + b"\x00" * 12,
    "javascriptWithScope": b"\x0fv\x00\x0f\x00\x00\x00\x02\x00\x00\x00\xff\x00\x05\x00\x00\x00\x00",
}


@pytest.mark.parametrize("element", list(OTHER_TEXT.values()), ids=list(OTHER_TEXT))
def test_the_other_text_types_keep_their_bytes(
    rust_mongo: Any, mongod: Any, element: bytes
) -> None:
    """mongod 8.2.11 stores and returns JavaScript, a symbol, a regex, a
    DBPointer and JavaScript with a scope whose text is not UTF-8."""

    def run(client: Any) -> list:
        db = _db(client)
        db.drop_collection("t")
        body = b"\x10_id\x00" + struct.pack("<i", 1) + element
        document = RawBSONDocument(struct.pack("<i", len(body) + 5) + body + b"\x00")
        db.command("insert", "t", documents=[document], codec_options=SURROGATE)
        stored = client.get_database("rm222", codec_options=RAW).t.find_one({})
        typed = list(db.t.aggregate([{"$project": {"_id": 0, "t": {"$type": "$v"}}}]))
        return [stored.raw, typed]

    same(run(rust_mongo), run(mongod))


def test_change_streams_carry_the_bytes(rust_mongo: Any, mongod_rs: Any) -> None:
    def run(client: Any) -> list:
        coll = _fresh(client, [{"_id": 0}])
        with coll.watch(full_document="updateLookup") as stream:
            coll.insert_many([raw({"_id": 1, "s": "\udcff\udcfe"})])
            coll.update_one({"_id": 1}, raw({"$set": {"t": "a\udcffb"}}))
            coll.delete_one(raw({"s": "\udcff\udcfe"}))
            events = []
            deadline = time.monotonic() + 10
            while len(events) < 3 and time.monotonic() < deadline:
                ev = stream.try_next()
                if ev is not None:
                    events.append(
                        [
                            ev["operationType"],
                            ev.get("fullDocument"),
                            ev.get("updateDescription"),
                            ev["documentKey"],
                        ]
                    )
        return events

    same(run(rust_mongo), run(mongod_rs))
