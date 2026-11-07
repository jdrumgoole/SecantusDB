"""What a `Slow query` line says of a read of a view, against mongod 8.2's
(found while working RM-362, 2026-11-07)."""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.opdebug import logged, seed


def _setup(db: Any) -> None:
    seed(db)
    db.command("create", "v", viewOn="c", pipeline=[{"$match": {"x": {"$gte": 1}}}])


def _lines(rust_mongo: Any, mongod_rs: Any, op: Any) -> dict:
    """`(command, ns, collectionType)` of each line `op` wrote, per server,
    with the database's name left out."""
    out = {}
    for server, client, is_mongod in (("rust", rust_mongo, False), ("mongod", mongod_rs, True)):
        lines, _ = logged(client, is_mongod, _setup, op)
        out[server] = [
            (
                next(iter(attr.get("command", {})), ""),
                str(attr.get("ns", "")).split(".", 1)[1],
                attr.get("collectionType"),
            )
            for _, attr in lines
        ]
    return out


@pytest.mark.backlog("RM-380")
def test_a_get_more_of_a_view_is_of_a_normal_collection(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.1's line for a `getMore` of a cursor over a view names the
    view as `collectionType: "normal"`; this server's says `view`."""
    got = _lines(rust_mongo, mongod_rs, lambda d: list(d.v.find(batch_size=4)))
    assert [g for g in got["mongod"] if g[0] == "getMore"] == [("getMore", "v", "normal")] * 2
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-380")
def test_a_find_and_modify_of_a_view_is_of_a_normal_collection(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.1's line for a `findAndModify` of a view, which it
    refuses, says `collectionType: "normal"`; this server's says `view`."""
    got = _lines(
        rust_mongo, mongod_rs, lambda d: d.v.find_one_and_update({"x": 1}, {"$set": {"q": 1}})
    )
    assert got["mongod"] == [("findAndModify", "v", "normal")]
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-380")
def test_an_explain_line_names_what_it_explains(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.1's line for an `explain` of a `find` of a view names the
    view (`<db>.v`, `collectionType: "view"`); this server's names
    `<db>.$cmd`, `normal`."""
    got = _lines(rust_mongo, mongod_rs, lambda d: d.command("explain", {"find": "v", "filter": {}}))
    assert got["mongod"] == [("explain", "v", "view")]
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-380")
def test_a_read_of_a_view_that_fails_names_its_collection(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.1's line for a read of a view that fails once it runs
    names the collection beneath the view (`<db>.c`, `normal`); this
    server's names the view."""
    got = _lines(
        rust_mongo,
        mongod_rs,
        lambda d: list(d.v.find({"$expr": {"$divide": [1, {"$subtract": ["$x", "$x"]}]}})),
    )
    assert got["mongod"] == [("find", "c", "normal")]
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-381")
def test_a_distinct_of_a_view_says_how_long_it_planned(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.1's line for a `distinct` of a view carries
    `planningTimeMicros`, as one of a collection does; this server's has
    none."""
    got = {}
    for server, client, is_mongod in (("rust", rust_mongo, False), ("mongod", mongod_rs, True)):
        lines, _ = logged(client, is_mongod, _setup, lambda d: d.v.distinct("y"))
        got[server] = [
            "planningTimeMicros" in attr
            for _, attr in lines
            if next(iter(attr.get("command", {})), "") == "distinct"
        ]
    assert got["mongod"] == [True]
    assert got["rust"] == got["mongod"]
