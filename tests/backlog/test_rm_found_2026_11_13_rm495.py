"""Gap tests for what a sweep of every command's `Slow query` line found on
2026-11-13 while working RM-495.

The reference is mongod 8.2.11 (`mongod_rs`); the Rust server is
`secantusd-rs` (`rust_mongo`).
"""

from __future__ import annotations

import re
from typing import Any

import pytest
from pymongo.errors import OperationFailure

from tests.backlog.opdebug import fresh, seed
from tests.backlog.test_rm_drop_reports import _line_namespaces


@pytest.mark.backlog("RM-595")
def test_a_command_refused_before_it_acquires_its_collection_names_cmd(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line for a `create` of a collection that exists
    (which succeeds) and for a `findAndModify` with neither `update` nor
    `remove` (FailedToParse) names `<db>.$cmd`: neither acquired the
    collection. This server's name `<db>.c`."""
    commands = [{"create": "c"}, {"findAndModify": "c"}]
    answers = {
        "rust": _line_namespaces(rust_mongo, False, commands),
        "mongod": _line_namespaces(mongod_rs, True, commands),
    }
    assert answers["mongod"] == {"create": "<db>.$cmd", "findAndModify": "<db>.$cmd"}
    assert answers["rust"] == answers["mongod"]


def _shown(found: dict) -> dict:
    """`convertToCapped`'s temporary collection, whose name is random, as
    `<tmp>`."""
    return {k: re.sub(r"\.tmp[A-Za-z0-9]{5}\.", ".<tmp>.", v) for k, v in found.items()}


@pytest.mark.backlog("RM-596")
def test_a_line_names_the_collection_a_command_works_through(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line names the collection a command works through,
    not its value: a `filemd5` its root's chunks (`<db>.fs.chunks`), a
    `convertToCapped` the temporary collection it builds
    (`<db>.tmpXXXXX.convertToCapped.c`), a `rolesInfo` of a role
    `admin.system.roles` (`rolesInfo: 1` names `<db>.$cmd`).
    This server's name `<db>.$cmd`, `<db>.c` and `<db>.$cmd`."""
    commands = [
        {"filemd5": 1, "root": "fs"},
        {"rolesInfo": "c"},
        {"convertToCapped": "c", "size": 100000},
    ]
    answers = {
        "rust": _shown(_line_namespaces(rust_mongo, False, commands)),
        "mongod": _shown(_line_namespaces(mongod_rs, True, commands)),
    }
    assert answers["mongod"] == {
        "filemd5": "<db>.fs.chunks",
        "rolesInfo": "admin.system.roles",
        "convertToCapped": "<db>.<tmp>.convertToCapped.c",
    }
    assert answers["rust"] == answers["mongod"]


def _delete_indexes(client: Any) -> Any:
    db = client[fresh("delidx")]
    seed(db)
    db.c.create_index("q")
    try:
        reply = db.command({"deleteIndexes": "c", "index": "q_1"})
        answer: Any = ("ok", reply.get("nIndexesWas"))
    except OperationFailure as e:
        answer = ("error", e.code)
    client.drop_database(db.name)
    return answer


@pytest.mark.backlog("RM-597")
def test_delete_indexes_is_drop_indexes(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 runs `deleteIndexes`, `dropIndexes`'s alias; this
    server refuses it as a command it does not know (59)."""
    answers = {"rust": _delete_indexes(rust_mongo), "mongod": _delete_indexes(mongod_rs)}
    assert answers["mongod"][0] == "ok"
    assert answers["rust"] == answers["mongod"]
