"""Every regex-shaped query crossed with every document-side value type.

A regex is a VALUE as well as a pattern, and that half was missing on both
servers. 104 shapes; 14 diverged on the Python server and 21 on the Rust one
when this was written:

* a bare `/ab/i` never matched a STORED regex equal to it, so
  `find({v: /ab/i})` missed `{v: /ab/i}` -- a silent wrong answer, not an error;
* `bson.Code` subclasses `str`, so JavaScript values were pattern-matched as
  text (mongod never applies a regex to code);
* `$eq` with a regex operand DEFERRED on the Rust server, which is an error
  there, for every document in the collection.

Run it against BOTH servers -- the matchers are separate, and the engine-parity
suites are satisfied by both being wrong together.

    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python tools/probes/regex_value_semantics.py
    PROBE_SERVER="mongodb://127.0.0.1:27055" ... # adds the Rust column
"""

import os, sys, tempfile, pymongo
from bson import Code, Decimal128, Regex, Binary, ObjectId

mon = pymongo.MongoClient(os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041"))
from secantus import SecantusDBServer

s = SecantusDBServer(port=0, storage_path=tempfile.mkdtemp())
s.start()
py = pymongo.MongoClient(s.uri)
targets = [("mongod", mon), ("python", py)]
if os.environ.get("PROBE_SERVER"):
    targets.append(("rust", pymongo.MongoClient(os.environ["PROBE_SERVER"])))

DOCVALS = [
    ("str-ab", "ab"),
    ("str-AB", "AB"),
    ("str-xab", "xab"),
    ("str-zz", "zz"),
    ("re-ab-i", Regex("ab", "i")),
    ("re-ab-none", Regex("ab", "")),
    ("re-cd-i", Regex("cd", "i")),
    ("arr-str", ["ab", "zz"]),
    ("arr-re", [Regex("ab", "i")]),
    ("code", Code("ab")),
    ("int", 5),
    ("null", None),
    ("missing", "__OMIT__"),
]
QUERIES = [
    ("bare /ab/i", lambda: Regex("ab", "i")),
    ("bare /ab/", lambda: Regex("ab", "")),
    ("$eq /ab/i", lambda: {"$eq": Regex("ab", "i")}),
    ("$ne /ab/i", lambda: {"$ne": Regex("ab", "i")}),
    ("$in [/ab/i]", lambda: {"$in": [Regex("ab", "i")]}),
    ("$nin [/ab/i]", lambda: {"$nin": [Regex("ab", "i")]}),
    ("$gt /ab/i", lambda: {"$gt": Regex("ab", "i")}),
    ("$regex /ab/i", lambda: {"$regex": Regex("ab", "i")}),
]

rows = []
for qn, qf in QUERIES:
    for dn, dv in DOCVALS:
        res = {}
        for label, cli in targets:
            c = cli["rxcmp"][f"c{abs(hash(qn + dn)) % 99999}"]
            c.drop()
            doc = {"_id": 1} if dv == "__OMIT__" else {"_id": 1, "v": dv}
            c.insert_one(doc)
            try:
                res[label] = f"n={len(list(c.find({'v': qf()}, {'_id': 1})))}"
            except Exception as e:
                res[label] = f"ERR {getattr(e, 'code', '?')}"
        rows.append((qn, dn, res))

bad = [(q, d, r) for q, d, r in rows if len({v for k, v in r.items()}) > 1]
print(f"total {len(rows)}  divergent {len(bad)}")
for q, d, r in bad:
    print(f"  {q:16s} doc={d:12s} " + "  ".join(f"{k}={v}" for k, v in r.items()))

sys.exit(1 if bad else 0)
