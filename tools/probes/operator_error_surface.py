"""Every query / update operator crossed with every pathological argument.

The point is COVERAGE, not cleverness: 20 query operators, 4 document
operators and 14 update operators against a fixed set of 21 bad arguments, in
three field positions, is 2,226 shapes -- and 583 of them disagreed with mongod
8.2.11 when this was written. A hand-picked sample of 32 had found 12, which is
the reason this exists: the sample missed `$bits*` entirely, and `$bits*` turned
out to be answering the wrong DOCUMENTS, not just the wrong message.

What it found, beyond wording:

* `$bits*` accepted a plain int and nothing else -- no array elements, no
  doubles, no Decimal128, no BinData values, and BinData masks rejected. 35 of
  44 shapes wrong.
* `{v: {$regex: BinData}}` and `{v: {$type: Code(...)}}` reached the client as
  `1 internal server error`.
* `{v: {$not: {a: 1}}}` MATCHED -- an ordinary field name degraded to equality
  and `$not` negated it -- where mongod refuses the query.
* `$rename` APPLIED when its target was a `bson.Code`.

Run it against BOTH servers. They have separate matchers, and the engine-parity
suites are satisfied by both being wrong together.

    # Python server (an embedded one is started when PROBE_SERVER is unset)
    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python tools/probes/operator_error_surface.py

    # Rust server
    crates/secantusdb/target/debug/secantusd-rs --port 27055 --storage-path /tmp/rs &
    PROBE_SERVER="mongodb://127.0.0.1:27055" \
      PROBE_MONGOD="mongodb://127.0.0.1:27041" \
      uv run python tools/probes/operator_error_surface.py

Set SWEEP_OUT=<path> to dump every divergence with both messages.
"""

import collections
import datetime
import os
import sys
import tempfile

import pymongo
from bson import Binary, Code, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp

from secantus import SecantusDBServer

mon = pymongo.MongoClient(os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041"))
srv = SecantusDBServer(port=0, storage_path=tempfile.mkdtemp())
srv.start()
py = pymongo.MongoClient(srv.uri)
rust = pymongo.MongoClient(os.environ["PROBE_SERVER"]) if os.environ.get("PROBE_SERVER") else None
DBN = "sweep"
DOC = {
    "_id": 1,
    "n": 5,
    "s": "abc",
    "arr": [1, 2, 3],
    "d": {"x": 1},
    "dec": Decimal128("2.5"),
    "b": True,
    "nul": None,
    "dt": datetime.datetime(2020, 1, 1),
}

#: The arguments each operator is crossed with. Zero and negative-zero
#: `Decimal128` are here because their ABSENCE hid a real bug: `$exists` read
#: every Decimal128 as truthy, and the corpus carried only `Decimal128("1.5")`,
#: which is truthy anyway. A value that is falsy in one BSON type and truthy in
#: another is exactly what this list needs to carry.
BAD = [
    "x",
    "",
    5,
    1.5,
    -1,
    0,
    True,
    False,
    None,
    [],
    [1],
    {},
    {"a": 1},
    Decimal128("1.5"),
    Decimal128("0"),
    Decimal128("-0"),
    Decimal128("NaN"),
    Regex("a", ""),
    Binary(b"z"),
    Binary(b""),
    ObjectId(),
    MinKey(),
    MaxKey(),
    Timestamp(1, 1),
    Int64(3),
    datetime.datetime(2020, 1, 1),
    Code("x=1"),
    float("nan"),
    float("inf"),
]

QUERY_OPS = [
    "$eq",
    "$ne",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$in",
    "$nin",
    "$exists",
    "$type",
    "$size",
    "$all",
    "$mod",
    "$regex",
    "$elemMatch",
    "$not",
    "$bitsAllSet",
    "$bitsAnySet",
    "$bitsAllClear",
    "$bitsAnyClear",
]
DOC_OPS = ["$and", "$or", "$nor", "$expr"]
UPDATE_OPS = [
    "$set",
    "$unset",
    "$inc",
    "$mul",
    "$min",
    "$max",
    "$push",
    "$pull",
    "$addToSet",
    "$pop",
    "$rename",
    "$bit",
    "$currentDate",
    "$setOnInsert",
]


def q_run(cli, q):
    coll = cli[DBN].c
    try:
        return ("ok", sorted(d["_id"] for d in coll.find(q, {"_id": 1})))
    except Exception as e:
        return (
            getattr(e, "code", "ERR"),
            str(e).split(", full error")[0].split(":: caused by :: ")[-1],
        )


def u_run(cli, upd):
    coll = cli[DBN].c
    coll.delete_many({})
    coll.insert_one(dict(DOC))
    try:
        coll.update_one({"_id": 1}, upd)
        return ("ok", "applied")
    except Exception as e:
        return (
            getattr(e, "code", "ERR"),
            str(e).split(", full error")[0].split(":: caused by :: ")[-1],
        )


for cli in [mon, py] + ([rust] if rust else []):
    cli.drop_database(DBN)
    cli[DBN].c.insert_one(dict(DOC))

shapes, pybad, rsbad = 0, [], []
for op in QUERY_OPS:
    for arg in BAD:
        for field in ("n", "arr", "s"):
            q = {field: {op: arg}}
            shapes += 1
            m, p = q_run(mon, q), q_run(py, q)
            if m != p:
                pybad.append((f"{{{field}: {{{op}: {arg!r}}}}}", m, p))
            if rust:
                r = q_run(rust, q)
                if m != r:
                    rsbad.append((f"{{{field}: {{{op}: {arg!r}}}}}", m, r))
for op in DOC_OPS:
    for arg in BAD:
        q = {op: arg}
        shapes += 1
        m, p = q_run(mon, q), q_run(py, q)
        if m != p:
            pybad.append((f"{{{op}: {arg!r}}}", m, p))
        if rust:
            r = q_run(rust, q)
            if m != r:
                rsbad.append((f"{{{op}: {arg!r}}}", m, r))
for op in UPDATE_OPS:
    for arg in BAD:
        for field in ("n", "arr", "s"):
            upd = {op: {field: arg}}
            shapes += 1
            m, p = u_run(mon, upd), u_run(py, upd)
            if m != p:
                pybad.append((f"{{{op}: {{{field}: {arg!r}}}}}", m, p))
            if rust:
                r = u_run(rust, upd)
                if m != r:
                    rsbad.append((f"{{{op}: {{{field}: {arg!r}}}}}", m, r))

print(
    f"\n=== {shapes} shapes: python {len(pybad)} divergent"
    + (f", rust {len(rsbad)} divergent" if rust else "")
    + " ===\n"
)
for name, rows in (("PYTHON", pybad), ("RUST", rsbad)):
    if not rows:
        continue
    buckets = collections.Counter(r[0].split(":")[0].split("{")[-1] or r[0] for r in rows)
    print(f"--- {name}, by operator ---")
    for k, v in buckets.most_common(20):
        print(f"  {v:4}  {k}")
    print()
with open(os.environ.get("SWEEP_OUT", os.devnull), "w") as out:
    for name, rows in (("PYTHON", pybad), ("RUST", rsbad)):
        for shape, expected, got in rows:
            out.write(f"{name} {shape}\n   mongod {expected}\n   server {got}\n")
srv.stop()
sys.exit(1 if (pybad or rsbad) else 0)
