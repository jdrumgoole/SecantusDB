"""What a line, a profile entry and `serverStatus` say around a view or a
time-series collection, against mongod 8.2.11 (found while working RM-380,
2026-11-10)."""

from __future__ import annotations

import contextlib
import datetime
from collections.abc import Callable
from typing import Any

import pytest
from pymongo.errors import PyMongoError

from tests.backlog.opdebug import fresh, logged, seed


def _setup(db: Any) -> None:
    seed(db)
    db.command("create", "v", viewOn="c", pipeline=[{"$match": {"x": {"$gte": 1}}}])
    db.create_collection("ts", timeseries={"timeField": "t", "metaField": "m"})
    db.ts.insert_many([{"t": datetime.datetime(2020, 1, 1), "m": i} for i in range(10)])


def _refused(op: Callable[[Any], Any]) -> Callable[[Any], None]:
    def run(db: Any) -> None:
        with contextlib.suppress(PyMongoError):
            op(db)

    return run


def _both(rust_mongo: Any, mongod_rs: Any, op: Callable[[Any], Any]) -> dict:
    return {
        server: logged(client, is_mongod, _setup, op)
        for server, client, is_mongod in (
            ("rust", rust_mongo, False),
            ("mongod", mongod_rs, True),
        )
    }


def _first(attr: dict) -> str:
    return next(iter(attr.get("command", {})), "")


def _ns(attr: dict) -> str:
    return str(attr.get("ns", "")).split(".", 1)[1]


def _lines(rust_mongo: Any, mongod_rs: Any, op: Callable[[Any], Any]) -> dict:
    """`(command, ns, collectionType, ok)` of each line `op` wrote."""
    return {
        server: [(_first(a), _ns(a), a.get("collectionType"), a.get("ok", 1)) for _, a in lines]
        for server, (lines, _) in _both(rust_mongo, mongod_rs, op).items()
    }


def _entries(rust_mongo: Any, mongod_rs: Any, op: Callable[[Any], Any]) -> dict:
    """`(command, ns, ok)` of each profile entry `op` wrote."""
    return {
        server: [(_first(e), _ns(e), e.get("ok", 1)) for e in entries]
        for server, (_, entries) in _both(rust_mongo, mongod_rs, op).items()
    }


@pytest.mark.backlog_fixed("RM-483")
def test_a_read_refused_before_it_resolves_names_the_view(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 refuses a `find` of a view whose projection does not
    parse, and an `explain` of a `find` of a view whose filter does not,
    before it resolves the view: its line names the view, `normal`, with no
    `resolvedViews`. This server resolved the view first."""

    def op(db: Any) -> None:
        _refused(lambda d: list(d.v.find({}, projection={"a": {"$bogusop": 1}})))(db)
        _refused(lambda d: d.command("explain", {"find": "v", "filter": {"x": {"$bogus": 1}}}))(db)

    got = {
        server: [
            (_first(a), _ns(a), a.get("collectionType"), "resolvedViews" in a) for _, a in lines
        ]
        for server, (lines, _) in _both(rust_mongo, mongod_rs, op).items()
    }
    assert got["mongod"] == [("find", "v", "normal", False), ("explain", "v", "normal", False)]
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog_fixed("RM-484")
def test_a_time_series_ddl_line_names_its_buckets(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a `collMod` or a `collStats` of a time-series
    collection names its buckets, `<db>.system.buckets.ts`,
    `timeseriesBuckets`; this server's named `<db>.ts`, `normal`."""

    def op(db: Any) -> None:
        db.command("collMod", "ts", expireAfterSeconds=100)
        db.command("collStats", "ts")

    got = _lines(rust_mongo, mongod_rs, op)
    assert got["mongod"] == [
        ("collMod", "system.buckets.ts", "timeseriesBuckets", 1),
        ("collStats", "system.buckets.ts", "timeseriesBuckets", 1),
    ]
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog_fixed("RM-485")
def test_a_refused_validate_of_a_view_names_cmd(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line for a `validate` or a `dropIndexes` of a view,
    each refused, names `<db>.$cmd`; this server's named the view."""

    def op(db: Any) -> None:
        _refused(lambda d: d.command("validate", "v"))(db)
        _refused(lambda d: d.command("dropIndexes", "v", index="*"))(db)

    got = _lines(rust_mongo, mongod_rs, op)
    assert got["mongod"] == [
        ("validate", "$cmd", "normal", 0),
        ("dropIndexes", "$cmd", "normal", 0),
    ]
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog_fixed("RM-485")
def test_an_unacquired_drop_indexes_names_cmd_and_is_not_profiled(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's `dropIndexes` of a collection that does not exist is
    refused before it acquires one: its line names `<db>.$cmd` and it is
    profiled nowhere. One refused once it holds its collection (an index
    that does not exist) names the collection and is profiled. This server
    named the missing collection and profiled it."""

    def op(db: Any) -> None:
        _refused(lambda d: d.command("dropIndexes", "nope", index="*"))(db)
        _refused(lambda d: d.command("dropIndexes", "c", index="nope_1"))(db)

    got = {
        server: (
            [(_first(a), _ns(a), a.get("ok", 1), a.get("errCode")) for _, a in lines],
            [(_first(e), _ns(e)) for e in entries],
        )
        for server, (lines, entries) in _both(rust_mongo, mongod_rs, op).items()
    }
    assert got["mongod"] == (
        [("dropIndexes", "$cmd", 0, 26), ("dropIndexes", "c", 0, 27)],
        [("dropIndexes", "c")],
    )
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog_fixed("RM-486")
def test_an_explain_of_a_write_is_not_profiled(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 writes no profile entry for an `explain` of an
    `update`, a `delete` or a `findAndModify`, at any verbosity; this server
    wrote one, named for the collection."""

    def op(db: Any) -> None:
        for write in (
            {"update": "c", "updates": [{"q": {}, "u": {"$set": {"a": 1}}}]},
            {"delete": "c", "deletes": [{"q": {"x": 1}, "limit": 1}]},
            {"findAndModify": "c", "query": {"x": 1}, "update": {"$set": {"a": 1}}},
        ):
            for verbosity in ("queryPlanner", "executionStats"):
                db.command("explain", write, verbosity=verbosity)

    got = _entries(rust_mongo, mongod_rs, op)
    assert got["mongod"] == []
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-487")
def test_a_write_to_a_view_has_a_statement_line(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's `update` and `delete` of a view, each refused, write
    a line for the statement (`<db>.v`, `normal`, `ok: 0`) before the
    command's; this server writes the command's alone."""

    def op(db: Any) -> None:
        _refused(lambda d: d.v.update_one({}, {"$set": {"a": 1}}))(db)
        _refused(lambda d: d.v.delete_one({}))(db)

    got = _lines(rust_mongo, mongod_rs, op)
    assert got["mongod"] == [
        ("q", "v", "normal", 0),
        ("update", "$cmd", "normal", 1),
        ("q", "v", "normal", 0),
        ("delete", "$cmd", "normal", 1),
    ]
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-488")
def test_a_refused_list_indexes_of_a_view_is_profiled(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 profiles a `listIndexes` and a `collStats` of a view,
    each refused; this server writes no entry for either."""

    def op(db: Any) -> None:
        _refused(lambda d: list(d.v.list_indexes()))(db)
        _refused(lambda d: d.command("collStats", "v"))(db)

    got = _entries(rust_mongo, mongod_rs, op)
    assert got["mongod"] == [("listIndexes", "v", 0.0), ("collStats", "v", 0.0)]
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-489")
def test_a_get_more_of_a_find_of_a_view_names_no_write_concern(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line and profile entry for a `getMore` of a `find` of
    a view carry no `writeConcern`, as one of a `find` of a collection; this
    server's carry the one an `aggregate`'s `getMore` runs under."""
    got = {}
    for server, (lines, entries) in _both(
        rust_mongo, mongod_rs, lambda d: list(d.v.find(batch_size=4))
    ).items():
        got[server] = [
            "writeConcern" in a for a in [a for _, a in lines] + entries if _first(a) == "getMore"
        ]
    assert got["mongod"] == [False] * 4
    assert got["rust"] == got["mongod"]


def _distinct_metric(client: Any) -> dict:
    db = client[fresh("metric")]
    db.c.insert_many([{"x": n} for n in range(3)])
    db.command("create", "v", viewOn="c", pipeline=[])
    before = client.admin.command("serverStatus")["metrics"]["commands"]["distinct"]
    db.v.distinct("x")
    after = client.admin.command("serverStatus")["metrics"]["commands"]["distinct"]
    client.drop_database(db.name)
    return {k: after[k] - before.get(k, 0) for k in after}


@pytest.mark.backlog("RM-490")
def test_a_distinct_of_a_view_is_not_counted_failed(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 counts a `distinct` of a view in
    `metrics.commands.distinct.total` and not in `failed`; this server
    counts it failed, as it reads the reply's int32 `ok` as a double."""
    got = {s: _distinct_metric(c) for s, c in (("rust", rust_mongo), ("mongod", mongod_rs))}
    assert (got["mongod"]["total"], got["mongod"]["failed"]) == (1, 0)
    assert (got["rust"]["total"], got["rust"]["failed"]) == (1, 0)


@pytest.mark.backlog("RM-491")
def test_a_command_metric_counts_rejections(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's `metrics.commands.<name>` counts `failed`, `rejected`
    and `total`; this server's has no `rejected`."""
    got = {s: sorted(_distinct_metric(c)) for s, c in (("rust", rust_mongo), ("mongod", mongod_rs))}
    assert got["mongod"] == ["failed", "rejected", "total"]
    assert got["rust"] == got["mongod"]
