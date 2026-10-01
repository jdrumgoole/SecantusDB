"""`updateDescription` per update operator, against mongod: one small case each.

`change_stream_fuzz.py` found the Rust server describing modifier updates as a
DEEP diff of the before and after documents, where mongod describes them by
what each operator touched: `$set: {a: {c: {a: 2}}}` over `{a: {a: null, c:
{c: 0}}}` is `updatedFields: {a: {c: {a: 2}}}` on mongod and `{a.c.a: 2}` plus
two removed fields here. This table pins mongod's answer operator by operator,
and pipeline updates (which mongod DOES describe by diffing, with its own
rules) separately.

    PROBE_MONGOD="mongodb://127.0.0.1:27041/?directConnection=true" \\
    PROBE_SERVER="mongodb://127.0.0.1:27058/?directConnection=true" \\
        python tools/probes/update_description.py [--show]

`--show` prints mongod's answer for every case, which is how the rules are read.
"""

from __future__ import annotations

import os
import sys
from typing import Any

import pymongo

MONGOD = os.environ.get("PROBE_MONGOD")
SERVER = os.environ.get("PROBE_SERVER")

#: (label, starting document without _id, update)
CASES: list[tuple[str, dict[str, Any], Any]] = [
    # $set
    ("set scalar", {"a": 1}, {"$set": {"a": 2}}),
    ("set same scalar", {"a": 1, "b": 0}, {"$set": {"a": 1, "b": 1}}),
    ("set new field", {"a": 1}, {"$set": {"b": 2}}),
    ("set doc over doc", {"a": {"x": 1, "y": 2}}, {"$set": {"a": {"x": 1, "z": 3}}}),
    ("set doc over scalar", {"a": 1}, {"$set": {"a": {"x": 1}}}),
    ("set dotted into doc", {"a": {"x": 1, "y": 2}}, {"$set": {"a.x": 5}}),
    ("set dotted new", {"a": {"x": 1}}, {"$set": {"a.z": 5}}),
    ("set dotted creates", {"b": 1}, {"$set": {"a.b.c": 5}}),
    ("set array over array", {"a": [1, 2, 3]}, {"$set": {"a": [1, 2]}}),
    ("set array longer", {"a": [1, 2]}, {"$set": {"a": [1, 2, 3]}}),
    ("set array empty", {"a": [1, 2, 3]}, {"$set": {"a": []}}),
    ("set array element", {"a": [1, 2, 3]}, {"$set": {"a.1": 9}}),
    ("set array element past end", {"a": [1]}, {"$set": {"a.3": 9}}),
    ("set nested array elem doc", {"a": [{"x": 1}, {"x": 2}]}, {"$set": {"a.1.x": 9}}),
    ("set equal doc", {"a": {"x": 1}}, {"$set": {"a": {"x": 1}}}),
    ("set equal number other type", {"a": 1}, {"$set": {"a": 1.0}}),
    # $unset
    ("unset field", {"a": 1, "b": 2}, {"$unset": {"a": ""}}),
    ("unset missing", {"a": 1}, {"$unset": {"z": ""}}),
    ("unset dotted", {"a": {"x": 1, "y": 2}}, {"$unset": {"a.x": ""}}),
    ("unset array element", {"a": [1, 2, 3]}, {"$unset": {"a.1": ""}}),
    # $inc / $mul / $min / $max
    ("inc", {"a": 1}, {"$inc": {"a": 2}}),
    ("inc new", {"a": 1}, {"$inc": {"b": 2}}),
    ("inc zero", {"a": 1}, {"$inc": {"a": 0}}),
    ("mul", {"a": 2}, {"$mul": {"a": 3}}),
    ("min no change", {"a": 1}, {"$min": {"a": 5}}),
    ("max change", {"a": 1}, {"$max": {"a": 5}}),
    # arrays
    ("push", {"a": [1, 2]}, {"$push": {"a": 3}}),
    ("push to missing", {"b": 1}, {"$push": {"a": 3}}),
    ("push to empty", {"a": []}, {"$push": {"a": 3}}),
    ("set nested past end", {"b": []}, {"$set": {"b.2.c": 1}}),
    ("push each", {"a": [1]}, {"$push": {"a": {"$each": [2, 3]}}}),
    ("push position 0", {"a": [1, 2]}, {"$push": {"a": {"$each": [0], "$position": 0}}}),
    ("push slice", {"a": [1, 2, 3]}, {"$push": {"a": {"$each": [4], "$slice": -2}}}),
    ("push sort", {"a": [3, 1]}, {"$push": {"a": {"$each": [2], "$sort": 1}}}),
    ("addToSet new", {"a": [1, 2]}, {"$addToSet": {"a": 3}}),
    ("addToSet existing", {"a": [1, 2]}, {"$addToSet": {"a": 2}}),
    ("pull middle", {"a": [1, 2, 3]}, {"$pull": {"a": 2}}),
    ("pull last", {"a": [1, 2, 3]}, {"$pull": {"a": 3}}),
    ("pull none", {"a": [1, 2, 3]}, {"$pull": {"a": 9}}),
    ("pullAll", {"a": [1, 2, 3, 2]}, {"$pullAll": {"a": [2]}}),
    ("pop last", {"a": [1, 2, 3]}, {"$pop": {"a": 1}}),
    ("pop first", {"a": [1, 2, 3]}, {"$pop": {"a": -1}}),
    ("pop empty", {"a": []}, {"$pop": {"a": 1}}),
    ("positional set", {"a": [1, 2, 3]}, None),  # filled below (needs a query)
    # $rename
    ("rename", {"a": 1}, {"$rename": {"a": "b"}}),
    ("rename onto existing", {"a": 1, "b": 2}, {"$rename": {"a": "b"}}),
    ("rename onto equal", {"a": [], "c": []}, {"$rename": {"a": "c"}}),
    ("rename doc", {"a": {"x": 1}, "b": {"y": 2}}, {"$rename": {"a": "b"}}),
    ("rename missing", {"a": 1}, {"$rename": {"z": "y"}}),
    # $currentDate / $setOnInsert are time- or upsert-shaped; skipped.
    # several operators at once
    ("set and unset", {"a": 1, "b": 2}, {"$set": {"c": 3}, "$unset": {"b": ""}}),
    ("set two dotted siblings", {"a": {"x": 1, "y": 2}}, {"$set": {"a.x": 5, "a.y": 6}}),
    # pipeline updates: mongod diffs these
    ("pipeline scalar", {"a": 1, "b": 2}, [{"$set": {"a": 5}}]),
    ("pipeline same value", {"a": 1}, [{"$set": {"a": 1}}]),
    ("pipeline doc", {"a": {"x": 1, "y": 2}}, [{"$set": {"a": {"$literal": {"x": 1, "z": 3}}}}]),
    ("pipeline array shorter", {"a": [1, 2, 3]}, [{"$set": {"a": {"$literal": [1, 2]}}}]),
    ("pipeline array empty", {"a": [1, 2, 3]}, [{"$set": {"a": {"$literal": []}}}]),
    ("pipeline array elem", {"a": [1, 2, 3]}, [{"$set": {"a": {"$literal": [1, 9, 3]}}}]),
    ("pipeline unset", {"a": 1, "b": 2}, [{"$unset": "a"}]),
    ("pipeline replace most", {"a": [1, [2, 3], {"x": 1}], "b": 1}, [{"$set": {"a": "y"}}]),
    ("pipeline small change big doc", {"a": "x" * 50, "b": 1}, [{"$set": {"b": 2}}]),
    ("pipeline project", {"a": 1, "b": 2, "c": 3}, [{"$project": {"a": 1}}]),
]


def run_case(db: Any, start: dict[str, Any], update: Any, label: str) -> Any:
    db.drop_collection("u")
    coll = db.u
    coll.insert_one({"_id": 1, **start})
    with coll.watch(max_await_time_ms=300) as stream:
        try:
            if label == "positional set":
                coll.update_one({"_id": 1, "a": 2}, {"$set": {"a.$": 9}})
            else:
                coll.update_one({"_id": 1}, update)
        except pymongo.errors.OperationFailure as e:
            return ("error", e.code)
        events = []
        empty = 0
        while empty < 2:
            ev = stream.try_next()
            if ev is None:
                empty += 1
                continue
            events.append(
                (ev["operationType"], ev.get("updateDescription"), ev.get("fullDocument"))
            )
    return events


def measure(uri: str) -> list[Any]:
    client = pymongo.MongoClient(uri)
    db = client.update_desc_probe
    out = [run_case(db, start, update, label) for label, start, update in CASES]
    client.drop_database("update_desc_probe")
    client.close()
    return out


def main() -> int:
    if not (MONGOD and SERVER):
        print("PROBE_MONGOD and PROBE_SERVER are required (see the module docstring)")
        return 2
    show = "--show" in sys.argv
    want, got = measure(MONGOD), measure(SERVER)
    bad = 0
    for (label, start, update), w, g in zip(CASES, want, got, strict=True):
        if show:
            print(f"{label:32} {start} {update}\n{'':32} mongod {w}")
        if w != g:
            bad += 1
            print(f"DIFF {label}: {start} {update}\n  mongod: {w}\n  ours:   {g}")
    print(f"=== updateDescription: {bad} of {len(CASES)} divergent ===")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
