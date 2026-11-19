"""Record mongod's KeyString bytes for a corpus of hard numbers.

Phase 4 of ``tasks/mongod-storage-model-plan.md`` ports mongod's KeyString to
``secantus_core::keystring``. The layout fixture (phase 0b) covers one value
per BSON type; this probe adds the numeric paths that need many cases to pin:
Decimal128 values with long continuations, at the exponent extremes, equal to
a double or not, and doubles at the subnormal, integer and 2**53 boundaries.

It starts a standalone mongod, inserts one document per value (``_id`` = the
value's position, so the RecordId is position + 1), builds ``{v: 1}`` and
``{v: -1}`` indexes, stops mongod, and reads each index table back with a
``wt`` utility. Output: JSON ``{mongod_version, values (canonical extended
JSON), ascending: [[key_hex, typebits_hex]...], descending: [...]}`` in
RecordId order -- the RecordId is the last bytes of each key, so a consumer
strips it by decoding.

Usage::

    .venv/bin/python tools/probes/mongod_keystring_corpus.py --wt .scratch/wt11-mdb/wt \\
        --out tests/fixtures/storage_model/mongod-8.2.11-keystring-numbers.json
"""

from __future__ import annotations

import argparse
import contextlib
import random
import shutil
import socket
import subprocess
import sys
import tempfile
import time
from decimal import getcontext
from pathlib import Path

from bson import json_util
from bson.decimal128 import Decimal128
from bson.int64 import Int64
from pymongo import MongoClient

DEFAULT_MONGOD = "/opt/homebrew/Cellar/mongodb-community@8.2/8.2.11/bin/mongod"


def corpus() -> list:
    getcontext().prec = 40
    fixed = [
        "0",
        "-0",
        "1",
        "-1",
        "1.0",
        "1.00",
        "0.1",
        "-0.1",
        "0.3",
        "0.30000000000000004",
        "1E75",
        "-1E75",
        "1E-330",
        "1E-6176",
        "-1E-6176",
        "1E+6144",
        "9.999999999999999999999999999999999E+6144",
        "-9.999999999999999999999999999999999E+6144",
        "0.1234567890123456789",
        "123456789012345678901234567890.1234",
        "1.000000000000000000000000000000001",
        "0.999999999999999999999999999999999",
        "9007199254740993",
        "9007199254740992",
        "-9007199254740993",
        "2147483648",
        "-2147483649",
        "9223372036854775807",
        "9223372036854775808",
        "-9223372036854775808",
        "-9223372036854775809",
        "1E+20",
        "1.5E+300",
        "1E+309",
        "2.2250738585072014E-308",
        "4.9E-324",
        "5E-324",
        "1E-400",
        "Infinity",
        "-Infinity",
        "NaN",
        "0E+10",
        "0E-10",
        "-0E-6176",
        "3.14159265358979323846264338327950",
        "12345678901234567890123456789012.34",
        "0.000000000000000000000000000000001",
    ]
    out: list = [Decimal128(s) for s in fixed]
    rnd = random.Random(20261119)
    for _ in range(120):
        digits = rnd.randint(1, 34)
        coeff = rnd.randint(1, 10**digits - 1)
        exp = rnd.choice([rnd.randint(-40, 40), rnd.randint(-6176, 6111)])
        sign = rnd.choice(["", "-"])
        out.append(Decimal128(f"{sign}{coeff}E{exp}"))
    doubles = [
        0.0,
        -0.0,
        0.5,
        1.5,
        -1.5,
        0.1,
        1e-310,
        -1e-310,
        5e-324,
        2.0**53,
        2.0**53 + 2,
        -(2.0**53),
        2.0**63,
        -(2.0**63),
        2.0**64,
        1e308,
        -1e308,
        1.7976931348623157e308,
        float("inf"),
        float("-inf"),
        float("nan"),
        123456.789,
        2147483647.5,
        -2147483648.5,
        1 / 3,
    ]
    out += doubles
    for _ in range(80):
        out.append(rnd.uniform(-1e6, 1e6) * 10 ** rnd.randint(-30, 30))
    out += [0, 1, -1, 2**31 - 1, -(2**31), Int64(2**53 + 1), Int64(-(2**63)), Int64(2**63 - 1)]
    return out


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def _dump(wt: str, home: Path, uri: str) -> list[tuple[str, str]]:
    cfg = "log=(enabled=true,path=journal,compressor=snappy)"
    out = subprocess.run(
        [wt, "-h", str(home), "-C", cfg, "dump", "-x", uri], capture_output=True, text=True
    )
    if out.returncode:
        raise SystemExit(out.stderr)
    lines = out.stdout.splitlines()
    data = lines[lines.index("Data") + 1 :]
    return [(data[i], data[i + 1]) for i in range(0, len(data) - 1, 2)]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--mongod", default=DEFAULT_MONGOD)
    ap.add_argument("--wt", required=True)
    ap.add_argument("--out", required=True, type=Path)
    args = ap.parse_args()
    version = subprocess.run(
        [args.mongod, "--version"], capture_output=True, text=True
    ).stdout.splitlines()[0]
    scratch = Path(__file__).resolve().parents[2] / ".scratch"
    scratch.mkdir(exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix="ks-corpus-", dir=scratch))
    db = work / "db"
    db.mkdir()
    port = _free_port()
    proc = subprocess.Popen(
        [
            args.mongod,
            "--dbpath",
            str(db),
            "--port",
            str(port),
            "--bind_ip",
            "127.0.0.1",
            "--logpath",
            str(work / "mongod.log"),
        ],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        deadline = time.time() + 30
        while True:
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.5).close()
                break
            except OSError as exc:
                if time.time() > deadline or proc.poll() is not None:
                    raise SystemExit("mongod did not start") from exc
                time.sleep(0.2)
        client = MongoClient(f"mongodb://127.0.0.1:{port}/?directConnection=true")
        values = corpus()
        coll = client["ks"]["n"]
        for i, v in enumerate(values):
            coll.insert_one({"_id": i, "v": v})
        coll.create_index([("v", 1)], name="asc")
        coll.create_index([("v", -1)], name="desc")
        entry = client["admin"].command(
            "aggregate", 1, pipeline=[{"$listCatalog": {}}, {"$match": {"name": "n"}}], cursor={}
        )["cursor"]["firstBatch"][0]
        idents = entry["idxIdent"]
        # The connection drops as mongod stops.
        with contextlib.suppress(Exception):
            client.admin.command("shutdown")
        proc.wait(timeout=60)
        asc = _dump(args.wt, db, f"table:{idents['asc']}")
        desc = _dump(args.wt, db, f"table:{idents['desc']}")
        out = {
            "mongod_version": version,
            "values": [
                json_util.dumps({"v": v}, json_options=json_util.CANONICAL_JSON_OPTIONS)
                for v in values
            ],
            "ascending": asc,
            "descending": desc,
        }
        args.out.write_text(json_util.dumps(out, indent=0) + "\n")
        print(f"wrote {args.out}: {len(values)} values")
    finally:
        if proc.poll() is None:
            proc.kill()
        shutil.rmtree(work, ignore_errors=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
