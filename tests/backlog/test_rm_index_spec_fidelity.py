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


@pytest.mark.backlog_fixed("RM-788")
def test_create_indexes_keeps_a_false_sparse_as_mongod_does(rust_mongo: Any, mongod: Any) -> None:
    """`createIndexes` with `sparse: false` dropped it; mongod keeps it in the spec, in the
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


@pytest.mark.backlog_fixed("RM-788")
def test_create_indexes_drops_the_options_mongod_parses_away(rust_mongo: Any, mongod: Any) -> None:
    """`prepareUnique: false` was refused and `dropDups` and `ns` were kept;
    mongod accepts all three and keeps none (mongod 8.2.11, 2026-11-24)."""
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


@pytest.mark.backlog_fixed("RM-791")
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


@pytest.mark.backlog_fixed("RM-788")
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


@pytest.mark.backlog_fixed("RM-789")
def test_reindex_writes_a_collmodded_spec_in_creation_order(tmp_path: Path) -> None:
    """`reIndex` left the specs as `collMod` made them; mongod drops and
    builds every index again from its spec: an option `collMod` added after
    `name` is written before `key`, as at creation (`unique`, `hidden`,
    `prepareUnique`; mongod 8.2.11, 2026-11-24)."""
    theirs = _mongod_after(tmp_path, "reindex")
    assert [f for f, _ in theirs["a_1"][0][1]] == ["v", "hidden", "key", "name"]
    ours = _ours_after(tmp_path, "reindex")
    assert list(ours) == list(theirs)
    assert ours == theirs


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


@pytest.mark.backlog_fixed("RM-790")
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


@pytest.mark.backlog_fixed("RM-790")
def test_every_collmod_that_moves_an_index_moves_it_in_the_planner(
    rust_mongo: Any, mongod: Any
) -> None:
    """Probed on mongod 8.2.11 (2026-11-25): `prepareUnique` on and off, `hidden`
    set and cleared, and a TTL `expireAfterSeconds` (even to the value it had)
    move the index to the end of the in-memory catalog the planner tries
    candidates from; a `collMod` that changes nothing does not."""

    def run(client: Any, label: str) -> list[Any]:
        db = client["ties"]
        steps = {
            "ttl-changed": ({"a": 1000}, [("a_1", {"expireAfterSeconds": 2000})]),
            "ttl-same": ({"a": 1000}, [("a_1", {"expireAfterSeconds": 1000})]),
            "ttl-on-b": ({"b": 1000}, [("b_1", {"expireAfterSeconds": 2000})]),
            "hide-unhide": ({}, [("a_1", {"hidden": True}), ("a_1", {"hidden": False})]),
            "unhide-nothing": ({}, [("a_1", {"hidden": False})]),
            "unset-nothing": ({}, [("a_1", {"prepareUnique": False})]),
            "set-twice": (
                {},
                [("a_1", {"prepareUnique": True}), ("b_1", {"prepareUnique": True})]
                + [("a_1", {"prepareUnique": True})],
            ),
            "set-then-unset": (
                {},
                [("a_1", {"prepareUnique": True}), ("b_1", {"prepareUnique": True})]
                + [("a_1", {"prepareUnique": False})],
            ),
        }
        ttl, changes = steps[label]
        name = f"coll_{label}".replace("-", "_")
        db.drop_collection(name)
        db[name].insert_many([{"a": i % 10, "b": i % 10, "c": i % 10} for i in range(200)])
        for key in "abc":
            spec = {"name": f"{key}_1", "key": {key: 1}}
            if key in ttl:
                spec["expireAfterSeconds"] = ttl[key]
            db.command("createIndexes", name, indexes=[spec])
        for index_name, change in changes:
            db.command("collMod", name, index={"name": index_name, **change})
        return _winner(client, name)

    labels = [
        "ttl-changed",
        "ttl-same",
        "ttl-on-b",
        "hide-unhide",
        "unhide-nothing",
        "unset-nothing",
        "set-twice",
        "set-then-unset",
    ]
    differences = {}
    for label in labels:
        theirs = run(mongod, label)
        ours = run(rust_mongo, label)
        if ours != theirs:
            differences[label] = {"secantusd-mdb": ours, "mongod": theirs}
    assert not differences, differences


def _moved_then_rebuilt(client: Any) -> dict:
    db = client["ties"]
    db.drop_collection("t")
    db["t"].insert_many([{"a": i % 10, "b": i % 10, "c": i % 10} for i in range(200)])
    for key in "abc":
        db.command("createIndexes", "t", indexes=[{"name": f"{key}_1", "key": {key: 1}}])
    db.command("collMod", "t", index={"name": "a_1", "prepareUnique": True})
    moved = _winner(client, "t")
    db.command("reIndex", "t")
    return {"moved": moved, "reindexed": _winner(client, "t")}


@pytest.mark.backlog_fixed("RM-790")
def test_reindex_puts_the_planner_candidates_back_in_build_order(tmp_path: Path) -> None:
    """`reIndex` drops and builds every index again, so the in-memory order a
    `collMod` made is gone: the tie goes to `a_1` again (mongod 8.2.11,
    2026-11-25)."""
    (tmp_path / "mongod").mkdir()
    with _standalone_mongod(tmp_path / "mongod") as client:
        theirs = _moved_then_rebuilt(client)
    assert theirs["moved"][0] == "b_1" and theirs["reindexed"][0] == "a_1", theirs
    with rust_mongo_server(tmp_path / "ours", "--standalone") as client:
        assert _moved_then_rebuilt(client) == theirs


def _refusals(client: Any, key: dict, options_list: list[dict]) -> dict:
    db = client["spec_found"]
    out = {}
    for options in options_list:
        db.drop_collection("t")
        ans = mongo_answer(
            lambda o=options: db.command(
                "createIndexes", "t", indexes=[{"key": key, "name": "ix", **o}]
            )
        )
        out[repr(options)] = ("ok",) if ans[0] == "ok" else ans
    return out


@pytest.mark.backlog("RM-794")
def test_a_wildcard_projection_is_parsed_as_a_projection_is(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 (2026-11-24) parses a `wildcardProjection` as an
    inclusion or exclusion projection and refuses one that mixes them (31254,
    31253), computes a field (51271) or collides two paths (31250), each as
    "Failed to parse projection: wildcardProjection :: caused by :: ...".
    This server accepts them all."""
    projections = [
        {"wildcardProjection": p}
        for p in (
            {"a": 1, "b": 0},
            {"a": 0, "b": 1},
            {"a": 0, "b": 0},
            {"a": {"$slice": 1}},
            {"_id": 1, "a": 0},
            {"a": "x"},
            {"a.b": 1, "a": 1},
        )
    ]
    theirs = _refusals(mongod, {"$**": 1}, projections)
    assert any(v[0] == "err" and v[1] == 31254 for v in theirs.values()), theirs
    assert _refusals(rust_mongo, {"$**": 1}, projections) == theirs


@pytest.mark.backlog("RM-795")
def test_a_clustered_option_on_an_index_spec_is_read_as_mongod_reads_it(
    rust_mongo: Any, mongod: Any
) -> None:
    """`clustered` is a field of mongod 8.2.11's index spec (2026-11-24): on
    its own it is refused as 67 "The 'unique' field is required when
    'clustered' is specified", with `unique` as 6100900 ("Cannot implicitly
    create a new collection with createIndex 'clustered' option"), `false`
    as 6492800 and a string as 14. This server refuses the field as
    unknown (197)."""
    options = [
        {"clustered": True},
        {"clustered": True, "unique": True},
        {"clustered": False},
        {"clustered": "x"},
    ]
    theirs = _refusals(mongod, {"a": 1}, options)
    assert theirs[repr({"clustered": True})][1] == 67, theirs
    assert _refusals(rust_mongo, {"a": 1}, options) == theirs
