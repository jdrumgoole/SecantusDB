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


@pytest.mark.backlog_fixed("RM-487")
def test_a_write_to_a_view_has_a_statement_line(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's `update` and `delete` of a view, each refused, write
    a line for the statement (`<db>.v`, `normal`, `ok: 0`) before the
    command's; this server wrote the command's alone."""

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


@pytest.mark.backlog_fixed("RM-487")
def test_an_unordered_write_to_a_view_has_a_line_per_statement(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 tries each statement of an unordered `update` or
    `delete` of a view, and writes a line for each it refuses before the
    command's, with the statement as its `command` (`multi` and `upsert`
    filled in) and the attributes of a write statement's line, in its
    order. This server wrote the command's line alone."""

    def op(db: Any) -> None:
        updates = [{"q": {}, "u": {"$set": {"a": 1}}}, {"q": {"b": 1}, "u": {"$set": {"a": 2}}}]
        deletes = [{"q": {}, "limit": 0}, {"q": {"b": 1}, "limit": 1}]
        _refused(lambda d: d.command("update", "v", updates=updates, ordered=False))(db)
        _refused(lambda d: d.command("delete", "v", deletes=deletes, ordered=False))(db)

    def statement(a: dict) -> tuple:
        if a["type"] == "command":
            return (_first(a), _ns(a))
        return (
            a["type"],
            _ns(a),
            a["command"],
            a["errCode"],
            a["locks"],
            [k for k in a if k != "cpuNanos"],
        )

    got = {
        server: [statement(a) for _, a in lines]
        for server, (lines, _) in _both(rust_mongo, mongod_rs, op).items()
    }
    assert [g[:2] for g in got["mongod"]] == [
        ("update", "v"),
        ("update", "v"),
        ("update", "$cmd"),
        ("remove", "v"),
        ("remove", "v"),
        ("delete", "$cmd"),
    ]
    assert got["mongod"][1][2] == {
        "q": {"b": 1},
        "u": {"$set": {"a": 2}},
        "multi": False,
        "upsert": False,
    }
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog_fixed("RM-488")
def test_a_refused_list_indexes_of_a_view_is_profiled(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 profiles a `listIndexes` and a `collStats` of a view,
    each refused; this server wrote no entry for either."""

    def op(db: Any) -> None:
        _refused(lambda d: list(d.v.list_indexes()))(db)
        _refused(lambda d: d.command("collStats", "v"))(db)

    got = _entries(rust_mongo, mongod_rs, op)
    assert got["mongod"] == [("listIndexes", "v", 0.0), ("collStats", "v", 0.0)]
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog_fixed("RM-488")
def test_a_refused_list_indexes_of_a_view_counts_its_lock(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 takes a `listIndexes`'s or a `collStats`'s read lock
    before it finds a view, so the line and the entry of one it refuses
    have the fields, the `locks` and the `storage` section of a read: a
    global IS on the line, two in the entry. This server refused before it
    took the lock, and its line had `locks: {}` and no `storage`."""

    def op(db: Any) -> None:
        _refused(lambda d: list(d.v.list_indexes()))(db)
        _refused(lambda d: d.command("collStats", "v"))(db)

    got = {}
    for server, (lines, entries) in _both(rust_mongo, mongod_rs, op).items():
        got[server] = [
            (_first(a), a.get("locks"), "storage" in a, a.get("errName")) for _, a in lines
        ] + [
            (_first(e), e.get("locks"), sorted(set(e) - {"ts"}), e.get("errName")) for e in entries
        ]
    line_locks = {"Global": {"acquireCount": {"r": 1}}}
    entry_locks = {"Global": {"acquireCount": {"r": 2}}}
    assert [g[:2] for g in got["mongod"]] == [
        ("listIndexes", line_locks),
        ("collStats", line_locks),
        ("listIndexes", entry_locks),
        ("collStats", entry_locks),
    ]
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog_fixed("RM-489")
def test_a_get_more_of_a_find_of_a_view_names_no_write_concern(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's line and profile entry for a `getMore` of a `find` of
    a view carry no `writeConcern`, as one of a `find` of a collection; this
    server's carried the one an `aggregate`'s `getMore` runs under, since it
    runs the `find` as an `aggregate`."""
    got = {}
    for server, (lines, entries) in _both(
        rust_mongo, mongod_rs, lambda d: list(d.v.find(batch_size=4))
    ).items():
        got[server] = [
            "writeConcern" in a for a in [a for _, a in lines] + entries if _first(a) == "getMore"
        ]
    assert got["mongod"] == [False] * 4
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog_fixed("RM-489")
def test_a_get_more_names_the_write_concern_its_cursor_was_opened_under(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's `getMore` runs under the write concern of the command
    that opened its cursor: an `aggregate`'s (of a collection or a view, a
    deferred first batch included), and none for a `find`'s, a time-series
    collection's too, which runs as an `aggregate` over the buckets."""

    def op(db: Any) -> None:
        list(db.ts.find(batch_size=4))
        list(db.c.find(batch_size=2))
        list(db.c.aggregate([], batchSize=2))
        list(db.v.aggregate([], batchSize=2))
        list(db.c.aggregate([], batchSize=0))

    got = {}
    for server, (lines, entries) in _both(rust_mongo, mongod_rs, op).items():
        got[server] = [
            [(_ns(a), "writeConcern" in a) for _, a in lines if _first(a) == "getMore"],
            [(_ns(e), "writeConcern" in e) for e in entries if _first(e) == "getMore"],
        ]
    assert ("ts", False) in got["mongod"][0] and ("c", True) in got["mongod"][0]
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


@pytest.mark.backlog_fixed("RM-490")
def test_a_distinct_of_a_view_is_not_counted_failed(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 counts a `distinct` of a view in
    `metrics.commands.distinct.total` and not in `failed`; this server
    counted it failed, as it read the reply's int32 `ok` as a double."""
    got = {s: _distinct_metric(c) for s, c in (("rust", rust_mongo), ("mongod", mongod_rs))}
    assert (got["mongod"]["total"], got["mongod"]["failed"]) == (1, 0)
    assert (got["rust"]["total"], got["rust"]["failed"]) == (1, 0)


@pytest.mark.backlog_fixed("RM-491")
def test_a_command_metric_counts_rejections(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's `metrics.commands.<name>` counts `failed`, `rejected`
    and `total`; this server's had no `rejected`."""
    got = {s: sorted(_distinct_metric(c)) for s, c in (("rust", rust_mongo), ("mongod", mongod_rs))}
    assert got["mongod"] == ["failed", "rejected", "total"]
    assert got["rust"] == got["mongod"]


def _rejected_metrics(client: Any) -> dict:
    db = client[fresh("rejected")]
    db.c.insert_many([{"x": n} for n in range(3)])
    shapes = [
        {"find": "c", "filter": {"x": 1}, "$db": db.name},
        {"distinct": "c", "key": "x", "query": {}, "$db": db.name},
        {"aggregate": "c", "pipeline": [{"$match": {"x": 1}}], "$db": db.name},
    ]
    for shape in shapes:
        client.admin.command({"setQuerySettings": shape, "settings": {"reject": True}})
    names = ("find", "distinct", "aggregate", "explain")

    def metrics() -> dict:
        return {n: client.admin.command("serverStatus")["metrics"]["commands"][n] for n in names}

    before = metrics()
    codes = []
    for op in (
        lambda: list(db.c.find({"x": 2})),
        lambda: db.c.distinct("x"),
        lambda: list(db.c.aggregate([{"$match": {"x": 2}}])),
        lambda: db.c.find({"x": 2}).explain(),
        lambda: db.command("find", "c", filter={"$bad": 1}),
    ):
        try:
            op()
            codes.append(None)
        except PyMongoError as e:
            codes.append(getattr(e, "code", None))
    after = metrics()
    for shape in shapes:
        client.admin.command({"removeQuerySettings": shape})
    client.drop_database(db.name)
    deltas = {n: {k: after[n][k] - before[n].get(k, 0) for k in after[n]} for n in names}
    return {"codes": codes, "metrics": deltas}


@pytest.mark.backlog_fixed("RM-491")
def test_a_query_settings_refusal_counts_rejected_not_failed(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 counts a read a query setting refuses (411
    `QueryRejectedBySettings`) in its command's `rejected` and `total`,
    never in `failed`; its `explain` is not refused and counts nothing
    rejected, and any other error still counts `failed`."""
    got = {s: _rejected_metrics(c) for s, c in (("rust", rust_mongo), ("mongod", mongod_rs))}
    assert got["mongod"]["codes"] == [411, 411, 411, None, 2]
    assert got["mongod"]["metrics"]["find"] == {"failed": 1, "rejected": 1, "total": 2}
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-589")
def test_a_refused_write_of_a_view_takes_mongods_locks(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 refuses a write of a view once it holds the namespace
    for the write: a retryable `update` after checking its session out (one
    more transition lock), an `insert` after its write rounds, and a
    statement of a transaction under the transaction's locks. This server
    refuses each before those, and its lines count fewer locks: the
    retryable statement and command one transition lock short, the insert
    no write round, the transaction's statement none at all."""

    def op(db: Any) -> None:
        _refused(lambda d: d.v.update_one({}, {"$set": {"a": 1}}))(db)
        _refused(lambda d: d.v.insert_one({"a": 1}))(db)

        def txn(d: Any) -> None:
            with d.client.start_session() as s, s.start_transaction():
                d.v.update_one({}, {"$set": {"a": 1}}, session=s)

        _refused(txn)(db)

    got = {
        server: [(_first(a), _ns(a), a.get("locks"), a.get("flowControl")) for _, a in lines]
        for server, (lines, _) in _both(rust_mongo, mongod_rs, op).items()
    }
    statement = got["mongod"][0]
    assert statement[:2] == ("q", "v")
    assert statement[2]["ReplicationStateTransition"] == {"acquireCount": {"w": 2}}
    assert got["rust"] == got["mongod"]


def _coll_stats_of_view(client: Any) -> list:
    from tests.backlog.conftest import mongo_answer

    db = client[fresh("collstats")]
    _setup(db)
    got = []
    for spec in ({}, {"latencyStats": {}}, {"count": {}}, {"storageStats": {}}):
        answer = mongo_answer(
            lambda s=spec: [sorted(d) for d in db.v.aggregate([{"$collStats": s}])]
        )
        got.append(answer[:2])
    client.drop_database(db.name)
    return got


@pytest.mark.backlog("RM-590")
def test_a_coll_stats_stage_of_a_view_answers(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 answers a `$collStats: {}` or `{latencyStats: {}}` of a
    view, and refuses `count` and `storageStats` (166); this server refuses
    every `$collStats` of a view."""
    got = {s: _coll_stats_of_view(c) for s, c in (("rust", rust_mongo), ("mongod", mongod_rs))}
    assert got["mongod"] == [
        ("ok", [["host", "localTime", "ns"]]),
        ("ok", [["host", "latencyStats", "localTime", "ns"]]),
        ("err", 166),
        ("err", 166),
    ]
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-591")
def test_an_out_into_a_view_names_the_view(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11's line and profile entry for an `$out` into a view, which
    it refuses, name the view; this server's line names the collection the
    pipeline reads, and it writes no entry."""
    got = {}
    for server, (lines, entries) in _both(
        rust_mongo, mongod_rs, _refused(lambda d: list(d.c.aggregate([{"$out": "v"}])))
    ).items():
        got[server] = [(_first(a), _ns(a), a.get("ok", 1)) for _, a in lines] + [
            (_first(e), _ns(e), e.get("ok", 1)) for e in entries
        ]
    assert got["mongod"] == [("aggregate", "v", 0), ("aggregate", "v", 0.0)]
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-592")
def test_a_merge_into_a_view_is_profiled(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 profiles a `$merge` into a view, which it refuses, under
    the collection the pipeline reads; this server writes no entry."""
    got = _entries(
        rust_mongo,
        mongod_rs,
        _refused(lambda d: list(d.c.aggregate([{"$merge": {"into": "v"}}]))),
    )
    assert got["mongod"] == [("aggregate", "c", 0.0)]
    assert got["rust"] == got["mongod"]


@pytest.mark.backlog("RM-593")
def test_a_change_stream_of_a_view_writes_no_line(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 refuses a `$changeStream` of a view (166) and writes no
    `Slow query` line for it; this server writes one."""
    got = _lines(
        rust_mongo,
        mongod_rs,
        _refused(
            lambda d: d.command("aggregate", "v", pipeline=[{"$changeStream": {}}], cursor={})
        ),
    )
    assert got["mongod"] == []
    assert got["rust"] == got["mongod"]
