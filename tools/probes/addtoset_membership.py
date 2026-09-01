"""`$addToSet` membership equality, plus the `$pop` / `$max` / `$min` arguments.

`$addToSet` dedups by BSON VALUE equality, which is not Python `==`: numerics
unify across the width, but a bool is its own type, documents and arrays are
ORDER-sensitive, `Code("ab")` is not `"ab"`, and regexes compare by pattern and
option set. 30 shapes; 5 diverged on the Rust server and 1 on the Python one
when this was written -- the Python one being `$max` over two regexes, which
never moved because `bson.Regex` defines no `__lt__` and both engines had
encoded that accident as "they are equal".

    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python tools/probes/addtoset_membership.py
    PROBE_SERVER="mongodb://127.0.0.1:27055" ... # adds the Rust column
"""

import os, sys, tempfile, pymongo
from bson import Code, Decimal128, Int64, Regex, Binary, ObjectId

targets = [
    ("mongod", pymongo.MongoClient(os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")))
]
from secantus import SecantusDBServer

_s = SecantusDBServer(port=0, storage_path=tempfile.mkdtemp())
_s.start()
targets.append(("python", pymongo.MongoClient(_s.uri)))
if os.environ.get("PROBE_SERVER"):
    targets.append(("rust", pymongo.MongoClient(os.environ["PROBE_SERVER"])))

ATS = [
    ("bool true into [1]", [1], True),
    ("int 1 into [true]", [True], 1),
    ("double 1.0 into [1]", [1], 1.0),
    ("Int64 1 into [1]", [1], Int64(1)),
    ("Dec 1 into [1]", [1], Decimal128("1")),
    ("doc reordered", [{"x": 1, "y": 2}], {"y": 2, "x": 1}),
    ("doc same order", [{"x": 1, "y": 2}], {"x": 1, "y": 2}),
    ("nested doc reorder", [{"d": {"x": 1, "y": 2}}], {"d": {"y": 2, "x": 1}}),
    ("Code vs str", ["ab"], Code("ab")),
    ("str vs Code", [Code("ab")], "ab"),
    ("Code vs Code", [Code("ab")], Code("ab")),
    ("null into [null]", [None], None),
    ("regex vs regex", [Regex("ab", "i")], Regex("ab", "i")),
    ("regex diff flags", [Regex("ab", "i")], Regex("ab", "m")),
    ("array vs array", [[1, 2]], [1, 2]),
    ("array reordered", [[1, 2]], [2, 1]),
    ("bool false into [0]", [0], False),
    ("doc extra key", [{"x": 1}], {"x": 1, "y": 2}),
    ("empty doc", [{}], {}),
    ("oid vs oid", [ObjectId("0" * 24)], ObjectId("0" * 24)),
]
POPMAX = [
    ("$pop dec -0", {"a": [1, 2, 3]}, {"$pop": {"a": Decimal128("-0")}}),
    ("$pop dec 1", {"a": [1, 2, 3]}, {"$pop": {"a": Decimal128("1")}}),
    ("$pop dec -1", {"a": [1, 2, 3]}, {"$pop": {"a": Decimal128("-1")}}),
    ("$pop dbl 1.0", {"a": [1, 2, 3]}, {"$pop": {"a": 1.0}}),
    ("$max Code>", {"v": Code("m")}, {"$max": {"v": Code("z")}}),
    ("$min Code<", {"v": Code("m")}, {"$min": {"v": Code("a")}}),
    ("$max str v Code", {"v": Code("m")}, {"$max": {"v": "zzz"}}),
    ("$min str v Code", {"v": Code("m")}, {"$min": {"v": "aaa"}}),
    ("$set Code", {"v": 1}, {"$set": {"v": Code("y=2")}}),
    ("$max regex", {"v": Regex("a", "")}, {"$max": {"v": Regex("z", "")}}),
]


def norm(x):
    return repr(x)


bad = 0
for kind, cases in (("ats", ATS), ("upd", POPMAX)):
    for name, base, arg in cases:
        res = {}
        for label, cli in targets:
            c = cli["atsall"][f"c{abs(hash(name)) % 99999}"]
            c.drop()
            doc = {"_id": 1, "a": list(base)} if kind == "ats" else dict(base, _id=1)
            c.insert_one(doc)
            try:
                spec = {"$addToSet": {"a": arg}} if kind == "ats" else arg
                c.update_one({"_id": 1}, spec)
                d = c.find_one({"_id": 1})
                d.pop("_id")
                res[label] = norm(d)
            except Exception as e:
                res[label] = f"ERR {getattr(e, 'code', '?')}"
        if len(set(res.values())) > 1:
            bad += 1
            print(f"  DIVERGE {name:20s} " + " | ".join(f"{k}={v}" for k, v in res.items()))
print(f"\n{len(ATS) + len(POPMAX)} shapes, {bad} divergent")

sys.exit(1 if bad else 0)
