"""What a `Slow query` line and a profile entry say of a drop, against
mongod 8.2.11's (found while working RM-363, 2026-11-07)."""

from __future__ import annotations

import contextlib
import json
from typing import Any

import pytest
from pymongo.errors import OperationFailure

from tests.backlog.opdebug import fresh, seed


def _line_namespaces(client: Any, is_mongod: bool, commands: list[dict]) -> dict:
    """The `ns` of each command's `Slow query` line, by command name."""
    db = client[fresh("dropdbns")]
    seed(db)
    last = client.admin.command("getLog", "global")["log"][-1]
    db.command("profile", 0, slowms=-1 if is_mongod else 0)
    try:
        for command in commands:
            shown = {
                k: v.replace("<db>", db.name) if isinstance(v, str) else v
                for k, v in command.items()
            }
            # `getLog` is refused off `admin`, `compact` on a primary; the
            # line of each is still written.
            with contextlib.suppress(OperationFailure):
                db.command(shown)
    finally:
        db.command("profile", 0, slowms=100)
    log = client.admin.command("getLog", "global")["log"]
    names = {next(iter(c)) for c in commands}
    found = {}
    for raw in log[log.index(last) + 1 :]:
        entry = json.loads(raw)
        command = entry.get("attr", {}).get("command", {})
        if entry.get("msg") == "Slow query" and isinstance(command, dict):
            name = next(iter(command), "")
            if name in names:
                found[name] = entry["attr"].get("ns", "").replace(db.name, "<db>")
    client.drop_database(db.name)
    return found


@pytest.mark.backlog_fixed("RM-383")
def test_a_drop_database_line_names_the_database(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a `dropDatabase`, and for a `dbStats`,
    names the database alone (`ns: "<db>"`); this server's named
    `<db>.$cmd`, as for a command that runs on no collection."""
    commands = [{"dbStats": 1}, {"dropDatabase": 1}]
    answers = {
        "rust": _line_namespaces(rust_mongo, False, commands),
        "mongod": _line_namespaces(mongod_rs, True, commands),
    }
    assert answers["mongod"] == {"dbStats": "<db>", "dropDatabase": "<db>"}
    assert answers["rust"] == answers["mongod"]


@pytest.mark.backlog_fixed("RM-495")
def test_a_command_s_string_argument_is_not_its_collection(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's lines for a `validate`, a `dataSize`, a `getLog` and
    a `compact` (refused on a replica-set primary, its line still written)
    name `<db>.$cmd`; this server's named the command's string argument as
    though it were a collection (`<db>.<db>.c`, `<db>.startupWarnings`,
    `<db>.c`; a `validate`'s named `<db>.$cmd` since RM-485)."""
    commands = [
        {"validate": "c"},
        {"dataSize": "<db>.c"},
        {"getLog": "startupWarnings"},
        {"compact": "c"},
    ]
    answers = {
        "rust": _line_namespaces(rust_mongo, False, commands),
        "mongod": _line_namespaces(mongod_rs, True, commands),
    }
    assert answers["mongod"] == {
        "validate": "<db>.$cmd",
        "dataSize": "<db>.$cmd",
        "getLog": "<db>.$cmd",
        "compact": "<db>.$cmd",
    }
    assert answers["rust"] == answers["mongod"]


def _view_drop_entries(client: Any, is_mongod: bool) -> list:
    db = client[fresh("dropview")]
    seed(db)
    db.command("create", "v", viewOn="c", pipeline=[])
    db.command("profile", 2, slowms=-1 if is_mongod else 0)
    try:
        db.drop_collection("v")
    finally:
        db.command("profile", 0, slowms=100)
    entries = [
        (
            e.get("op"),
            e.get("ns", "").replace(db.name, "<db>"),
            next(iter(e.get("command", {})), ""),
        )
        for e in db.system.profile.find().sort("$natural", 1)
    ]
    client.drop_database(db.name)
    return [e for e in entries if e[2] != "profile"]


@pytest.mark.backlog_fixed("RM-384")
def test_dropping_a_view_is_profiled(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 profiles a `drop` of a view as it profiles a `drop` of
    a collection (`{op: "command", ns: "<db>.v"}`); this server wrote the
    view's `drop` line and no profile entry, taking its `{ns, ok}` reply,
    which has no index count, for a drop of nothing."""
    answers = {
        "rust": _view_drop_entries(rust_mongo, False),
        "mongod": _view_drop_entries(mongod_rs, True),
    }
    assert answers["mongod"] == [("command", "<db>.v", "drop")]
    assert answers["rust"] == answers["mongod"]
