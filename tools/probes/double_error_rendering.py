"""How each server renders a DOUBLE inside an error message.

mongod has TWO renderings and they are not interchangeable (measured 8.2.11,
2026-09-07):

  VALUE form  (`Value::toString`)  -- C's `%g`, precision 6: `-0`, `-1`,
      `1.23457e+06`, `-2.14748e+09`, `0.000123457`.
  SPEC form   (a stage's echoed specification) -- the shortest round-trip
      form, keeping a whole double's `.0`: `-0.0`, `-1.0`, `1234567.0`,
      `-2147483648.0`.

One renderer was serving both, so any single rule is wrong for half the
messages, and switching the shared renderer fixes the value messages while
silently breaking `$graphLookup` -- which is why this probe covers BOTH
vocabularies rather than the one that was wrong.

Run it like the other probes. `PROBE_MONGOD` points at the reference server;
the Rust column is read from `/tmp/rust_addr.txt` (or `PROBE_SERVER`) and is
skipped with a note when no Rust server is up, so a clean run in a checkout
that has not built the extension is never mistaken for a compared one.
"""

import os
import sys
import tempfile

import pymongo

REF = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
VALS = [-0.0, -1.0, 1.5, 1234567.0, -2147483648.0, 1e308, 0.000123456789, 100.0]


def cut(msg, marker, tail):
    if marker not in msg:
        return msg[:44]
    return msg.split(marker)[-1].split(tail)[0]


CASES = [
    (
        "VALUE $mergeObjects",
        lambda v: [{"$project": {"_id": 0, "r": {"$mergeObjects": v}}}],
        "but input ",
        " is of type",
    ),
    (
        "VALUE $replaceRoot",
        lambda v: [{"$replaceRoot": {"newRoot": {"$literal": v}}}],
        "value was: ",
        ". Type of",
    ),
    ("VALUE $ln", lambda v: [{"$project": {"_id": 0, "r": {"$ln": v}}}], "but is ", "\x00"),
    ("VALUE $log10", lambda v: [{"$project": {"_id": 0, "r": {"$log10": v}}}], "but is ", "\x00"),
    ("SPEC  $firstN", lambda v: [{"$addFields": {"x": {"$firstN": v}}}], "found $firstN: ", "\x00"),
    (
        "SPEC  $graphLookup",
        lambda v: [
            {
                "$graphLookup": {
                    "startWith": v,
                    "connectFromField": "a",
                    "connectToField": "b",
                    "as": "c",
                }
            }
        ],
        "startWith: ",
        ",",
    ),
]


def run(coll, pipeline):
    try:
        list(coll.aggregate(pipeline))
        return "NO ERROR"
    except pymongo.errors.OperationFailure as exc:
        return str(exc).split(", full error")[0]


def targets():
    out = [("mongod", pymongo.MongoClient(REF, directConnection=True)["dr"]["t"])]
    sys.path.insert(0, "src")
    from secantus import SecantusDBServer

    srv = SecantusDBServer(port=0, storage_path=tempfile.mkdtemp(prefix="drpy"))
    srv.start()
    h, p = srv.address
    out.append(("python", pymongo.MongoClient(h, p, directConnection=True)["dr"]["t"]))
    rust_uri = os.environ.get("PROBE_SERVER")
    if not rust_uri:
        try:
            with open("/tmp/rust_addr.txt") as handle:
                port = int(handle.read().split(", ")[1].rstrip(")"))
            rust_uri = f"mongodb://127.0.0.1:{port}"
        except OSError:
            rust_uri = None
    if rust_uri:
        out.append(("rust", pymongo.MongoClient(rust_uri, directConnection=True)["dr"]["t"]))
    else:
        print("NOTE: no Rust server -- the rust column is NOT being compared.")
    return srv, out


def main():
    srv, tg = targets()
    for _, coll in tg:
        coll.drop()
        coll.insert_one({"_id": 1})
    bad = 0
    for label, build, marker, tail in CASES:
        for v in VALS:
            got = {name: cut(run(coll, build(v)), marker, tail) for name, coll in tg}
            ref = got["mongod"]
            off = {n: g for n, g in got.items() if n != "mongod" and g != ref}
            if off:
                bad += 1
                print(
                    f"{label:22} {v!r:16} mongod={ref!r}  "
                    + "  ".join(f"{n}={g!r}" for n, g in off.items())
                )
    print(f"\n{bad} divergent renderings across {len(CASES) * len(VALS)} shapes")
    srv.stop()


if __name__ == "__main__":
    main()
