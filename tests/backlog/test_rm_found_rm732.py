"""Gap tests for what working the hashed-index item (RM-732) found, against
mongod 8.2.11 (measured 2026-11-23)."""

from __future__ import annotations

import datetime
import re
from typing import Any

import bson
import pytest

from tests.backlog.conftest import mongo_answer, mongod_rs, rust_mongo  # noqa: F401  (fixtures)
from tests.backlog.test_rm_hashed_index import _created, _every_type


def _meta_array(client: Any) -> tuple:
    client.drop_database("rm_found732")
    db = client.rm_found732
    db.command("create", "ts", timeseries={"timeField": "t", "metaField": "m"})
    made = mongo_answer(
        lambda: db.command("createIndexes", "ts", indexes=[{"key": {"m": "hashed"}, "name": "mh"}])
    )[0]
    reply = db.command(
        "insert",
        "ts",
        documents=[{"t": datetime.datetime(2020, 1, 1), "m": [1, 2]}],
    )
    errors = [
        (e["code"], e["errmsg"].split(" :: caused by :: ")[-1])
        for e in reply.get("writeErrors", [])
    ]
    count = db.ts.count_documents({})
    client.drop_database("rm_found732")
    return made, errors, count


@pytest.mark.backlog("RM-769")
def test_a_hashed_meta_index_refuses_an_array_meta_field(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 refuses a measurement whose `metaField` is an array
    when the collection has a hashed index on it, with the write error 16766
    ("Error: hashed indexes do not currently support array values. Found
    array at path: meta") and stores nothing."""
    theirs = _meta_array(mongod_rs)
    assert theirs[1][0][0] == 16766 and theirs[2] == 0, theirs
    assert _meta_array(rust_mongo) == theirs


def _hinted_sorts(client: Any) -> dict:
    client.drop_database("rm_found732")
    db = client.rm_found732
    db.t.insert_many([{"_id": i, "a": v} for i, v in enumerate(_every_type())] + [{"_id": 1000}])
    db.t.create_index([("a", "hashed")], name="ah")
    out = {
        "no limit": [d["_id"] for d in db.t.find().sort("a", 1).hint("ah")],
        "limit": [d["_id"] for d in db.t.find().sort("a", 1).hint("ah").limit(10)],
    }
    client.drop_database("rm_found732")
    return out


@pytest.mark.backlog("RM-770")
def test_a_sort_by_a_hashed_field_leaves_ties_as_mongods_sorter_does(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """A sort on the field of a hashed index the query is hinted to (the
    index orders nothing, so the plan is SORT over FETCH over the scan)
    reads the documents in the index's order, and mongod 8.2.11 leaves
    documents that compare equal where its sorter does: over 41 values and a
    document missing the field, with no limit the missing one (id 1000)
    comes before the null (24) and the NaN Decimal128 (11) before the NaN
    double (6); with a limit only the NaNs come so. This server leaves them
    in the order it read them (24, 1000, 6, 11)."""
    theirs = _hinted_sorts(mongod_rs)
    assert _hinted_sorts(rust_mongo) == theirs


def _wiredtiger_text(answer: tuple) -> tuple:
    """A refusal quoting WiredTiger's own message, with only what is the
    engine's BUILD and not mongod's behaviour taken out. Compared as they
    are: the code (2), the spec echo, WiredTiger's `22: Invalid argument`,
    that the text comes from `wiredtiger_config_validate` at error level,
    and the key it rejects. Normalised:

    - the time and the process and thread ids, which differ every run;
    - the source function, its signature and its line: mongod's compiler
      decides whether it is `int __config_check_search(WT_SESSION_IMPL *,
      ...), 114` (clang, macOS) or `__config_check_search, 114` (gcc,
      Linux), and this server's vendored WiredTiger (mongodb-7.0.33) names
      it `config_check_search` at line 65;
    - the colon after `configuration key`, which the two WiredTiger
      versions word differently."""
    if answer[0] != "err" or "wiredtiger_config_validate" not in answer[2]:
        return answer
    text = re.sub(r"\[\d+:\d+\]\[\d+:0x[0-9a-f]+\], ", "<time and thread>, ", answer[2])
    text = re.sub(
        r"(wiredtiger_config_validate: \[WT_VERB_DEFAULT\]\[ERROR\]: )"
        r"(?:int )?\w+(?:\(.*?\))?, \d+: ",
        r"\1<function, line>: ",
        text,
    )
    return answer[0], answer[1], re.sub(r"configuration key:? ", "configuration key ", text)


def _plain_options(client: Any) -> dict:
    out: dict = {}
    for key, opts in [
        ({"a": 1}, {"prepareUnique": True}),
        ({"a": 1}, {"weights": {"a": 1}}),
        ({"a": 1}, {"default_language": "english"}),
        ({"a": 1}, {"wildcardProjection": {"a": 1}}),
        ({"a": 1}, {"storageEngine": {"wiredTiger": {"configString": "bogus=1"}}}),
    ]:
        client.drop_database("rm_hashed")
        answer = _created(client, "s", {"key": key, "name": "i1", **opts})
        out[f"{key} {opts}"] = _wiredtiger_text(answer)
    client.drop_database("rm_hashed")
    return out


@pytest.mark.backlog_fixed("RM-771")
def test_create_indexes_validates_its_options_as_mongod_does(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 accepts `prepareUnique` on `createIndexes`, refuses
    `weights` on a key that is not `text` (67), `wildcardProjection` on one
    that is not a wildcard key with 2 BadValue ("The field
    'wildcardProjection' is only allowed in 'wildcard' indexes"), and a
    `storageEngine` option WiredTiger rejects (2, with WiredTiger's own
    text); `default_language` it accepts on any key. This server refused the
    first, took the second and fifth, and worded the fourth's refusal
    otherwise (67)."""
    theirs = _plain_options(mongod_rs)
    assert theirs["{'a': 1} {'prepareUnique': True}"] == ("ok",), theirs
    assert _plain_options(rust_mongo) == theirs


def _conflict(client: Any) -> list:
    client.drop_database("rm_found732")
    db = client.rm_found732
    db.d.create_index([("a", 1)], name="ah")
    out = []
    for spec in (
        {"key": {"a": 1}, "name": "ah", "sparse": True},
        {"sparse": True, "key": {"a": 1}, "name": "ah"},
        {"key": {"a": 1}, "name": "ah", "unique": True},
        {"name": "ah", "unique": True, "key": {"a": 1}},
    ):
        ans = mongo_answer(lambda spec=spec: db.command("createIndexes", "d", indexes=[spec]))
        out.append(ans[2].split("Requested index:")[1] if ans[0] == "err" else ans)
    client.drop_database("rm_found732")
    return out


@pytest.mark.backlog_fixed("RM-772")
def test_an_index_conflict_quotes_the_requested_spec_in_mongods_order(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """The requested spec of an `IndexKeySpecsConflict` is quoted as mongod
    8.2.11 writes it: `v`, then `unique` when it is set, then `key` and
    `name`, then `sparse`. This server wrote `sparse` before `key`."""
    theirs = _conflict(mongod_rs)
    assert "sparse: true }" in theirs[0], theirs
    assert _conflict(rust_mongo) == theirs


def _dbref_equality(client: Any) -> list:
    client.drop_database("rm_found732")
    db = client.rm_found732
    db.c.insert_many([{"_id": i, "a": bson.DBRef("c", i)} for i in range(3)])
    db.c.create_index([("a", 1)], name="a1")
    plan = db.c.find({"a": bson.DBRef("c", 1)}).explain()["queryPlanner"]["winningPlan"]
    out = []
    while plan:
        out.append((plan["stage"], plan.get("indexBounds")))
        plan = plan.get("inputStage")
    out.append([d["_id"] for d in db.c.find({"a": bson.DBRef("c", 1)})])
    client.drop_database("rm_found732")
    return out


@pytest.mark.backlog("RM-773")
def test_an_equality_to_a_dbref_reads_the_index(
    rust_mongo: Any,  # noqa: F811
    mongod_rs: Any,  # noqa: F811
) -> None:
    """mongod 8.2.11 reads an index for `{a: DBRef("c", 1)}`, an equality to
    the document `{$ref: "c", $id: 1}`, with the bounds `[{ $ref: "c", $id:
    1 }, { $ref: "c", $id: 1 }]`. This server takes its `$ref` for an
    operator and plans a COLLSCAN."""
    theirs = _dbref_equality(mongod_rs)
    assert theirs[0][0] == "FETCH", theirs
    assert _dbref_equality(rust_mongo) == theirs
