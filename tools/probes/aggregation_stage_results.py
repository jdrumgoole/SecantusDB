"""What each aggregation STAGE actually emits, compared against mongod.

`aggregation_stage_specs.py` next door crosses every stage with pathological
arguments and compares the ERRORS. This compares the DOCUMENTS a well-formed
stage produces, which is a different surface and the one where a silently wrong
answer lives — the pipeline succeeds, the shape looks right, and the rows are
wrong.

That distinction has paid off twice already in this directory, both times on the
first run: `query_result_sets.py` (which documents match, versus the operator
ERROR probes) found a crash, and `update_result_documents.py` (what the document
becomes, versus `update_operators.py`'s errors) found several wrong writes.
Aggregation was the remaining half.

**Field ORDER is compared**, not just content. `==` on a document ignores key
order, and an ordering divergence is invisible to every content comparison —
which is how 28 of 34 change-stream events stayed wrong for a whole campaign.
Stages that BUILD documents (`$group`, `$project`, `$replaceRoot`, `$facet`) are
exactly where mongod's key sequence is a contract a driver renders.

**Row order is compared only where the pipeline DEFINES one.** mongod does not
promise an order otherwise, and comparing an unpromised one reports noise as a
finding. An earlier version of this probe appended `{$sort: {_id: 1}}` to every
pipeline to make the order deterministic — and that trailing sort CHANGED
mongod's answer: `$group` alone returns `lo: MinKey()`, while `$group` followed
by `$sort` returns `lo: {'': MinKey()}`. A normalising cast in front of every
assertion is the second way probes lie in this repo's own catalogue, and it lied
here on the first run. Everything else is compared as a multiset.

Some ARRAY values have no promised order either — `$addToSet` builds a set, and
`$graphLookup` does not order its `as` array — so those fields are sorted before
comparison, per case, rather than globally.

**Deliberately excluded:** `$sample`, `$rand` and anything reading the clock —
their answers are not reproducible, so a divergence would be noise. `$out` and
`$merge` write collections rather than emitting rows.

The corpus is one document per value CLASS, with `_id` naming the class, so a
divergence names itself rather than printing a row number.

    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python \\
        tools/probes/aggregation_stage_results.py

Set ``PROBE_SERVER`` to a running Rust server's URI to compare that one instead
of the embedded extension.
"""

from __future__ import annotations

import datetime as dt
import sys
from pathlib import Path

import pymongo
from bson import Decimal128, Int64, MaxKey, MinKey, ObjectId

sys.path.insert(0, str(Path(__file__).parent))
from _servers import probe_targets, report  # noqa: E402

OID = ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")
WHEN = dt.datetime(2026, 1, 2, 3, 4, 5)

#: One document per value class. `_id` is the label, so a wrong row names itself.
DOCS = [
    {"_id": "int", "g": "a", "v": 1, "arr": [1, 2, 3], "sub": {"k": 1}},
    {"_id": "int2", "g": "a", "v": 2, "arr": [], "sub": {"k": 2}},
    {"_id": "dbl", "g": "b", "v": 1.5, "arr": [1], "sub": {"k": 3}},
    {"_id": "negzero", "g": "b", "v": -0.0, "arr": [0], "sub": {}},
    {"_id": "nan", "g": "c", "v": float("nan"), "arr": [None], "sub": {"k": None}},
    {"_id": "inf", "g": "c", "v": float("inf"), "arr": [[1, 2]], "sub": {"k": [1]}},
    {"_id": "dec", "g": "d", "v": Decimal128("1.5"), "arr": [Decimal128("2")]},
    {"_id": "long", "g": "d", "v": Int64(2**40), "arr": [Int64(1)]},
    {"_id": "str", "g": "e", "v": "abc", "arr": ["x", "y"]},
    {"_id": "null", "g": "e", "v": None, "arr": [1, None]},
    {"_id": "missing", "g": "f", "arr": [1]},
    {"_id": "bool", "g": "f", "v": True, "arr": [True, False]},
    {"_id": "date", "g": "g", "v": WHEN, "arr": [WHEN]},
    {"_id": "oid", "g": "g", "v": OID, "arr": [OID]},
    {"_id": "doc", "g": "h", "v": {"n": 1}, "arr": [{"n": 1}, {"n": 2}]},
    {"_id": "minkey", "g": "h", "v": MinKey(), "arr": [MinKey(), MaxKey()]},
]

#: A second collection for `$lookup` / `$graphLookup` / `$unionWith`.
SIDE = [
    {"_id": 1, "key": "a", "label": "alpha", "parent": None},
    {"_id": 2, "key": "b", "label": "beta", "parent": "a"},
    {"_id": 3, "key": "c", "label": "gamma", "parent": "b"},
    {"_id": 4, "key": None, "label": "nullkey", "parent": None},
]

#: Stages whose pipeline DEFINES a row order, so the order is compared.
ORDERED = {
    "sort-asc",
    "sort-desc",
    "sort-multi",
    "limit-skip",
    "setwindowfields",
    "setwindowfields-rank",
    "fill-locf",
    "densify",
    "bucket",
    "bucketauto",
}

#: Fields holding an array whose ORDER mongod does not promise.
UNORDERED_FIELDS = {
    "group-addtoset": ("set",),
    "graphlookup": ("chain",),
    # `$sortByCount` ties are mongod's hash order, not insertion order and not
    # sorted: probed both ways round on 8.2.11 (2026-09-09) and it answers the
    # same sequence regardless of insert order, so it is an implementation
    # detail rather than a contract. Every count in this corpus is equal, so
    # comparing it would report noise forever.
    "facet": ("byG",),
}

STAGES: list[tuple[str, list]] = [
    # --- reshaping ---
    ("project-include", [{"$project": {"v": 1}}]),
    ("project-exclude", [{"$project": {"v": 0}}]),
    ("project-computed", [{"$project": {"double": {"$multiply": ["$v", 2]}}}]),
    ("project-nested", [{"$project": {"sub.k": 1}}]),
    ("project-rename", [{"$project": {"renamed": "$v"}}]),
    ("addfields", [{"$addFields": {"extra": {"$type": "$v"}}}]),
    ("set-overwrite", [{"$set": {"v": "$g"}}]),
    ("unset", [{"$unset": "arr"}]),
    ("replaceroot", [{"$replaceRoot": {"newRoot": {"only": "$g"}}}]),
    ("replacewith-sub", [{"$match": {"sub": {"$exists": True}}}, {"$replaceWith": "$sub"}]),
    # --- $unwind, including the options that change row counts ---
    ("unwind", [{"$unwind": "$arr"}]),
    ("unwind-preserve", [{"$unwind": {"path": "$arr", "preserveNullAndEmptyArrays": True}}]),
    ("unwind-index", [{"$unwind": {"path": "$arr", "includeArrayIndex": "i"}}]),
    (
        "unwind-index-preserve",
        [
            {
                "$unwind": {
                    "path": "$arr",
                    "includeArrayIndex": "i",
                    "preserveNullAndEmptyArrays": True,
                }
            }
        ],
    ),
    ("unwind-scalar", [{"$unwind": "$v"}]),
    # --- $group and its accumulators ---
    ("group-sum", [{"$group": {"_id": "$g", "n": {"$sum": "$v"}}}]),
    ("group-avg", [{"$group": {"_id": "$g", "n": {"$avg": "$v"}}}]),
    ("group-min-max", [{"$group": {"_id": "$g", "lo": {"$min": "$v"}, "hi": {"$max": "$v"}}}]),
    ("group-count", [{"$group": {"_id": "$g", "n": {"$count": {}}}}]),
    ("group-push", [{"$group": {"_id": "$g", "all": {"$push": "$v"}}}]),
    ("group-addtoset", [{"$group": {"_id": "$g", "set": {"$addToSet": "$v"}}}]),
    ("group-first-last", [{"$group": {"_id": "$g", "f": {"$first": "$v"}, "l": {"$last": "$v"}}}]),
    ("group-null-id", [{"$group": {"_id": None, "n": {"$sum": 1}}}]),
    ("group-by-value", [{"$group": {"_id": "$v", "n": {"$sum": 1}}}]),
    ("group-by-doc", [{"$group": {"_id": {"g": "$g", "t": {"$type": "$v"}}, "n": {"$sum": 1}}}]),
    ("group-stddev", [{"$group": {"_id": "$g", "s": {"$stdDevPop": "$v"}}}]),
    ("group-mergeobjects", [{"$group": {"_id": "$g", "m": {"$mergeObjects": "$sub"}}}]),
    (
        "group-topn",
        [{"$group": {"_id": "$g", "t": {"$topN": {"n": 2, "sortBy": {"v": 1}, "output": "$v"}}}}],
    ),
    # --- bucketing and faceting ---
    ("sortbycount", [{"$sortByCount": "$g"}]),
    (
        "bucket",
        [
            {"$match": {"v": {"$type": "number"}}},
            {"$bucket": {"groupBy": "$v", "boundaries": [0, 1, 2, 100], "default": "other"}},
        ],
    ),
    (
        "bucketauto",
        [{"$match": {"v": {"$type": "number"}}}, {"$bucketAuto": {"groupBy": "$v", "buckets": 2}}],
    ),
    ("facet", [{"$facet": {"byG": [{"$sortByCount": "$g"}], "total": [{"$count": "n"}]}}]),
    ("count", [{"$count": "total"}]),
    # --- joins ---
    (
        "lookup-simple",
        [{"$lookup": {"from": "side", "localField": "g", "foreignField": "key", "as": "j"}}],
    ),
    (
        "lookup-pipeline",
        [
            {
                "$lookup": {
                    "from": "side",
                    "let": {"gg": "$g"},
                    "pipeline": [
                        {"$match": {"$expr": {"$eq": ["$key", "$$gg"]}}},
                        {"$project": {"label": 1}},
                    ],
                    "as": "j",
                }
            }
        ],
    ),
    (
        "lookup-unwound",
        [
            {"$lookup": {"from": "side", "localField": "g", "foreignField": "key", "as": "j"}},
            {"$unwind": {"path": "$j", "preserveNullAndEmptyArrays": True}},
        ],
    ),
    (
        "graphlookup",
        [
            {
                "$graphLookup": {
                    "from": "side",
                    "startWith": "$g",
                    "connectFromField": "parent",
                    "connectToField": "key",
                    "as": "chain",
                }
            }
        ],
    ),
    (
        "unionwith",
        [
            {"$project": {"g": 1}},
            {"$unionWith": {"coll": "side", "pipeline": [{"$project": {"key": 1}}]}},
        ],
    ),
    # --- window functions and gap filling ---
    (
        "setwindowfields",
        [
            {"$match": {"v": {"$type": "number"}}},
            {
                "$setWindowFields": {
                    "partitionBy": "$g",
                    "sortBy": {"_id": 1},
                    "output": {
                        "run": {"$sum": "$v", "window": {"documents": ["unbounded", "current"]}}
                    },
                }
            },
        ],
    ),
    (
        "setwindowfields-rank",
        [{"$setWindowFields": {"sortBy": {"_id": 1}, "output": {"r": {"$rank": {}}}}}],
    ),
    ("fill-value", [{"$fill": {"output": {"v": {"value": 0}}}}]),
    ("fill-locf", [{"$fill": {"sortBy": {"_id": 1}, "output": {"v": {"method": "locf"}}}}]),
    (
        "densify",
        [
            {"$match": {"_id": {"$in": ["int", "int2"]}}},
            {"$densify": {"field": "v", "range": {"step": 1, "bounds": "full"}}},
        ],
    ),
    # --- ordering and slicing ---
    ("sort-asc", [{"$sort": {"v": 1}}]),
    ("sort-desc", [{"$sort": {"v": -1}}]),
    ("sort-multi", [{"$sort": {"g": 1, "v": -1}}]),
    ("limit-skip", [{"$sort": {"_id": 1}}, {"$skip": 2}, {"$limit": 3}]),
    ("redact", [{"$redact": {"$cond": [{"$eq": ["$g", "a"]}, "$$KEEP", "$$PRUNE"]}}]),
]

#: A case whose answer is known without a server. If mongod disagrees with THIS,
#: the harness is broken and every other number in the run is worthless.
SELF_CHECK = ("count", [{"$count": "total"}], [{"total": len(DOCS)}])


def _seed(client):
    db = client["aggresults"]
    db.drop_collection("c")
    db.drop_collection("side")
    db["c"].insert_many([dict(d) for d in DOCS])
    db["side"].insert_many([dict(d) for d in SIDE])
    return db


def _normalise(row, unordered_fields):
    """Sort the arrays whose order mongod does not promise, in place."""
    for field in unordered_fields:
        value = row.get(field)
        if isinstance(value, list):
            row[field] = sorted(value, key=repr)
    return row


def _run(db, pipeline, ordered, unordered_fields):
    try:
        rows = list(db["c"].aggregate(pipeline))
    except pymongo.errors.OperationFailure as exc:
        return ("ERR", exc.code)
    # `repr` of a dict preserves KEY ORDER, which is the half a content compare
    # would silently drop.
    reprs = [repr(_normalise(r, unordered_fields)) for r in rows]
    # Only a pipeline that DEFINES an order gets its order compared.
    return ("OK", reprs if ordered else sorted(reprs))


def main() -> int:
    with probe_targets() as (mongod, targets):
        want_db = _seed(mongod)
        name, pipeline, expected = SELF_CHECK
        got = _run(want_db, pipeline, True, ())
        if got != ("OK", [repr(d) for d in expected]):
            print(f"SELF-CHECK FAILED on mongod ({name}): {got}", file=sys.stderr)
            print("The harness is wrong; every other number here is worthless.", file=sys.stderr)
            return 2
        ours = [(label, _seed(client)) for label, client in targets]
        divergent = {label: 0 for label, _ in targets}
        for stage, pipeline in STAGES:
            ordered = stage in ORDERED
            unordered_fields = UNORDERED_FIELDS.get(stage, ())
            want = _run(want_db, pipeline, ordered, unordered_fields)
            for label, db in ours:
                got = _run(db, pipeline, ordered, unordered_fields)
                if got == want:
                    continue
                divergent[label] += 1
                print(f"<<< [{label}] {stage}")
                if want[0] == "OK" and got[0] == "OK":
                    if len(want[1]) != len(got[1]):
                        print(f"    row count: mongod {len(want[1])}, ours {len(got[1])}")
                    for i, (w, g) in enumerate(zip(want[1], got[1], strict=False)):
                        if w != g:
                            print(f"    row {i}\n      mongod {w[:150]}\n      ours   {g[:150]}")
                            break
                else:
                    print(f"    mongod {want}\n    ours   {got}")
        return report("aggregation stage results", len(STAGES), divergent)


if __name__ == "__main__":
    raise SystemExit(main())
