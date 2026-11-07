"""The namespace a `Slow query` line names, against mongod 8.2's (found
while working RM-361, 2026-11-07)."""

from __future__ import annotations

import json
from typing import Any

import pytest

from tests.backlog.opdebug import fresh


def _rename_line_ns(client: Any, is_mongod: bool) -> str | None:
    db = client[fresh("renamens")]
    db.c.insert_one({"_id": 1})
    db.command("profile", 2, slowms=-1 if is_mongod else 0)
    try:
        db.c.rename("c2")
    finally:
        db.command("profile", 0, slowms=100)
    found = None
    for raw in client.admin.command("getLog", "global")["log"]:
        entry = json.loads(raw)
        command = entry.get("attr", {}).get("command", {})
        if (
            entry.get("msg") == "Slow query"
            and isinstance(command, dict)
            and command.get("renameCollection") == f"{db.name}.c"
        ):
            found = entry["attr"].get("ns")
    client.drop_database(db.name)
    return found.replace(db.name, "<db>") if found else found


@pytest.mark.backlog("RM-378")
def test_a_rename_line_names_the_collection_it_renames(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.1's line for a `renameCollection`, which runs against
    `admin`, names the collection it renames (`<db>.c`); this server's
    names `admin.<db>.c`, the command's database joined to its argument."""
    answers = {
        "rust": _rename_line_ns(rust_mongo, False),
        "mongod": _rename_line_ns(mongod_rs, True),
    }
    assert answers["mongod"] == "<db>.c"
    assert answers["rust"] == answers["mongod"]
