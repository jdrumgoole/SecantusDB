"""The Rust server's bad-hint error must not leak a RUST type name.

mongod's own message here is a multi-line planner diagnostic, and this project
deliberately does not reproduce it — the CODE matches and the text names the
hint instead (`tasks/backlog.md`; the wording moved between 6.0.16 and 8.2.11,
so the gate asserts the rejection rather than the text).

What was not deliberate: three of the five commands formatted the hint with
Rust's `Debug` on a `Bson`, so a client got

    hint String("x") does not correspond to an existing index

`String(…)` is a Rust type name and means nothing to a MongoDB client. The
Python server says `'x'` for the same input, and `find` / `count` already said
`"x"` — only the write commands leaked it.

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


@pytest.mark.parametrize("name", sorted(COMMANDS), ids=sorted(COMMANDS))
def test_the_hint_error_names_the_value_not_the_rust_type(db, name: str) -> None:
    message = _message(db, COMMANDS[name])
    assert message == 'hint "x" does not correspond to an existing index'
    assert "String(" not in message, "Rust's Debug format leaked to the client"


def test_a_key_spec_hint_is_rendered_as_a_document(db) -> None:
    message = _message(db, {"find": "c", "hint": {"nope": 1}})
    assert "does not correspond to an existing index" in message
    for leak in ("Int32(", "Document(", "String("):
        assert leak not in message, f"Rust's Debug format leaked: {leak}"
