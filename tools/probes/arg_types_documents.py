"""Feed wrong-typed arguments to commands that structurally dereference them.

Twice this session a caller-supplied scalar reached code assuming a document and
crashed as "internal server error": `pipeline: [42]` -> len(), `update: 5` ->
.keys(). This sweeps the same shape across command arguments that are documents,
arrays, or nested specs, and compares against mongod.
"""

import os
import tempfile

import pymongo

from secantus import SecantusDBServer

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
BAD = [5, "x", True, [1, 2]]


def cases():
    for bad in BAD:
        yield (f"find.filter={bad!r}", {"find": "c", "filter": bad})
        yield (f"find.sort={bad!r}", {"find": "c", "sort": bad})
        yield (f"find.projection={bad!r}", {"find": "c", "projection": bad})
        yield (f"count.query={bad!r}", {"count": "c", "query": bad})
        yield (f"distinct.query={bad!r}", {"distinct": "c", "key": "a", "query": bad})
        yield (f"delete.q={bad!r}", {"delete": "c", "deletes": [{"q": bad, "limit": 1}]})
        yield (
            f"update.q={bad!r}",
            {"update": "c", "updates": [{"q": bad, "u": {"$set": {"a": 1}}}]},
        )
        yield (f"update.u={bad!r}", {"update": "c", "updates": [{"q": {}, "u": bad}]})
        yield (f"insert.documents={bad!r}", {"insert": "c", "documents": bad})
        yield (f"aggregate.pipeline={bad!r}", {"aggregate": "c", "pipeline": bad, "cursor": {}})
        yield (
            f"createIndexes.key={bad!r}",
            {"createIndexes": "c", "indexes": [{"key": bad, "name": "i"}]},
        )
        yield (f"fam.query={bad!r}", {"findAndModify": "c", "query": bad, "remove": True})
        yield (
            f"fam.sort={bad!r}",
            {"findAndModify": "c", "query": {}, "sort": bad, "remove": True},
        )
        yield (
            f"fam.fields={bad!r}",
            {"findAndModify": "c", "query": {}, "fields": bad, "remove": True},
        )


def run(cli, dbn, cmd):
    db = cli[dbn]
    db.c.drop()
    db.c.insert_one({"_id": 1, "a": 1})
    try:
        db.command(dict(cmd))
        return ("OK", None)
    except Exception as e:
        return ("ERR", getattr(e, "code", None))


d = tempfile.mkdtemp()
s = SecantusDBServer(port=0, storage_path=d)
s.start()
sec = pymongo.MongoClient(f"mongodb://{s.address[0]}:{s.address[1]}", directConnection=True)
mon = pymongo.MongoClient(MONGOD, directConnection=True, serverSelectionTimeoutMS=8000)
crashes, diffs, total = [], [], 0
for i, (label, cmd) in enumerate(cases()):
    total += 1
    a = run(mon, f"t{i}", cmd)
    b = run(sec, f"t{i}", cmd)
    if b == ("ERR", 1):
        crashes.append((label, a, b))
    elif a != b:
        diffs.append((label, a, b))
print(f"  cases: {total}   CRASHES (code 1): {len(crashes)}   other divergences: {len(diffs)}\n")
if crashes:
    print("  === crashes (internal server error) ===")
    for label, mongo, _ours in crashes:
        print(f"    {label:<34} mongod={mongo}")
for label, mongo, ours in diffs:
    print(f"    {label:<34} mongod={str(mongo):<18} secantus={ours}")
sec.close()
s.stop()
