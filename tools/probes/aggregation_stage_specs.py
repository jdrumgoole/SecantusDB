"""Every aggregation STAGE crossed with every pathological argument.

Stage specs are a separate surface from the operator ones
(`operator_error_surface.py`) and had never been swept. First run: **167 of 725
shapes divergent** against mongod 8.2.11.

What it found beyond wording:

* `$unset` validated NOTHING -- a non-string, non-array spec raised a bare
  TypeError (`1 internal server error`), an empty string and an empty array were
  accepted and did nothing, and a document spec silently iterated its KEYS.
* `$out: ""` and `$merge: ""` were accepted and wrote to a nameless collection.
* `$documents` ran against a collection, where mongod refuses it outright --
  it is a collection-LESS stage.
* `$count`, `$out` and `$merge` crashed on a `bson.Code` spec, and `$unset`
  silently accepted one, because `Code` subclasses `str`.

Run it against BOTH servers -- they have separate pipelines.

    # Python server (an embedded one is started when PROBE_SERVER is unset)
    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python tools/probes/aggregation_stage_specs.py

    # Rust server
    crates/secantusdb/target/debug/secantusd-rs --port 27055 --storage-path /tmp/rs &
    PROBE_SERVER="mongodb://127.0.0.1:27055" \
      PROBE_MONGOD="mongodb://127.0.0.1:27041" \
      uv run python tools/probes/aggregation_stage_specs.py

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
# The Rust server has its OWN stage-spec validation, so a fix on the Python
# side proves nothing about it. Optional so the probe still runs without the
# extension built.
rust = pymongo.MongoClient(os.environ["PROBE_SERVER"]) if os.environ.get("PROBE_SERVER") else None
TARGETS = [("python", py)] + ([("rust", rust)] if rust is not None else [])
DBN = "stagesweep"
DOC = {"_id": 1, "n": 5, "s": "abc", "arr": [1, 2, 3], "d": {"x": 1}}

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
    Regex("a", ""),
    Binary(b"z"),
    ObjectId(),
    MinKey(),
    MaxKey(),
    Timestamp(1, 1),
    Int64(3),
    datetime.datetime(2020, 1, 1),
    Code("x=1"),
    float("nan"),
]

STAGES = [
    "$match",
    "$project",
    "$limit",
    "$skip",
    "$sort",
    "$group",
    "$unwind",
    "$count",
    "$addFields",
    "$set",
    "$unset",
    "$replaceRoot",
    "$replaceWith",
    "$sample",
    "$sortByCount",
    "$facet",
    "$bucket",
    "$lookup",
    "$redact",
    "$densify",
    "$fill",
    "$setWindowFields",
    "$graphLookup",
    "$bucketAuto",
    "$geoNear",
    "$unionWith",
    "$documents",
    "$out",
    "$merge",
]

for _label, _cli in [("mongod", mon), *TARGETS]:
    _cli.drop_database(DBN)
    _cli[DBN].c.insert_one(dict(DOC))


def run(cli, stage):
    try:
        return ("ok", [d.get("_id") for d in cli[DBN].c.aggregate([stage])])
    except Exception as e:
        return (
            getattr(e, "code", "ERR"),
            str(e).split(", full error")[0].split(":: caused by :: ")[-1],
        )


tot = 0
rows = []
bad = collections.Counter()
for st in STAGES:
    for arg in BAD:
        tot += 1
        stage = {st: arg}
        expected = run(mon, stage)
        got = {label: run(cli, stage) for label, cli in TARGETS}
        off = {label: v for label, v in got.items() if v != expected}
        if off:
            for label in off:
                bad[label] += 1
            rows.append((f"{{{st}: {arg!r}}}", expected, got, set(off)))

summary = ", ".join(f"{label} {bad[label]}" for label, _ in TARGETS)
print(f"\n=== {tot} stage shapes: {summary} divergent ===\n")
for label, _ in TARGETS:
    hits = [r for r in rows if label in r[3]]
    if not hits:
        continue
    print(f"  --- {label} ---")
    for k, v in collections.Counter(r[0].split(":")[0].strip("{") for r in hits).most_common(14):
        print(f"  {v:4}  {k}")
with open(os.environ.get("SWEEP_OUT", os.devnull), "w") as out:
    for shape, expected, got, off in rows:
        out.write(f"{shape}\n   mongod {expected}\n")
        for label, v in got.items():
            out.write(f"   {label:6s} {v}{'  <-- diverges' if label in off else ''}\n")
srv.stop()
sys.exit(1 if rows else 0)
sys.exit(1 if bad else 0)
