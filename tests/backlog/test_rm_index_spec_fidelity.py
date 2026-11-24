"""How an index's spec is kept in the catalog mongod reads (RM-734's
follow-ups), against mongod 8.2.11.

mongod keeps the options a `createIndexes` spec gave, `sparse: false` and
`background` among them, and drops only the ones it parses away (`unique`,
`hidden` and `prepareUnique` when false, `dropDups`, `ns`). A rebuild of an
index (`reIndex`, or a `--repair` that salvages) writes its spec again in
creation order, a `collMod`'s fields moved. And its planner breaks a tie between plans by
the in-memory order of the collection's indexes, where an index a `collMod`
changed has moved to the end.
"""

from __future__ import annotations

import contextlib
import os
import socket
import subprocess
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pymongo
import pytest

from tests.backlog.conftest import RS_BINARY, mongo_answer, rust_mongo_server
from tests.mongod_reference import MONGOD


def _specs(client: Any, db: str, coll: str) -> dict[str, list[tuple[str, Any]]]:
    """Each index's spec as the catalog (`$listCatalog`) and as `listIndexes`
    show it: the fields in order, with their values."""
    out: dict[str, Any] = {}
    for entry in client[db][coll].aggregate([{"$listCatalog": {}}]):
        for ix in entry["md"]["indexes"]:
            if ix["spec"]["name"] != "_id_":
                out[ix["spec"]["name"]] = [("catalog", list(ix["spec"].items()))]
    for ix in client[db][coll].list_indexes():
        if ix["name"] != "_id_":
            out[ix["name"]].append(("list", [(k, v) for k, v in ix.items()]))
    return out


# ----------------------------------------------------- the options kept --

#: (case, the spec given beside `name` and `key`, the key)
_OPTION_CASES: list[tuple[str, dict, dict]] = [
    ("sparse-false", {"sparse": False}, {"a": 1}),
    ("sparse-false-unique-false", {"unique": False, "sparse": False}, {"a": 1}),
    (
        "unique-sparse-false-hidden-false",
        {"unique": True, "sparse": False, "hidden": False},
        {"a": 1},
    ),
    ("sparse-false-2dsphere", {"sparse": False}, {"a": "2dsphere"}),
    ("sparse-false-wildcard", {"sparse": False}, {"a.$**": 1}),
    ("sparse-false-hashed", {"sparse": False}, {"a": "hashed"}),
    ("sparse-false-ttl", {"sparse": False, "expireAfterSeconds": 100}, {"a": 1}),
    (
        "sparse-false-partial",
        {"sparse": False, "partialFilterExpression": {"a": {"$gt": 1}}},
        {"a": 1},
    ),
    ("unique-false", {"unique": False}, {"a": 1}),
    ("hidden-false", {"hidden": False}, {"a": 1}),
    ("background-true", {"background": True}, {"a": 1}),
    ("background-false", {"background": False}, {"a": 1}),
    ("expire-zero", {"expireAfterSeconds": 0}, {"a": 1}),
]


def _created(client: Any, db: str, case: tuple[str, dict, dict]) -> Any:
    label, options, key = case
    client[db].drop_collection(label)
    client[db].command("createIndexes", label, indexes=[{"name": "ix", "key": key, **options}])
    return _specs(client, db, label)


@pytest.mark.backlog("RM-788")
def test_create_indexes_keeps_a_false_sparse_as_mongod_does(rust_mongo: Any, mongod: Any) -> None:
    """`createIndexes` with `sparse: false` keeps it in the spec, in the
    catalog and in `listIndexes`; `unique: false` and `hidden: false` are
    dropped; `background` and `expireAfterSeconds: 0` are kept (mongod
    8.2.11, 2026-11-24)."""
    differences = {}
    for case in _OPTION_CASES:
        theirs = mongo_answer(lambda c=case: _created(mongod, "spec_opts", c))
        ours = mongo_answer(lambda c=case: _created(rust_mongo, "spec_opts", c))
        if ours != theirs:
            differences[case[0]] = {"secantusd-mdb": ours, "mongod": theirs}
    assert not differences, differences


def _legacy(client: Any, db: str, label: str, options: dict) -> Any:
    client[db].drop_collection(label)
    client[db].command("createIndexes", label, indexes=[{"name": "ix", "key": {"a": 1}, **options}])
    return _specs(client, db, label)


@pytest.mark.backlog("RM-788")
def test_create_indexes_drops_the_options_mongod_parses_away(rust_mongo: Any, mongod: Any) -> None:
    """`prepareUnique: false` is accepted and not kept, and `dropDups` and
    `ns` are accepted and not kept (mongod 8.2.11, 2026-11-24)."""
    differences = {}
    for label, options in [
        ("prepare-unique-false", {"prepareUnique": False}),
        ("drop-dups", {"dropDups": True}),
        ("ns", {"ns": "spec_opts.ns"}),
    ]:
        theirs = mongo_answer(lambda lb=label, op=options: _legacy(mongod, "spec_opts", lb, op))
        ours = mongo_answer(lambda lb=label, op=options: _legacy(rust_mongo, "spec_opts", lb, op))
        if ours != theirs:
            differences[label] = {"secantusd-mdb": ours, "mongod": theirs}
    assert not differences, differences


@pytest.mark.backlog("RM-791")
def test_create_indexes_keeps_a_flag_as_given_and_refuses_a_hidden_that_is_no_bool(
    rust_mongo: Any, mongod: Any
) -> None:
    """The catalog keeps `sparse` and `background` as given (a `1.0` stays a
    double, a `1` an int, where `listIndexes` shows them as booleans), a
    `background` that is no number or bool is refused, `hidden` must be a
    bool, and `prepareUnique: true` is an index armed at creation (mongod
    8.2.11, 2026-11-24)."""
    differences = {}
    for label, options in [
        ("sparse-double", {"sparse": 1.0}),
        ("sparse-zero-double", {"sparse": 0.0}),
        ("sparse-int", {"sparse": 1}),
        ("sparse-zero", {"sparse": 0}),
        ("background-int", {"background": 1}),
        ("background-string", {"background": "x"}),
        ("hidden-int", {"hidden": 1}),
        ("hidden-string", {"hidden": "x"}),
        ("prepare-unique", {"prepareUnique": True}),
    ]:
        theirs = mongo_answer(lambda lb=label, op=options: _legacy(mongod, "spec_opts", lb, op))
        ours = mongo_answer(lambda lb=label, op=options: _legacy(rust_mongo, "spec_opts", lb, op))
        if ours != theirs:
            differences[label] = {"secantusd-mdb": ours, "mongod": theirs}
    assert not differences, differences


def _twice(client: Any, label: str, first: dict, second: dict) -> Any:
    db = client["spec_opts"]
    db.drop_collection(label)
    base = {"key": {"a": 1}}
    db.command("createIndexes", label, indexes=[{"name": "a", **base, **first}])
    reply = db.command(
        "createIndexes",
        label,
        indexes=[{"name": "a" if "name" not in second else "b", **base, **second}],
    )
    return reply.get("note"), reply["numIndexesAfter"]


@pytest.mark.backlog("RM-788")
def test_create_indexes_compares_a_spec_without_sparse_false_and_background(
    rust_mongo: Any, mongod: Any
) -> None:
    """A spec with `sparse: false` is the index one without `sparse` is, and
    `background` takes no part in the comparison: asked for again they are
    "all indexes already exist"; another name for the same key is 85
    (mongod 8.2.11, 2026-11-24)."""
    differences = {}
    for label, first, second in [
        ("none-then-false", {}, {"sparse": False}),
        ("false-then-none", {"sparse": False}, {}),
        ("false-then-false", {"sparse": False}, {"sparse": False}),
        ("false-then-other-name", {"sparse": False}, {"name": "b"}),
        ("none-then-background", {}, {"background": True}),
        ("background-then-none", {"background": True}, {}),
        ("false-then-true", {"sparse": False}, {"sparse": True}),
        ("ttl-zero-then-none", {"expireAfterSeconds": 0}, {}),
    ]:
        theirs = mongo_answer(lambda lb=label, a=first, b=second: _twice(mongod, lb, a, b))
        ours = mongo_answer(lambda lb=label, a=first, b=second: _twice(rust_mongo, lb, a, b))
        if ours != theirs:
            differences[label] = {"secantusd-mdb": ours, "mongod": theirs}
    assert not differences, differences


# ------------------------------------------------- an index is rebuilt --


def _free_port() -> int:
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


@contextlib.contextmanager
def _standalone_mongod(dbpath: Path) -> Iterator[Any]:
    assert MONGOD, "no mongod on PATH: the reference server is required"
    port = _free_port()
    proc = subprocess.Popen(
        [MONGOD, "--dbpath", str(dbpath), "--port", str(port), "--bind_ip", "127.0.0.1", "--quiet"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    client = pymongo.MongoClient(
        f"mongodb://127.0.0.1:{port}/?directConnection=true", serverSelectionTimeoutMS=30_000
    )
    try:
        client.admin.command("ping")
        yield client
    finally:
        with contextlib.suppress(pymongo.errors.PyMongoError):
            client.admin.command("shutdown", force=True)
        client.close()
        try:
            proc.wait(timeout=60)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()


def _collmodded(client: Any) -> None:
    """Indexes with their spec fields moved by `collMod`s: a TTL index whose
    setting is written last, an index hidden, and one armed `prepareUnique`
    then hidden."""
    db = client["app"]
    db["c"].insert_many([{"_id": i, "a": i, "b": i, "d": i} for i in range(5)])
    db.command(
        "createIndexes",
        "c",
        indexes=[
            {"name": "d_1", "key": {"d": 1}, "expireAfterSeconds": 100, "sparse": True},
            {"name": "a_1", "key": {"a": 1}},
            {"name": "b_1", "key": {"b": 1}},
            {"name": "e_1", "key": {"e": 1}},
        ],
    )
    db.command("collMod", "c", index={"name": "a_1", "hidden": True})
    db.command("collMod", "c", index={"name": "d_1", "expireAfterSeconds": 50})
    db.command("collMod", "c", index={"name": "b_1", "prepareUnique": True})
    db.command("collMod", "c", index={"name": "b_1", "hidden": True})


def _break_the_index_files(client: Any, store: Path) -> list[str]:
    """Names the files of every index the catalog lists; `--repair` runs
    after they are overwritten (a healthy store needs no rebuild)."""
    return [
        ident.removeprefix("table:") + ".wt"
        for entry in client["app"]["c"].aggregate([{"$listCatalog": {}}])
        for ident in entry["idxIdent"].values()
    ]


def _overwrite(store: Path, files: list[str]) -> None:
    for name in files:
        path = store / name
        size = path.stat().st_size
        with open(path, "r+b") as handle:
            handle.seek(4096)
            handle.write(os.urandom(max(size - 4096, 100)))


def _mongod_after(tmp_path: Path, rebuild: str) -> Any:
    store = tmp_path / "mongod"
    store.mkdir()
    with _standalone_mongod(store) as client:
        _collmodded(client)
        if rebuild == "reindex":
            client["app"].command("reIndex", "c")
        files = _break_the_index_files(client, store)
    if rebuild == "repair":
        _overwrite(store, files)
        done = subprocess.run(
            [MONGOD, "--dbpath", str(store), "--repair", "--quiet"],
            capture_output=True,
            timeout=300,
        )
        assert done.returncode == 0, done.stderr[-2000:]
    with _standalone_mongod(store) as client:
        return _specs(client, "app", "c")


def _ours_after(tmp_path: Path, rebuild: str) -> Any:
    store = tmp_path / "ours"
    with rust_mongo_server(store, "--standalone") as client:
        _collmodded(client)
        if rebuild == "reindex":
            client["app"].command("reIndex", "c")
        files = _break_the_index_files(client, store)
    if rebuild == "repair":
        _overwrite(store, files)
        done = subprocess.run(
            [str(RS_BINARY), "--storage-path", str(store), "--repair"],
            capture_output=True,
            timeout=300,
        )
        assert done.returncode == 0, done.stderr[-2000:]
    with rust_mongo_server(store, "--standalone") as client:
        return _specs(client, "app", "c")


@pytest.mark.backlog("RM-789")
def test_reindex_writes_a_collmodded_spec_in_creation_order(tmp_path: Path) -> None:
    """`reIndex` drops and builds every index again from its spec: an option
    `collMod` added after `name` is written before `key`, as at creation
    (`unique`, `hidden`, `prepareUnique`; mongod 8.2.11, 2026-11-24)."""
    theirs = _mongod_after(tmp_path, "reindex")
    assert [f for f, _ in theirs["a_1"][0][1]] == ["v", "hidden", "key", "name"]
    assert _ours_after(tmp_path, "reindex") == theirs


# ------------------------------------------------------- planner ties --


def _winner(client: Any, coll: str) -> list[Any]:
    plan = client["ties"].command(
        "explain", {"find": coll, "filter": {"a": 3, "b": 3, "c": 3}}, verbosity="queryPlanner"
    )["queryPlanner"]

    def index_of(stage: dict) -> Any:
        while "inputStage" in stage:
            stage = stage["inputStage"]
        return stage.get("indexName")

    return [index_of(plan["winningPlan"]), [index_of(p) for p in plan["rejectedPlans"][:2]]]


def _tied(client: Any, label: str, moved: list[str]) -> list[Any]:
    db = client["ties"]
    db.drop_collection(label)
    db[label].insert_many([{"a": i % 10, "b": i % 10, "c": i % 10} for i in range(200)])
    for key in "abc":
        db.command("createIndexes", label, indexes=[{"name": f"{key}_1", "key": {key: 1}}])
    for name in moved:
        db.command("collMod", label, index={"name": name, "prepareUnique": True})
    return _winner(client, label)


@pytest.mark.backlog("RM-790")
def test_a_planner_tie_goes_by_the_in_memory_index_order(rust_mongo: Any, mongod: Any) -> None:
    """Three single-field indexes cost the same for `{a: 3, b: 3, c: 3}`, so
    the first candidate wins, and candidates come in the collection's
    in-memory index order: build order, with an index a `collMod` changed
    moved to the end (mongod 8.2.11, 2026-11-24). `secantusd-mdb` keeps
    build order, and picks `a_1` after the `collMod` that moves it."""
    differences = {}
    for label, moved in [("plain", []), ("a-moved", ["a_1"]), ("a-then-b-moved", ["a_1", "b_1"])]:
        theirs = _tied(mongod, label, moved)
        ours = _tied(rust_mongo, label, moved)
        if ours != theirs:
            differences[label] = {"secantusd-mdb": ours, "mongod": theirs}
    assert not differences, differences
