"""Drive representative workloads against a running server URI to collect a PGO
profile.

Used by the standalone ``secantusd-rs`` binary's two-stage PGO release build
(``.github/workflows/release-binaries.yml``): the instrumented binary runs with
``LLVM_PROFILE_FILE`` set, this script exercises its hot paths (CRUD +
aggregation + indexing), then the binary is stopped so the profiling runtime
flushes ``.profraw`` on exit. The workload mirrors the six-workload benchmark so
the profile covers the same paths the benchmark measures.

Standalone (pymongo only), so it runs in the minimal release-build CI image.
"""

from __future__ import annotations

import argparse

from pymongo import MongoClient


def run(uri: str, n: int, reps: int) -> None:
    client = MongoClient(uri, directConnection=True)
    db = client["pgo"]
    for r in range(reps):
        coll = db[f"c{r}"]
        coll.drop()
        docs = [
            {"_id": i, "g": i % 10, "v": i, "s": f"str-{i}", "tags": [i % 3, i % 5]}
            for i in range(n)
        ]
        coll.insert_many(docs)
        coll.create_index("v")
        # find by indexed range
        _ = list(coll.find({"v": {"$gte": n // 4, "$lt": n // 2}}))
        # full scan
        _ = list(coll.find({}))
        # update_many (operator update over a scan)
        coll.update_many({"g": {"$lt": 5}}, {"$set": {"tag": "x"}, "$inc": {"v": 1}})
        # aggregation: single-stage group and a multi-stage unwind→group→sort
        _ = list(
            coll.aggregate(
                [
                    {"$group": {"_id": "$g", "n": {"$sum": 1}, "sum": {"$sum": "$v"}}},
                    {"$sort": {"_id": 1}},
                ]
            )
        )
        _ = list(
            coll.aggregate(
                [
                    {"$match": {"g": {"$gte": 0}}},
                    {"$unwind": "$tags"},
                    {"$group": {"_id": "$tags", "t": {"$sum": "$v"}}},
                    {"$sort": {"t": -1}},
                ]
            )
        )
        # delete half
        coll.delete_many({"g": {"$gte": 5}})
        coll.drop()
    client.close()


def main() -> None:
    ap = argparse.ArgumentParser(
        description="Drive PGO-profiling workloads against a MongoDB-wire URI."
    )
    ap.add_argument("--uri", required=True, help="mongodb:// URI of the server to profile")
    ap.add_argument("--n", type=int, default=5000, help="documents per rep (default 5000)")
    ap.add_argument("--reps", type=int, default=3, help="workload repetitions (default 3)")
    args = ap.parse_args()
    run(args.uri, args.n, args.reps)
    print(f"pgo_workload: done ({args.reps} reps x {args.n} docs)")


if __name__ == "__main__":
    main()
