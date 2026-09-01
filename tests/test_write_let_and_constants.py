"""``let`` and per-statement ``c`` on the write commands.

Found by the pymongo gauge on 2026-09-01: three `*_with_let_option` tests in
`test_crud_unified.py` failed because a pipeline update naming a `let` variable
was REFUSED with `Use of undefined variable`. The variable was bound; the
parse-time constant-FOLD check simply had no value for it, folded anyway, and
reported the evaluator's undefined-variable error as though the query were
wrong. `update` silently applied nothing (`n: 0` with a writeError);
`findAndModify` failed the whole command.

Everything here is mongod 8.2.11's behaviour, measured the same day.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient
from pymongo.errors import OperationFailure

from secantus import SecantusDBServer


@pytest.fixture(scope="module")
def server(wt_home_module):
    with SecantusDBServer(port=0, storage_path=wt_home_module) as srv:
        yield srv


@pytest.fixture
def db(server, request):
    client = MongoClient(server.uri, serverSelectionTimeoutMS=5000)
    database = client["letdb_" + request.node.name[:40]]
    database.c.drop()
    database.c.insert_many([{"_id": i} for i in (1, 2, 3)])
    try:
        yield database
    finally:
        client.close()


def _docs(db):
    return list(db.c.find({}, sort=[("_id", 1)]))


def test_command_let_reaches_a_pipeline_update(db):
    reply = db.command(
        {
            "update": "c",
            "let": {"x": 5},
            "updates": [{"q": {}, "u": [{"$set": {"v": "$$x"}}], "multi": True}],
        }
    )
    assert (reply["n"], reply["nModified"]) == (3, 3)
    assert _docs(db) == [{"_id": 1, "v": 5}, {"_id": 2, "v": 5}, {"_id": 3, "v": 5}]


def test_command_let_reaches_a_find_and_modify_pipeline(db):
    reply = db.command(
        {
            "findAndModify": "c",
            "let": {"x": 9},
            "query": {},
            "update": [{"$set": {"v": "$$x"}}],
            "new": True,
        }
    )
    assert reply["value"] == {"_id": 1, "v": 9}


def test_statement_constants_are_bound(db):
    """``c`` is the STATEMENT's own constants map. It was not bound at all."""
    reply = db.command(
        {
            "update": "c",
            "updates": [{"q": {}, "u": [{"$set": {"v": "$$y"}}], "c": {"y": 7}, "multi": True}],
        }
    )
    assert (reply["n"], reply["nModified"]) == (3, 3)
    assert all(d["v"] == 7 for d in _docs(db))


def test_statement_constants_win_over_command_let(db):
    reply = db.command(
        {
            "update": "c",
            "let": {"y": 1},
            "updates": [{"q": {}, "u": [{"$set": {"v": "$$y"}}], "c": {"y": 7}, "multi": True}],
        }
    )
    assert reply["nModified"] == 3
    assert all(d["v"] == 7 for d in _docs(db))


def test_constants_are_rejected_on_a_non_pipeline_update(db):
    reply = db.command(
        {"update": "c", "updates": [{"q": {}, "u": {"$set": {"w": 1}}, "c": {"z": 2}}]}
    )
    assert reply["n"] == 0
    assert reply["writeErrors"][0]["code"] == 51198
    assert (
        reply["writeErrors"][0]["errmsg"]
        == "Constant values may only be specified for pipeline updates"
    )
    assert _docs(db) == [{"_id": 1}, {"_id": 2}, {"_id": 3}]


def test_a_genuinely_undefined_variable_is_still_rejected(db):
    """The fix must not turn the check off -- only stop it firing for a name the
    command DOES bind."""
    reply = db.command(
        {"update": "c", "updates": [{"q": {}, "u": [{"$set": {"v": "$$zz"}}], "multi": True}]}
    )
    assert reply["n"] == 0
    assert reply["writeErrors"][0]["code"] == 17276
    assert reply["writeErrors"][0]["errmsg"] == (
        "Invalid $set :: caused by :: Use of undefined variable: zz"
    )


def test_a_constant_that_errors_takes_the_executor_prefix(db):
    """With the VALUE in hand the fold runs, and mongod reports it from the
    executor -- naming the command, so ``update`` and ``findAndModify`` differ."""
    reply = db.command(
        {
            "update": "c",
            "let": {"cv": "x"},
            "updates": [{"q": {}, "u": [{"$set": {"v": {"$abs": "$$cv"}}}], "multi": True}],
        }
    )
    assert reply["writeErrors"][0]["errmsg"] == (
        "Plan executor error during update :: caused by :: "
        "$abs only supports numeric types, not string"
    )
    with pytest.raises(OperationFailure) as exc:
        db.command(
            {
                "findAndModify": "c",
                "let": {"cv": "x"},
                "query": {},
                "update": [{"$set": {"v": {"$abs": "$$cv"}}}],
            }
        )
    assert exc.value.details["errmsg"] == (
        "Plan executor error during findAndModify :: caused by :: "
        "$abs only supports numeric types, not string"
    )


@pytest.mark.parametrize(
    ("command", "statements", "field", "struct"),
    [
        ("delete", "deletes", "c", "delete.deletes"),
        ("delete", "deletes", "zz", "delete.deletes"),
        # No `$`-prefix carve-out inside a nested STATEMENT, unlike the command
        # envelope.
        ("delete", "deletes", "$db", "delete.deletes"),
        ("update", "updates", "zz", "update.updates"),
        ("update", "updates", "$db", "update.updates"),
    ],
)
def test_unknown_statement_fields_are_rejected(db, command, statements, field, struct):
    statement = (
        {"q": {}, "limit": 1} if command == "delete" else {"q": {}, "u": [{"$set": {"a": 1}}]}
    )
    statement[field] = 1
    with pytest.raises(OperationFailure) as exc:
        db.command({command: "c", statements: [statement]})
    assert exc.value.details["code"] == 40415
    assert exc.value.details["errmsg"] == f"BSON field '{struct}.{field}' is an unknown field."
    # Nothing is written: mongod parses every statement before running any.
    assert _docs(db) == [{"_id": 1}, {"_id": 2}, {"_id": 3}]
