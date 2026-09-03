"""Differential-probe the update operator family against a real mongod.

For each (seed document, update spec) case: apply it to both servers and compare
the resulting document AND the reply's n/nModified/upserted, plus the error
code+message when one rejects it. Any divergence is a fidelity bug in one of them.
"""

import os
import sys

sys.path.insert(0, os.path.dirname(__file__))

from bson import (  # noqa: E402
    Binary,
    Code,
    Decimal128,
    Int64,
    MaxKey,
    MinKey,
    Regex,
    Timestamp,
)

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
    # ---- non-finite and boundary operands, added 2026-09-03.
    #
    # None of these value CLASSES was in this file. The same gap in
    # `agg_expressions.py` hid thirteen crash-class bugs, and in
    # `index_result_sets.py` it hid silent data loss through a partial index --
    # both found the day this block was written. A write path is where a crash
    # matters most, so these are here rather than left to a read sweep.
    ({"a": 1}, {"$inc": {"a": float("inf")}}),
    ({"a": 1}, {"$inc": {"a": float("nan")}}),
    ({"a": float("inf")}, {"$inc": {"a": 1}}),
    ({"a": float("inf")}, {"$inc": {"a": float("-inf")}}),
    ({"a": float("nan")}, {"$inc": {"a": 1}}),
    ({"a": 1}, {"$mul": {"a": float("inf")}}),
    ({"a": 0}, {"$mul": {"a": float("inf")}}),
    ({"a": float("nan")}, {"$mul": {"a": 0}}),
    ({"a": Decimal128("2.5")}, {"$inc": {"a": 1}}),
    ({"a": Decimal128("2.5")}, {"$mul": {"a": 2}}),
    ({"a": 1}, {"$inc": {"a": Decimal128("NaN")}}),
    ({"a": 1}, {"$inc": {"a": Decimal128("Infinity")}}),
    # int64 overflow, the width above the int32 case already covered.
    ({"a": Int64(2**63 - 1)}, {"$inc": {"a": 1}}),
    ({"a": Int64(-(2**63))}, {"$inc": {"a": -1}}),
    ({"a": Int64(2**62)}, {"$mul": {"a": 4}}),
    # $min / $max against the ends of the BSON order and the non-finite values.
    ({"a": 5}, {"$min": {"a": float("nan")}}),
    ({"a": float("nan")}, {"$min": {"a": 5}}),
    # NaN vs the infinities specifically: the sort order ranks NaN below
    # -Infinity, so `$min` sets it even against the smallest finite bound and
    # `$max` never chooses it. Measured against mongod 8.2.11 (2026-09-03) --
    # a dedicated test asserted these against our own servers only, which
    # proves nothing about mongod.
    ({"a": float("-inf")}, {"$min": {"a": float("nan")}}),
    ({"a": float("nan")}, {"$min": {"a": float("-inf")}}),
    ({"a": float("nan")}, {"$max": {"a": 5}}),
    ({"a": float("nan")}, {"$max": {"a": float("-inf")}}),
    ({"a": 5}, {"$max": {"a": float("nan")}}),
    ({"a": float("inf")}, {"$max": {"a": float("nan")}}),
    ({"a": 5}, {"$max": {"a": float("inf")}}),
    ({"a": 5}, {"$min": {"a": MinKey()}}),
    ({"a": 5}, {"$max": {"a": MaxKey()}}),
    ({"a": MaxKey()}, {"$min": {"a": 5}}),
    # $set / $unset of the classes this file never wrote at all.
    ({"a": 1}, {"$set": {"a": MinKey()}}),
    ({"a": 1}, {"$set": {"a": float("nan")}}),
    ({"a": 1}, {"$set": {"a": Decimal128("-0")}}),
    ({"a": 1}, {"$set": {"a": Binary(b"z")}}),
    ({"a": 1}, {"$set": {"a": Timestamp(1, 1)}}),
    ({"a": 1}, {"$set": {"a": Regex("p", "i")}}),
    ({"a": 1}, {"$set": {"a": Code("x=1")}}),
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
