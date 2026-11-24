"""The order of an index's catalog entry and of the indexes in a collection
(RM-734), against mongod 8.2.11 as a one-member replica set.

mongod's durable catalog (`$listCatalog`'s `md.indexes`) keeps indexes in build
order and each spec's fields in the order mongod wrote them: `v`, `unique`,
`hidden`, `prepareUnique`, `key`, `name`, then the rest at creation, and
after `name` for an option a `collMod` adds. A `collMod` that changes an
index also moves it to the end of mongod's in-memory catalog, where
`dropIndexes: "*"` takes them from; a `collMod` that changes nothing does
not, and a restart puts them back in build order.
"""

from __future__ import annotations

import time
import uuid
from typing import Any

import pymongo
import pytest

from tests.mongod_live.conftest import same


def _setup(co: Any, *specs: dict) -> None:
    for s in specs:
        co.database.command("createIndexes", co.name, indexes=[s])


def _plain(co: Any) -> None:
    _setup(
        co,
        {"name": "a_1", "key": {"a": 1}},
        {"name": "b_1", "key": {"b": 1}},
        {"name": "d_1", "key": {"d": 1}},
    )


def _ttl_first(co: Any) -> None:
    _setup(
        co,
        {"name": "d_1", "key": {"d": 1}, "expireAfterSeconds": 100},
        {"name": "a_1", "key": {"a": 1}},
        {"name": "b_1", "key": {"b": 1}},
    )


def _hidden_first(co: Any) -> None:
    _setup(
        co,
        {"name": "e_1", "key": {"e": 1}, "hidden": True},
        {"name": "a_1", "key": {"a": 1}},
        {"name": "b_1", "key": {"b": 1}},
    )


def _unique(co: Any) -> None:
    _setup(
        co,
        {"name": "a_1", "key": {"a": 1}, "unique": True},
        {"name": "b_1", "key": {"b": 1}},
        {"name": "d_1", "key": {"d": 1}},
    )


def _a(**mod: Any) -> dict:
    return {"name": "a_1", **mod}


def _b(**mod: Any) -> dict:
    return {"name": "b_1", **mod}


#: (setup, the `index` of each `collMod` in turn)
_SCENARIOS = {
    "hide": (_plain, [_a(hidden=True)]),
    "hide-then-unhide": (_plain, [_a(hidden=True), _a(hidden=False)]),
    "hide-two": (_plain, [_b(hidden=True), _a(hidden=True)]),
    "hide-again-is-no-move": (_plain, [_a(hidden=True), _b(hidden=True), _a(hidden=True)]),
    "unhide-unhidden-is-no-move": (_plain, [_a(hidden=False)]),
    "hide-hide-unhide": (_plain, [_a(hidden=True), _b(hidden=True), _a(hidden=False)]),
    "hidden-at-create-hide-again": (_hidden_first, [{"name": "e_1", "hidden": True}]),
    "hidden-at-create-unhide": (_hidden_first, [{"name": "e_1", "hidden": False}]),
    "prepare-unique": (_plain, [_a(prepareUnique=True)]),
    "prepare-unique-off-when-unset": (_plain, [_a(prepareUnique=False)]),
    "prepare-unique-twice": (
        _plain,
        [_a(prepareUnique=True), _b(prepareUnique=True), _a(prepareUnique=True)],
    ),
    "prepare-unique-then-unique": (_plain, [_a(prepareUnique=True), _a(unique=True)]),
    "unique-of-a-unique-index": (_unique, [_a(unique=True)]),
    "expire-added": (_plain, [_a(expireAfterSeconds=5)]),
    "expire-changed": (_ttl_first, [{"name": "d_1", "expireAfterSeconds": 200}]),
    "expire-the-same": (_ttl_first, [{"name": "d_1", "expireAfterSeconds": 100}]),
    "expire-and-hide-in-one": (_plain, [_a(hidden=True, expireAfterSeconds=5)]),
    "hide-and-expire-in-one": (_plain, [_a(expireAfterSeconds=5, hidden=True)]),
    "hide-then-prepare-unique": (_plain, [_a(hidden=True), _a(prepareUnique=True)]),
    "prepare-unique-then-hide": (_plain, [_a(prepareUnique=True), _a(hidden=True)]),
    "prepare-unique-hide-unique": (
        _plain,
        [_a(prepareUnique=True), _a(hidden=True), _a(unique=True)],
    ),
}


def _catalog_specs(co: Any) -> list[list[str]]:
    out = []
    for entry in co.aggregate([{"$listCatalog": {}}]):
        for ix in entry["md"]["indexes"]:
            if ix["spec"]["name"] != "_id_":
                out.append(list(ix["spec"]))
    return out


def _drops(client: Any, scenario: str) -> dict:
    setup, mods = _SCENARIOS[scenario]
    db = client["rm734_order"]
    name = "c" + uuid.uuid4().hex[:8]
    co = db.create_collection(name)
    setup(co)
    for m in mods:
        db.command("collMod", name, index=m)
    listed = [i["name"] for i in co.list_indexes()]
    catalog = _catalog_specs(co)
    with db.watch(show_expanded_events=True, max_await_time_ms=200) as stream:
        co.drop_indexes()
        events, end = [], time.monotonic() + 10
        while len(events) < len(listed) - 1 and time.monotonic() < end:
            ev = stream.try_next()
            if ev is not None:
                events.append(ev)
    drops = [
        (e["operationDescription"]["indexes"][0]["name"], list(e["operationDescription"]["indexes"][0]))
        for e in events
        if e["operationType"] == "dropIndexes"
    ]
    return {"listIndexes": listed, "catalog": catalog, "drops": drops}


@pytest.mark.parametrize("scenario", sorted(_SCENARIOS))
def test_a_collmod_moves_an_index_in_the_drop_order_and_writes_its_spec_in_mongods_order(
    scenario: str, rust_mongo: Any, mongod_rs: Any
) -> None:
    same(_drops(rust_mongo, scenario), _drops(mongod_rs, scenario))


def test_the_catalog_spec_of_a_new_index_is_in_mongods_order(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    specs = [
        {"name": "x", "key": {"a": 1}, "expireAfterSeconds": 5, "unique": True},
        {"name": "x", "key": {"a": 1}, "hidden": True, "unique": True},
        {"name": "x", "key": {"a": 1}, "sparse": True, "unique": True, "hidden": True},
        {"name": "x", "key": {"a": 1}, "collation": {"locale": "en"}, "unique": True},
    ]

    def run(client: Any) -> list:
        out = []
        for spec in specs:
            db = client["rm734_create"]
            co = db.create_collection("c" + uuid.uuid4().hex[:8])
            db.command("createIndexes", co.name, indexes=[spec])
            out.append(_catalog_specs(co))
        return out

    same(run(rust_mongo), run(mongod_rs))


def test_a_collmod_keeps_the_options_position_when_it_changes_a_value(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    def run(client: Any) -> list:
        db = client["rm734_value"]
        co = db.create_collection("c" + uuid.uuid4().hex[:8])
        _setup(co, {"name": "a_1", "key": {"a": 1}, "expireAfterSeconds": 100})
        out = []
        for mod in ({"hidden": True}, {"expireAfterSeconds": 9}, {"hidden": False}, {"hidden": True}):
            db.command("collMod", co.name, index={"name": "a_1", **mod})
            out.append(_catalog_specs(co))
        return out

    same(run(rust_mongo), run(mongod_rs))
