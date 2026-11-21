"""Record what a one-member mongod 8.2.11 replica set keeps in its `local`
database, and how the state moves across a kill -9 and a restart.

Phase 4b-6 of ``tasks/mongod-storage-model-plan.md``. For a fresh
``--replSet`` store this writes, as canonical extended JSON (field order and
BSON types intact):

* each document of ``local.replset.minvalid``, ``.oplogTruncateAfterPoint``,
  ``.election``, ``.initialSyncId``, ``local.system.rollback.id`` and
  ``local.system.replset``, with the table's creation config (the logging
  flag, key and value format) from ``collStats.wiredTiger.creationString``;
* the oplog's first entry (the ``initiating set`` no-op) and the default
  ``size`` of ``local.oplog.rs`` (``maxSize``);
* the same documents after writes, after a ``kill -9`` and after the restart
  (``oplogTruncateAfterPoint`` follows the oldest oplog hole; the election
  term moves on).

Usage: .venv/bin/python tools/probes/mongod_replset_state.py --out FILE
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import socket
import subprocess
import tempfile
import time
from pathlib import Path

from bson import json_util
from pymongo import MongoClient

MONGOD = "/opt/homebrew/Cellar/mongodb-community@8.2/8.2.11/bin/mongod"
KEPT = [
    ("local", "replset.minvalid"),
    ("local", "replset.oplogTruncateAfterPoint"),
    ("local", "replset.election"),
    ("local", "replset.initialSyncId"),
    ("local", "system.rollback.id"),
    ("local", "system.replset"),
    ("config", "system.indexBuilds"),
]


def _port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def ej(doc):
    return json.loads(json_util.dumps(doc, json_options=json_util.CANONICAL_JSON_OPTIONS))


def table(client, db, coll):
    st = client[db].command("collStats", coll)
    cfg = st["wiredTiger"]["creationString"]
    pick = {}
    for key in ("key_format", "value_format", "block_compressor", "split_pct", "app_metadata"):
        m = re.search(rf"(?:^|,){key}=(\([^)]*\)|[^,]*)", cfg)
        pick[key] = m.group(1) if m else None
    m = re.search(r"log=\(enabled=(\w+)\)", cfg)
    pick["log_enabled"] = m.group(1) if m else None
    return pick


def snapshot(client):
    out = {}
    for db, coll in KEPT:
        entry = {"docs": [ej(d) for d in client[db][coll].find()]}
        try:
            entry["table"] = table(client, db, coll)
            entry["options"] = ej(
                next(iter(client[db].list_collections(filter={"name": coll})))["options"]
            )
        except StopIteration:
            entry["missing"] = True
        out[f"{db}.{coll}"] = entry
    oplog = client.local["oplog.rs"]
    stats = client.local.command("collStats", "oplog.rs")
    out["oplog"] = {
        "maxSize": stats["maxSize"],
        "first": [
            ej({k: v for k, v in e.items() if k != "wall"})
            for e in oplog.find().sort("$natural", 1).limit(2)
        ],
        "last": ej(
            {
                k: v
                for k, v in next(oplog.find().sort("$natural", -1).limit(1)).items()
                if k != "wall"
            }
        ),
        "table": table(client, "local", "oplog.rs"),
    }
    return out


def start(path, port):
    return subprocess.Popen(
        [
            MONGOD,
            "--replSet",
            "rs0",
            "--port",
            str(port),
            "--dbpath",
            path,
            "--bind_ip",
            "127.0.0.1",
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def wait_up(port):
    for _ in range(100):
        try:
            c = MongoClient("127.0.0.1", port, directConnection=True, serverSelectionTimeoutMS=500)
            c.admin.command("ping")
            return c
        except Exception:
            time.sleep(0.2)
    raise SystemExit("mongod did not start")


def wait_primary(c):
    for _ in range(100):
        if c.admin.command("hello").get("isWritablePrimary"):
            return
        time.sleep(0.2)
    raise SystemExit("no primary")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    args = ap.parse_args()
    tmp = Path(tempfile.mkdtemp(prefix="replstate-", dir=os.environ.get("PROBE_TMP", ".scratch")))
    port = _port()
    proc = start(str(tmp), port)
    result = {
        "mongod": subprocess.run(
            [MONGOD, "--version"], capture_output=True, text=True
        ).stdout.splitlines()[0]
    }
    try:
        c = wait_up(port)
        c.admin.command(
            "replSetInitiate", {"_id": "rs0", "members": [{"_id": 0, "host": f"127.0.0.1:{port}"}]}
        )
        wait_primary(c)
        time.sleep(3)
        result["after_initiate"] = snapshot(c)
        c.t.x.insert_many([{"i": i} for i in range(50)])
        time.sleep(2)
        result["after_writes"] = snapshot(c)
        # a clean restart: SIGTERM, wait, start again
        proc.terminate()
        proc.wait()
        c.close()
        proc = start(str(tmp), port)
        c = wait_up(port)
        wait_primary(c)
        time.sleep(3)
        result["after_clean_restart"] = snapshot(c)
        # kill -9, as a crash, twice
        for name in ("after_kill9_restart", "after_second_kill9_restart"):
            c.t.x.insert_many([{"j": i} for i in range(20)])
            time.sleep(1)
            os.kill(proc.pid, signal.SIGKILL)
            proc.wait()
            c.close()
            proc = start(str(tmp), port)
            c = wait_up(port)
            wait_primary(c)
            time.sleep(3)
            result[name] = snapshot(c)
            result[name]["data_rows"] = c.t.x.count_documents({})
    finally:
        proc.kill()
        proc.wait()
        shutil.rmtree(tmp, ignore_errors=True)
    Path(args.out).write_text(json.dumps(result, indent=2, sort_keys=False) + "\n")


if __name__ == "__main__":
    main()
