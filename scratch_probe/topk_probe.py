"""Ties under a blocking sort with a limit: the order mongod returns tied
documents in. Prints one JSON line per case: name and the _id order."""
import json, sys, itertools, random
import pymongo

port = int(sys.argv[1]) if len(sys.argv) > 1 else 27017
c = pymongo.MongoClient("127.0.0.1", port, directConnection=True)
db = c.topk_probe
cases = []
# Collections: n docs, keys drawn from a small set so ties abound.
rng = random.Random(7)
shapes = []
for n in (3, 5, 8, 12, 20, 40):
    for kinds in (1, 2, 3):
        keys = [rng.randrange(kinds) for _ in range(n)]
        shapes.append((n, kinds, keys))
for n, kinds, keys in shapes:
    db.drop_collection("c")
    db.c.insert_many([{"_id": i, "a": k} for i, k in enumerate(keys)])
    for direction in (1, -1):
        for limit in (2, 3, 4, 7, 11):
            for skip in (0, 1, 2):
                name = f"n{n}k{kinds}d{direction}l{limit}s{skip}"
                got = [d["_id"] for d in db.c.find({}, {"_id": 1}).sort("a", direction).skip(skip).limit(limit)]
                agg = [d["_id"] for d in db.c.aggregate([{"$sort": {"a": direction}}, {"$skip": skip}, {"$limit": limit}, {"$project": {"_id": 1}}])]
                print(json.dumps({"case": name, "keys": keys, "find": got, "agg": agg}))
