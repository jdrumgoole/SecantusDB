"""RM-710 / RM-711: measure Linux x86-64 mongod's bucketCatalog.memoryUsage.

Each case runs on a fresh standalone mongod in an amd64 container (the
official image, libstdc++), started as the test harness starts its
reference mongod (`--quiet`, standalone). On this Mac Docker is Colima,
started with `colima start --vm-type vz --vz-rosetta` so amd64 images run
under Rosetta. Set DOCKER_HOST to Colima's socket, and DOCKER_CONFIG to a
directory holding a `{}` config.json if `~/.docker/config.json` names
Docker Desktop's credential helper. Usage:

    DOCKER_HOST=unix://$HOME/.colima/default/docker.sock \\
        .venv/bin/python tools/probes/linux_bucket_catalog.py 8.2.11 8.2.12

Prints one JSON line per (version, run, case).
"""

import datetime as dt
import json
import subprocess
import sys
import time

import pymongo

T = dt.datetime(2026, 1, 1)


def start(version: str) -> tuple[str, int]:
    cid = subprocess.check_output(
        [
            "docker",
            "run",
            "-d",
            "--rm",
            "--platform",
            "linux/amd64",
            "-p",
            "127.0.0.1::27017",
            f"mongo:{version}",
            "mongod",
            "--bind_ip_all",
            "--quiet",
        ],
        text=True,
    ).strip()
    port = int(
        subprocess.check_output(["docker", "port", cid, "27017/tcp"], text=True)
        .strip()
        .splitlines()[0]
        .rsplit(":", 1)[1]
    )
    deadline = time.time() + 90
    while True:
        try:
            c = pymongo.MongoClient(port=port, serverSelectionTimeoutMS=2000)
            c.admin.command("ping")
            c.close()
            return cid, port
        except Exception:
            if time.time() > deadline:
                raise
            time.sleep(1)


def stop(cid: str) -> None:
    subprocess.run(["docker", "stop", "-t", "5", cid], capture_output=True, check=True)


def mem(c) -> int | None:
    return c.admin.command("serverStatus").get("bucketCatalog", {}).get("memoryUsage")


def coll(c, meta=True):
    db = c["rm_bucket_catalog"]
    opts = {"timeField": "t", "metaField": "m"} if meta else {"timeField": "t"}
    db.create_collection("ts", timeseries=opts)
    return db.ts


def gap_test(c):
    ts = coll(c)
    ts.insert_one({"t": T, "m": "a"})
    first = mem(c)
    ts.insert_many([{"t": T + dt.timedelta(seconds=i), "m": "a", "v": i} for i in range(1, 5)])
    return [first, mem(c)]


def one(doc, meta=True):
    def f(c):
        coll(c, meta).insert_one(doc)
        return mem(c)

    return f


def two_measurements(c):
    ts = coll(c)
    ts.insert_one({"t": T, "m": "a"})
    ts.insert_one({"t": T + dt.timedelta(seconds=1), "m": "a"})
    return mem(c)


def expiry(c):
    ts = coll(c)
    ts.insert_one({"t": T, "m": -1})
    c.admin.command("setParameter", 1, timeseriesIdleBucketExpiryMemoryUsageThreshold=30000)
    for i in range(20):
        ts.insert_one({"t": T, "m": i})
    s = c.admin.command("serverStatus").get("bucketCatalog", {})
    return {
        k: s.get(k)
        for k in (
            "numBuckets",
            "numBucketsClosedDueToMemoryThreshold",
            "numBucketsArchivedDueToMemoryThreshold",
            "memoryUsage",
        )
    } | {"stored": c["rm_bucket_catalog"]["system.buckets.ts"].count_documents({})}


CASES = {
    "gap_test [one meas m='a', then 4 with v]": gap_test,
    "one meas m='a'": one({"t": T, "m": "a"}),
    "one meas m=1 (int)": one({"t": T, "m": 1}),
    "one meas m=41 chars": one({"t": T, "m": "x" * 41}),
    "one meas, no metaField": one({"t": T}, meta=False),
    "one meas m='a' +1 field": one({"t": T, "m": "a", "x": 1}),
    "one meas m='a' +2 fields": one({"t": T, "m": "a", "x": 1, "y": 2}),
    "two meas m='a'": two_measurements,
    "expiry at 30000": expiry,
}


def main() -> None:
    for version in sys.argv[1:]:
        for run in (1, 2):
            for name, fn in CASES.items():
                cid, port = start(version)
                try:
                    c = pymongo.MongoClient(port=port)
                    print(
                        json.dumps({"version": version, "run": run, "case": name, "got": fn(c)}),
                        flush=True,
                    )
                    c.close()
                finally:
                    stop(cid)


if __name__ == "__main__":
    main()
