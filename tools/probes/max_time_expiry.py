"""`maxTimeMS` actually expiring, per command, against mongod.

Validation of the ARGUMENT is covered elsewhere (`arg_types_*`). This probe asks
the other half: when the budget runs out mid-operation, does the server stop
and say so -- and in the same words?

mongod's answer depends on WHERE the budget ran out, measured on 8.2.11
(2026-09-30) over 100,000 documents:

* `find` / `aggregate` / `distinct` / `count` wrap an execution-time expiry in
  their executor prefix (`Executor error during find command: <ns> :: caused by
  :: operation exceeded time limit`, and `... <cmd> command on namespace: <ns>`
  for the other three);
* `findAndModify` / `update` / `delete` over a scan that writes nothing, and
  `createIndexes`, send the bare message;
* every one of them fails the whole COMMAND (`ok: 0`) -- a write never reports
  it as a per-statement `writeErrors` entry.

With a small budget mongod sometimes expires BEFORE execution and sends the bare
form even for `find` (seen at 1ms, and at 5ms on a loaded box), so the budget
here is 20ms over 300,000 documents and each case runs several
times; the MODAL outcome is compared. A self-check aborts the run if mongod did
not expire the heaviest case at all, because then the box is fast enough that
nothing below measured an expiry.

    PROBE_MONGOD="mongodb://127.0.0.1:27041/?directConnection=true" \\
    PROBE_SERVER="mongodb://127.0.0.1:27055/?directConnection=true" \\
        python tools/probes/max_time_expiry.py
"""

from __future__ import annotations

import collections
import os
import re
import sys
from pathlib import Path
from typing import Any

from pymongo.errors import OperationFailure

sys.path.insert(0, str(Path(__file__).parent))
from _servers import DEFAULT_MONGOD, probe_targets, report  # noqa: E402

DB = "maxtime_probe"
COLL = "c"
N_DOCS = 300_000
BUDGET_MS = 20
REPEATS = 5

#: Matches nothing, so the write commands scan the whole collection and write
#: nothing -- the shape whose mongod answer is stable.
NO_MATCH = {"b": {"$regex": "x"}}

CASES: list[tuple[str, dict[str, Any]]] = [
    ("find regex", {"find": COLL, "filter": NO_MATCH}),
    ("find sort", {"find": COLL, "filter": {}, "sort": {"b": 1}}),
    ("agg $match", {"aggregate": COLL, "pipeline": [{"$match": NO_MATCH}], "cursor": {}}),
    ("agg $group", {"aggregate": COLL, "pipeline": [{"$group": {"_id": "$b"}}], "cursor": {}}),
    ("agg $sort", {"aggregate": COLL, "pipeline": [{"$sort": {"b": -1}}], "cursor": {}}),
    ("distinct", {"distinct": COLL, "key": "b"}),
    ("distinct query", {"distinct": COLL, "key": "b", "query": NO_MATCH}),
    ("count query", {"count": COLL, "query": NO_MATCH}),
    ("findAndModify", {"findAndModify": COLL, "query": NO_MATCH, "update": {"$set": {"q": 1}}}),
    (
        "update",
        {"update": COLL, "updates": [{"q": NO_MATCH, "u": {"$set": {"z": 1}}, "multi": True}]},
    ),
    ("delete", {"delete": COLL, "deletes": [{"q": NO_MATCH, "limit": 0}]}),
    (
        "createIndexes",
        {"createIndexes": COLL, "indexes": [{"key": {"b": 1, "_id": -1}, "name": "mt_b"}]},
    ),
]

#: The case mongod must expire, or the run measured nothing.
SELF_CHECK = "agg $sort"


def outcome(db: Any, cmd: dict[str, Any]) -> str:
    try:
        reply = db.command(dict(cmd, maxTimeMS=BUDGET_MS))
    except OperationFailure as e:
        msg = (e.details or {}).get("errmsg", "")
        head, sep, _ = msg.partition(" :: caused by :: ")
        form = head.replace(f"{DB}.{COLL}", "<ns>") if sep else "bare"
        # An index build's own UUIDs are fresh per attempt.
        form = re.sub(
            r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", "<uuid>", form
        )
        return f"ok:0 code {e.code} {form}"
    errs = reply.get("writeErrors") or []
    if errs:
        return f"ok:1 writeErrors code {errs[0].get('code')}"
    return "ok:1"


def modal(db: Any, cmd: dict[str, Any]) -> str:
    counts = collections.Counter(outcome(db, cmd) for _ in range(REPEATS))
    return counts.most_common(1)[0][0]


def seed(client: Any) -> Any:
    db = client[DB]
    db[COLL].drop()
    db[COLL].insert_many([{"a": i, "b": str(i)} for i in range(N_DOCS)])
    return db


def main() -> int:
    divergent: dict[str, int] = {}
    # The embedded Python server takes mongod's topology: a standalone and a
    # replica-set member answer some of these differently.
    import pymongo

    ref = pymongo.MongoClient(
        os.environ.get("PROBE_MONGOD", DEFAULT_MONGOD),
        directConnection=True,
    )
    set_name = ref.admin.command("hello").get("setName")
    ref.close()
    with probe_targets(replica_set=set_name) as (mongod, targets):
        ref_db = seed(mongod)
        expected = {name: modal(ref_db, cmd) for name, cmd in CASES}
        if not expected[SELF_CHECK].startswith("ok:0"):
            print(
                f"SELF-CHECK FAILED: mongod did not expire {SELF_CHECK!r}: {expected[SELF_CHECK]}"
            )
            return 2
        for label, client in targets:
            db = seed(client)
            divergent[label] = 0
            for name, cmd in CASES:
                got = modal(db, cmd)
                if got != expected[name]:
                    divergent[label] += 1
                    print(f"{label:6} {name:16} mongod: {expected[name]}")
                    print(f"{'':6} {'':16} ours:   {got}")
            db.client.drop_database(DB)
        mongod.drop_database(DB)
    return report(f"maxTimeMS expiry: {len(CASES)} commands", len(CASES), divergent)


if __name__ == "__main__":
    sys.exit(main())
