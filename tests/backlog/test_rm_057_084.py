"""Gap tests for `tasks/backlog-rust-mongodb.md`, RM-057 to RM-084.

The reference is mongod 8.2.11, asked live: a standalone (`mongod`) or a
one-member replica set of this module's own (`mongod_rs`) where the item
needs transactions or change streams. The Rust server is `secantusd-rs`
(`rust_mongo`), which always advertises itself as a replica-set primary.
"""

from __future__ import annotations

import contextlib
import re
import shutil
import subprocess
import threading
import time
import uuid
from collections.abc import Iterator
from typing import Any

import pymongo
import pytest
from bson import Timestamp
from pymongo.errors import OperationFailure

from tests.backlog.conftest import _RS_BANNER, RS_BINARY, drain_output, mongo_answer

# ------------------------------------------------------------- fixtures --


@pytest.fixture(scope="module")
def mongod_rs() -> Iterator[Any]:
    """A one-member replica set (mongod 8.2.11): the reference for
    transactions, change streams and `$$CLUSTER_TIME`."""
    from tests.test_mongod_differential import MONGOD, _start_mongod, _stop_mongod

    assert MONGOD, "no mongod on PATH: the reference server is required"
    proc, uri, dbpath = _start_mongod(extra_args=("--replSet", "rs0"))
    hostport = uri.split("//", 1)[1].rstrip("/")
    try:
        direct = pymongo.MongoClient(uri, directConnection=True, serverSelectionTimeoutMS=10_000)
        direct.admin.command(
            "replSetInitiate", {"_id": "rs0", "members": [{"_id": 0, "host": hostport}]}
        )
        deadline = time.monotonic() + 60
        while time.monotonic() < deadline:
            if direct.admin.command("hello").get("isWritablePrimary"):
                break
            time.sleep(0.25)
        else:
            raise AssertionError("mongod replica set did not elect a primary")
        direct.close()
        client = pymongo.MongoClient(
            f"mongodb://{hostport}/?replicaSet=rs0", serverSelectionTimeoutMS=10_000
        )
        yield client
        client.close()
    finally:
        _stop_mongod(proc, dbpath)


@contextlib.contextmanager
def fresh_rust(tmp_path: Any) -> Iterator[Any]:
    """A `secantusd-rs` nobody has written to yet."""
    assert RS_BINARY.exists(), f"{RS_BINARY} is not built"
    proc = subprocess.Popen(
        [str(RS_BINARY), "--port", "0", "--storage-path", str(tmp_path / "store")],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        assert proc.stdout is not None
        m = _RS_BANNER.search(proc.stdout.readline())
        assert m, "secantusd-rs did not start"
        drain_output(proc)
        client = pymongo.MongoClient(
            host=m.group(1), port=int(m.group(2)), serverSelectionTimeoutMS=10_000
        )
        yield client
        client.close()
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
        shutil.rmtree(tmp_path / "store", ignore_errors=True)


def _name() -> str:
    return "c" + uuid.uuid4().hex[:10]


def code_of(answer: tuple) -> tuple:
    """`("ok",)` or `("err", code)`: the outcome without the wording."""
    return answer[:2] if answer[0] == "err" else ("ok",)


def strip_ids(docs: list[dict]) -> list[dict]:
    return [{k: v for k, v in d.items() if k != "_id"} for d in docs]


# ------------------------------------------------- 7.080 plan order (057) --


def _txn_order(client: Any) -> list:
    db = client["rm057"]
    c = db[_name()]
    c.insert_many([{"_id": 10, "a": 5}, {"_id": 11, "a": 0}])
    c.create_index("a")
    with client.start_session() as s, s.start_transaction():
        c.insert_many([{"_id": 1, "a": 3}, {"_id": 2, "a": 1}, {"_id": 3, "a": 2}], session=s)
        c.update_one({"_id": 10}, {"$set": {"a": -1}}, session=s)
        out = [d["_id"] for d in c.find({"a": {"$gte": -5}}, session=s)]
        s.abort_transaction()
    return out


@pytest.mark.backlog_fixed("RM-057")
def test_order_under_a_transactions_own_writes(rust_mongo: Any, mongod_rs: Any) -> None:
    """A `find` inside a transaction over its own uncommitted writes returns
    them in the order of the index the plan reads, as mongod 8.2.11 does."""
    assert _txn_order(rust_mongo) == _txn_order(mongod_rs)


def _capped_order(client: Any) -> list:
    db = client["rm057"]
    name = _name()
    db.create_collection(name, capped=True, size=4096, max=4)
    c = db[name]
    c.create_index("a")
    c.insert_many([{"_id": i, "a": a} for i, a in enumerate([4, 2, 9, 1, 7, 3])])
    return [
        [d["_id"] for d in c.find({"a": {"$gt": 0}})],
        [d["_id"] for d in c.find({"a": {"$lt": 8}}).limit(2)],
        [d["_id"] for d in c.find()],
    ]


@pytest.mark.backlog_fixed("RM-057")
def test_order_on_a_capped_collection(rust_mongo: Any, mongod: Any) -> None:
    """A capped collection with an index: the order a range query returns,
    with and without a limit, and the natural order -- as mongod 8.2.11."""
    assert _capped_order(rust_mongo) == _capped_order(mongod)


# ------------------------------------------ 7.077 update / findAndModify --


def _upsert_order(client: Any, query: dict) -> list:
    c = client["rm059"][_name()]
    c.update_one(query, {"$set": {"x": 1}}, upsert=True)
    return list(c.find_one({}, {"_id": 0}).keys())


@pytest.mark.backlog("RM-059")
def test_upsert_field_order_from_several_equalities(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 does not seed the fields of an upsert from four equalities
    in a fixed order: measured 2026-10-14, one mongod process gave `b, a, c,
    d` on some runs and `d, c, a, b` on others, another `c, a, b, d` and `d,
    b, a, c`, whatever order the query (or an explicit `$and`) wrote them in,
    and never the sorted order this server stores. So this asks that this
    server's order be one the reference produced over eight runs."""
    for q in (
        {"a": 1, "b": 2, "c": 3, "d": 4},
        {"d": 1, "c": 2, "b": 3, "a": 4},
        {"$and": [{"d": 1}, {"c": 2}, {"b": 3}, {"a": 4}]},
    ):
        seen = {tuple(_upsert_order(mongod, q)) for _ in range(8)}
        assert tuple(_upsert_order(rust_mongo, q)) in seen, (q, seen)


def _where_update(client: Any) -> tuple:
    c = client["rm060"][_name()]
    c.insert_many([{"_id": 1, "a": 1}, {"_id": 2, "a": 2}])

    def run() -> tuple:
        r = c.update_many({"$where": "this.a == 1"}, {"$set": {"b": 1}})
        return (r.matched_count, r.modified_count, list(c.find({}, {"_id": 1, "b": 1})))

    return mongo_answer(run)


@pytest.mark.backlog("RM-060")
def test_where_in_an_update_query(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 (with its default JavaScript engine) runs `$where` in an
    update's query and updates the matching document."""
    assert _where_update(rust_mongo) == _where_update(mongod)


def _text_update(client: Any) -> tuple:
    c = client["rm061"][_name()]
    c.insert_one({"_id": 1, "t": "hello"})
    return mongo_answer(lambda: c.update_one({"$text": {"$search": "hello"}}, {"$set": {"b": 1}}))


@pytest.mark.backlog_fixed("RM-061")
def test_text_in_an_update_query_without_a_text_index(rust_mongo: Any) -> None:
    """mongod 8.2.11: `27 IndexNotFound`, `text index required for $text query`."""
    assert _text_update(rust_mongo) == ("err", 27, "text index required for $text query")


def _cluster_time_update(client: Any) -> tuple:
    c = client["rm062"][_name()]
    c.insert_one({"_id": 1})
    ans = mongo_answer(
        lambda: c.update_one({"_id": 1}, [{"$set": {"t": "$$CLUSTER_TIME"}}]).modified_count
    )
    if ans[0] == "err":
        return code_of(ans)
    return (ans, type(c.find_one({"_id": 1}).get("t")).__name__)


@pytest.mark.backlog_fixed("RM-062")
def test_cluster_time_in_a_pipeline_update(rust_mongo: Any, mongod_rs: Any) -> None:
    """On a replica set mongod 8.2.11 sets the field to the operation's
    cluster time, a Timestamp."""
    expected = _cluster_time_update(mongod_rs)
    assert expected == (("ok", 1), Timestamp.__name__)
    assert _cluster_time_update(rust_mongo) == expected


def _replace_with_scalar(client: Any) -> tuple:
    c = client["rm063"][_name()]
    c.insert_one({"_id": 1, "a": 5, "b": {"c": 1}, "d": "x"})
    return mongo_answer(lambda: c.update_one({"_id": 1}, [{"$replaceWith": "$a"}]))


@pytest.mark.backlog_fixed("RM-063")
def test_replace_with_a_non_document_in_an_update(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 prints the whole input document in the 40228 message."""
    assert _replace_with_scalar(rust_mongo) == _replace_with_scalar(mongod)


def _current_date_missing_coll(client: Any) -> tuple:
    c = client["rm065"][_name()]
    return code_of(mongo_answer(lambda: c.update_one({}, {"$currentDate": {"a": "bad"}})))


@pytest.mark.backlog_fixed("RM-065")
def test_current_date_parse_error_on_a_missing_collection(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 reports the `$currentDate` parse error even when the
    collection does not exist."""
    expected = _current_date_missing_coll(mongod)
    assert expected[0] == "err"
    assert _current_date_missing_coll(rust_mongo) == expected


_UNMATCHED_UPDATES = [
    {"$inc": {"a": "x"}},
    {"$mul": {"a": [1]}},
    {"$rename": {"a": 1}},
    {"$rename": {"a.$": "b"}},
    {"$currentDate": {"a": {"x": 1}}},
    {"$bit": {"a": {"nand": 1}}},
    {"$pop": {"a": 2}},
    {"$addToSet": {"a": {"$each": 1}}},
    {"$push": {"a": {"$each": [1], "$sort": 2}}},
    {"$min": {"a": 1}, "$max": {"a": 2}},
    {"$set": {"_id": 5}},
]


def _unmatched_update_errors(client: Any, seed: bool) -> list:
    db = client["rm065"]
    name = _name()
    if seed:
        db[name].insert_one({"z": 1})
    out = []
    for u in _UNMATCHED_UPDATES:
        r = db.command("update", name, updates=[{"q": {"nomatch": 1}, "u": u}], check=False)
        f = db.command("findAndModify", name, query={"nomatch": 1}, update=u, check=False)
        out.append(
            (
                [(e["code"], e["errmsg"]) for e in r.get("writeErrors", [])],
                (f.get("ok"), f.get("code"), f.get("errmsg")),
            )
        )
    return out


@pytest.mark.backlog_fixed("RM-065")
@pytest.mark.parametrize("seed", [False, True], ids=["missing-collection", "no-match"])
def test_update_parse_errors_without_a_match(rust_mongo: Any, mongod: Any, seed: bool) -> None:
    """mongod 8.2.11 reports an update's parse-time operand errors -- `$inc`
    of a string, `$rename` to a number, `$pop: 2`, ... -- whether or not a
    document matches, on `update` and `findAndModify` alike. The Rust server
    reported them only once a document reached the engine."""
    assert _unmatched_update_errors(rust_mongo, seed) == _unmatched_update_errors(mongod, seed)


# ------------------------------------------------------- 7.076 DDL/admin --


@pytest.mark.backlog_fixed("RM-066")
def test_list_databases_always_has_admin_and_config(tmp_path: Any, mongod: Any) -> None:
    """A fresh mongod 8.2.11 lists `admin`, `config` and `local`."""
    assert {"admin", "config", "local"} <= set(mongod.list_database_names())
    with fresh_rust(tmp_path) as client:
        assert {"admin", "config", "local"} <= set(client.list_database_names())


def _unique_conversion_keys(client: Any) -> list:
    db = client["rm067"]
    name = _name()
    c = db[name]
    c.insert_one({"t": 1})
    c.create_index("t", expireAfterSeconds=100)
    db.command("collMod", name, index={"keyPattern": {"t": 1}, "prepareUnique": True})
    db.command("collMod", name, index={"keyPattern": {"t": 1}, "unique": True})
    (ix,) = [i for i in c.list_indexes() if i["name"] == "t_1"]
    return list(ix.keys())


@pytest.mark.backlog_fixed("RM-067")
def test_list_indexes_key_order_after_conversion_to_unique(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 lists `unique` before `expireAfterSeconds`."""
    assert _unique_conversion_keys(rust_mongo) == _unique_conversion_keys(mongod)


@pytest.mark.backlog_fixed("RM-068")
def test_db_stats_reports_the_filesystem(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 reports the filesystem's used and total bytes, both > 0."""
    for client in (mongod, rust_mongo):
        db = client["rm068"]
        db[_name()].insert_one({"a": 1})
        s = db.command("dbStats")
        assert (s.get("fsUsedSize", 0) > 0, s.get("fsTotalSize", 0) > 0) == (True, True)


# ---------------------------------------------------------- 7.075 explain --


def _rejected_plans(client: Any) -> int:
    c = client["rm070"][_name()]
    c.insert_many([{"a": i % 7, "b": i % 5} for i in range(50)])
    c.create_index("a")
    c.create_index("b")
    ex = c.find({"a": 1, "b": 1}).explain()
    return len(ex["queryPlanner"]["rejectedPlans"])


@pytest.mark.backlog_fixed("RM-070")
def test_explain_reports_rejected_plans(rust_mongo: Any, mongod: Any) -> None:
    """With an index on each of two equalities, mongod 8.2.11 reports the
    plans it rejected (2: the other index, and their intersection)."""
    assert _rejected_plans(rust_mongo) == _rejected_plans(mongod)


def _group_explain(client: Any) -> Any:
    db = client["rm071"]
    name = _name()
    db[name].insert_many([{"a": i % 3} for i in range(10)])
    ex = db.command("aggregate", name, pipeline=[{"$group": {"_id": "$a"}}], explain=True)
    return ex.get("explainVersion")


@pytest.mark.backlog("RM-071")
def test_explain_of_a_group_pushed_into_the_query(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 pushes `$group` into a slot-based plan: `explainVersion: "2"`."""
    assert _group_explain(rust_mongo) == _group_explain(mongod)


def _view_count_explain(client: Any) -> Any:
    db = client["rm072"]
    name = _name()
    db[name].insert_many([{"a": i} for i in range(5)])
    db.command("create", name + "_v", viewOn=name, pipeline=[{"$match": {"a": {"$gt": 1}}}])
    ex = db.command("explain", {"count": name + "_v"}, verbosity="queryPlanner")
    return ex.get("explainVersion")


@pytest.mark.backlog("RM-072")
def test_explain_of_count_on_a_view(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 explains `count` on a view as a slot-based plan."""
    assert _view_count_explain(rust_mongo) == _view_count_explain(mongod)


def _expr_false_stage(client: Any) -> Any:
    c = client["rm074"][_name()]
    c.insert_one({"a": 1})
    return c.find({"$expr": False}).explain()["queryPlanner"]["winningPlan"].get("stage")


def _explain_leftovers(client: Any) -> list:
    db = client[f"rm074_{_name()}"]
    db.c.insert_many([{"a": [1, 2]}, {"a": 3}])
    db.c.create_index("a")

    def bounds() -> Any:
        plan = db.command("explain", {"find": "c", "filter": {"a": {"$lt": [5]}}})
        stage = plan["queryPlanner"]["winningPlan"]
        return stage.get("inputStage", stage).get("indexBounds")

    mr = {
        "mapReduce": "c",
        "map": "function(){emit(1,1)}",
        "reduce": "function(k,v){return 1}",
        "out": {"inline": 1},
    }
    return [
        mongo_answer(bounds),
        mongo_answer(lambda: sorted(str(d["a"]) for d in db.c.find({"$where": "this.a == 3"}))),
        mongo_answer(lambda: db.command("explain", mr)["ok"])[:2],
    ]


@pytest.mark.backlog("RM-074")
def test_explain_leftovers(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 bounds a comparison to an array (`[MinKey, [ 5 ]]`),
    runs `$where` in `find`, and explains `mapReduce`."""
    assert _explain_leftovers(rust_mongo) == _explain_leftovers(mongod)


@pytest.mark.backlog_fixed("RM-074")
def test_explain_of_expr_false_is_eof(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 folds `{$expr: false}` and explains it as `EOF`."""
    assert _expr_false_stage(rust_mongo) == _expr_false_stage(mongod) == "EOF"


_EXPR_FILTERS = [
    {"$expr": True},
    {"$expr": 0},
    {"$expr": {"$and": [True, {"$eq": ["$a", 1]}]}},
    {"$expr": {"$or": [False, {"$eq": ["$a", 1]}]}},
    {"$expr": {"$eq": ["$a", 1]}},
    {"$expr": {"$gt": [1, "$a"]}},
    {"$expr": {"$gte": ["$a", None]}},
    {"$expr": {"$lte": ["$a", {"$add": [1, 1]}]}},
    {"$expr": {"$eq": ["$$CURRENT.a", 1]}},
    {"$expr": {"$and": [{"$eq": ["$a", 1]}, {"$lt": ["$b", 4]}]}},
    {"$expr": {"$or": [{"$eq": ["$a", 1]}, {"$eq": ["$a", 2]}]}},
    {"$expr": {"$in": ["$a", [1, 2]]}},
    {"$expr": {"$in": ["$a", [1, None]]}},
    {"$expr": {"$in": ["$a", []]}},
    {"$expr": {"$eq": ["$a", [1]]}},
    {"$expr": {"$eq": ["$a", "$b"]}},
    {"$expr": {"$abs": "$b"}},
    {"b": 2, "$expr": {"$eq": ["$a", 2]}},
    {"b": 2, "$and": [{"$expr": {"$eq": ["$a", 2]}}, {"$expr": "$q"}]},
]


def _expr_plans(client: Any) -> list:
    """`parsedQuery`, the winning plan's stages and filters, and the order of
    `find`, for each filter, over three collections: no index, an index on
    `a`, and a multikey index on `a`."""
    db = client[f"rm074_{_name()}"]
    docs = [{"_id": i, "a": i % 3, "b": i} for i in range(6)]
    db.plain.insert_many(docs)
    db.indexed.insert_many(docs)
    db.indexed.create_index("a")
    db.multikey.insert_many(docs + [{"_id": 9, "a": [1, 2]}])
    db.multikey.create_index("a")

    def shape(node: dict) -> dict:
        out = {k: node[k] for k in ("stage", "filter", "indexBounds") if k in node}
        if "inputStage" in node:
            out["inputStage"] = shape(node["inputStage"])
        return out

    out = []
    for coll in (db.plain, db.indexed, db.multikey):
        for f in _EXPR_FILTERS:
            plan = coll.find(f).explain()["queryPlanner"]
            order = [d["_id"] for d in coll.find(f)]
            out.append((coll.name, f, plan["parsedQuery"], shape(plan["winningPlan"]), order))
    return out


@pytest.mark.backlog_fixed("RM-074")
def test_expr_is_optimised_and_rewritten_for_the_planner(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 optimises `$expr` (a constant folds to `$alwaysFalse` or
    drops out, constants print as `$const`) and derives index-usable twins
    from it (`{a: {$_internalExprEq: 1}}`), which choose the plan and so the
    order `find` returns. The Rust server explained `$expr` as written and
    always scanned the collection."""
    for got, want in zip(_expr_plans(rust_mongo), _expr_plans(mongod), strict=True):
        assert got == want


# --------------------------------------------------- 7.074 change streams --


def _bad_token(client: Any, data: str) -> tuple:
    c = client["rm075"][_name()]
    c.insert_one({"a": 1})

    def run() -> Any:
        with c.watch(resume_after={"_data": data}, max_await_time_ms=100) as s:
            return s.try_next()

    return code_of(mongo_answer(run))


@pytest.mark.backlog_fixed("RM-075")
def test_undecodable_resume_token_errors(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 parses `_data` as a KeyString and reports where it
    failed (15, 50811) rather than one error for every bad token."""
    for data in ("82", "00", "8200000000"):
        assert _bad_token(rust_mongo, data) == _bad_token(mongod_rs, data), data


def _well_formed_foreign_token(client: Any) -> tuple:
    """Resume a collection stream from a token in mongod's own format that
    decodes cleanly (a v2 event token from another deployment)."""
    c = client["rm075"][_name()]
    c.insert_one({"a": 1})
    data = "8200000000000000012B042C0100296E5A1004000102030405060708090A0B0C0D0E0F4600"

    def run() -> Any:
        with c.watch(resume_after={"_data": data}, max_await_time_ms=100) as s:
            return s.try_next()

    return code_of(mongo_answer(run))


@pytest.mark.backlog("RM-075")
def test_well_formed_mongod_token_is_looked_up(rust_mongo: Any, mongod_rs: Any) -> None:
    """A token in mongod's format that decodes: mongod 8.2.11 looks for it
    in its oplog and answers 280 (`the resume token was not found`); this
    server cannot read the format and answers 40647."""
    expected = _well_formed_foreign_token(mongod_rs)
    assert expected == ("err", 280)
    assert _well_formed_foreign_token(rust_mongo) == expected


def _change_stream_explain_ns(client: Any) -> Any:
    db = client["rm076"]
    name = _name()
    db[name].insert_one({"a": 1})
    ex = db.command("aggregate", name, pipeline=[{"$changeStream": {}}], explain=True)
    stages = ex.get("stages") or []
    first = stages[0].get("$cursor", {}) if stages else ex
    return first.get("queryPlanner", {}).get("namespace")


@pytest.mark.backlog("RM-076")
def test_explain_of_a_change_stream_scans_the_oplog(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11 explains a `$changeStream` as a scan of `local.oplog.rs`."""
    expected = _change_stream_explain_ns(mongod_rs)
    assert expected == "local.oplog.rs"
    assert _change_stream_explain_ns(rust_mongo) == expected


def _delete_order(client: Any) -> list:
    c = client["rm077"][_name()]
    c.insert_many([{"_id": 1, "a": 1}, {"_id": 2, "a": 1}])
    with c.watch() as s:
        c.update_many({}, {"$set": {"b": 1}})
        c.delete_many({})
        out = []
        deadline = time.monotonic() + 10
        while len(out) < 4 and time.monotonic() < deadline:
            ev = s.try_next()
            if ev is not None:
                out.append((ev["operationType"], ev["documentKey"]["_id"]))
    return out


@pytest.mark.backlog_fixed("RM-077")
def test_delete_order_after_a_multi_update(rust_mongo: Any, mongod_rs: Any) -> None:
    """The order a `delete` with `limit: 0` deletes in after a `multi`
    update, as seen in the change stream (mongod 8.2.11 reversed it)."""
    assert _delete_order(rust_mongo) == _delete_order(mongod_rs)


def _start_after_drop_database(client: Any) -> Any:
    db = client["rm078_" + uuid.uuid4().hex[:6]]
    token = None
    with db.watch() as s:
        db["c"].insert_one({"a": 1})
        client.drop_database(db.name)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            ev = s.try_next()
            if ev is not None and ev["operationType"] == "dropDatabase":
                token = ev["_id"]
                break
    assert token is not None, "no dropDatabase event"

    def run() -> Any:
        with db.watch(start_after=token) as s2:
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                ev = s2.try_next()
                if ev is not None:
                    return ev["operationType"]
            return None

    return mongo_answer(run)


@pytest.mark.backlog_fixed("RM-078")
def test_start_after_a_drop_database_token(rust_mongo: Any, mongod_rs: Any) -> None:
    """At database scope, `startAfter` the token of a `dropDatabase` event:
    mongod 8.2.11 delivers the `invalidate` again."""
    expected = _start_after_drop_database(mongod_rs)
    assert expected == ("ok", "invalidate")
    assert _start_after_drop_database(rust_mongo) == expected


def _not_found_message(client: Any) -> tuple:
    c = client["rm078"][_name()]
    tokens = {}
    with c.watch() as s:
        c.insert_one({"_id": "k" * 300})
        c.insert_one({"_id": "other"})
        deadline = time.monotonic() + 10
        while len(tokens) < 2 and time.monotonic() < deadline:
            ev = s.try_next()
            if ev is not None:
                tokens[ev["documentKey"]["_id"]] = ev["_id"]["_data"]
    assert len(tokens) == 2, "no insert events"

    def run() -> Any:
        with c.watch(
            [{"$match": {"documentKey._id": "other"}}], resume_after={"_data": tokens["k" * 300]}
        ) as s2:
            return s2.try_next()

    ans = mongo_answer(run)
    if ans[0] != "err":
        return ans
    printed = re.findall(r'_data: "([^"]*)"', ans[2])
    return (ans[1], "the resume token was not found" in ans[2], printed == [tokens["other"]])


@pytest.mark.backlog_fixed("RM-078")
def test_resume_token_not_found_message_names_the_event_found(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11's 280 `cannot resume stream; the resume token was not
    found. {_data: ...}` names the token of the event the stream found in
    the resumed one's place (here the `other` insert its `$match` lets
    through), not the token it was asked to resume from."""
    expected = _not_found_message(mongod_rs)
    assert expected == (280, True, True)
    assert _not_found_message(rust_mongo) == expected


@contextlib.contextmanager
def auth_rust(tmp_path: Any) -> Iterator[tuple]:
    """A fresh `secantusd-rs --auth` with `root`, `alice@admin` and
    `bob@p5b` (password `pw`); yields `(host, port)`."""
    proc = subprocess.Popen(
        [str(RS_BINARY), "--auth", "--port", "0", "--storage-path", str(tmp_path / "auth")],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    try:
        assert proc.stdout is not None
        m = _RS_BANNER.search(proc.stdout.readline())
        assert m, "secantusd-rs did not start"
        drain_output(proc)
        host, port = m.group(1), int(m.group(2))
        local = pymongo.MongoClient(host=host, port=port, directConnection=True)
        local.admin.command("createUser", "root", pwd="pw", roles=["root"])
        root = pymongo.MongoClient(
            host=host, port=port, directConnection=True, username="root", password="pw"
        )
        root.admin.command("createUser", "alice", pwd="pw", roles=["readWriteAnyDatabase"])
        root.p5b.command("createUser", "bob", pwd="pw", roles=[{"role": "readWrite", "db": "p5b"}])
        yield host, port
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=20)
        except subprocess.TimeoutExpired:
            proc.kill()
        shutil.rmtree(tmp_path / "auth", ignore_errors=True)


def _login(host: str, port: int, user: str, db: str) -> Any:
    return pymongo.MongoClient(
        host=host, port=port, directConnection=True, username=user, password="pw", authSource=db
    )


def _txn_event_uid(tmp_path: Any) -> bytes:
    """The `lsid.uid` on a transaction's change event, run by `bob@p5b`."""
    with auth_rust(tmp_path) as (host, port):
        bob = _login(host, port, "bob", "p5b")
        c = bob.p5b[_name()]
        with c.watch() as s:
            with bob.start_session() as sess, sess.start_transaction():
                c.insert_one({"_id": 1}, session=sess)
            deadline = time.monotonic() + 10
            while time.monotonic() < deadline:
                ev = s.try_next()
                if ev is not None:
                    return bytes(ev["lsid"]["uid"])
        raise AssertionError("no change event")


@pytest.mark.backlog_fixed("RM-078")
def test_transaction_event_uid_hashes_the_user(tmp_path: Any) -> None:
    """mongod 8.2.11 (an auth-enabled replica set, measured 2026-10-16)
    stamps a transaction's change events with `lsid.uid` the SHA-256 of
    `user@db`: `bob@p5b` here, `alice@admin` for an admin user."""
    import hashlib

    assert _txn_event_uid(tmp_path) == hashlib.sha256(b"bob@p5b").digest()


def _local_session_uids(tmp_path: Any) -> list:
    """`$listLocalSessions: {}` as `alice@admin`, after `bob@p5b` has used a
    session of his own: whose sessions it lists, by `uid`."""
    import hashlib

    names = {
        hashlib.sha256(b"alice@admin").digest(): "alice",
        hashlib.sha256(b"bob@p5b").digest(): "bob",
        hashlib.sha256(b"root@admin").digest(): "root",
        hashlib.sha256(b"").digest(): "nobody",
    }
    with auth_rust(tmp_path) as (host, port):
        bob = _login(host, port, "bob", "p5b")
        with bob.start_session() as s:
            bob.p5b.x.find_one({}, session=s)
        alice = _login(host, port, "alice", "admin")
        with alice.start_session() as s:
            alice.p5b.x.find_one({}, session=s)
            rows = list(alice.admin.aggregate([{"$listLocalSessions": {}}], session=s))
        return sorted({names.get(bytes(r["_id"]["uid"]), "?") for r in rows})


@pytest.mark.backlog("RM-156")
def test_list_local_sessions_lists_the_callers_by_user_hash(tmp_path: Any) -> None:
    """mongod 8.2.11 (an auth-enabled replica set, measured 2026-10-16):
    `$listLocalSessions: {}` lists only the calling user's sessions, each
    `_id.uid` the SHA-256 of `user@db`."""
    assert _local_session_uids(tmp_path) == ["alice"]


def _pre_image_in_txn(client: Any) -> Any:
    db = client["rm079"]
    name = _name()
    db.create_collection(name, changeStreamPreAndPostImages={"enabled": True})
    c = db[name]
    c.insert_one({"_id": 1, "a": 1})
    with c.watch(full_document_before_change="required") as s:
        with client.start_session() as sess, sess.start_transaction():
            c.update_one({"_id": 1}, {"$set": {"a": 2}}, session=sess)
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            ev = s.try_next()
            if ev is not None:
                return (ev["operationType"], ev.get("fullDocumentBeforeChange"))
    return None


@pytest.mark.backlog_fixed("RM-079")
def test_pre_image_of_an_update_in_a_transaction(rust_mongo: Any, mongod_rs: Any) -> None:
    """`fullDocumentBeforeChange: "required"` on an update made in a
    transaction (not probed before): mongod 8.2.11 delivers the pre-image."""
    expected = _pre_image_in_txn(mongod_rs)
    assert expected == ("update", {"_id": 1, "a": 1})
    assert _pre_image_in_txn(rust_mongo) == expected


def _stream_option(client: Any, option: dict) -> Any:
    db = client["rm079"]
    name = _name()
    c = db[name]
    c.insert_one({"_id": 1, "a": 1})
    reply = mongo_answer(
        lambda: db.command(
            "aggregate", name, pipeline=[{"$changeStream": option}], cursor={"batchSize": 0}
        )
    )
    if reply[0] == "err":
        return code_of(reply)
    cid = reply[1]["cursor"]["id"]
    c.update_one({"_id": 1}, {"$set": {"a": 2}})
    db.create_collection(name + "_b")
    events: list = []
    deadline = time.monotonic() + 5
    while len(events) < 2 and time.monotonic() < deadline:
        more = db.command("getMore", cid, collection=name, maxTimeMS=200)
        events += more["cursor"]["nextBatch"]
    db.command("killCursors", name, cursors=[cid])
    return [(e["operationType"], e.get("fullDocument")) for e in events]


@pytest.mark.backlog("RM-079")
@pytest.mark.parametrize(
    "option",
    [
        {"fullDocument": "updateLookup", "matchCollectionUUIDForUpdateLookup": True},
        {"showSystemEvents": True},
    ],
    ids=["matchCollectionUUIDForUpdateLookup", "showSystemEvents"],
)
def test_change_stream_options_not_probed(rust_mongo: Any, mongod_rs: Any, option: dict) -> None:
    """Two `$changeStream` options the 2026-10-11 probe left out: whether
    mongod 8.2.11 accepts them, and the events an update delivers."""
    assert _stream_option(rust_mongo, option) == _stream_option(mongod_rs, option)


def _expanded_txn_events(client: Any) -> list:
    c = client["rm079"][_name()]
    c.insert_one({"_id": 0})
    out: list = []
    with c.watch(show_expanded_events=True) as s:
        with client.start_session() as sess, sess.start_transaction():
            c.insert_one({"_id": 1}, session=sess)
            c.insert_one({"_id": 2}, session=sess)
        c.insert_one({"_id": 3})
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            ev = s.try_next()
            if ev is not None:
                out.append(ev["operationType"])
                if ev.get("documentKey", {}).get("_id") == 3:
                    break
    return out


@pytest.mark.backlog_fixed("RM-079")
def test_end_of_transaction_with_expanded_events(rust_mongo: Any, mongod_rs: Any) -> None:
    """The events of a two-insert transaction under `showExpandedEvents`
    (`endOfTransaction` was not measured): as mongod 8.2.11 delivers them."""
    assert _expanded_txn_events(rust_mongo) == _expanded_txn_events(mongod_rs)


def _split_events(client: Any) -> list:
    c = client["rm079"][_name()]
    c.insert_one({"_id": 1, "a": "x" * (7 << 20)})
    pipeline = [{"$changeStreamSplitLargeEvent": {}}]
    out: list = []
    with c.watch(pipeline, full_document="updateLookup") as s:
        c.update_one({"_id": 1}, {"$set": {"b": "y" * (7 << 20)}})
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            ev = s.try_next()
            if ev is not None:
                split = ev.get("splitEvent")
                out.append((split, sorted(k for k in ev if k != "splitEvent")))
                if split is None or split["fragment"] == split["of"]:
                    break
    return out


def _resume_from_a_fragment(client: Any) -> list:
    c = client["rm079"][_name()]
    c.insert_one({"_id": 1, "a": "x" * (7 << 20)})
    pipeline = [{"$changeStreamSplitLargeEvent": {}}]
    first = None
    with c.watch(pipeline, full_document="updateLookup") as s:
        c.update_one({"_id": 1}, {"$set": {"b": "y" * (7 << 20)}})
        c.insert_one({"_id": 2})
        deadline = time.monotonic() + 20
        while first is None and time.monotonic() < deadline:
            first = s.try_next()
    assert first is not None and first.get("splitEvent", {}).get("fragment") == 1
    out: list = []
    with c.watch(pipeline, full_document="updateLookup", resume_after=first["_id"]) as s:
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            ev = s.try_next()
            if ev is not None:
                out.append((ev.get("splitEvent"), sorted(k for k in ev if k != "splitEvent")))
                if ev.get("documentKey", {}).get("_id") == 2:
                    break
    return out


@pytest.mark.backlog_fixed("RM-079")
def test_resume_from_a_fragment(rust_mongo: Any, mongod_rs: Any) -> None:
    """Resuming from the first fragment's token gives the fragments after
    it, then the next event, as mongod 8.2.11 does."""
    expected = _resume_from_a_fragment(mongod_rs)
    assert len(expected) > 1
    assert _resume_from_a_fragment(rust_mongo) == expected


@pytest.mark.backlog_fixed("RM-079")
def test_an_event_over_16mb_is_split(rust_mongo: Any, mongod_rs: Any) -> None:
    """An update event over the BSON limit (a 7 MB document, a 7 MB `$set`,
    `fullDocument: updateLookup`) under `$changeStreamSplitLargeEvent`: the
    fragments and the fields each carries, as mongod 8.2.11 splits it."""
    expected = _split_events(mongod_rs)
    assert len(expected) > 1
    assert _split_events(rust_mongo) == expected


# -------------------------------------------------------- 7.073 cursors --


def _cursor_sees_writes(client: Any) -> list:
    c = client["rm080"][_name()]
    c.insert_many([{"_id": i} for i in range(1, 5)])
    cur = c.find().batch_size(1)
    first = next(cur)["_id"]
    c.delete_one({"_id": 4})
    c.insert_one({"_id": 5})
    return [first] + [d["_id"] for d in cur]


@pytest.mark.backlog("RM-080")
def test_get_more_sees_writes_made_since_the_find(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 reads on as the cursor is read: a document deleted
    after the first batch does not appear, one inserted does."""
    assert _cursor_sees_writes(rust_mongo) == _cursor_sees_writes(mongod)


def _index_order(client: Any) -> list:
    c = client["rm081"][_name()]
    c.insert_one({"a": 1, "z": 1, "m": 1})
    c.create_index("z")
    c.create_index("a")
    c.create_index("m")
    return [i["name"] for i in c.list_indexes()]


@pytest.mark.backlog_fixed("RM-081")
def test_list_indexes_in_build_order(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 lists indexes in the order they were built."""
    assert _index_order(rust_mongo) == _index_order(mongod) == ["_id_", "z_1", "a_1", "m_1"]


def _duplicate_key_index(client: Any) -> Any:
    c = client["rm081"][_name()]
    c.insert_one({"_id": 1, "z": 1, "a": 1})
    c.create_index("z", unique=True)
    c.create_index("a", unique=True)
    got = mongo_answer(lambda: c.insert_one({"_id": 2, "z": 1, "a": 1}))
    return re.findall(r"index: (\S+)", str(got))


@pytest.mark.backlog_fixed("RM-081")
def test_duplicate_key_names_the_first_built_unique_index(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 checks unique indexes in build order: a document
    duplicating `z_1` (built first) and `a_1` is refused by `z_1`, not by
    the first in name order."""
    got, want = _duplicate_key_index(rust_mongo), _duplicate_key_index(mongod)
    assert got == want


def _aggregate_after_index_drop(client: Any) -> tuple:
    c = client["rm082"][_name()]
    c.insert_many([{"a": i} for i in range(20)])
    c.create_index("a")
    cur = c.aggregate([{"$match": {"a": {"$gte": 0}}}], batchSize=2, hint="a_1")
    next(cur)
    next(cur)
    c.drop_index("a_1")
    return code_of(mongo_answer(lambda: len(list(cur))))


@pytest.mark.backlog_fixed("RM-082")
def test_aggregate_cursor_after_its_index_is_dropped(rust_mongo: Any, mongod: Any) -> None:
    """What mongod 8.2.11 answers to the `getMore` of an `aggregate` whose
    `$match` reads an index dropped since."""
    assert _aggregate_after_index_drop(rust_mongo) == _aggregate_after_index_drop(mongod)


def _meta_projection(client: Any) -> list:
    c = client["rm083"][_name()]
    c.create_index("a")
    c.insert_one({"_id": 1, "a": 1})
    rid = c.find_one({}, {"r": {"$meta": "recordId"}})
    key = c.find_one({"a": 1}, {"k": {"$meta": "indexKey"}}, hint="a_1")
    return [sorted(rid), sorted(key)]


@pytest.mark.backlog_fixed("RM-083")
def test_meta_record_id_and_index_key_projections(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 adds the record id and the index key."""
    assert _meta_projection(rust_mongo) == _meta_projection(mongod)


def _meta_pipeline(client: Any, pipeline: list, index: dict | None = None) -> tuple:
    """The documents, or the error code (messages are this server's own)."""
    db = client["rm083"]
    name = _name()
    if index:
        db[name].create_index(list(index.items()))
    db[name].insert_many(
        [{"_id": i, "a": 3 - i, "loc": {"type": "Point", "coordinates": [i, 0]}} for i in range(3)]
    )
    command = {"aggregate": name, "pipeline": pipeline, "cursor": {}}
    ans = mongo_answer(lambda: db.command(command)["cursor"]["firstBatch"])
    return ans if ans[0] == "ok" else ans[:2]


@pytest.mark.backlog_fixed("RM-083")
@pytest.mark.parametrize("stage", ["$project", "$addFields"])
def test_meta_sort_key_in_aggregate(rust_mongo: Any, mongod: Any, stage: str) -> None:
    """mongod 8.2.11: `{$meta: "sortKey"}` after a `$sort` on `a` gives the
    key as an array (`[3]`), in `$project` and in `$addFields` alike."""
    pipeline = [{"$sort": {"a": 1}}, {stage: {"k": {"$meta": "sortKey"}}}]
    assert _meta_pipeline(rust_mongo, pipeline) == _meta_pipeline(mongod, pipeline)


@pytest.mark.backlog_fixed("RM-083")
def test_group_keyed_on_meta(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 groups a `$group` keyed on `{$meta: "recordId"}`."""
    pipeline = [{"$group": {"_id": {"$meta": "recordId"}, "n": {"$sum": 1}}}]
    assert _meta_pipeline(rust_mongo, pipeline) == _meta_pipeline(mongod, pipeline)


@pytest.mark.backlog_fixed("RM-083")
def test_meta_geo_near_distance_after_geo_near(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 gives `{$meta: "geoNearDistance"}` after a `$geoNear`."""
    pipeline = [
        {"$geoNear": {"near": {"type": "Point", "coordinates": [0, 0]}, "key": "loc"}},
        {"$project": {"d": {"$meta": "geoNearDistance"}}},
    ]
    index = {"loc": "2dsphere"}
    assert _meta_pipeline(rust_mongo, pipeline, index) == _meta_pipeline(mongod, pipeline, index)


def _dollar_hint_find(client: Any) -> tuple:
    db = client["rm083"]
    name = _name()
    db[name].insert_one({"a": 1})
    return code_of(mongo_answer(lambda: db.command({"find": name, "$hint": {"a": 1}})))


@pytest.mark.backlog_fixed("RM-083")
def test_find_refuses_an_unknown_dollar_field(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 refuses a `$`-prefixed field `find` does not know."""
    expected = _dollar_hint_find(mongod)
    assert expected[0] == "err"
    assert _dollar_hint_find(rust_mongo) == expected


# --------------------------------------------------- 7.072 transactions --


def _drop_under_read_txn(client: Any) -> tuple:
    db = client["rm084"]
    name = _name()
    db[name].insert_one({"_id": 1})
    with client.start_session() as s:
        s.start_transaction()
        db[name].find_one({}, session=s)
        ans = code_of(mongo_answer(lambda: db.command("drop", name, maxTimeMS=500)))
        s.abort_transaction()
    return (ans, name in db.list_collection_names())


@pytest.mark.backlog("RM-084")
def test_drop_waits_for_a_transaction_that_read(rust_mongo: Any, mongod_rs: Any) -> None:
    """mongod 8.2.11: a `drop` of a collection a transaction has read waits,
    answers 50 MaxTimeMSExpired at its `maxTimeMS`, and keeps the collection."""
    expected = _drop_under_read_txn(mongod_rs)
    assert expected == (("err", 50), True)
    assert _drop_under_read_txn(rust_mongo) == expected


def _drop_under_write_txn(client: Any) -> tuple:
    db = client["rm084"]
    name = _name()
    db[name].insert_one({"_id": 1})
    result: list = []
    with client.start_session() as s:
        s.start_transaction()
        db[name].insert_one({"_id": 2}, session=s)
        t = threading.Thread(
            target=lambda: result.append(
                code_of(mongo_answer(lambda: db.command("drop", name, maxTimeMS=500)))
            ),
            daemon=True,
        )
        t.start()
        t.join(timeout=5)
        answered_in_time = not t.is_alive()
        with contextlib.suppress(OperationFailure):
            s.commit_transaction()
        t.join(timeout=10)
    return (answered_in_time, result, name in db.list_collection_names())


@pytest.mark.backlog("RM-084")
def test_drop_under_a_transaction_that_wrote_honours_max_time(
    rust_mongo: Any, mongod_rs: Any
) -> None:
    """mongod 8.2.11 answers 50 at the drop's `maxTimeMS` (here 0.5 s, while
    the transaction is still open) and keeps the collection."""
    expected = _drop_under_write_txn(mongod_rs)
    assert expected == (True, [("err", 50)], True)
    assert _drop_under_write_txn(rust_mongo) == expected
