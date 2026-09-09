"""What `find`'s `projection` actually returns, compared against mongod.

`apply_projection` is on the hot read path of every `find` and had **no probe
at all**. A wrong projection is a silently wrong answer: the query matches the
right documents and then hands back the wrong fields.

The surface is bigger than it looks, because projection is where several rules
INTERACT:

* inclusion and exclusion cannot be mixed, except that `_id` may always be
  excluded — and the error for mixing them is its own shape;
* a dotted path projects a SUBTREE, and the parent document survives with only
  that branch -- and a path that is an ANCESTOR of another projected path is
  refused outright (`31249` / `31250`), rather than narrowing it;
* `$slice`, `$elemMatch` and the positional `$` each rewrite an ARRAY rather
  than selecting a field, and the positional form depends on the QUERY;
* a computed field (`$literal`, an expression, a nested `$cond`) turns an
  otherwise-exclusion projection into an inclusion one.

Values matter as much as paths, so the corpus carries one document per value
CLASS with `_id` naming it — the classes that have bitten elsewhere in this
directory (missing versus null, empty array versus empty document, arrays of
documents) are the ones projection is most likely to disagree on.

**Field ORDER is compared.** mongod emits projected fields in a specific
sequence — `_id` first, then the document's own order, with computed fields
appended — and `==` on a dict ignores that entirely. It is what a driver
renders, so it is behaviour.

    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python \\
        tools/probes/projection_results.py

Set ``PROBE_SERVER`` to a running Rust server's URI to compare that one instead
of the embedded extension.
"""

from __future__ import annotations

import datetime as dt
import sys
from pathlib import Path

import pymongo
from bson import Decimal128, MaxKey, MinKey, ObjectId

sys.path.insert(0, str(Path(__file__).parent))
from _servers import probe_targets, report  # noqa: E402

OID = ObjectId("64b7f9a2c1d2e3f4a5b6c7d8")
WHEN = dt.datetime(2026, 1, 2, 3, 4, 5)

DOCS = [
    {"_id": "flat", "a": 1, "b": 2, "c": 3},
    {"_id": "nested", "a": {"x": 1, "y": {"z": 2}}, "b": 5},
    {"_id": "emptysub", "a": {}, "b": 1},
    {"_id": "nullsub", "a": None, "b": 1},
    {"_id": "missing", "b": 1},
    {"_id": "arr", "a": [1, 2, 3, 4, 5], "b": 1},
    {"_id": "emptyarr", "a": [], "b": 1},
    {"_id": "arrdoc", "a": [{"x": 1, "y": 9}, {"x": 2, "y": 8}, {"x": 3, "y": 7}], "b": 1},
    {"_id": "arrnested", "a": [[1, 2], [3, 4]], "b": 1},
    {"_id": "arrnull", "a": [None, 1], "b": 1},
    {"_id": "scalarpath", "a": 5, "b": 1},
    {"_id": "types", "a": {"n": float("nan"), "d": Decimal128("1.5"), "o": OID, "t": WHEN}, "b": 1},
    {"_id": "keys", "a": {"mk": MinKey(), "xk": MaxKey(), "e": {}, "z": []}, "b": 1},
    {"_id": "deep", "a": {"x": {"y": {"z": {"w": 1}}}}, "b": 1},
]

#: `(name, filter, projection)`. The filter matters for the positional `$`.
CASES: list[tuple[str, dict, dict]] = [
    # --- inclusion / exclusion, and the `_id` special case ---
    ("include-one", {}, {"a": 1}),
    ("include-two", {}, {"a": 1, "b": 1}),
    ("include-no-id", {}, {"a": 1, "_id": 0}),
    ("exclude-one", {}, {"a": 0}),
    ("exclude-id-only", {}, {"_id": 0}),
    ("include-id-only", {}, {"_id": 1}),
    ("exclude-two", {}, {"a": 0, "b": 0}),
    # Truthiness of the spec value, not just 0/1.
    ("include-true", {}, {"a": True}),
    ("exclude-false", {}, {"a": False}),
    ("include-number", {}, {"a": 2}),
    ("exclude-zero-double", {}, {"a": 0.0}),
    # --- dotted paths ---
    ("dotted-include", {}, {"a.x": 1}),
    ("dotted-exclude", {}, {"a.x": 0}),
    ("dotted-deep", {}, {"a.x.y.z": 1}),
    ("dotted-into-array", {}, {"a.x": 1, "_id": 1}),
    ("dotted-missing", {}, {"a.nosuch": 1}),
    ("dotted-numeric", {}, {"a.0": 1}),
    ("dotted-two-branches", {}, {"a.x": 1, "a.y": 1}),
    ("dotted-parent-and-child", {}, {"a": 1, "a.x": 1}),
    # --- $slice ---
    ("slice-n", {}, {"a": {"$slice": 2}}),
    ("slice-negative", {}, {"a": {"$slice": -2}}),
    ("slice-skip-limit", {}, {"a": {"$slice": [1, 2]}}),
    ("slice-past-end", {}, {"a": {"$slice": 99}}),
    ("slice-with-include", {}, {"a": {"$slice": 2}, "b": 1}),
    # --- $elemMatch ---
    ("elemmatch", {}, {"a": {"$elemMatch": {"x": {"$gt": 1}}}}),
    ("elemmatch-nomatch", {}, {"a": {"$elemMatch": {"x": {"$gt": 99}}}}),
    ("elemmatch-scalar", {}, {"a": {"$elemMatch": {"$gt": 2}}}),
    # --- the positional `$`, which depends on the QUERY ---
    ("positional", {"a.x": 2}, {"a.$": 1}),
    ("positional-scalar", {"a": 3}, {"a.$": 1}),
    # --- computed fields ---
    ("computed-literal", {}, {"lit": {"$literal": 7}}),
    ("computed-expr", {}, {"twice": {"$multiply": ["$b", 2]}}),
    ("computed-with-exclude-id", {}, {"lit": {"$literal": 1}, "_id": 0}),
    ("computed-rename", {}, {"renamed": "$a"}),
    ("computed-over-missing", {}, {"t": {"$type": "$a"}}),
    ("computed-cond", {}, {"c": {"$cond": [{"$gt": ["$b", 1]}, "hi", "lo"]}}),
    # --- the shapes mongod refuses ---
    ("mix-include-exclude", {}, {"a": 1, "b": 0}),
    ("empty-projection", {}, {}),
]


def _seed(client):
    db = client["projresults"]
    db.drop_collection("c")
    db["c"].insert_many([dict(d) for d in DOCS])
    return db


def _run(db, filt, projection):
    try:
        rows = db["c"].find(filt, projection).sort("_id", 1)
        # `repr` preserves KEY ORDER, which a content compare silently drops.
        return ("OK", [repr(r) for r in rows])
    except pymongo.errors.OperationFailure as exc:
        return ("ERR", exc.code, str(exc.details.get("errmsg", ""))[:120])


def main() -> int:
    with probe_targets() as (mongod, targets):
        want_db = _seed(mongod)
        # SELF-CHECK: a projection whose answer needs no server to predict. If
        # mongod disagrees the harness is broken and every number below is
        # worthless.
        check = _run(want_db, {"_id": "flat"}, {"a": 1})
        if check != ("OK", [repr({"_id": "flat", "a": 1})]):
            print(f"SELF-CHECK FAILED on mongod: {check}", file=sys.stderr)
            return 2
        ours = [(label, _seed(client)) for label, client in targets]
        divergent = {label: 0 for label, _ in targets}
        for name, filt, projection in CASES:
            want = _run(want_db, filt, projection)
            for label, db in ours:
                got = _run(db, filt, projection)
                if got == want:
                    continue
                divergent[label] += 1
                print(f"<<< [{label}] {name}   {projection}")
                if want[0] == "OK" and got[0] == "OK":
                    for i, (w, g) in enumerate(zip(want[1], got[1], strict=False)):
                        if w != g:
                            print(f"    row {i}\n      mongod {w[:130]}\n      ours   {g[:130]}")
                            break
                    else:
                        print(f"    row count: mongod {len(want[1])}, ours {len(got[1])}")
                else:
                    print(f"    mongod {want}\n    ours   {got}")
        return report("projection results", len(CASES), divergent)


if __name__ == "__main__":
    raise SystemExit(main())
