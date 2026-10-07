"""`errorLabels` on server errors, per command and per code, against mongod.

A driver decides whether to retry, resume a change stream, or retry a
transaction from the labels the SERVER attaches, not from the code alone, so a
missing label silently changes driver behaviour. Found from the PHP extension's
`tests/cursor/bug1419-001.phpt`: a `getMore` failing with 280
(`ChangeStreamFatalError`) must carry `NonResumableChangeStreamError`.

Each case arms `failCommand` for one command with one code (`times: 1`) and
records the code and the SORTED labels the reply carries. The contexts matter
as much as the codes -- mongod labels a write differently inside a retryable
write (`txnNumber`, no transaction) and inside a transaction -- so every code
runs in each context. The client has retries OFF, so what comes back is the
server's first answer. The code NAME is compared too: mongod names 450-odd
codes the Rust server used to report as `Location<n>`.

Two outcomes are not a reply at all: on 303 / 354 mongod DROPS the connection
(both servers then show `client-error AutoReconnect`, which is agreement on the
drop, not a vacuous match), and 330 is left out because injecting it crashes
mongod 8.2.11.

Needs both servers with failpoints enabled; compares the Rust server only (the
Python server is not the reference):

    mongod --replSet rs0 --setParameter enableTestCommands=1 ...   (initiated)
    secantusd-rs --enable-test-commands ...
    PROBE_MONGOD="mongodb://127.0.0.1:27041/?directConnection=true" \\
    PROBE_SERVER="mongodb://127.0.0.1:27056/?directConnection=true" \\
        python tools/probes/error_labels.py
"""

from __future__ import annotations

import os
import sys
from collections.abc import Callable
from typing import Any

import pymongo
from bson import Int64
from pymongo.errors import NotPrimaryError, OperationFailure, PyMongoError

MONGOD = os.environ.get("PROBE_MONGOD")
SERVER = os.environ.get("PROBE_SERVER")
DB, COLL = "labels_probe", "c"

#: Retryable, resumable, transient, and plain codes. 280 is the case that
#: started this; the rest are the codes drivers key their decisions on.
CODES = [
    # NonResumableChangeStreamError on every command
    280,  # ChangeStreamFatalError
    286,  # ChangeStreamHistoryLost
    # the resumable / retryable / transient families (network, state change)
    6,
    7,
    89,
    91,
    189,
    262,
    9001,
    10107,
    11600,
    11602,
    13435,
    13436,
    133,
    134,
    150,
    175,
    234,
    317,
    358,
    384,
    401,
    402,
    406,
    407,
    412,
    453,
    50915,
    # transient in a transaction only
    24,
    112,
    239,
    246,
    250,
    251,
    267,
    272,
    # SystemOverloadedError (462 also carries the context label)
    433,
    449,
    450,
    462,
    # labelled nothing anywhere
    43,
    50,
    63,
    2,
    11601,
    # failCommand refuses these without errorExtraInfo: 40671
    11000,
    13388,
    121,
    65,
    249,
    283,
    # mongod drops the connection rather than reply
    303,
    354,
]


def arm(client: pymongo.MongoClient, command: str, code: int) -> None:
    client.admin.command(
        "configureFailPoint",
        "failCommand",
        mode={"times": 1},
        data={"failCommands": [command], "errorCode": code},
    )


def disarm(client: pymongo.MongoClient) -> None:
    client.admin.command("configureFailPoint", "failCommand", mode="off")


def outcome(fn: Callable[[], Any]) -> tuple[Any, ...]:
    try:
        fn()
        return ("ok",)
    except (OperationFailure, NotPrimaryError) as e:
        # pymongo raises NotPrimaryError (not OperationFailure) for the
        # state-change codes; its `details` is the same server reply, so read
        # the labels from it rather than recording a client-side type.
        details = e.details or {}
        return (
            details.get("code"),
            details.get("codeName"),
            tuple(sorted(details.get("errorLabels", []))),
        )
    except PyMongoError as e:
        # Any other client-side error is a probe fault, not a comparison; name
        # its type so it can never be mistaken for agreement.
        return ("client-error", type(e).__name__)


def contexts(client: pymongo.MongoClient, code: int) -> dict[str, tuple[str, Callable[[], Any]]]:
    """Every case is a RAW command: a driver helper would retry a read or
    resume a change stream on its own and hide the server's first answer."""
    db = client[DB]

    def open_cursor(pipeline: list[dict[str, Any]] | None) -> int:
        if pipeline is None:
            reply = db.command("find", COLL, batchSize=1)
        else:
            reply = db.command("aggregate", COLL, pipeline=pipeline, cursor={"batchSize": 0})
        return reply["cursor"]["id"]

    def getmore(pipeline: list[dict[str, Any]] | None) -> Callable[[], Any]:
        def run() -> None:
            cursor_id = open_cursor(pipeline)
            arm(client, "getMore", code)
            db.command("getMore", cursor_id, collection=COLL, maxTimeMS=50)

        return run

    def retryable_insert() -> None:
        with client.start_session() as s:
            db.command({"insert": COLL, "documents": [{"x": 1}], "txnNumber": Int64(1)}, session=s)

    def txn_insert() -> None:
        with client.start_session() as s, s.start_transaction():
            db.command({"insert": COLL, "documents": [{"x": 1}]}, session=s)

    def commit(code: int) -> Callable[[], Any]:
        def run() -> None:
            with client.start_session() as s:
                s.start_transaction()
                db.command({"insert": COLL, "documents": [{"x": 1}]}, session=s)
                arm(client, "commitTransaction", code)
                # Raw, not `commit_transaction()`: the driver retries a commit.
                # The session adds `txnNumber` / `autocommit: false` itself.
                client.admin.command("commitTransaction", 1, session=s)

        return run

    stream = [{"$changeStream": {}}]
    return {
        "find": ("find", lambda: db.command("find", COLL)),
        "getMore on a find cursor": ("", getmore(None)),
        "aggregate": ("aggregate", lambda: db.command("aggregate", COLL, pipeline=[], cursor={})),
        "aggregate $changeStream": (
            "aggregate",
            lambda: db.command("aggregate", COLL, pipeline=stream, cursor={}),
        ),
        "getMore on a change stream": ("", getmore(stream)),
        "insert": ("insert", lambda: db.command({"insert": COLL, "documents": [{"x": 1}]})),
        "insert, retryable write": ("insert", retryable_insert),
        "insert, in a transaction": ("insert", txn_insert),
        "count": ("count", lambda: db.command("count", COLL)),
        "commitTransaction": ("", commit(code)),
    }


def measure(uri: str) -> list[tuple[str, tuple[Any, ...]]]:
    # A socket timeout so a server that never answers is a visible
    # `client-error NetworkTimeout` row rather than a probe that hangs forever.
    client = pymongo.MongoClient(uri, retryReads=False, retryWrites=False, socketTimeoutMS=15000)
    client.drop_database(DB)
    client[DB][COLL].insert_many([{"_id": i} for i in range(3)])
    rows = []
    for code in CODES:
        print(f"... {uri.split('@')[-1][:30]} code {code}", file=sys.stderr, flush=True)
        for label, (command, fn) in contexts(client, code).items():
            if command:
                arm(client, command, code)
            rows.append((f"{code} {label}", outcome(fn)))
            disarm(client)
    client.drop_database(DB)
    client.close()
    return rows


def main() -> int:
    if not (MONGOD and SERVER):
        print("PROBE_MONGOD and PROBE_SERVER are required (see the module docstring)")
        return 2
    want, got = measure(MONGOD), measure(SERVER)
    # Self-check: the case this probe exists for must carry a label on mongod,
    # or failpoints are off and every row compares nothing.
    if dict(want).get("280 getMore on a find cursor", ("", "", ()))[-1] == ():
        print("SELF-CHECK FAILED: mongod attached no label to 280 on getMore")
        return 2
    bad = 0
    for (label, w), (_, g) in zip(want, got, strict=True):
        if w != g:
            bad += 1
            print(f"DIFF {label}\n  mongod: {w}\n  ours:   {g}")
    print(f"=== error labels: {bad} of {len(want)} divergent ===")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
