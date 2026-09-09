"""Query-language RESULT SETS: which documents each operator matches.

Every other probe here covers expressions, stages, errors or indexed lookups.
This one isolates the MATCH engine -- no indexes, one collection -- and for each
`(operator, argument)` compares the matched `_id` SET against mongod. A wrong
set is a silently wrong ANSWER, the worst class there is, and none of the other
probes can see it.

The corpus is one document per value CLASS with the class as its `_id`, so a
divergence names itself: `mongod-only: ['nan']` says more than a row count.
The argument list crosses every class against every comparison operator, which
is where the bugs live -- NaN, signed zero, bool-vs-int and missing-vs-null all
disagree with Python's `==` in ways CLAUDE.md records as recurring.

Run it against BOTH servers. The Python and Rust match engines are separate
ports, and the parity suites pin them to EACH OTHER -- which is satisfied by
both being wrong.

    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python \\
        tools/probes/query_result_sets.py

Set ``PROBE_SERVER`` to a running Rust server's URI to compare that one instead
of the embedded extension.
"""

from __future__ import annotations

import datetime
import sys
from pathlib import Path

from bson import Binary, Code, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp

sys.path.insert(0, str(Path(__file__).parent))
from _servers import probe_targets, report  # noqa: E402

OID = ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")
WHEN = datetime.datetime(2026, 1, 2, 3, 4, 5)

#: One document per value CLASS. `_id` is the label so a divergence names itself.
DOCS = [
    {"_id": "int0", "v": 0},
    {"_id": "int1", "v": 1},
    {"_id": "intneg", "v": -1},
    {"_id": "long", "v": Int64(2**40)},
    {"_id": "dbl", "v": 1.5},
    {"_id": "dblneg0", "v": -0.0},
    {"_id": "dbl0", "v": 0.0},
    {"_id": "nan", "v": float("nan")},
    {"_id": "inf", "v": float("inf")},
    {"_id": "neginf", "v": float("-inf")},
    {"_id": "dec", "v": Decimal128("1.5")},
    {"_id": "decnan", "v": Decimal128("NaN")},
    {"_id": "true", "v": True},
    {"_id": "false", "v": False},
    {"_id": "str", "v": "abc"},
    {"_id": "strempty", "v": ""},
    {"_id": "null", "v": None},
    {"_id": "missing", "w": 1},
    {"_id": "arr", "v": [1, 2, 3]},
    {"_id": "arrempty", "v": []},
    {"_id": "arrnull", "v": [None]},
    {"_id": "arrnested", "v": [[1, 2]]},
    {"_id": "arrmixed", "v": [1, "a", None]},
    {"_id": "doc", "v": {"k": 1}},
    {"_id": "docempty", "v": {}},
    {"_id": "docnested", "v": {"k": {"j": 2}}},
    {"_id": "oid", "v": OID},
    {"_id": "date", "v": WHEN},
    {"_id": "ts", "v": Timestamp(1, 1)},
    {"_id": "bin", "v": Binary(b"z", 0)},
    {"_id": "regex", "v": Regex("a", "i")},
    {"_id": "code", "v": Code("x=1")},
    {"_id": "mink", "v": MinKey()},
    {"_id": "maxk", "v": MaxKey()},
    {"_id": "docarr", "v": [{"k": 1}, {"k": 2}]},
]

ARGS = [
    1,
    0,
    -1,
    1.5,
    0.0,
    -0.0,
    float("nan"),
    float("inf"),
    True,
    False,
    "abc",
    "",
    None,
    [1, 2, 3],
    [],
    {"k": 1},
    {},
    OID,
    WHEN,
    Decimal128("1.5"),
    Int64(1),
    MinKey(),
    MaxKey(),
    Binary(b"z", 0),
    Timestamp(1, 1),
]
LABEL = {repr(a): a for a in ARGS}

FILTERS = []
for a in ARGS:
    r = repr(a)
    FILTERS.append((f"eq/{r}", {"v": a}))
    for op in ["$eq", "$ne", "$gt", "$gte", "$lt", "$lte"]:
        FILTERS.append((f"{op}/{r}", {"v": {op: a}}))
    FILTERS.append((f"$in/{r}", {"v": {"$in": [a]}}))
    FILTERS.append((f"$nin/{r}", {"v": {"$nin": [a]}}))
FILTERS += [
    ("$exists/true", {"v": {"$exists": True}}),
    ("$exists/false", {"v": {"$exists": False}}),
    ("$size/0", {"v": {"$size": 0}}),
    ("$size/3", {"v": {"$size": 3}}),
    ("$all/[1]", {"v": {"$all": [1]}}),
    ("$all/[]", {"v": {"$all": []}}),
    ("$mod", {"v": {"$mod": [2, 0]}}),
    ("$not$eq1", {"v": {"$not": {"$eq": 1}}}),
    ("$not$gt0", {"v": {"$not": {"$gt": 0}}}),
    ("$elemMatch", {"v": {"$elemMatch": {"$gt": 1}}}),
    ("$elemMatch doc", {"v": {"$elemMatch": {"k": 1}}}),
    ("$regex a", {"v": {"$regex": "a"}}),
    ("$regex ^a", {"v": {"$regex": "^a", "$options": "i"}}),
    ("dotted", {"v.k": 1}),
    ("dotted nested", {"v.k.j": 2}),
    ("dotted idx", {"v.0": 1}),
    ("dotted arr", {"v.k": {"$gt": 0}}),
    ("$or", {"$or": [{"v": 1}, {"v": "abc"}]}),
    ("$and", {"$and": [{"v": {"$gt": 0}}, {"v": {"$lt": 10}}]}),
    ("$nor", {"$nor": [{"v": 1}]}),
    ("$expr eq", {"$expr": {"$eq": ["$v", 1]}}),
    ("$expr gt", {"$expr": {"$gt": ["$v", 0]}}),
]
for t in [
    "double",
    "string",
    "object",
    "array",
    "binData",
    "undefined",
    "objectId",
    "bool",
    "date",
    "null",
    "regex",
    "javascript",
    "int",
    "timestamp",
    "long",
    "decimal",
    "minKey",
    "maxKey",
    "number",
]:
    FILTERS.append((f"$type/{t}", {"v": {"$type": t}}))


def _seed(client):
    db = client["queryresultsets"]
    db.drop_collection("c")
    db["c"].insert_many(DOCS)
    return db


def _ids(db, flt):
    try:
        return ("OK", tuple(sorted(d["_id"] for d in db["c"].find(flt, {"_id": 1}))))
    except Exception as exc:  # noqa: BLE001 -- the error IS the observation
        code = getattr(exc, "code", None)
        detail = getattr(exc, "details", {}) or {}
        return (code, detail.get("errmsg", str(exc)).split(":: caused by :: ")[-1][:90])


def main() -> int:
    with probe_targets() as (mongod, targets):
        want_db = _seed(mongod)
        ours = [(label, _seed(client)) for label, client in targets]
        divergent = {label: 0 for label, _ in targets}
        for name, flt in FILTERS:
            want = _ids(want_db, flt)
            for label, db in ours:
                got = _ids(db, flt)
                if got == want:
                    continue
                divergent[label] += 1
                print(f"<<< [{label}] {name}   {flt}")
                if want[0] == "OK" and got[0] == "OK":
                    # Name the documents, not the counts: which CLASS moved is
                    # the whole finding.
                    print(f"    mongod-only: {sorted(set(want[1]) - set(got[1]))}")
                    print(f"    ours-only:   {sorted(set(got[1]) - set(want[1]))}")
                else:
                    print(f"    mongod {want}")
                    print(f"    ours   {got}")
        return report("query result sets", len(FILTERS), divergent)


if __name__ == "__main__":
    raise SystemExit(main())
