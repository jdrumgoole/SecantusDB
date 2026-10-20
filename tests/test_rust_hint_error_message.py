"""The Rust server's bad-hint error is mongod's planner diagnostic.

mongod 8.2.11 refuses a hint that names no index from its query planner, and
prints the query it was planning: `error processing query: ns=<ns>Tree:
<query tree>\nSort: ...\nProj: ...\n planner returned error :: caused by ::
hint provided does not correspond to an existing index`. The Rust server
reproduces it (`explain::debug_string`, 2026-10-20); the cases here are the
replies mongod gave for these commands, measured that day.

This file began as a guard against a RUST type name leaking into the message
(`hint String("x") does not correspond...`, Rust's `Debug` on a `Bson`). The
message no longer quotes the hint at all, and the guard stays.

Gated on the `_secantus_server` extension, like `test_rust_server_smoke.py`.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")


@pytest.fixture(scope="module")
def rs(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("rs_hint") / "wt"), 0)
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def db(rs):
    host, port = rs.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    d = cli["hinterr"]
    d.c.drop()
    d.c.insert_one({"_id": 1})
    try:
        yield d
    finally:
        cli.close()


COMMANDS = {
    "find": {"find": "c", "hint": "x"},
    "count": {"count": "c", "hint": "x"},
    "delete": {"delete": "c", "deletes": [{"q": {}, "limit": 0, "hint": "x"}]},
    "update": {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}, "hint": "x"}]},
    "findAndModify": {
        "findAndModify": "c",
        "query": {},
        "update": {"$set": {"a": 1}},
        "hint": "x",
    },
}


def _message(db, cmd: dict) -> str:
    try:
        reply = db.command(cmd)
    except pymongo.errors.OperationFailure as exc:
        return str(exc.details.get("errmsg", ""))
    # `update` / `delete` report a bad hint per statement, with `ok: 1`.
    errors = reply.get("writeErrors") or []
    assert errors, f"expected a bad-hint error for {cmd}, got {reply}"
    return str(errors[0].get("errmsg", ""))


# What mongod 8.2.11 answered for each command over an empty filter.
MONGOD_SAYS = (
    "error processing query: ns=hinterr.cTree: $and\nSort: {}\nProj: {}\n planner "
    "returned error :: caused by :: hint provided does not correspond to an existing index"
)


@pytest.mark.parametrize("name", sorted(COMMANDS), ids=sorted(COMMANDS))
def test_the_hint_error_is_mongods_planner_diagnostic(db, name: str) -> None:
    message = _message(db, COMMANDS[name])
    assert message == MONGOD_SAYS
    assert "String(" not in message, "Rust's Debug format leaked to the client"


def test_a_key_spec_hint_is_refused_in_the_same_words(db) -> None:
    message = _message(db, {"find": "c", "hint": {"nope": 1}})
    assert message == MONGOD_SAYS
    for leak in ("Int32(", "Document(", "String("):
        assert leak not in message, f"Rust's Debug format leaked: {leak}"
