"""What a `Slow query` line and a profile entry say of a drop, against
mongod 8.2.11's (found while working RM-363, 2026-11-07)."""

from __future__ import annotations

import json
from typing import Any

import pytest

from tests.backlog.opdebug import fresh, seed


def _drop_database_line_ns(client: Any, is_mongod: bool) -> str | None:
    db = client[fresh("dropdbns")]
    seed(db)
    last = client.admin.command("getLog", "global")["log"][-1]
    db.command("profile", 2, slowms=-1 if is_mongod else 0)
    try:
        db.command("dropDatabase")
    finally:
        db.command("profile", 0, slowms=100)
    log = client.admin.command("getLog", "global")["log"]
    found = None
    for raw in log[log.index(last) + 1 :]:
        entry = json.loads(raw)
        command = entry.get("attr", {}).get("command", {})
        if (
            entry.get("msg") == "Slow query"
            and isinstance(command, dict)
            and "dropDatabase" in command
        ):
            found = entry["attr"].get("ns")
    client.drop_database(db.name)
    return found.replace(db.name, "<db>") if found else found


@pytest.mark.backlog("RM-383")
def test_a_drop_database_line_names_the_database(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a `dropDatabase` names the database alone
    (`ns: "<db>"`); this server's names `<db>.$cmd`, as for a command that
    runs on no collection."""
    answers = {
        "rust": _drop_database_line_ns(rust_mongo, False),
        "mongod": _drop_database_line_ns(mongod_rs, True),
    }
    assert answers["mongod"] == "<db>"
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


@pytest.mark.backlog("RM-384")
def test_dropping_a_view_is_profiled(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 profiles a `drop` of a view as it profiles a `drop` of
    a collection (`{op: "command", ns: "<db>.v"}`); this server writes the
    view's `drop` line and no profile entry."""
    answers = {
        "rust": _view_drop_entries(rust_mongo, False),
        "mongod": _view_drop_entries(mongod_rs, True),
    }
    assert answers["mongod"] == [("command", "<db>.v", "drop")]
    assert answers["rust"] == answers["mongod"]
