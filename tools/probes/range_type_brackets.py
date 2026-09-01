"""Do the range operators bracket by BSON type, the way mongod's do?

`{v: {$gt: 3}}` matches numbers greater than 3 and NOTHING else — not the
string "z", not a date, not a `MaxKey`. Only three brackets (bool, document,
array) were enforced when this probe was written, and the consequence was not
subtle: a collection holding a single `MaxKey` returned that document for
*every* `$gt` query on the field, because `pymongo`'s `MaxKey.__gt__` returns
True unconditionally and nothing upstream said the comparison was meaningless.
96 of 112 (bound, operator, collation) shapes disagreed with mongod 8.2.11.

The corpus is one document per bracket, so a bound of any type has exactly one
in-bracket neighbour and thirteen out-of-bracket ones — a leak shows up as an
extra `_id`, not as a missing one, which is the direction that is easy to miss
by eye.

Two rules this exists to pin, both easy to get backwards:

* A `MinKey` / `MaxKey` **bound** compares across every type. Only the bound: a
  document whose VALUE is a `MaxKey` stays bracketed out of `{$gt: 3}`. The
  first fix here applied the exception to both sides and kept the original bug,
  because the `MaxKey` is on the document side.
* `bson.Code` SUBCLASSES `str`. Without an arm of its own a JavaScript value
  takes the string rank, sorts among the strings and matches a string bound —
  and, being unhashable, crashes the cached collation key on a collated sort.

Run it against BOTH servers; they have separate matchers and the engine-parity
suites are satisfied by both being wrong together (which is what happened here:
the Rust side pinned JavaScript to the string rank with a comment saying it did
so deliberately, to match Python).

    # Python server (an embedded one is started when PROBE_SERVER is unset)
    PROBE_MONGOD="mongodb://127.0.0.1:27041" uv run python tools/probes/range_type_brackets.py

    # Rust server
    crates/secantusdb/target/debug/secantusd-rs --port 27055 --storage-path /tmp/rs &
    PROBE_SERVER="mongodb://127.0.0.1:27055" \
      PROBE_MONGOD="mongodb://127.0.0.1:27041" \
      uv run python tools/probes/range_type_brackets.py
"""

import datetime
import os
import sys
import tempfile

import pymongo
from bson import Binary, Code, Decimal128, Int64, MaxKey, MinKey, ObjectId, Regex, Timestamp

MONGOD = os.environ.get("PROBE_MONGOD", "mongodb://127.0.0.1:27041")
SERVER = os.environ.get("PROBE_SERVER")
DB_NAME = "probe_range_brackets"

#: One document per BSON type bracket.
CORPUS = [
    {"_id": 1, "v": Code("x=1")},
    {"_id": 2, "v": "abc"},
    {"_id": 3, "v": 5},
    {"_id": 4, "v": Binary(b"ab")},
    {"_id": 5, "v": Regex("a", "i")},
    {"_id": 6, "v": Timestamp(1, 1)},
    {"_id": 7, "v": MinKey()},
    {"_id": 8, "v": MaxKey()},
    {"_id": 9, "v": Decimal128("2.5")},
    {"_id": 10, "v": datetime.datetime(2020, 1, 1)},
    {"_id": 11, "v": ObjectId("507f1f77bcf86cd799439011")},
    {"_id": 12, "v": True},
    {"_id": 13, "v": None},
    {"_id": 14, "v": [1, 2]},
    {"_id": 15, "v": {"a": 1}},
    {"_id": 16, "v": Int64(7)},
]

#: One bound per bracket, plus the two that lift bracketing and the one mongod
#: refuses outright.
BOUNDS = [
    "ab",
    3,
    Binary(b"a"),
    Timestamp(0, 1),
    MinKey(),
    MaxKey(),
    Code("a"),
    Regex("a", ""),
    datetime.datetime(2019, 1, 1),
    ObjectId("000000000000000000000000"),
    True,
    Decimal128("1"),
    [1],
    {"a": 0},
]
OPS = ["$gt", "$gte", "$lt", "$lte"]
COLLATIONS = [None, {"locale": "en", "strength": 2}]


def _run(client, query, collation):
    try:
        cursor = client[DB_NAME].c.find(query, {"_id": 1})
        if collation:
            cursor = cursor.collation(collation)
        return ("ok", sorted(d["_id"] for d in cursor))
    except Exception as exc:  # noqa: BLE001 — any refusal is a result here
        return (getattr(exc, "code", "ERR"), str(exc).split(", full error")[0])


def main() -> int:
    mongod = pymongo.MongoClient(MONGOD, serverSelectionTimeoutMS=3000)
    embedded = None
    if SERVER:
        server = pymongo.MongoClient(SERVER, serverSelectionTimeoutMS=3000)
    else:
        from secantus import SecantusDBServer

        embedded = SecantusDBServer(port=0, storage_path=tempfile.mkdtemp())
        embedded.start()
        server = pymongo.MongoClient(embedded.uri, serverSelectionTimeoutMS=3000)
    try:
        for client in (mongod, server):
            client.drop_database(DB_NAME)
            client[DB_NAME].c.insert_many(CORPUS)
        divergent = total = 0
        for collation in COLLATIONS:
            for bound in BOUNDS:
                for op in OPS:
                    query = {"v": {op: bound}}
                    total += 1
                    expected = _run(mongod, query, collation)
                    got = _run(server, query, collation)
                    if expected == got:
                        continue
                    divergent += 1
                    print(f"DIFF collation={bool(collation)} {op} {bound!r}")
                    print(f"   mongod {expected}")
                    print(f"   server {got}")
        print(f"\n{total} (bound, operator, collation) shapes: {divergent} divergent")
        return 1 if divergent else 0
    finally:
        if embedded is not None:
            embedded.stop()


if __name__ == "__main__":
    sys.exit(main())
