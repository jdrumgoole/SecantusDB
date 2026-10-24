"""RM-185: the locks each command takes, as `serverStatus.locks` counts
them (`crates/secantus-commands/src/lock_manager.rs`, `statement_rounds`),
against mongod 8.2.11."""

from __future__ import annotations

import contextlib
import itertools
import json
import uuid
from collections.abc import Callable, Iterator
from typing import Any

import pymongo
import pytest


@pytest.fixture(scope="module")
def mongod() -> Iterator[Any]:
    """The reference, with time-based plan yielding off.

    mongod's executor yields -- releases and re-takes its locks -- once an
    operation has run `internalQueryExecYieldPeriodMS` (10 ms), and each
    re-take counts. Its builds differ here: on Linux x86-64 (8.2.12, CI, the
    exemplar build: Joe, 2026-10-21) `$out` reads 7 global IS, on macOS
    arm64 8 idle and 9 on a loaded machine; with the period off, macOS reads
    7 as Linux does. So the period is off, and the counts are the Linux
    build's on any machine. Load moved them another way as well, through
    slow-operation logging, which `_no_slow_operations` turns off."""
    from tests.mongod_reference import MONGOD, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod(
        extra_args=("--setParameter", "internalQueryExecYieldPeriodMS=86400000")
    )
    client = pymongo.MongoClient(uri, serverSelectionTimeoutMS=10_000)
    try:
        yield client
    finally:
        client.close()
        _stop_mongod(proc, dbpath)


#: A `slowms` no operation reaches.
_NEVER_SLOW = 86_400_000


@pytest.fixture(scope="module", autouse=True)
def _no_slow_operations(rust_mongo: Any, mongod: Any) -> Iterator[None]:
    """Slow-operation logging off, on both servers.

    mongod takes the global lock in IS for each operation it logs as slow
    (to read the storage statistics for the `Slow query` line), so an
    operation that crosses `slowms` (100 ms) counts one more global IS for
    itself and one for each write inside it. On a loaded machine creating a
    collection crosses it: `$out` replacing its target read 8 global IS
    instead of 7, and `$merge` creating its target 4 instead of 2, in all
    five reference runs (RM-274; reproduced 2026-10-24 by twelve reference
    mongods at once, and by one beside fsync-heavy disk load, but not by CPU
    load; with `slowms` out of reach, 120 of 120 runs read 7 and 2). The
    counts would then measure the machine. Both servers take that lock
    (RM-299, `test_a_slow_operation_takes_the_global_lock_to_log`), so the
    same `slowms` keeps both counts the machine's own.
    The same `profile` command goes to both servers, so neither logs."""
    saved = []
    for client in (rust_mongo, mongod):
        saved.append((client, client.admin.command("profile", -1)["slowms"]))
        client.admin.command("profile", 0, slowms=_NEVER_SLOW)
    yield
    for client, slowms in saved:
        client.admin.command("profile", 0, slowms=slowms)


# Internal resources mongod's background threads lock; this server has none.
_INTERNAL = {"Metadata", "oplog", "MultiDocumentTransactionsBarrier"}


def _flat(client: Any) -> dict:
    locks = client.admin.command("serverStatus").get("locks", {})
    return {
        (res, mode): n
        for res, v in locks.items()
        if res not in _INTERNAL
        for mode, n in v.get("acquireCount", {}).items()
    }


def _quiet(fn: Callable[[], Any]) -> None:
    # An error answer is part of what is measured.
    with contextlib.suppress(Exception):
        fn()


def _deltas(client: Any, cases: list) -> list:
    out = []
    for label, fn in cases:
        before = _flat(client)
        _quiet(fn)
        after = _flat(client)
        keys = set(before) | set(after)
        out.append(
            (
                label,
                sorted(
                    (k, after.get(k, 0) - before.get(k, 0))
                    for k in keys
                    if after.get(k, 0) != before.get(k, 0)
                ),
            )
        )
    return out


def _fresh(prefix: str) -> str:
    """A database name never used before: mongod takes an extra global IS
    building an index on a namespace a drop has just freed, so a reused
    name measures that, not the build."""
    return f"{prefix}_{uuid.uuid4().hex[:8]}"


# mongod's index build runs on a thread of its own, and the command takes
# the global IS each time it looks at the build: more the longer the build
# takes (measured 1 to 4 on mongod 8.2.11 for one shape). Every other count
# of a build is fixed, so the global IS is compared as a floor: mongod takes
# at least as many as this server.
_BUILD_POLLS = ("Global", "r")


def _same_counts(got: list, want: list) -> None:
    assert [label for label, _ in got] == [label for label, _ in want]
    for (label, ours), (_, theirs) in zip(got, want, strict=True):
        ours, theirs = dict(ours), dict(theirs)
        if label.startswith("createIndex"):
            assert ours.get(_BUILD_POLLS, 0) >= 1, label
            assert theirs.get(_BUILD_POLLS, 0) >= ours.get(_BUILD_POLLS, 0), label
            ours.pop(_BUILD_POLLS, None)
            theirs.pop(_BUILD_POLLS, None)
        assert (label, ours) == (label, theirs), (
            f"mongod's five runs of {label!r}: {_RUNS.get(label)}"
        )


#: Each case's five reference runs, by label, from the last `_reference`.
_RUNS: dict[str, list] = {}


def _reference(measure: Callable[[Any], list], client: Any) -> list:
    """mongod's counts for each case: the least of five runs. Its
    background threads (the TTL monitor, the session refresh) take the
    global lock too, and one that fires inside a case's window can only
    ADD to that case's count. Three runs were not enough under a loaded
    machine: on 2026-10-20, with four test workers, a background lock landed
    in all three windows of one case."""
    runs = [measure(client) for _ in range(5)]
    out = []
    for cases in zip(*runs, strict=True):
        label = cases[0][0]
        counts = [dict(c[1]) for c in cases]
        # Kept for the failure message: an extra lock in all five runs is
        # not background noise, and the runs say where it sits.
        _RUNS[label] = counts
        keys = set().union(*counts)
        least = {k: min(c.get(k, 0) for c in counts) for k in keys}
        out.append((label, sorted((k, n) for k, n in least.items() if n)))
    return out


def _crud(client: Any) -> list:
    db = client[_fresh("rm185")]
    db.c.insert_many([{"_id": i, "x": i} for i in range(10)])
    n = itertools.count()
    return _deltas(
        client,
        [
            ("insert_one", lambda: db.c.insert_one({})),
            ("insert_many", lambda: db.c.insert_many([{}, {}])),
            ("insert creating a collection", lambda: db[f"n{next(n)}"].insert_one({})),
            ("find", lambda: db.c.find_one({"_id": 1})),
            ("find and getMore", lambda: list(db.c.find().batch_size(3))),
            ("update", lambda: db.c.update_one({"_id": 1}, {"$set": {"a": 1}})),
            (
                "update of two statements",
                lambda: db.command(
                    "update",
                    "c",
                    updates=[
                        {"q": {"_id": 1}, "u": {"$set": {"a": 2}}},
                        {"q": {"_id": 2}, "u": {"$set": {"a": 2}}},
                    ],
                ),
            ),
            (
                "delete of two statements",
                lambda: db.command(
                    "delete",
                    "c",
                    deletes=[{"q": {"_id": 3}, "limit": 1}, {"q": {"_id": 4}, "limit": 1}],
                ),
            ),
            ("findAndModify", lambda: db.c.find_one_and_update({"_id": 5}, {"$set": {"y": 1}})),
            ("aggregate", lambda: list(db.c.aggregate([{"$match": {}}]))),
            ("count", lambda: db.command("count", "c")),
            ("distinct", lambda: db.c.distinct("x")),
            ("listCollections", lambda: db.list_collection_names()),
            ("listIndexes", lambda: list(db.c.list_indexes())),
            ("create", lambda: db.command("create", "k1")),
            ("create of one that exists", lambda: db.command("create", "k1")),
            ("drop", lambda: db.command("drop", "k1")),
            ("drop of one that is not there", lambda: db.command("drop", "k9")),
            ("ping", lambda: db.command("ping")),
            ("killCursors", lambda: db.command("killCursors", "c", cursors=[])),
            ("dropDatabase", lambda: client.drop_database(_fresh("rm185b"))),
        ],
    )


@pytest.mark.backlog_fixed("RM-185")
def test_lock_counts_per_command(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 counts one IS on the global lock for a read, a round of
    IX on the state transition, global, database and collection locks per
    write statement (three for an insert that creates its collection), and
    the collection in X for a drop."""
    assert _crud(rust_mongo) == _reference(_crud, mongod)


def _builds(client: Any) -> list:
    db = client[_fresh("rm185c")]
    db.c.insert_many([{"_id": i, "x": i} for i in range(10)])
    return _deltas(
        client,
        [
            ("createIndexes", lambda: db.c.create_index("x")),
            ("dropIndexes", lambda: db.c.drop_index("x_1")),
            ("$out", lambda: list(db.c.aggregate([{"$match": {}}, {"$out": "o"}]))),
            ("dbStats", lambda: db.command("dbStats")),
        ],
    )


@pytest.mark.backlog_fixed("RM-185")
def test_lock_counts_of_index_builds_out_and_db_stats(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's counts for an index build (its phases take the
    collection in X and IX several times), `dropIndexes`, an `$out` and
    `dbStats` (IS on each collection)."""
    _same_counts(_builds(rust_mongo), _reference(_builds, mongod))


def _variations(client: Any) -> list:
    db = client[_fresh("rm185d")]
    db.c.insert_many([{"x": i, "y": i} for i in range(5)])
    db.e.insert_one({})
    db.e.delete_many({})
    cases = [
        ("createIndex", lambda: db.c.create_index("x")),
        ("createIndex of one that exists", lambda: db.c.create_index("x")),
        ("createIndex on an empty collection", lambda: db.e.create_index("x")),
        (
            "createIndexes of two",
            lambda: db.c.create_indexes(
                [pymongo.IndexModel("y"), pymongo.IndexModel([("x", 1), ("y", 1)])]
            ),
        ),
        ("createIndex creating the collection", lambda: db.newc.create_index("x")),
        ("dropIndexes of all", lambda: db.c.drop_indexes()),
        ("dropIndex of one that is not there", lambda: db.c.drop_index("nope_1")),
        ("$out replacing its target", lambda: list(db.c.aggregate([{"$out": "o1"}]))),
        ("$out again", lambda: list(db.c.aggregate([{"$out": "o1"}]))),
    ]
    for n in (1, 3):
        name = _fresh(f"rm185s{n}")
        for i in range(n):
            client[name][f"c{i}"].insert_one({})
        cases.append((f"dbStats over {n}", lambda name=name: client[name].command("dbStats")))
    cases.append(("dbStats of no database", lambda: client[_fresh("rm185none")].command("dbStats")))
    return _deltas(client, cases)


@pytest.mark.backlog_fixed("RM-185")
def test_lock_counts_of_index_build_and_out_shapes(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11: an index build takes the collection in X three times
    and IX five (once each on an empty collection, a write round when every
    index is there already, two when it creates the collection); `$out`
    replaces an existing target with one more X round; `dbStats` takes IS
    on each collection."""
    _same_counts(_variations(rust_mongo), _reference(_variations, mongod))


def _merge(client: Any) -> list:
    db = client[_fresh("rm185m")]
    db.c.insert_many([{"_id": i} for i in range(5)])
    pipeline = [{"$merge": {"into": "m"}}]
    return _deltas(
        client,
        [
            ("$merge creating its target", lambda: list(db.c.aggregate(pipeline))),
            ("$merge into it again", lambda: list(db.c.aggregate(pipeline))),
        ],
    )


@pytest.mark.backlog_fixed("RM-185")
def test_lock_counts_of_merge(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$merge` takes the global IS twice and a write round
    for each document it writes (two more when it creates the target)."""
    assert _merge(rust_mongo) == _reference(_merge, mongod)


def _merge_modes(client: Any) -> list:
    cases = []
    for n in (0, 1, 3):
        db = client[_fresh("rm185n")]
        if n:
            db.c.insert_many([{"_id": i} for i in range(n)])
        else:
            db.create_collection("c")
        db.t.insert_one({"_id": 0})
        cases.append(
            (f"$merge of {n}", lambda db=db: list(db.c.aggregate([{"$merge": {"into": "t"}}])))
        )
    for matched, not_matched in [
        ("replace", "insert"),
        ("keepExisting", "insert"),
        ("merge", "discard"),
        ([{"$set": {"z": 1}}], "insert"),
    ]:
        db = client[_fresh("rm185w")]
        db.c.insert_many([{"_id": i} for i in range(4)])
        db.t.insert_one({"_id": 0})
        spec = {"into": "t", "whenMatched": matched, "whenNotMatched": not_matched}
        cases.append(
            (
                f"$merge {matched} / {not_matched}",
                lambda db=db, spec=spec: list(db.c.aggregate([{"$merge": spec}])),
            )
        )
    db = client[_fresh("rm185f")]
    db.c.insert_many([{"_id": i} for i in range(4)])
    db.t.insert_one({"_id": 99})
    cases.append(
        (
            "$merge fail with no duplicate",
            lambda: list(db.c.aggregate([{"$merge": {"into": "t", "whenMatched": "fail"}}])),
        )
    )
    return _deltas(client, cases)


@pytest.mark.backlog_fixed("RM-185")
def test_lock_counts_of_merge_modes(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `$merge`: the global IS twice and a write round per
    document, whatever `whenMatched` / `whenNotMatched` say; `whenMatched:
    "fail"` one round for a batch that inserts cleanly."""
    assert _merge_modes(rust_mongo) == _reference(_merge_modes, mongod)


def _merge_fail_duplicates(client: Any) -> list:
    db = client[_fresh("rm185d2")]
    db.c.insert_many([{"_id": i} for i in range(4)])
    db.t.insert_many([{"_id": 0}, {"_id": 2}])
    spec = {"into": "t", "whenMatched": "fail"}
    return _deltas(
        client,
        [("$merge fail with two duplicates", lambda: list(db.c.aggregate([{"$merge": spec}])))],
    )


def _merge_fail_shapes(client: Any) -> list:
    cases = []
    for n, dups in [
        (1, [0]),
        (2, [0]),
        (2, [1]),
        (4, [1]),
        (4, [3]),
        (4, [0, 1, 2, 3]),
        (8, [3, 4, 5]),
    ]:
        db = client[_fresh("rm185df")]
        db.c.insert_many([{"_id": i} for i in range(n)])
        db.t.insert_many([{"_id": d} for d in dups])
        spec = {"into": "t", "whenMatched": "fail"}
        cases.append(
            (
                f"$merge fail, {n} documents, duplicates at {dups}",
                lambda db=db, spec=spec: list(db.c.aggregate([{"$merge": spec}])),
            )
        )
    return _deltas(client, cases)


@pytest.mark.backlog_fixed("RM-185")
def test_lock_counts_of_merge_fail_with_duplicates(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `whenMatched: "fail"` meets a duplicate and retries:
    the whole batch at once (rolled back; a round, not taken for one
    document), then a try from the start of what is left that commits up to
    the next duplicate, a round each, and one for the run after the last
    duplicate (measured 2026-10-24). This server took the batch's one."""
    assert _merge_fail_duplicates(rust_mongo) == _reference(_merge_fail_duplicates, mongod)
    assert _merge_fail_shapes(rust_mongo) == _reference(_merge_fail_shapes, mongod)


def _slow_cases(client: Any) -> list:
    db = client[_fresh("rm185slow")]
    db.c.insert_many([{"_id": i, "x": i} for i in range(5)])
    db.t.insert_one({"_id": 0})
    return [
        ("ping", lambda: db.command("ping")),
        ("getLog", lambda: client.admin.command("getLog", "global")),
        ("find", lambda: db.c.find_one({"_id": 1})),
        ("find and getMore", lambda: list(db.c.find().batch_size(2))),
        ("insert of two", lambda: db.c.insert_many([{}, {}])),
        (
            "update of two statements",
            lambda: db.command(
                "update",
                "c",
                updates=[
                    {"q": {"_id": 1}, "u": {"$set": {"a": 1}}},
                    {"q": {"_id": 2}, "u": {"$set": {"a": 1}}},
                ],
            ),
        ),
        ("delete", lambda: db.c.delete_one({"_id": 3})),
        ("findAndModify", lambda: db.c.find_one_and_update({"_id": 4}, {"$set": {"y": 1}})),
        ("$merge into a target", lambda: list(db.c.aggregate([{"$merge": {"into": "t"}}]))),
        (
            "$merge creating its target",
            lambda: list(db.c.aggregate([{"$merge": {"into": "m"}}])),
        ),
        ("$out", lambda: list(db.c.aggregate([{"$out": "o"}]))),
        ("createIndex", lambda: db.c.create_index("x")),
        ("createIndex of one that exists", lambda: db.c.create_index("x")),
        ("drop", lambda: db.command("drop", "o")),
    ]


def _slow(client: Any) -> list:
    """`_slow_cases` with every operation logged as slow."""
    client.admin.command("profile", 0, slowms=0)
    try:
        return _deltas(client, _slow_cases(client))
    finally:
        client.admin.command("profile", 0, slowms=_NEVER_SLOW)


@pytest.mark.backlog_fixed("RM-299")
def test_a_slow_operation_takes_the_global_lock_to_log(rust_mongo: Any, mongod: Any) -> None:
    """mongod logs an operation slower than `slowms` and, when the operation
    took the global lock at all, takes it again in IS to read its storage
    statistics for the line: one more global IS for each operation logged
    -- each `getMore`, each update and delete statement, each write a
    `$merge` makes, an index build's own thread -- and none for `ping`
    (measured 8.2.11, 2026-10-24: with `slowms` 0, `$merge` creating its
    target reads 8 global IS where it reads 2 with nothing logged). This
    server wrote the command's line and took no lock for it."""
    assert _slow(rust_mongo) == _reference(_slow, mongod)


def _log_line(line: str) -> dict:
    """A `getLog` line: mongod's structured log, one JSON document each
    (this server's too since RM-302)."""
    return json.loads(line)


def _lines_after(client: Any, last: str | None) -> list:
    """The `getLog` lines after `last`. mongod returns only the latest 1024,
    so once its log is full the count stops growing: the window is found by
    the line it starts after, not by a position."""
    log = client.admin.command("getLog", "global")["log"]
    if last is None:
        return log
    for i in range(len(log) - 1, -1, -1):
        if log[i] == last:
            return log[i + 1 :]
    return log


def _slow_lines(client: Any) -> list:
    """Each case's `Slow query` lines, as (component, type, the command's
    first field), with every operation logged as slow."""
    cases = _slow_cases(client)
    client.admin.command("profile", 0, slowms=0)
    out = []
    try:
        for label, fn in cases:
            log = client.admin.command("getLog", "global")["log"]
            last = log[-1] if log else None
            _quiet(fn)
            lines = []
            for entry in map(_log_line, _lines_after(client, last)):
                if entry.get("msg") != "Slow query":
                    continue
                command = entry["attr"].get("command", {})
                first = next(iter(command), "")
                # The `getLog` that reads the log is logged in its window,
                # and the driver's monitor sends `hello` whenever it likes.
                if first in ("hello", "isMaster", "ismaster"):
                    continue
                if first != "getLog" or label == "getLog":
                    lines.append((entry["c"], entry["attr"].get("type"), first))
            out.append((label, lines))
    finally:
        client.admin.command("profile", 0, slowms=_NEVER_SLOW)
    return out


@pytest.mark.backlog_fixed("RM-299")
def test_a_write_statement_logs_a_line_of_its_own(rust_mongo: Any, mongod: Any) -> None:
    """mongod writes a `Slow query` line, component `WRITE`, for each update
    and delete statement before the command's own -- a `$merge`'s writes are
    update statements -- and an index build over documents logs one on its
    thread (component `STORAGE`, type `none`) (measured 8.2.11, 2026-10-24).
    This server logged the command alone."""
    assert _slow_lines(rust_mongo) == _slow_lines(mongod)


def _admin_commands(client: Any) -> list:
    db = client[_fresh("rm185a")]
    db.x.insert_many([{"a": i} for i in range(3)])
    db.y.insert_one({})
    admin = client.admin
    find = {"find": "x", "filter": {"a": 1}}
    update = {"update": "x", "updates": [{"q": {"a": 1}, "u": {"$set": {"b": 1}}}]}
    return _deltas(
        client,
        [
            *[
                (name, lambda name=name, arg=arg: admin.command(name, arg))
                for name, arg in [
                    ("buildInfo", 1),
                    ("connPoolStats", 1),
                    ("connectionStatus", 1),
                    ("features", 1),
                    ("getClusterParameter", "*"),
                    ("getCmdLineOpts", 1),
                    ("getLog", "global"),
                    ("getParameter", "*"),
                    ("hostInfo", 1),
                    ("listCommands", 1),
                    ("top", 1),
                    ("whatsmyuri", 1),
                ]
            ],
            ("dropConnections", lambda: admin.command("dropConnections", hostAndPort=[])),
            ("rolesInfo", lambda: db.command("rolesInfo", 1)),
            ("usersInfo", lambda: db.command("usersInfo", 1)),
            ("dataSize", lambda: db.command("dataSize", f"{db.name}.x")),
            ("planCacheClear", lambda: db.command("planCacheClear", "x")),
            ("planCacheListFilters", lambda: db.command("planCacheListFilters", "x")),
            ("planCacheClearFilters", lambda: db.command("planCacheClearFilters", "x")),
            (
                "planCacheSetFilter",
                lambda: db.command("planCacheSetFilter", "x", query={"a": 1}, indexes=[{"_id": 1}]),
            ),
            ("explain find", lambda: db.command("explain", find)),
            (
                "explain aggregate",
                lambda: db.command("explain", {"aggregate": "x", "pipeline": [], "cursor": {}}),
            ),
            ("explain update", lambda: db.command("explain", update)),
            (
                "explain delete",
                lambda: db.command("explain", {"delete": "x", "deletes": [{"q": {}, "limit": 1}]}),
            ),
            ("dbHash", lambda: db.command("dbHash")),
            ("filemd5 of no chunks", lambda: db.command("filemd5", 1, root="fs")),
            (
                "listDatabases by name",
                lambda: admin.command("listDatabases", filter={"name": db.name}),
            ),
            ("listDatabases of names", lambda: admin.command("listDatabases", nameOnly=True)),
        ],
    )


@pytest.mark.backlog_fixed("RM-185")
def test_lock_counts_of_admin_commands(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's locks for the commands that read no data (none at
    all), `rolesInfo` / `usersInfo` (a global IS), the older read path of
    `dataSize`, `filemd5` and the plan-cache commands (the replication state
    transition lock first), `explain` (the explained command's read, or one
    write round), `dbHash` (the database in S) and `listDatabases` (a global
    IS more for each database whose size it reads) -- measured 2026-10-24.
    This server took a write chain for most of them."""
    assert _admin_commands(rust_mongo) == _reference(_admin_commands, mongod)


def _before_parsing(client: Any) -> list:
    db = client[_fresh("rm185p")]
    db.x.insert_one({})
    return _deltas(
        client,
        [
            ("explain of nothing", lambda: db.command("explain", 1)),
            ("dataSize of nothing", lambda: db.command("dataSize", 1)),
            ("planCacheClear of nothing", lambda: db.command("planCacheClear", 1)),
            (
                "validateDBMetadata",
                lambda: db.command(
                    "validateDBMetadata", apiParameters={"version": "1", "strict": True}
                ),
            ),
        ],
    )


@pytest.mark.backlog("RM-185")
def test_lock_counts_before_a_command_parses(rust_mongo: Any, mongod: Any) -> None:
    """mongod takes a command's locks inside the command, after it parses, so
    one refused as malformed takes none; and `validateDBMetadata` reads each
    collection (8 global IS for one database of two collections; measured
    8.2.11, 2026-10-24). This server takes a command's chain before running
    it, and gives `validateDBMetadata` the generic one."""
    assert _before_parsing(rust_mongo) == _reference(_before_parsing, mongod)
