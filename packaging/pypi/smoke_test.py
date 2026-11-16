"""Smoke-test an installed SecantusDB wheel: both servers, both console scripts.

Run with the venv's own interpreter after ``pip install <wheel> pymongo
"psycopg[binary]"``. Exits non-zero on any failure.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

import psycopg
import pymongo
from secantusdb import MongoServer, PostgresServer

with MongoServer() as server:
    client = pymongo.MongoClient(server.uri, serverSelectionTimeoutMS=10000)
    client.smoke.things.insert_one({"x": 1})
    found = client.smoke.things.find_one({}, {"_id": 0})
    assert found == {"x": 1}, found
    storage = server.storage_path
    print("MongoDB server OK:", server.uri)
assert storage is not None and not storage.exists(), f"{storage} was not removed"

with PostgresServer() as server, psycopg.connect(server.dsn, autocommit=True) as conn:
    row = conn.execute("select 1 + 1").fetchone()
    assert row == (2,), row
    print("PostgreSQL server OK:", server.dsn)

scripts = Path(sys.executable).parent
for command in ("secantusd-rs", "secantusd-pg"):
    out = subprocess.run(
        [os.fspath(scripts / command), "--version"],
        capture_output=True,
        text=True,
        check=True,
        timeout=60,
    ).stdout
    assert command in out, out
    print(out.strip().splitlines()[0])
