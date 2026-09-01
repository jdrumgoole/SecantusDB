"""Range operators are TYPE-BRACKETED in mongod: {$gt: "ab"} matches strings and
nothing else. Does the Python server bracket every type, or only some?"""
import os, sys, tempfile, datetime, pymongo
from bson import Code, Regex, Binary, Decimal128, MinKey, MaxKey, Timestamp, ObjectId, Int64
sys.path.insert(0, "/Users/jdrumgoole/GIT/SecantusDB-rust-widen/src")
from secantus import SecantusDBServer
mon = pymongo.MongoClient("mongodb://127.0.0.1:27041")
srv = SecantusDBServer(port=0, storage_path=tempfile.mkdtemp()); srv.start()
py = pymongo.MongoClient(f"mongodb://{srv.address[0]}:{srv.address[1]}")
rust = pymongo.MongoClient(os.environ["PROBE_SERVER"]) if os.environ.get("PROBE_SERVER") else None
DBN = "brk"
DOCS = [
    {"_id": 1, "v": Code("x=1")}, {"_id": 2, "v": "abc"}, {"_id": 3, "v": 5},
    {"_id": 4, "v": Binary(b"ab")}, {"_id": 5, "v": Regex("a", "i")},
    {"_id": 6, "v": Timestamp(1, 1)}, {"_id": 7, "v": MinKey()}, {"_id": 8, "v": MaxKey()},
    {"_id": 9, "v": Decimal128("2.5")}, {"_id": 10, "v": datetime.datetime(2020, 1, 1)},
    {"_id": 11, "v": ObjectId("507f1f77bcf86cd799439011")}, {"_id": 12, "v": True},
    {"_id": 13, "v": None}, {"_id": 14, "v": [1, 2]}, {"_id": 15, "v": {"a": 1}},
    {"_id": 16, "v": Int64(7)},
]
for c in [mon, py] + ([rust] if rust else []):
    c.drop_database(DBN); c[DBN].c.insert_many(DOCS)
BOUNDS = ["ab", 3, Binary(b"a"), Timestamp(0, 1), MinKey(), MaxKey(), Code("a"),
          Regex("a", ""), datetime.datetime(2019, 1, 1),
          ObjectId("000000000000000000000000"), True, Decimal128("1"), [1], {"a": 0}]
OPS = ["$gt", "$gte", "$lt", "$lte"]


def run(cli, f, coll=None):
    try:
        cur = cli[DBN].c.find(f, {"_id": 1})
        if coll:
            cur = cur.collation(coll)
        return ("ok", sorted(d["_id"] for d in cur))
    except Exception as ex:
        return (getattr(ex, "code", "ERR"), str(ex).split(", full error")[0][:50])


npy = nrs = tot = 0
for coll in [None, {"locale": "en", "strength": 2}]:
    for bound in BOUNDS:
        for op in OPS:
            f = {"v": {op: bound}}
            m, p = run(mon, f, coll), run(py, f, coll)
            tot += 1
            if m != p:
                npy += 1
                print(f"PY coll={bool(coll)} {op} {bound!r}\n   mongod {m}\n   python {p}")
            if rust:
                r = run(rust, f, coll)
                if m != r:
                    nrs += 1
print(f"\n{tot} (bound, op, collation) shapes: python {npy} divergent" +
      (f", rust {nrs} divergent" if rust else ""))
srv.stop()
