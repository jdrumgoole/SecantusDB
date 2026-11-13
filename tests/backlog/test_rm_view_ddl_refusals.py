"""What a `dataSize` of a view and a `dropIndexes` of `system.views` answer,
against mongod 8.2.11 (found while working RM-485, 2026-11-13)."""

from __future__ import annotations

from typing import Any

import pytest
from pymongo.errors import OperationFailure

from tests.backlog.opdebug import fresh, seed


def _answer(client: Any, op: Any) -> tuple:
    db = client[fresh("viewddl")]
    seed(db)
    db.command("create", "v", viewOn="c", pipeline=[])
    try:
        reply = op(db)
        return ("ok", reply.get("ok"))
    except OperationFailure as e:
        return ("err", e.code)
    finally:
        client.drop_database(db.name)


@pytest.mark.backlog("RM-587")
def test_a_data_size_of_a_view_is_refused(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 refuses a `dataSize` of a view with
    CommandNotSupportedOnView; this server answers `ok: 1`."""

    def op(db: Any) -> Any:
        return db.command("dataSize", f"{db.name}.v")

    got = {"rust": _answer(rust_mongo, op), "mongod": _answer(mongod_rs, op)}
    assert got["mongod"] == ("err", 166)
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-588")
def test_a_drop_indexes_of_system_views_answers(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 drops the indexes of `<db>.system.views`, which a view
    creates; this server refuses it with NamespaceNotFound."""

    def op(db: Any) -> Any:
        return db.command("dropIndexes", "system.views", index="*")

    got = {"rust": _answer(rust_mongo, op), "mongod": _answer(mongod_rs, op)}
    assert got["mongod"] == ("ok", 1.0)
    assert got["rust"] == got["mongod"]
