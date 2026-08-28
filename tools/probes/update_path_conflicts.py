import tempfile

import pymongo

from secantus import SecantusDBServer

CASES = [
    ("same path, two ops", {"a": 1}, {"$set": {"a": 2}, "$inc": {"a": 1}}),
    ("same path, set+unset", {"a": 1}, {"$set": {"a": 2}, "$unset": {"a": ""}}),
    ("prefix conflict a / a.b", {"a": {"b": 1}}, {"$set": {"a": 2}, "$inc": {"a.b": 1}}),
    ("prefix conflict a.b / a", {"a": {"b": 1}}, {"$set": {"a.b": 2}, "$inc": {"a": 1}}),
    ("sibling paths ok", {"a": {"b": 1}}, {"$set": {"a.b": 2}, "$inc": {"a.c": 1}}),
    ("disjoint paths ok", {"a": 1, "b": 1}, {"$set": {"a": 2}, "$inc": {"b": 1}}),
    ("rename vs set conflict", {"a": 1, "b": 2}, {"$rename": {"a": "b"}, "$set": {"b": 9}}),
    ("rename src also set", {"a": 1}, {"$rename": {"a": "c"}, "$set": {"a": 5}}),
    ("deep prefix a.b / a.b.c", {"a": {"b": {"c": 1}}}, {"$set": {"a.b": 2}, "$inc": {"a.b.c": 1}}),
    ("array idx conflict", {"a": [1, 2]}, {"$set": {"a.0": 5}, "$inc": {"a.0": 1}}),
    ("array idx siblings ok", {"a": [1, 2]}, {"$set": {"a.0": 5}, "$inc": {"a.1": 1}}),
]


def run(cli, dbn, seed, upd):
    db = cli[dbn]
    db.c.drop()
    doc = dict(seed)
    doc["_id"] = 1
    db.c.insert_one(doc)
    try:
        r = db.command({"update": "c", "updates": [{"q": {"_id": 1}, "u": upd}]})
        we = r.get("writeErrors")
        if we:
            return f"ERR {we[0].get('code')}"
        after = db.c.find_one({"_id": 1})
        after.pop("_id", None)
        return f"OK {after}"
    except Exception as e:
        return f"RAISE {getattr(e, 'code', None)}"


d = tempfile.mkdtemp()
s = SecantusDBServer(port=0, storage_path=d, replica_set_name="secantus")
s.start()
sec = pymongo.MongoClient(f"mongodb://{s.address[0]}:{s.address[1]}", directConnection=True)
mon = pymongo.MongoClient(
    "mongodb://127.0.0.1:27036", directConnection=True, serverSelectionTimeoutMS=8000
)
bad = 0
print(f"  {'case':<26}{'mongod':<26}{'secantus':<26}")
for i, (label, seed, upd) in enumerate(CASES):
    a = run(mon, f"cf{i}", seed, upd)
    b = run(sec, f"cf{i}", seed, upd)
    ok = a == b
    if not ok:
        bad += 1
    print(f"  {label:<26}{a:<26}{b:<26}{'OK' if ok else 'DIFF'}")
print(f"\n  {len(CASES) - bad}/{len(CASES)} match")
sec.close()
s.stop()
