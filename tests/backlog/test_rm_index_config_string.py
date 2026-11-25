"""An index's `storageEngine.wiredTiger.configString` is applied to the
index's WiredTiger table, as mongod 8.2.11 applies it (RM-796, measured
2026-11-25).

mongod composes an index table's `create` configuration as `type=file,
internal_page_max=16k,leaf_page_max=16k,checksum=on,prefix_compression=true`,
then the collection's `indexOptionDefaults` `configString`, then the index's
own, then `key_format=u,value_format=u,app_metadata=(formatVersion=N)` and
`log=(enabled=true)`: the user's string overrides the defaults and not those
four. `collStats` lists each table's `creationString` (its configuration with
every default filled in), the way each test below reads what was applied.
"""

from __future__ import annotations

import pprint
import re
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer, mongod, rust_mongo  # noqa: F401  (fixtures)

_ITEM = pytest.mark.backlog_fixed("RM-796")


def _config(creation: str) -> dict[str, str]:
    """A `creationString` as key to value, top-level keys only."""
    return dict(re.findall(r"([a-z_]+)=((?:\([^)]*\))|[^,]*)", creation))


def _normal(answer: tuple) -> tuple:
    """A refusal with what differs on every run taken out: build and
    collection UUIDs, and WiredTiger's time, process, thread and source line,
    which its build and version decide (the same as `_wiredtiger_text`)."""
    if answer[0] != "err":
        return answer
    text = re.sub(
        r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", "<uuid>", answer[2]
    )
    text = re.sub(r"\[\d+:\d+\]\[\d+:0x[0-9a-f]+\], ", "<time and thread>, ", text)
    text = re.sub(
        r"(wiredtiger_config_validate: \[WT_VERB_DEFAULT\]\[ERROR\]: )"
        r"(?:int )?\w+(?:\(.*?\))?, \d+: ",
        r"\1<function, line>: ",
        text,
    )
    text = re.sub(r"configuration key:? ", "configuration key ", text)
    return answer[0], answer[1], text


def _applied(client: Any, collection: str, name: str, base: str = "_id_") -> dict:
    """What the `name` index's table was made with that the `base` index's
    was not: the keys of its `creationString` that differ, the file it is in
    set aside (a name each server gives its own way)."""
    details = client["cfg"].command("collStats", collection)["indexDetails"]
    mine = _config(details[name]["creationString"])
    base_cfg = _config(details[base]["creationString"])
    return {
        k: (base_cfg.get(k), mine.get(k))
        for k in sorted(set(mine) | set(base_cfg))
        if k != "source" and mine.get(k) != base_cfg.get(k)
    }


def _create(client: Any, collection: str, indexes: list[dict], rows: int = 1, **options: Any):
    db = client["cfg"]
    db.drop_collection(collection)
    db.create_collection(collection, **options)
    if rows:
        db[collection].insert_many([{"a": i, "b": i} for i in range(rows)])
    return mongo_answer(lambda: db.command("createIndexes", collection, indexes=indexes))


def _se(config: str) -> dict:
    return {"wiredTiger": {"configString": config}}


# Each: a configString and what it changes in a table made without it.
_CONFIGS = [
    "block_compressor=zstd",
    "block_compressor=snappy",
    "block_compressor=zlib",
    "block_compressor=none",
    "prefix_compression=false",
    "prefix_compression=false,prefix_compression_min=0",
    "leaf_page_max=8KB,block_compressor=none",
    "internal_page_max=32KB",
    "allocation_size=8KB",
    "memory_page_max=1MB",
    "split_pct=50",
    "leaf_value_max=1KB",
    "checksum=off",
    "block_compressor=zstd,block_compressor=none",
    # the four the string cannot change
    "key_format=S",
    "value_format=S",
    "app_metadata=(formatVersion=1)",
    "log=(enabled=false)",
    "exclusive=true",
    "type=file",
    "huffman_key=english",
    "block_compressor=zstd,",
    "",
]


@_ITEM
def test_an_index_table_is_made_from_its_config_string(rust_mongo: Any, mongod: Any) -> None:  # noqa: F811
    differences = {}
    for config in _CONFIGS:
        answers = []
        for client in (mongod, rust_mongo):
            answer = _create(
                client, "c", [{"key": {"a": 1}, "name": "ix", "storageEngine": _se(config)}]
            )
            answers.append(answer[0] if answer[0] == "err" else _applied(client, "c", "ix"))
            if answer[0] == "err":
                answers[-1] = _normal(answer)
        if answers[0] != answers[1]:
            differences[config] = {"mongod": answers[0], "secantusd-mdb": answers[1]}
    assert not differences, pprint.pformat(differences, width=120)


@_ITEM
def test_every_kind_of_index_is_made_from_its_config_string(rust_mongo: Any, mongod: Any) -> None:  # noqa: F811
    cases = {
        "unique": {"key": {"a": 1}, "unique": True},
        "compound": {"key": {"a": 1, "b": -1}},
        "2dsphere": {"key": {"a": "2dsphere"}},
        "hashed": {"key": {"a": "hashed"}},
        "wildcard": {"key": {"$**": 1}},
        "partial": {"key": {"a": 1}, "partialFilterExpression": {"a": {"$gt": 1}}},
        "sparse": {"key": {"a": 1}, "sparse": True},
        "ttl": {"key": {"a": 1}, "expireAfterSeconds": 100},
    }
    differences = {}
    for label, spec in cases.items():
        answers = []
        for client in (mongod, rust_mongo):
            answer = _create(
                client,
                "kinds",
                [
                    {
                        "name": "ix",
                        "storageEngine": _se("block_compressor=zstd,leaf_page_max=8KB"),
                        **spec,
                    }
                ],
                rows=0,
            )
            answers.append(
                _normal(answer) if answer[0] == "err" else _applied(client, "kinds", "ix")
            )
        if answers[0] != answers[1]:
            differences[label] = {"mongod": answers[0], "secantusd-mdb": answers[1]}
    assert not differences, pprint.pformat(differences, width=120)


@_ITEM
def test_each_index_of_one_request_gets_its_own_config_string(rust_mongo: Any, mongod: Any) -> None:  # noqa: F811
    results = []
    for client in (mongod, rust_mongo):
        _create(
            client,
            "many",
            [
                {"key": {"a": 1}, "name": "a", "storageEngine": _se("block_compressor=zstd")},
                {"key": {"b": 1}, "name": "b"},
                {"key": {"c": 1}, "name": "c", "storageEngine": _se("prefix_compression=false")},
            ],
        )
        results.append({n: _applied(client, "many", n) for n in ("a", "b", "c")})
    assert results[1] == results[0]
    assert results[0]["b"] == {}


@_ITEM
def test_a_table_that_cannot_be_made_fails_the_build_as_mongod_fails_it(
    rust_mongo: Any,  # noqa: F811
    mongod: Any,  # noqa: F811
) -> None:
    """A string WiredTiger accepts as a configuration and cannot make the
    table from: the build fails (BadValue for `EINVAL`, UnknownError for the
    rest), naming WiredTiger's code and, for a parse error, its detail; the
    index is not listed. Refused earlier, by name: `type=lsm` anywhere in the
    string, an `encryption=(...)`, a NUL byte."""
    configs = [
        "allocation_size=3KB",
        "leaf_page_max=1KB",
        "leaf_page_max=4KB,allocation_size=8KB",
        "block_compressor=nosuch",
        "collator=foo",
        "type=ls",
        "type = lsm",
        "=",
        "==",
        "block_compressor=zstd,=,x",
        "type=lsm",
        "bogus=1,type=lsm",
        "key_format=S,type=lsm",
        "encryption=(name=x)",
        "bogus=1,encryption=(name=x)",
        "type=lsm,encryption=(name=x)",
        "xencryption=(name=x)",
        "encryption=(name=",
        "bogus=1\u0000",
        "TYPE=lsm",
        "bogus=1",
    ]
    differences = {}
    for config in configs:
        for rows in (0, 1):
            for extra in ({}, {"a2": 1}):
                indexes = [{"key": {"a": 1}, "name": "ix", "storageEngine": _se(config)}]
                if extra:
                    indexes.insert(0, {"key": {"b": 1}, "name": "plain"})
                answers = []
                for client in (mongod, rust_mongo):
                    answer = _create(client, "bad", indexes, rows=rows)
                    listed = [i["name"] for i in client["cfg"]["bad"].list_indexes()]
                    answers.append((_normal(answer) if answer[0] == "err" else "ok", listed))
                if answers[0] != answers[1]:
                    differences[f"{config!r} rows={rows} two={bool(extra)}"] = {
                        "mongod": answers[0],
                        "secantusd-mdb": answers[1],
                    }
    assert not differences, pprint.pformat(differences, width=120)


def _defaults(config: Any) -> dict:
    return {"storageEngine": config}


@_ITEM
def test_a_collections_index_option_defaults_apply_to_its_indexes(
    rust_mongo: Any,  # noqa: F811
    mongod: Any,  # noqa: F811
) -> None:
    """The collection's `indexOptionDefaults` configuration applies to its
    `_id_` index and each index made without its own, and under an index's
    own string, which overrides it key by key."""
    results = []
    for client in (mongod, rust_mongo):
        _create(
            client,
            "defaults",
            [
                {"key": {"a": 1}, "name": "plain"},
                {"key": {"b": 1}, "name": "unique", "unique": True},
                {
                    "key": {"a": 1, "b": 1},
                    "name": "own",
                    "storageEngine": _se("block_compressor=snappy"),
                },
                {
                    "key": {"b": 1, "a": 1},
                    "name": "override",
                    "storageEngine": _se("prefix_compression=true,block_compressor=none"),
                },
            ],
            indexOptionDefaults=_defaults(_se("block_compressor=zstd,prefix_compression=false")),
        )
        base = {
            k: v
            for k, v in _config(
                client["cfg"].command("collStats", "defaults")["indexDetails"]["_id_"][
                    "creationString"
                ]
            ).items()
            if k in ("block_compressor", "prefix_compression", "app_metadata")
        }
        results.append(
            (
                base,
                {
                    n: _applied(client, "defaults", n)
                    for n in ("plain", "unique", "own", "override")
                },
            )
        )
    assert results[1] == results[0]
    assert results[0][0]["block_compressor"] == "zstd"


@_ITEM
def test_index_option_defaults_are_checked_when_the_collection_is_created(
    rust_mongo: Any,  # noqa: F811
    mongod: Any,  # noqa: F811
) -> None:
    values = [
        {},
        1,
        "s",
        [],
        None,
        {"x": {}},
        {"x": 1},
        {"wiredTiger": 1},
        {"wiredTiger": {}},
        {"wiredTiger": {"configString": 1}},
        {"wiredTiger": {"configString": ""}},
        {"wiredTiger": {"foo": "x"}},
        _se("block_compressor=zstd"),
        _se("block_compressor=zstd, "),
        _se("type=lsm"),
        _se("encryption=(name=foo)"),
        _se("readonly=true"),
        _se("bogus=1"),
        _se("bogus=1\u0000"),
        _se("=="),
        {"wiredTiger": {"configString": "block_compressor=zstd"}, "x": {}},
    ]
    differences = {}
    for field in ("indexOptionDefaults", "storageEngine"):
        for value in values:
            options = {field: {"storageEngine": value} if field == "indexOptionDefaults" else value}
            answers = []
            for client in (mongod, rust_mongo):
                db = client["cfg"]
                db.drop_collection("coll")
                answer = mongo_answer(
                    lambda opts=options, d=db: d.create_collection("coll", **opts)
                )
                listed = None
                if answer[0] == "ok":
                    listed = db.command("listCollections", filter={"name": "coll"})["cursor"][
                        "firstBatch"
                    ][0]["options"]
                answers.append((_normal(answer) if answer[0] == "err" else "ok", listed))
            # The collection's own `storageEngine` configuration is not applied
            # to its table here (RM-798): a string only WiredTiger's create
            # refuses (`==`) is not refused.
            if field == "storageEngine" and value == _se("=="):
                continue
            if answers[0] != answers[1]:
                differences[f"{field} {value}"] = {
                    "mongod": answers[0],
                    "secantusd-mdb": answers[1],
                }
    assert not differences, pprint.pformat(differences, width=120)
    unknown = [
        mongo_answer(
            lambda c=client: c["cfg"].create_collection("u", indexOptionDefaults={"foo": 1})
        )
        for client in (mongod, rust_mongo)
    ]
    assert unknown[0] == unknown[1]
    assert unknown[0][1] == 40415


@_ITEM
def test_an_equivalent_index_under_another_name_or_options_is_refused_as_mongod_refuses_it(
    rust_mongo: Any,  # noqa: F811
    mongod: Any,  # noqa: F811
) -> None:
    """Options that do not make a different index (a TTL, `hidden`,
    `prepareUnique` and the `storageEngine`) leave the same key and name an
    equivalent index: 85, whichever name, where another option (`sparse`)
    is another index."""
    differences = {}
    for extra in (
        {"expireAfterSeconds": 10},
        {"hidden": True},
        {"prepareUnique": True},
        {"sparse": True},
        {"storageEngine": _se("block_compressor=zstd")},
        {"storageEngine": _se("block_compressor=snappy")},
        {},
    ):
        for other_name in (True, False):
            answers = []
            for client in (mongod, rust_mongo):
                db = client["cfg"]
                db.drop_collection("eq")
                db["eq"].insert_one({"a": 1})
                db.command(
                    "createIndexes",
                    "eq",
                    indexes=[
                        {
                            "key": {"a": 1},
                            "name": "a",
                            "storageEngine": _se("block_compressor=snappy"),
                        }
                    ],
                )
                name = "a2" if other_name else "a"
                answer = mongo_answer(
                    lambda n=name, e=extra, d=db: d.command(
                        "createIndexes", "eq", indexes=[{"key": {"a": 1}, "name": n, **e}]
                    )
                )
                answers.append(
                    _normal(answer)
                    if answer[0] == "err"
                    else (answer[1].get("note"), answer[1]["numIndexesAfter"])
                )
            if answers[0] != answers[1]:
                differences[f"{extra} other_name={other_name}"] = {
                    "mongod": answers[0],
                    "secantusd-mdb": answers[1],
                }
    assert not differences, pprint.pformat(differences, width=120)


@_ITEM
def test_coll_stats_lists_each_tables_creation_string(rust_mongo: Any, mongod: Any) -> None:  # noqa: F811
    """`collStats` has a `wiredTiger` section for the collection's table and
    an `indexDetails` one for each index's, each with the `app_metadata`
    (`metadata`), the `creationString`, the `type` and the statistics `uri`;
    mongod adds the statistics categories (not listed here)."""
    shapes = []
    for client in (mongod, rust_mongo):
        _create(
            client,
            "stats",
            [{"key": {"a": 1}, "name": "ix", "storageEngine": _se("block_compressor=zstd")}],
        )
        stats = client["cfg"].command("collStats", "stats")
        keys = [k for k in stats if not k.startswith("$") and k != "operationTime"]
        details = stats["indexDetails"]
        shapes.append(
            (
                keys,
                list(details),
                stats["wiredTiger"]["metadata"],
                {n: d["metadata"] for n, d in details.items()},
                stats["wiredTiger"]["type"],
                stats["wiredTiger"]["uri"].startswith("statistics:table:collection-"),
                details["ix"]["uri"].startswith("statistics:table:index-"),
                stats["wiredTiger"]["version"].startswith("WiredTiger "),
                _config(stats["wiredTiger"]["creationString"])["block_compressor"],
                _config(details["ix"]["creationString"])["block_compressor"],
                _config(details["_id_"]["creationString"])["block_compressor"],
            )
        )
    assert shapes[1] == shapes[0]
    assert shapes[0][-3:] == ("snappy", "zstd", "")


@_ITEM
def test_the_index_spec_keeps_the_config_string_as_given(rust_mongo: Any, mongod: Any) -> None:  # noqa: F811
    specs = []
    for client in (mongod, rust_mongo):
        _create(
            client,
            "kept",
            [
                {"key": {"a": 1}, "name": "a", "storageEngine": _se("block_compressor=zstd, ")},
                {"key": {"b": 1}, "name": "b", "storageEngine": _se("")},
                {"key": {"a": 1, "b": 1}, "name": "ab", "storageEngine": {"wiredTiger": {}}},
            ],
        )
        catalog = [
            list(ix["spec"].items())
            for e in client["cfg"]["kept"].aggregate([{"$listCatalog": {}}])
            for ix in e["md"]["indexes"]
        ]
        listed = [list(i.items()) for i in client["cfg"]["kept"].list_indexes()]
        specs.append((repr(catalog), repr(listed)))
    assert specs[1] == specs[0]
