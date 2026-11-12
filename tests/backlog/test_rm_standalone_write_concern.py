"""RM-551: a standalone's write concern is a replica set's.

`secantusd-rs --standalone` keeps the replica-set member's write-concern
machinery: its implicit default is `w: "majority"`, so a write sent without
a write concern waits for the journal under the transition lock and the
global lock in IS, and it accepts `setDefaultRWConcern`. mongod 8.2.11 as a
standalone implies `w: 1` (no journal wait, no locks for one) and refuses
both default-concern commands with 51300 (measured 2026-11-12)."""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer, rust_mongo_server

#: Internal resources mongod's background threads lock; this server has none.
_INTERNAL = {"Metadata", "oplog", "MultiDocumentTransactionsBarrier"}


@pytest.fixture(scope="module")
def rust_standalone(tmp_path_factory: pytest.TempPathFactory) -> Iterator[Any]:
    with rust_mongo_server(tmp_path_factory.mktemp("standalone"), "--standalone") as client:
        yield client


def _flat(client: Any) -> dict:
    locks = client.admin.command("serverStatus").get("locks", {})
    return {
        (res, mode): n
        for res, v in locks.items()
        if res not in _INTERNAL
        for mode, n in v.get("acquireCount", {}).items()
    }


def _insert_locks(client: Any) -> dict:
    """The locks an `insert` sent with no write concern takes: the least of
    five runs, as a background thread's lock can only add to one."""
    db = client["rm_standalone_wc"]
    db.c.insert_one({})
    runs = []
    for _ in range(5):
        before = _flat(client)
        db.command("insert", "c", documents=[{}])
        after = _flat(client)
        runs.append({k: after.get(k, 0) - before.get(k, 0) for k in set(before) | set(after)})
    client.drop_database("rm_standalone_wc")
    keys = set().union(*runs)
    least = {k: min(r.get(k, 0) for r in runs) for k in keys}
    return {f"{r}.{m}": n for (r, m), n in sorted(least.items()) if n}


def _answers(client: Any) -> dict:
    return {
        "getDefaultRWConcern": mongo_answer(lambda: client.admin.command("getDefaultRWConcern"))[
            :2
        ],
        "insert locks": _insert_locks(client),
        # Last: on this server it stores a default, which the insert above
        # must not see.
        "setDefaultRWConcern": mongo_answer(
            lambda: client.admin.command("setDefaultRWConcern", defaultWriteConcern={"w": 1})
        )[:2],
    }


@pytest.mark.backlog("RM-551")
def test_a_standalone_implies_w_1(rust_standalone: Any, mongod: Any) -> None:
    """mongod 8.2.11 as a standalone: an insert with no write concern takes
    one write round and no journal wait (`w: 1` implied), and both
    default-concern commands answer 51300. This server's standalone waited
    for the journal (one more transition IX and global IS) and accepted
    `setDefaultRWConcern`."""
    want = _answers(mongod)
    assert want["setDefaultRWConcern"] == ("err", 51300)
    assert "Global.r" not in want["insert locks"], want
    assert _answers(rust_standalone) == want
