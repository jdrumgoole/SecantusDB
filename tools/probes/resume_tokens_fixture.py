"""Capture change-stream resume tokens from a real mongod, per event kind.

Writes `tests/fixtures/storage_model/mongod-8.2.11-resume-tokens.json`: for
every kind of event a stream delivers, the token's `_data` and `_typeBits` as
mongod made them, with what the token must say (taken from the EVENT, not
from the token): the cluster time, `txnOpIndex`, the collection's UUID, and
the event identifier (`operationType` plus `documentKey` or
`operationDescription`). `crates/secantus-storage/tests/resume_tokens.rs`
decodes each token with this repo's KeyString decoder, compares it with
that, and builds the same token again from the same inputs byte for byte.

Needs a one-member replica set (mongod refuses `$changeStream` on a
standalone):

    mongod --replSet rs0 --port 27019 --bind_ip 127.0.0.1 --dbpath .scratch/m &
    PROBE_MONGOD=mongodb://127.0.0.1:27019 .venv/bin/python \
        tools/probes/resume_tokens_fixture.py
"""

import json
import os
import pathlib
import time

import pymongo
from bson import Binary, Int64, json_util

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27019")
OUT = pathlib.Path(__file__).parents[2] / (
    "tests/fixtures/storage_model/mongod-8.2.11-resume-tokens.json"
)
CANON = json_util.JSONOptions(json_mode=json_util.JSONMode.CANONICAL)

client = pymongo.MongoClient(MONGOD, directConnection=True)
VERSION = client.admin.command("buildInfo")["version"]
assert VERSION.startswith("8.2.11"), VERSION
cases: list[dict] = []


def ejson(value):
    return json.loads(json_util.dumps(value, json_options=CANON))


def drain(stream, want, seconds=60):
    out, end = [], time.time() + seconds
    while len(out) < want and time.time() < end:
        event = stream.try_next()
        if event is not None:
            out.append(event)
    assert len(out) == want, (len(out), want)
    return out


def identifier(event):
    ident = {"operationType": event["operationType"]}
    if "documentKey" in event:
        ident["documentKey"] = event["documentKey"]
    if "operationDescription" in event:
        ident["operationDescription"] = event["operationDescription"]
    return ident


def record(name, event, scope, op_index=0, from_invalidate=False, fragment=None, uuid=None):
    token = event["_id"]
    ident = None if event["operationType"] == "invalidate" else identifier(event)
    cases.append(
        {
            "name": name,
            "scope": scope,
            "data": token["_data"],
            "typeBits": token["_typeBits"].hex().upper() if "_typeBits" in token else None,
            "clusterTime": [event["clusterTime"].time, event["clusterTime"].inc],
            "tokenType": 128,
            "txnOpIndex": op_index,
            "fromInvalidate": from_invalidate,
            "uuid": (uuid if uuid is not None else event.get("collectionUUID")),
            "eventIdentifier": ident,
            "fragmentNum": fragment,
        }
    )


def uuid_hex(event):
    u = event.get("collectionUUID")
    return u.hex() if u is not None else None


def fix_uuid():
    for case in cases:
        if isinstance(case["uuid"], (bytes, Binary)):
            case["uuid"] = bytes(case["uuid"]).hex()
        if case["eventIdentifier"] is not None:
            case["eventIdentifier"] = ejson(case["eventIdentifier"])


for name in ("rt", "rt2"):
    client.drop_database(name)
db = client.rt
db.create_collection("c")

# --- every CRUD kind, with the `_id` types whose type bits differ.
with (
    db.c.watch(show_expanded_events=True) as s,
    client.watch(show_expanded_events=True) as cs,
    db.watch(show_expanded_events=True) as ds,
):
    ids = [1, Int64(2**40), 1.5, "str", {"a": 1, "b": 2}, {"a": Int64(5), "b": 2.5}, None]
    for i in ids:
        db.c.insert_one({"_id": i, "x": 1})
    db.c.update_one({"_id": 1}, {"$set": {"x": 2}})
    db.c.replace_one({"_id": 1}, {"y": 1})
    db.c.delete_one({"_id": 1})
    events = drain(s, len(ids) + 3)
    names = [f"insert id={i!r}" for i in ids] + ["update", "replace", "delete"]
    for n, e in zip(names, events, strict=True):
        record(n, e, "collection")
    # The same events, as database and cluster streams see them: same token.
    for e, d, g in zip(events, drain(ds, len(events)), drain(cs, len(events)), strict=True):
        assert e["_id"] == d["_id"] == g["_id"]
    record("delete (database scope)", d, "database")
    record("delete (cluster scope)", g, "cluster")

# --- a batched insert, and a transaction: txnOpIndex 0, 1, 2.
with db.c.watch(show_expanded_events=True) as s:
    db.c.insert_many([{"_id": 100}, {"_id": 101}, {"_id": 102}])
    with client.start_session() as ses, ses.start_transaction():
        db.c.insert_one({"_id": 200}, session=ses)
        db.c.update_one({"_id": 200}, {"$set": {"q": 1}}, session=ses)
        db.c.delete_one({"_id": 200}, session=ses)
    events = drain(s, 6)
    for i, e in enumerate(events[:3]):
        record(f"batched insert op {i}", e, "collection", op_index=i)
    for i, e in enumerate(events[3:]):
        record(f"transaction op {i}", e, "collection", op_index=i)

# --- DDL, expanded.
db2 = client.rt2
with db2.watch(show_expanded_events=True) as s:
    db2.create_collection("e1", capped=True, size=100000)
    db2.create_collection("e2", validator={"x": {"$gt": 0}})
    db2.e1.create_indexes(
        [pymongo.IndexModel("a", unique=True), pymongo.IndexModel([("b", 1), ("c", -1)])]
    )
    db2.command("collMod", "e2", validator={"y": {"$gt": 0}}, validationLevel="moderate")
    db2.command("collMod", "e1", index={"keyPattern": {"a": 1}, "hidden": True})
    db2.e1.drop_indexes()
    db2.e1.rename("e3")
    db2.create_collection("e4")
    db2.e3.rename("e4", dropTarget=True)
    db2.e4.drop()
    client.drop_database("rt2")
    events = drain(s, 15)
    for e in events:
        if e["operationType"] != "invalidate":
            record(f"ddl {e['operationType']}", e, "database")
    # `invalidate`, from the collection stream of a dropped collection.
db3 = client.rt3
client.drop_database("rt3")
db3.create_collection("g")
with db3.g.watch(show_expanded_events=True) as s:
    db3.g.insert_one({"_id": 1})
    db3.g.drop()
    drop_event = drain(s, 2)
    record("drop (collection scope)", drop_event[1], "collection")
    inv = drain(s, 1)[0]
    assert inv["operationType"] == "invalidate", inv
    cases.append(
        {
            "name": "invalidate (collection drop)",
            "scope": "collection",
            "data": inv["_id"]["_data"],
            "typeBits": None,
            "clusterTime": [inv["clusterTime"].time, inv["clusterTime"].inc],
            "tokenType": 128,
            "txnOpIndex": 0,
            "fromInvalidate": True,
            "uuid": cases[-1]["uuid"],
            "eventIdentifier": cases[-1]["eventIdentifier"],
            "fragmentNum": None,
        }
    )
with db3.watch() as s:
    db3.create_collection("h")
    client.drop_database("rt3")
    ev = drain(s, 2)
    inv = drain(s, 1)[0]
    assert inv["operationType"] == "invalidate", inv
    cases.append(
        {
            "name": "invalidate (dropDatabase)",
            "scope": "database",
            "data": inv["_id"]["_data"],
            "typeBits": None,
            "clusterTime": [inv["clusterTime"].time, inv["clusterTime"].inc],
            "tokenType": 128,
            "txnOpIndex": 0,
            "fromInvalidate": True,
            "uuid": None,
            "eventIdentifier": {"operationType": "dropDatabase"},
            "fragmentNum": None,
        }
    )

# --- split fragments: fragmentNum.
db4 = client.rt4
client.drop_database("rt4")
db4.create_collection("c", changeStreamPreAndPostImages={"enabled": True})
db4.c.insert_one({"_id": 1})
big = "x" * (9 * 1024 * 1024)
with db4.c.watch(
    [{"$changeStreamSplitLargeEvent": {}}],
    full_document_before_change="required",
    show_expanded_events=True,
    max_await_time_ms=500,
) as s:
    db4.c.update_one({"_id": 1}, {"$set": {"big": big}})
    db4.c.update_one({"_id": 1}, {"$set": {"big2": big}, "$unset": {"big": ""}})
    events = drain(s, 3)
    for i, e in enumerate(events[1:]):
        e = dict(e)
        e["operationType"] = "update"
        e["documentKey"] = {"_id": 1}
        e["clusterTime"] = events[1]["clusterTime"]
        e["collectionUUID"] = events[0]["collectionUUID"]
        record(f"split fragment {i}", e, "collection", fragment=i)
client.drop_database("rt4")

# --- high water marks: tokenType 0.
db5 = client.rt5
client.drop_database("rt5")
db5.create_collection("a")
hwm = []
with db5.a.watch(max_await_time_ms=200) as s:
    hwm.append(("open", s.resume_token["_data"]))
    db5.other.insert_one({"_id": 1})
    time.sleep(0.5)
    s.try_next()
    hwm.append(("after a write elsewhere", s.resume_token["_data"]))
client.drop_database("rt5")

fix_uuid()
for name, data in hwm:
    cases.append(
        {
            "name": f"high water mark, {name}",
            "scope": "collection",
            "data": data,
            "typeBits": None,
            "clusterTime": [int(data[2:10], 16), int(data[10:18], 16)],
            "tokenType": 0,
            "txnOpIndex": 0,
            "fromInvalidate": False,
            "uuid": None,
            "eventIdentifier": None,
            "fragmentNum": None,
        }
    )
OUT.write_text(
    json.dumps({"mongod_version": f"db version v{VERSION}", "cases": cases}, indent=1) + "\n"
)
print(f"wrote {len(cases)} tokens to {OUT}")
