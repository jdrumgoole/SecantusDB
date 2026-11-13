"""What a `renameCollection` refuses, and how, against mongod 8.2.1's
(found while working RM-378, 2026-11-09)."""

from __future__ import annotations

from typing import Any

import pytest
from pymongo.errors import OperationFailure

from tests.backlog.opdebug import fresh


def _rename_system_views(client: Any) -> tuple[int | None, str]:
    db = client[fresh("renamesys")]
    db.c.insert_one({"_id": 1})
    db.command("create", "v", viewOn="c", pipeline=[])
    try:
        client.admin.command({"renameCollection": f"{db.name}.system.views", "to": f"{db.name}.c9"})
        answer: tuple[int | None, str] = (None, "ok")
    except OperationFailure as e:
        answer = (e.code, str(e.details.get("errmsg", "")).replace(db.name, "<db>"))
    client.drop_database(db.name)
    return answer


@pytest.mark.backlog_fixed("RM-482")
def test_renaming_system_views_is_a_write_to_it(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.1 refuses to rename `<db>.system.views` as a write to it
    (20 IllegalOperation, "error with source namespace: cannot write to
    <db>.system.views"); this server refused it as a view (166, "cannot
    rename view: <db>.system.views"), its view check running before the
    namespace check."""
    answers = {
        "rust": _rename_system_views(rust_mongo),
        "mongod": _rename_system_views(mongod_rs),
    }
    assert answers["mongod"] == (
        20,
        "error with source namespace: cannot write to <db>.system.views",
    )
    assert answers["rust"] == answers["mongod"]


def _rename(client: Any, source: str, target: str) -> tuple[int | None, str]:
    """Rename `<db>.<source>` to `<db>.<target>` in a database holding a
    collection `c` and a view `v` on it, and return the code and message."""
    db = client[fresh("renamens")]
    db.c.insert_one({"_id": 1})
    db.command("create", "v", viewOn="c", pipeline=[])
    try:
        client.admin.command(
            {"renameCollection": f"{db.name}.{source}", "to": f"{db.name}.{target}"}
        )
        answer: tuple[int | None, str] = (None, "ok")
    except OperationFailure as e:
        answer = (e.code, str(e.details.get("errmsg", "")).replace(db.name, "<db>"))
    client.drop_database(db.name)
    return answer


@pytest.mark.backlog_fixed("RM-482")
@pytest.mark.parametrize(
    ("source", "target"),
    [
        ("c", "system.views"),
        ("v", "system.views"),
        ("v", "system.profile"),
        ("system.views", "v"),
        ("system.views", "system.profile"),
    ],
)
def test_a_rename_touching_system_views_is_refused_by_namespace(
    rust_mongo: Any, mongod_rs: Any, source: str, target: str
) -> None:
    """The namespace check comes before the view check, in both directions:
    renaming onto `<db>.system.views` is a write to it, not onto a view."""
    assert _rename(rust_mongo, source, target) == _rename(mongod_rs, source, target)
