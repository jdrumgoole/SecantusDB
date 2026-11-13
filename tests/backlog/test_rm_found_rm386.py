"""Gap tests for what working RM-386 (a command refused as it parses never
runs) found on 2026-11-13, comparing with mongod 8.2.11 (a one-member
replica set, macOS arm64)."""

from __future__ import annotations

import json
import socket
import struct
import time
from typing import Any

import bson
import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_slow_query_namespace import _both, _command


def _op_msg(sock: socket.socket, command: dict, request_id: int) -> dict:
    body = struct.pack("<I", 0) + b"\x00" + bson.encode(command)
    sock.sendall(struct.pack("<iiii", 16 + len(body), request_id, 0, 2013) + body)
    header = sock.recv(16, socket.MSG_WAITALL)
    length = struct.unpack("<i", header[:4])[0]
    return bson.decode(sock.recv(length - 16, socket.MSG_WAITALL)[5:])


def _ran(client: Any, is_mongod: bool, on: str, command: dict) -> tuple:
    """What one command, the first after a connection's `hello`, left behind:
    its error code, whether it was the connection's first command (6788700
    within 250 ms), its `Slow query` lines and its profile entries."""
    db = client.rm386f
    client.drop_database(db.name)
    db.c.insert_one({"_id": 1})
    db.command("profile", 2, slowms=-1 if is_mongod else 0)
    last = client.admin.command("getLog", "global")["log"][-1]
    host, port = client.address
    with socket.create_connection((host, port)) as sock:
        local_host, local_port = sock.getsockname()[:2]
        local = f"{local_host}:{local_port}"
        hello = {"hello": 1, "client": {"driver": {"name": "rm386f", "version": "1"}}}
        _op_msg(sock, {**hello, "$db": "admin"}, 1)
        reply = _op_msg(sock, {**command, "comment": "probe", "$db": on}, 2)
        time.sleep(0.3)
        _op_msg(sock, {"ping": 1, "$db": "admin"}, 3)
    db.command("profile", 0, slowms=100)
    log = client.admin.command("getLog", "global")["log"]
    lines = [json.loads(line) for line in log[log.index(last) + 1 :]]
    # The connection's thread, from a line that names its address (mongod
    # in the fixture runs `--quiet`, so its metadata line is not there).
    thread = next(
        e["ctx"]
        for e in lines
        if e["id"] in (51800, 51803) and e.get("attr", {}).get("remote") == local
    )
    elapsed = [
        e["attr"]["elapsedMillis"] for e in lines if e["id"] == 6788700 and e["ctx"] == thread
    ]
    slow = [
        e
        for e in lines
        if e.get("msg") == "Slow query"
        and isinstance(e["attr"].get("command"), dict)
        and e["attr"]["command"].get("comment") == "probe"
    ]
    profiled = db.system.profile.count_documents({"command.comment": "probe"})
    code = "ok" if reply.get("ok") == 1 else reply.get("code")
    return code, bool(elapsed) and elapsed[0] < 250, len(slow), profiled


# --------------------------------------- create / dbStats parse in run --


_CREATE_AND_DB_STATS = {
    "create-unknown-field": ("rm386f", {"create": "d", "bogus": 1}),
    "create-storage-engine-int": ("rm386f", {"create": "d", "storageEngine": 1}),
    "create-size-0": ("rm386f", {"create": "d", "capped": True, "size": 0}),
    "create-view-on-int": ("rm386f", {"create": "d", "viewOn": 1}),
    "create-auto-index-id": ("rm386f", {"create": "d", "autoIndexId": True}),
    "db-stats-unknown-field": ("rm386f", {"dbStats": 1, "bogus": 1}),
    "db-stats-scale-string": ("rm386f", {"dbStats": 1, "scale": "x"}),
    "list-databases-not-admin": ("rm386f", {"listDatabases": 1}),
}


@pytest.mark.backlog("RM-602")
def test_create_and_db_stats_refused_as_they_parse_never_run(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 refuses a `create` or a `dbStats` with an unknown or
    mistyped field, or a `size` out of range, as the command parses: it is
    not a connection's first command (6788700), writes no `Slow query` line
    and no profile entry. A `listDatabases` on another database than
    `admin` is refused once it has parsed: logged, not counted. This server
    parses these inside the run, so it counts each, and logs the first
    seven."""
    answers = {
        name: (_ran(rust_mongo, False, on, cmd), _ran(mongod_rs, True, on, cmd))
        for name, (on, cmd) in _CREATE_AND_DB_STATS.items()
    }
    mongod = {name: m for name, (_, m) in answers.items()}
    rust = {name: r for name, (r, _) in answers.items()}
    assert [counted for _, counted, _, _ in mongod.values()] == [False] * 8
    assert rust == mongod


# ------------------------------------------- nested pipelines' parse --


def _nested_pipelines(client: Any) -> dict:
    db = client.rm386n
    client.drop_database(db.name)
    db.c.insert_one({"_id": 1})

    def run(*stages: dict) -> tuple:
        return mongo_answer(lambda: list(db.c.aggregate(list(stages))))

    return {
        "facet-unknown-stage": run({"$facet": {"a": [{"$bogus": 1}]}}),
        "lookup-unknown-stage": run(
            {"$lookup": {"from": "c", "as": "x", "pipeline": [{"$bogus": 1}]}}
        ),
        "union-with-unknown-stage": run({"$unionWith": {"coll": "c", "pipeline": [{"$bogus": 1}]}}),
        "union-with-pipeline-int": run({"$unionWith": {"coll": "c", "pipeline": 1}}),
    }


@pytest.mark.backlog("RM-603")
def test_a_nested_pipeline_is_parsed_with_the_command(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 parses the pipeline of a `$facet`, a `$lookup` and a
    `$unionWith` as it parses the command's own: an unknown stage there is
    40324, and a `$unionWith` `pipeline` that is not an array is 14. This
    server answers the first three with BadValue (2, "not supported by the
    Rust server") and runs the fourth, answering ok."""
    unknown = ("err", 40324, "Unrecognized pipeline stage name: '$bogus'")
    expected = {
        "facet-unknown-stage": unknown,
        "lookup-unknown-stage": unknown,
        "union-with-unknown-stage": unknown,
        "union-with-pipeline-int": ("err", 14, "A pipeline must be an array of objects"),
    }
    assert _nested_pipelines(mongod_rs) == expected
    assert _nested_pipelines(rust_mongo) == expected


# --------------------------------- stage fields of the wrong type (14) --


def _wrong_typed_stage_fields(client: Any) -> dict:
    db = client.rm386w
    client.drop_database(db.name)
    db.c.insert_one({"_id": 1})

    def run(stage: dict) -> tuple:
        return mongo_answer(lambda: list(db.c.aggregate([stage])))

    return {
        "set-window-fields-output": run({"$setWindowFields": {"output": 1}}),
        "densify-field": run({"$densify": {"field": 1}}),
        "fill-output": run({"$fill": {"output": 1}}),
    }


@pytest.mark.backlog("RM-604")
def test_a_stage_field_of_the_wrong_type_is_a_type_mismatch(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 refuses a `$setWindowFields` `output`, a `$densify`
    `field` and a `$fill` `output` of the wrong type with 14, naming the
    field; this server answers BadValue (2), "aggregation pipeline uses a
    stage or operator not supported by the Rust server"."""
    expected = {
        "set-window-fields-output": (
            "err",
            14,
            "BSON field '$setWindowFields.output' is the wrong type 'int', expected type 'object'",
        ),
        "densify-field": (
            "err",
            14,
            "BSON field '$densify.field' is the wrong type 'int', expected type 'string'",
        ),
        "fill-output": (
            "err",
            14,
            "BSON field '$fill.output' is the wrong type 'int', expected type 'object'",
        ),
    }
    assert _wrong_typed_stage_fields(mongod_rs) == expected
    assert _wrong_typed_stage_fields(rust_mongo) == expected


# ------------------------------------------------- the Atlas stages --


def _atlas_stages(client: Any) -> dict:
    db = client.rm386s
    client.drop_database(db.name)
    db.c.insert_one({"_id": 1})
    return {
        name: mongo_answer(lambda name=name: list(db.c.aggregate([{name: {}}])))
        for name in ("$search", "$vectorSearch", "$listSearchIndexes")
    }


@pytest.mark.backlog("RM-605")
def test_an_atlas_stage_is_refused_as_mongod_refuses_it(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 with no `mongot` refuses `$search`, `$vectorSearch` and
    `$listSearchIndexes` as they run with 31082 (SearchNotEnabled), in two
    messages of its own; this server answers 115 (CommandNotSupported) with
    one message, the wording of `$listSearchIndexes`'s from an older
    mongod."""
    search = (
        "Using $search and $vectorSearch aggregation stages requires additional "
        "configuration. Please connect to Atlas or an AtlasCLI local deployment to "
        "enable.For more information on how to connect, see "
        "https://dochub.mongodb.org/core/atlas-cli-deploy-local-reqs."
    )
    indexes = (
        "Using Atlas Search Database Commands and the $listSearchIndexes aggregation "
        "stage requires additional configuration. Please connect to Atlas or an "
        "AtlasCLI local deployment to enable. For more information on how to connect, "
        "see https://dochub.mongodb.org/core/atlas-cli-deploy-local-reqs."
    )
    expected = {
        "$search": ("err", 31082, search),
        "$vectorSearch": ("err", 31082, search),
        "$listSearchIndexes": ("err", 31082, indexes),
    }
    assert _atlas_stages(mongod_rs) == expected
    assert _atlas_stages(rust_mongo) == expected


# ------------------------------------------- getMore's cursor id type --


def _get_more_int_id(client: Any) -> tuple:
    db = client.rm386g
    client.drop_database(db.name)
    db.c.insert_one({"_id": 1})
    return mongo_answer(lambda: db.command({"getMore": 5, "collection": "c"}))


@pytest.mark.backlog("RM-606")
def test_a_get_more_takes_an_int_cursor_id(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 reads a `getMore` cursor id given as an int as the
    cursor id (`{getMore: 1}` with no `collection` is refused for the
    missing field, not the id's type); this server refuses the int with 14,
    "BSON field 'getMore.getMore' is the wrong type 'int', expected type
    'long'"."""
    assert _get_more_int_id(rust_mongo) == _get_more_int_id(mongod_rs)
    assert _get_more_int_id(mongod_rs)[:2] == ("err", 43)


# ------------------------------------- refused commands' profile entries --


_PROFILED = {
    "distinct-unknown-field": ("rm386f", {"distinct": "c", "key": "a", "bogus": 1}),
    "distinct-no-key": ("rm386f", {"distinct": "c"}),
    "find-and-modify-neither": ("rm386f", {"findAndModify": "c"}),
    "find-and-modify-undefined-variable": (
        "rm386f",
        {"findAndModify": "c", "remove": True, "query": {"$expr": {"$eq": ["$a", "$$nope"]}}},
    ),
    "create-indexes-no-key": ("rm386f", {"createIndexes": "c", "indexes": [{"name": "x"}]}),
    "create-capped-no-size": ("rm386f", {"create": "d", "capped": True}),
    "insert-system-namespace": ("rm386f", {"insert": "system.foo", "documents": [{}]}),
    "explain-insert": ("rm386f", {"explain": {"insert": "c", "documents": [{}]}}),
    "db-stats-scale-0": ("rm386f", {"dbStats": 1, "scale": 0}),
    "query-settings-unknown-field": (
        "admin",
        {"aggregate": 1, "pipeline": [{"$querySettings": {"bogus": 1}}], "cursor": {}},
    ),
}


@pytest.mark.backlog("RM-607")
def test_a_command_refused_as_it_runs_is_profiled_as_mongod_profiles_it(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 profiles a `distinct` refused for its fields (one
    entry); it profiles no `findAndModify` with neither `update` nor `remove`
    or with an undefined `$$variable`, no `createIndexes` refused for a
    spec, no `create` refused for its capped options, no write to a
    `system.` namespace it does not take, no `explain` of an `insert`, no
    `dbStats` of `scale: 0`. This server does each the other way. (Each is
    run, and logged, on both.)"""
    answers = {
        name: (_ran(rust_mongo, False, on, cmd)[3], _ran(mongod_rs, True, on, cmd)[3])
        for name, (on, cmd) in _PROFILED.items()
    }
    assert {name: r for name, (r, _) in answers.items()} == {
        name: m for name, (_, m) in answers.items()
    }


# ------------------------------------ a failed update statement's lines --


@pytest.mark.backlog("RM-608")
def test_an_update_with_a_failed_statement_writes_two_lines(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 writes two `Slow query` lines for an `update` whose one
    statement fails as it parses its update (`{$bogus: {a: 1}}`, a write
    error in an `ok: 1` reply): the statement's and the command's. This
    server writes one."""

    def run(client: Any, db: Any) -> None:
        _command(
            client, db, "<db>", {"update": "c", "updates": [{"q": {}, "u": {"$bogus": {"a": 1}}}]}
        )

    answers = _both(run, rust_mongo, mongod_rs)
    assert len(answers["mongod"][0]) == 2
    assert len(answers["rust"][0]) == len(answers["mongod"][0])
