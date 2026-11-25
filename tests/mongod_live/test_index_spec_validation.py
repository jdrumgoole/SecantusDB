"""How `createIndexes` reads the options of an index spec, against mongod
8.2.11 (measured 2026-11-24, RM-771 and RM-791): which type each option
takes and which values it coerces, the refusal each bad value gets, which of
two refusals answers when a spec has both, and the spec the catalog and
`listIndexes` then show."""

from __future__ import annotations

import itertools
from typing import Any

import bson
from bson.decimal128 import Decimal128
from bson.int64 import Int64

from tests.backlog.conftest import mongo_answer, mongod, rust_mongo  # noqa: F401  (fixtures)
from tests.backlog.test_rm_found_rm732 import _wiredtiger_text

_OID = bson.ObjectId("6b062255d4978503d9904707")
_VALUES: dict[str, Any] = {
    "true": True,
    "false": False,
    "int1": 1,
    "int0": 0,
    "long1": Int64(1),
    "dbl1": 1.0,
    "dbl0": 0.0,
    "dbl.5": 0.5,
    "nan": float("nan"),
    "dec1": Decimal128("1"),
    "str": "x",
    "null": None,
    "obj": {"a": 1},
    "arr": [1],
    "oid": _OID,
}


def _created(client: Any, key: dict, options: dict) -> tuple:
    """The answer to creating one index, and what the catalog and
    `listIndexes` then keep of it."""
    db = client["spec_validation"]
    db.drop_collection("t")
    ans = mongo_answer(
        lambda: db.command("createIndexes", "t", indexes=[{"key": key, "name": "ix", **options}])
    )
    if ans[0] == "err":
        return _wiredtiger_text(ans)
    catalog = [
        list(ix["spec"].items())
        for entry in db.t.aggregate([{"$listCatalog": {}}])
        for ix in entry["md"]["indexes"]
        if ix["spec"]["name"] == "ix"
    ]
    listed = [list(ix.items()) for ix in db.t.list_indexes() if ix["name"] == "ix"]
    return ("ok", repr(catalog), repr(listed))


def _differences(ours_client: Any, theirs_client: Any, cases: list[tuple[dict, dict]]) -> dict:
    out = {}
    for key, options in cases:
        theirs = _created(theirs_client, key, options)
        ours = _created(ours_client, key, options)
        if repr(ours) != repr(theirs):
            out[f"{key} {options}"] = {"secantusd-mdb": ours, "mongod": theirs}
    return out


def test_every_flag_takes_what_mongod_takes(rust_mongo: Any, mongod: Any) -> None:  # noqa: F811
    """`sparse` and `background` are kept as given, `unique` and
    `prepareUnique` as bools, `hidden` only as one; the rest is refused
    (14) with the field's own wording."""
    cases = [
        ({"a": 1}, {flag: value})
        for flag in ("sparse", "background", "hidden", "unique", "prepareUnique")
        for value in _VALUES.values()
    ]
    assert not _differences(rust_mongo, mongod, cases)


def test_every_typed_option_takes_what_mongod_takes(rust_mongo: Any, mongod: Any) -> None:  # noqa: F811
    cases = [
        ({"a": 1}, {option: value})
        for option, values in {
            "weights": [{"a": 1}, {}, 1, "x", {"a": "x"}, {"b": 1}],
            "default_language": ["english", "bogus", "", 1, None],
            "language_override": ["lang", 1, "", None],
            "textIndexVersion": [1, 3, "x"],
            "2dsphereIndexVersion": [1, 3, "x"],
            "bits": [26, "x"],
            "min": [-1, "x"],
            "max": [1, "x"],
            "bucketSize": [1, "x"],
            "coarsestIndexedLevel": [1, "x"],
            "finestIndexedLevel": [1, "x"],
            "v": [1, 2, 3, 0, "x", 1.0],
            "ns": ["x"],
            "dropDups": [True],
            "bogus": [1],
        }.items()
        for value in values
    ]
    assert not _differences(rust_mongo, mongod, cases)


def test_a_wildcard_projection_belongs_to_a_top_level_wildcard_key(
    rust_mongo: Any,  # noqa: F811
    mongod: Any,  # noqa: F811
) -> None:
    cases = [
        (key, {"wildcardProjection": value})
        for key in ({"a": 1}, {"a": "text"}, {"a": "hashed"}, {"a.$**": 1}, {"$**": 1}, {"$**": -1})
        for value in ({"a": 1}, {}, 1, "x", None)
    ]
    assert not _differences(rust_mongo, mongod, cases)


def test_weights_belong_to_a_text_key_and_the_options_of_other_kinds_are_kept(
    rust_mongo: Any,  # noqa: F811
    mongod: Any,  # noqa: F811
) -> None:
    cases = [
        (key, {option: value})
        for key in ({"a": 1}, {"a": "2dsphere"}, {"a": "hashed"}, {"a.$**": 1}, {"a": "2d"})
        for option, value in (
            ("weights", {"a": 1}),
            ("default_language", "english"),
            ("prepareUnique", True),
            ("textIndexVersion", 3),
            ("2dsphereIndexVersion", 3),
            ("language_override", "l"),
        )
    ]
    assert not _differences(rust_mongo, mongod, cases)


def test_a_storage_engine_option_is_checked_as_mongod_checks_it(
    rust_mongo: Any,  # noqa: F811
    mongod: Any,  # noqa: F811
) -> None:
    cases = [
        ({"a": 1}, {"storageEngine": value})
        for value in (
            {"wiredTiger": {"configString": "bogus=1"}},
            {"wiredTiger": {"configString": "block_compressor=zlib"}},
            {"wiredTiger": {"configString": ""}},
            {"wiredTiger": {"configString": 1}},
            {"wiredTiger": {"x": 1}},
            {"wiredTiger": {}},
            {"wiredTiger": 1},
            {"wiredTiger": None},
            {"foo": {}},
            {"foo": 1},
            {"wiredTiger": {}, "foo": {}},
            {"foo": {}, "wiredTiger": {"configString": "bogus=1"}},
            {},
            1,
            "x",
        )
    ]
    assert not _differences(rust_mongo, mongod, cases)


def test_an_option_that_excludes_another_is_refused_as_mongod_refuses_it(
    rust_mongo: Any,  # noqa: F811
    mongod: Any,  # noqa: F811
) -> None:
    cases = [
        ({"a": 1}, options)
        for options in (
            {"unique": True, "prepareUnique": True},
            {"unique": 1.0, "prepareUnique": 1},
            {"unique": False, "prepareUnique": True},
            {"unique": True, "prepareUnique": False},
            {"sparse": 1, "partialFilterExpression": {"a": 1}},
            {"sparse": 1.0, "partialFilterExpression": {"a": 1}},
            {"sparse": 0, "partialFilterExpression": {"a": 1}},
        )
    ] + [({"a": "hashed"}, {"unique": True, "prepareUnique": True})]
    assert not _differences(rust_mongo, mongod, cases)


_FAULTS: dict[str, dict] = {
    "v-str": {"v": "x"},
    "v-3": {"v": 3},
    "weights": {"weights": {"a": 1}},
    "weights-int": {"weights": 1},
    "hidden-int": {"hidden": 1},
    "sparse-str": {"sparse": "x"},
    "unique-str": {"unique": "x"},
    "bg-str": {"background": "x"},
    "prepare-str": {"prepareUnique": "x"},
    "se-bad": {"storageEngine": {"wiredTiger": {"configString": "bogus=1"}}},
    "se-int": {"storageEngine": 1},
    "bogus": {"bogus": 1},
    "wcp": {"wildcardProjection": {"a": 1}},
    "ttl-str": {"expireAfterSeconds": "x"},
    "ttl-neg": {"expireAfterSeconds": -1},
    "dl-int": {"default_language": 1},
    "pfe-str": {"partialFilterExpression": "x"},
    "coll-str": {"collation": "x"},
    "coll-bad": {"collation": {"locale": "zz"}},
    "bits-str": {"bits": "x"},
}


def test_the_refusal_that_answers_is_the_one_mongod_reaches_first(
    rust_mongo: Any,  # noqa: F811
    mongod: Any,  # noqa: F811
) -> None:
    """A spec with two faults: an unknown field first, then each field in the
    order the spec gives them (type, `v`, the key, a wildcard projection),
    then what is checked once they are read (`weights`, the TTL, the
    collation, the storage engine)."""
    cases = [
        ({"a": 1}, {**_FAULTS[a], **_FAULTS[b]})
        for a, b in itertools.permutations(_FAULTS, 2)
        if not set(_FAULTS[a]) & set(_FAULTS[b])
    ]
    assert not _differences(rust_mongo, mongod, cases)


def test_a_bad_key_answers_in_the_order_the_spec_gives_its_fields(
    rust_mongo: Any,  # noqa: F811
    mongod: Any,  # noqa: F811
) -> None:
    out = {}
    for key in ({}, {"a": 0}, {"a": "zz"}, {"$a": 1}):
        for name in (
            "v-str",
            "v-3",
            "weights",
            "hidden-int",
            "sparse-str",
            "bogus",
            "wcp",
            "ttl-str",
            "se-bad",
            "pfe-str",
        ):
            for position in ("before", "after"):
                fault = _FAULTS[name]
                options = (
                    {"key": key, "name": "ix", **fault}
                    if position == "before"
                    else {"name": "ix", **fault, "key": key}
                )
                results = []
                for client in (mongod, rust_mongo):
                    db = client["spec_validation"]
                    db.drop_collection("t")
                    results.append(
                        _wiredtiger_text(
                            mongo_answer(
                                lambda db=db, options=options: db.command(
                                    "createIndexes", "t", indexes=[options]
                                )
                            )
                        )
                    )
                if repr(results[0]) != repr(results[1]):
                    out[f"{key} {name} key-{position}"] = results
    assert not out
