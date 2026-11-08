"""The answers of the commands this server parses before it takes their
locks (RM-185): `explain`, `dataSize`, the plan cache commands and
`validateDBMetadata`. The parse moved ahead of the locks, and with it the
plan cache commands' empty namespace and `validateDBMetadata`'s
`apiParameters`, which mongod parses as a struct of its own. Measured on
8.2.11, 2026-11-08.
"""

from __future__ import annotations

from typing import Any

import pytest
from bson import SON

from tests.mongod_live.conftest import mongo_answer, same

#: Fields of a reply that are the server's own, not the command's.
_PER_SERVER = {"$clusterTime", "operationTime"}

#: A fixed name: the cases' ids carry it, and every xdist worker must
#: collect the same ids. Each worker starts servers of its own.
_DB = "rm185_parse_first"

CASES: list[list[tuple[str, Any]]] = [
    [("explain", 1)],
    [("explain", None)],
    [("explain", {})],
    [("explain", {"find": 1})],
    [("explain", {"find": ""})],
    [("explain", {"count": 1})],
    [("explain", {"explain": {"find": "x"}})],
    [("explain", {"nosuch": "x"})],
    [("explain", {"ping": 1})],
    [("explain", {"find": "x"}), ("verbosity", 1)],
    [("explain", {"find": "x"}), ("verbosity", "bogus")],
    [("explain", {"find": "x"}), ("bogus", 1)],
    [("explain", {"find": "x", "$db": "other"})],
    [("explain", {"find": "x"}), ("writeConcern", {"w": 1})],
    [("dataSize", 1)],
    [("dataSize", None)],
    [("dataSize", "nodot")],
    [("dataSize", f"{_DB}.x"), ("min", {"a": 1})],
    [("dataSize", f"{_DB}.x"), ("max", {"a": 1})],
    [("dataSize", f"{_DB}.x"), ("keyPattern", 1)],
    [("dataSize", f"{_DB}.x"), ("bogus", 1)],
    *(
        [(command, value)]
        for command in (
            "planCacheClear",
            "planCacheClearFilters",
            "planCacheListFilters",
            "planCacheSetFilter",
        )
        for value in (1, None, "")
    ),
    [("validateDBMetadata", 1)],
    [("validateDBMetadata", 1), ("apiParameters", None)],
    [("validateDBMetadata", 1), ("apiParameters", 1)],
    [("validateDBMetadata", 1), ("apiParameters", {})],
    [("validateDBMetadata", 1), ("apiParameters", {"version": None})],
    [("validateDBMetadata", 1), ("apiParameters", {"version": 1})],
    [("validateDBMetadata", 1), ("apiParameters", {"strict": True})],
    [("validateDBMetadata", 1), ("apiParameters", SON([("version", "1"), ("strict", 1)]))],
    [("validateDBMetadata", 1), ("apiParameters", SON([("version", "1"), ("strict", None)]))],
    [
        ("validateDBMetadata", 1),
        ("apiParameters", SON([("version", "1"), ("deprecationErrors", 1)])),
    ],
    [("validateDBMetadata", 1), ("apiParameters", SON([("version", "1"), ("bogus", 1)]))],
    [("validateDBMetadata", 1), ("apiParameters", {"bogus": 1})],
    [("validateDBMetadata", 1), ("apiParameters", SON([("strict", 1), ("version", 1)]))],
    [("validateDBMetadata", 1), ("collection", 1), ("apiParameters", {"version": 1})],
    [("validateDBMetadata", 1), ("apiParameters", {"version": 1}), ("collection", 1)],
    [("validateDBMetadata", 1), ("db", 1), ("apiParameters", {"version": "1"})],
    [("validateDBMetadata", 1), ("db", _DB), ("apiParameters", {"version": "2"})],
    [("validateDBMetadata", 1), ("db", _DB), ("apiParameters", {"version": ""})],
    [
        ("validateDBMetadata", 1),
        ("db", _DB),
        ("collection", None),
        ("apiParameters", {"version": "1"}),
    ],
    [("validateDBMetadata", 1), ("db", _DB), ("bogus", 1), ("apiParameters", {"version": "1"})],
]


def _answer(client: Any, command: list[tuple[str, Any]]) -> tuple:
    db = client[_DB]
    db.x.replace_one({"_id": 1}, {"_id": 1}, upsert=True)
    answer = mongo_answer(lambda: db.command(SON(command)))
    if answer[0] == "ok":
        return ("ok", {k: v for k, v in answer[1].items() if k not in _PER_SERVER})
    return answer


@pytest.mark.parametrize("command", CASES, ids=[repr(c) for c in CASES])
def test_parsed_before_its_locks(rust_mongo: Any, mongod: Any, command: list) -> None:
    same(_answer(rust_mongo, command), _answer(mongod, command))
