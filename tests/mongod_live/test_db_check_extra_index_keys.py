"""`dbCheck`'s `extraIndexKeysCheck` against mongod 8.2 (RM-416).

The batch entries a one-member replica set's primary writes for an index:
the MD5 of each batch's KeyString-encoded keys, where batches start and
end, and the last key checked, over every BSON type an index key can hold
(Decimal128 aside: see the backlog), compound, descending, multikey,
nested, sparse, partial and collated indexes, and `start` / `end` /
`maxCount`; and the health-log warning for a missing index.
"""

from __future__ import annotations

import datetime
import time
from typing import Any

import pytest
from bson import Binary, Code, CodecOptions, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp

from tests.mongod_live.conftest import mongod_rs, rust_mongo, same  # noqa: F401

pytestmark = pytest.mark.timeout(120)

_TYPES = [
    True,
    False,
    datetime.datetime(2020, 1, 1),
    datetime.datetime(1960, 1, 1),
    ObjectId("0123456789ab0123456789ab"),
    {"a": 1, "b": "s"},
    [],
    Binary(b"ab", 0),
    Binary(b"z" * 300, 5),
    Regex("a.b", "i"),
    Code("f"),
    Code("g", {"q": 1}),
    Timestamp(5, 6),
    MinKey(),
    MaxKey(),
    "a\x00b",
    "",
    None,
    1.5,
    -2.25,
    0.1,
    -0.1,
    1e300,
    -1e300,
    float("nan"),
    float("inf"),
    float("-inf"),
    -0.0,
    1e-300,
    -1e-310,
    Int64(-(2**63)),
    Int64(2**63 - 1),
    -(2**31),
    123456.789,
    -98765.4321,
    2.0**60,
    255,
    256,
    -65536,
]

# (name, documents, key pattern, index options, command options)
_CASES: list[tuple[str, list[dict], Any, dict, dict]] = [
    ("a", [{"_id": i, "x": i % 3} for i in range(5)], "x", {}, {"maxDocsPerBatch": 2}),
    (
        "types",
        [{"_id": i, "x": v} for i, v in enumerate(_TYPES)],
        "x",
        {},
        {"maxDocsPerBatch": 3},
    ),
    (
        "typesdesc",
        [{"_id": i, "y": 0, "x": v} for i, v in enumerate(_TYPES)],
        [("y", 1), ("x", -1)],
        {},
        {"maxDocsPerBatch": 4},
    ),
    ("desc", [{"_id": 1, "x": 1}], [("x", -1)], {}, {}),
    (
        "cmpd",
        [{"_id": i, "x": i % 2, "y": i} for i in range(5)],
        [("x", 1), ("y", -1)],
        {},
        {"maxDocsPerBatch": 2},
    ),
    ("dup", [{"_id": i, "x": 1} for i in range(5)], "x", {}, {"maxDocsPerBatch": 2}),
    ("arr", [{"_id": 1, "x": [3, 1, [2], 1]}, {"_id": 2, "x": []}], "x", {}, {}),
    ("nest", [{"_id": 1, "a": {"b": 2}}, {"_id": 2, "a": [{"b": 1}, {"c": 1}]}], "a.b", {}, {}),
    ("sparse", [{"_id": 1, "x": 1}, {"_id": 2}], "x", {"sparse": True}, {}),
    (
        "partial",
        [{"_id": 1, "x": 1}, {"_id": 2, "x": 5}],
        "x",
        {"partialFilterExpression": {"x": {"$gt": 2}}},
        {},
    ),
    (
        "coll",
        [{"_id": 1, "x": "a"}, {"_id": 2, "x": "B"}],
        "x",
        {"collation": {"locale": "en"}},
        {},
    ),
    ("idx_id", [{"_id": 1}, {"_id": "s"}], None, {}, {"secondaryIndex": "_id_"}),
    ("noidx", [{"_id": 1}], None, {}, {"secondaryIndex": "nope"}),
    ("empty", [], "x", {}, {}),
    ("mc", [{"_id": i, "x": i} for i in range(5)], "x", {}, {"maxDocsPerBatch": 2, "maxCount": 3}),
    (
        "range",
        [{"_id": i, "x": i} for i in range(5)],
        "x",
        {},
        {"start": {"x": 1}, "end": {"x": 3}},
    ),
]


def _run(client: Any) -> list:
    db = client["rm416live"]
    client.drop_database("rm416live")
    out = []
    # A collated key is the collator's bytes, which need not be UTF-8.
    oplog = client.local.get_collection(
        "oplog.rs", codec_options=CodecOptions(unicode_decode_error_handler="surrogateescape")
    )
    for name, docs, key, idx_opts, cmd in _CASES:
        if docs:
            db[name].insert_many(docs)
        else:
            db.create_collection(name)
        index = db[name].create_index(key, **idx_opts) if key else None
        db.command(
            {
                "dbCheck": name,
                "validateMode": "extraIndexKeysCheck",
                "secondaryIndex": index,
                **cmd,
            }
        )
        ns = f"rm416live.{name}"
        deadline = time.monotonic() + 10
        while True:
            entries = [e["o"] for e in oplog.find({"o.dbCheck": ns}).sort("$natural", 1)]
            if any(o.get("type") == "stop" for o in entries) or time.monotonic() > deadline:
                break
            time.sleep(0.05)
        batches = [
            (o["md5"], o["batchStart"], o["batchEnd"], o["lastKeyChecked"])
            for o in entries
            if o.get("type") == "batch"
        ]
        warnings = [
            (h["msg"], h["data"]["error"].split(" and uuid ")[0])
            for h in client.local["system.healthlog"].find({"namespace": ns, "severity": "warning"})
        ]
        out.append((name, [o.get("type") for o in entries], batches, warnings))
    client.drop_database("rm416live")
    return out


def test_extra_index_keys_check_as_mongod(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    for ours, theirs in zip(_run(rust_mongo), _run(mongod_rs), strict=True):
        same(ours, theirs)
