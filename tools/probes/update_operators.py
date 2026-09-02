"""Differential-probe the update operator family against a real mongod.

For each (seed document, update spec) case: apply it to both servers and compare
the resulting document AND the reply's n/nModified/upserted, plus the error
code+message when one rejects it. Any divergence is a fidelity bug in one of them.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(__file__))

from _servers import probe_targets, report  # noqa: E402

CASES = [
    # ---- $set / $unset edge cases
    ({"a": 1}, {"$set": {"a": 2}}),
    ({"a": 1}, {"$set": {"b.c": 3}}),
    ({"a": [1, 2, 3]}, {"$set": {"a.1": 9}}),
    ({"a": [1, 2, 3]}, {"$set": {"a.5": 9}}),  # sparse grow
    ({"a": {"b": 1}}, {"$unset": {"a.b": ""}}),
    ({"a": [1, 2]}, {"$unset": {"a.0": ""}}),  # leaves null hole
    ({"a": 1}, {"$unset": {"nope": ""}}),
    # ---- $inc / $mul type coercion
    ({"a": 1}, {"$inc": {"a": 1.5}}),
    ({"a": 2**31 - 1}, {"$inc": {"a": 1}}),  # int32 overflow
    ({"a": 1}, {"$mul": {"a": 2.0}}),
    ({"a": 1}, {"$mul": {"missing": 3}}),
    ({"a": "x"}, {"$inc": {"a": 1}}),  # error case
    ({}, {"$inc": {"a": 1}}),
    # ---- $min / $max cross-type
    ({"a": 5}, {"$min": {"a": 3}}),
    ({"a": 5}, {"$min": {"a": "s"}}),  # cross-type BSON order
    ({"a": 5}, {"$max": {"a": None}}),
    # ---- array operators
    ({"a": [1, 2]}, {"$push": {"a": 3}}),
    ({"a": [1, 2]}, {"$push": {"a": {"$each": [3, 4], "$slice": -2}}}),
    ({"a": [3, 1, 2]}, {"$push": {"a": {"$each": [], "$sort": 1}}}),
    ({"a": [1, 2, 3]}, {"$pull": {"a": {"$gte": 2}}}),
    ({"a": [1, 2, 2]}, {"$pull": {"a": 2}}),
    ({"a": [1, 2]}, {"$addToSet": {"a": 2}}),
    ({"a": [1, 2]}, {"$addToSet": {"a": {"$each": [2, 3]}}}),
    ({"a": [1, 2, 3]}, {"$pop": {"a": 1}}),
    ({"a": [1, 2, 3]}, {"$pop": {"a": -1}}),
    ({"a": []}, {"$pop": {"a": 1}}),
    ({"a": 5}, {"$push": {"a": 1}}),  # error: not an array
    # ---- $rename
    ({"a": 1, "b": 2}, {"$rename": {"a": "c"}}),
    ({"a": 1}, {"$rename": {"a": "a"}}),  # error
    ({"a": {"b": 1}}, {"$rename": {"a.b": "a.c"}}),
    ({"a": 1}, {"$rename": {"missing": "z"}}),
    # ---- mixed / conflicting
    ({"a": 1}, {"$set": {"a": 2}, "$inc": {"a": 1}}),  # conflict error
    ({"a": 1}, {"$set": {"b": 1}, "$unset": {"a": ""}}),
    ({"a": 1}, {"$setOnInsert": {"z": 9}}),
    # ---- $currentDate
    ({"a": 1}, {"$currentDate": {"d": {"$type": "timestamp"}}}),
    ({"a": 1}, {"$currentDate": {"d": True}}),
]


def run(cli, dbname, seed, upd):
    db = cli[dbname]
    db.c.drop()
    doc = dict(seed)
    doc["_id"] = 1
    db.c.insert_one(doc)
    try:
        r = db.command({"update": "c", "updates": [{"q": {"_id": 1}, "u": upd}]})
        after = db.c.find_one({"_id": 1})
        if after:
            after.pop("_id", None)
        # $currentDate values are non-deterministic; compare presence/type only.
        for k, v in list((after or {}).items()):
            if hasattr(v, "year") or type(v).__name__ == "Timestamp":
                after[k] = f"<{type(v).__name__}>"
        we = r.get("writeErrors")
        if we:
            return ("ERR", we[0].get("code"), str(we[0].get("errmsg", ""))[:70])
        return ("OK", r.get("n"), r.get("nModified"), repr(after))
    except Exception as e:
        code = getattr(e, "code", None)
        return ("RAISE", code, str(e).split(", full error")[0][:70])


def main():
    with probe_targets(replica_set="secantus") as (mon, targets):
        divergent = {label: 0 for label, _ in targets}
        for i, (seed, upd) in enumerate(CASES):
            expected = run(mon, f"du{i}", seed, upd)
            got = {label: run(cli, f"du{i}", seed, upd) for label, cli in targets}
            off = {k for k, v in got.items() if v != expected}
            if not off:
                continue
            for label in off:
                divergent[label] += 1
            print(f"  seed={seed}  update={upd}")
            print(f"    mongod  : {expected}")
            for label, value in got.items():
                mark = "   <-- diverges" if label in off else ""
                print(f"    {label:8s}: {value}{mark}")
            print()
        return report("update operators", len(CASES), divergent)


sys.exit(main())
