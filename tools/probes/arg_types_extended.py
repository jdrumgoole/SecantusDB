"""Extended wrong-type sweep: more commands, and argument classes beyond documents.

The first sweep covered document-valued arguments on eight commands and found 45
crashes. This widens it two ways:
  * commands the first pass skipped (createIndexes, create, collMod, listIndexes,
    renameCollection, mapReduce, and the $lookup sub-spec)
  * argument CLASSES beyond documents -- numeric (limit/skip/batchSize/maxTimeMS),
    string (names), and boolean args, each fed a value of the wrong kind.
"""

import os
import tempfile

import pymongo

from secantus import SecantusDBServer

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")

#: Point the probe at a RUNNING server instead of an embedded Python one --
#: this is how the Rust server gets swept (start `secantusd-rs --port N
#: --storage-path DIR`, then `PROBE_SERVER=mongodb://127.0.0.1:N`). The probe
#: drives the wire, so the server under test is just a URI; there is nothing
#: Python-specific about it.
SERVER = os.environ.get("PROBE_SERVER")

# For a slot expecting a document/array, feed scalars. For a slot expecting a
# number, feed a document/string. For a string slot, feed a number/document.
DOCISH = [5, "x", True]
NUMISH = [{}, "x", [1]]
STRISH = [5, {}, [1]]
BOOLISH = [{}, [1]]


def cases():
    for b in DOCISH:
        yield (f"createIndexes.indexes={b!r}", {"createIndexes": "c", "indexes": b})
        yield (
            f"createIndexes.key={b!r}",
            {"createIndexes": "c", "indexes": [{"key": b, "name": "i"}]},
        )
        yield (f"create.storageEngine={b!r}", {"create": "newc", "storageEngine": b})
        yield (f"collMod.index={b!r}", {"collMod": "c", "index": b})
        yield (f"listIndexes.cursor={b!r}", {"listIndexes": "c", "cursor": b})
        yield (f"aggregate.cursor={b!r}", {"aggregate": "c", "pipeline": [], "cursor": b})
        yield (f"aggregate.let={b!r}", {"aggregate": "c", "pipeline": [], "cursor": {}, "let": b})
        yield (f"find.collation={b!r}", {"find": "c", "collation": b})
        yield (f"find.let={b!r}", {"find": "c", "let": b})
        yield (f"find.min={b!r}", {"find": "c", "min": b})
        yield (f"find.max={b!r}", {"find": "c", "max": b})
        yield (f"lookup.spec={b!r}", {"aggregate": "c", "pipeline": [{"$lookup": b}], "cursor": {}})
        yield (f"group.spec={b!r}", {"aggregate": "c", "pipeline": [{"$group": b}], "cursor": {}})
        yield (f"match.spec={b!r}", {"aggregate": "c", "pipeline": [{"$match": b}], "cursor": {}})
        yield (f"sort.spec={b!r}", {"aggregate": "c", "pipeline": [{"$sort": b}], "cursor": {}})
    for b in NUMISH:
        yield (f"find.limit={b!r}", {"find": "c", "limit": b})
        yield (f"find.skip={b!r}", {"find": "c", "skip": b})
        yield (f"find.batchSize={b!r}", {"find": "c", "batchSize": b})
        yield (f"find.maxTimeMS={b!r}", {"find": "c", "maxTimeMS": b})
        yield (f"delete.limit={b!r}", {"delete": "c", "deletes": [{"q": {}, "limit": b}]})
        yield (
            f"agg.batchSize={b!r}",
            {"aggregate": "c", "pipeline": [], "cursor": {"batchSize": b}},
        )
        yield (f"limit.stage={b!r}", {"aggregate": "c", "pipeline": [{"$limit": b}], "cursor": {}})
        yield (f"skip.stage={b!r}", {"aggregate": "c", "pipeline": [{"$skip": b}], "cursor": {}})
    for b in STRISH:
        yield (f"distinct.key={b!r}", {"distinct": "c", "key": b})
        yield (
            f"createIndexes.name={b!r}",
            {"createIndexes": "c", "indexes": [{"key": {"a": 1}, "name": b}]},
        )
        yield (f"count.stage={b!r}", {"aggregate": "c", "pipeline": [{"$count": b}], "cursor": {}})
        yield (
            f"unwind.stage={b!r}",
            {"aggregate": "c", "pipeline": [{"$unwind": b}], "cursor": {}},
        )
    for b in BOOLISH:
        yield (f"find.singleBatch={b!r}", {"find": "c", "singleBatch": b, "limit": 1})
        yield (
            f"fam.upsert={b!r}",
            {"findAndModify": "c", "query": {}, "update": {"$set": {"a": 1}}, "upsert": b},
        )
        yield (
            f"update.multi={b!r}",
            {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}, "multi": b}]},
        )


def run(cli, dbn, cmd):
    db = cli[dbn]
    db.c.drop()
    db.c.insert_one({"_id": 1, "a": 1})
    try:
        r = db.command(dict(cmd))
        we = (r.get("writeErrors") or [{}])[0]
        return ("ERR", we.get("code")) if we else ("OK", None)
    except pymongo.errors.PyMongoError as e:
        return ("ERR", getattr(e, "code", None))


if SERVER:
    s = None
    sec = pymongo.MongoClient(SERVER, directConnection=True, serverSelectionTimeoutMS=8000)
    print(f"  server under test: {SERVER}")
else:
    d = tempfile.mkdtemp()
    s = SecantusDBServer(port=0, storage_path=d)
    s.start()
    sec = pymongo.MongoClient(f"mongodb://{s.address[0]}:{s.address[1]}", directConnection=True)
    print("  server under test: embedded Python SecantusDBServer")
mon = pymongo.MongoClient(MONGOD, directConnection=True, serverSelectionTimeoutMS=8000)
crashes, diffs, total = [], [], 0
for i, (label, cmd) in enumerate(cases()):
    total += 1
    a = run(mon, f"x{i}", cmd)
    b = run(sec, f"x{i}", cmd)
    if b == ("ERR", 1):
        crashes.append((label, a))
    elif a != b:
        diffs.append((label, a, b))
print(f"  cases: {total}   CRASHES (code 1): {len(crashes)}   other divergences: {len(diffs)}\n")
if crashes:
    print("  === crashes ===")
    for label, mongo in crashes:
        print(f"    {label:<34} mongod={mongo}")
if diffs:
    print("\n  === divergences ===")
    for label, mongo, ours in diffs:
        print(f"    {label:<34} mongod={str(mongo):<16} secantus={ours}")
sec.close()
if s is not None:
    s.stop()
