import os
import sys

sys.path.insert(0, os.path.dirname(__file__))

from _servers import probe_targets, report  # noqa: E402

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


with probe_targets(replica_set="secantus") as (mon, targets):
    divergent = {label: 0 for label, _ in targets}
    for i, (label, seed, upd) in enumerate(CASES):
        expected = run(mon, f"cf{i}", seed, upd)
        got = {name: run(cli, f"cf{i}", seed, upd) for name, cli in targets}
        off = {k for k, v in got.items() if v != expected}
        if not off:
            continue
        for name in off:
            divergent[name] += 1
        print(f"  {label}")
        print(f"    mongod  : {expected}")
        for name, value in got.items():
            mark = "   <-- diverges" if name in off else ""
            print(f"    {name:8s}: {value}{mark}")
        print()
    sys.exit(report("update path conflicts", len(CASES), divergent))
