"""The command an `update` or `delete` statement's `Slow query` line shows,
against mongod 8.2's (found while working RM-478, 2026-11-13)."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.opdebug import logged


def _statement_commands(client: Any, is_mongod: bool) -> list[dict]:
    """The `command` of each statement's line (the lines naming the
    collection, not `<db>.$cmd`) of a commented `delete` and `update`."""

    def setup(db: Any) -> None:
        db.c.insert_many([{"_id": 1}, {"_id": 2}])

    def op(db: Any) -> None:
        db.command({"delete": "c", "deletes": [{"q": {"_id": 1}, "limit": 1}], "comment": "probe"})
        db.command(
            {
                "update": "c",
                "updates": [{"q": {"_id": 2}, "u": {"$set": {"a": 1}}}],
                "comment": "probe",
            }
        )

    lines, _ = logged(client, is_mongod, setup, op)
    return [
        {k: v for k, v in attr["command"].items() if k != "lsid"}
        for _, attr in lines
        if not attr["ns"].endswith(".$cmd")
    ]


@pytest.mark.backlog("RM-581")
def test_a_statement_line_carries_its_commands_comment(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a `delete` or `update` statement shows the
    statement with its command's `comment` (`{q, limit, comment}`,
    `{q, u, multi, upsert, comment}`); this server's shows the statement without it."""
    mongod = _statement_commands(mongod_rs, True)
    assert [c.get("comment") for c in mongod] == ["probe", "probe"]
    assert _statement_commands(rust_mongo, False) == mongod
