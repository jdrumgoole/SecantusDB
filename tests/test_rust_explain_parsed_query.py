"""`explain`'s `parsedQuery` on the RUST server.

mongod does NOT echo the filter you sent. It echoes the `MatchExpression` tree
**after normalisation**: a bare equality grows an explicit `$eq`, several
top-level fields become an `$and` whose children are sorted by mongod's
internal match-type ordinal, `$ne` becomes `$not`/`$eq`, an `$in` of one
collapses to `$eq`, an `$in` of none becomes `$alwaysFalse`, `$all` splits into
equalities, `$type` becomes numeric BSON codes, and `$comment` disappears.

The Rust server answered with the filter **as sent**, which diverged from
mongod on 44 of the 56 shapes in `tools/probes/explain_shapes.py`. The Python
server matched all 56, because only it had `secantus.explain.canonical_match`;
`secantus-core::explain` is now the port of it, so the two servers agree with
each other and with mongod.

The expectations here are the PYTHON server's answers for the same query, which
`tools/probes/explain_shapes.py` pins against a real mongod — so this file is a
cross-server agreement test and the probe is the oracle. Gated on the
`_secantus_server` extension, like `test_rust_server_smoke.py`.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

from secantus import SecantusDBServer  # noqa: E402

#: Filters whose normalised form differs from the filter as sent. Every one of
#: these came back unnormalised from the Rust server before the port.
FILTERS: list[dict] = [
    {"a": 1},
    {"a": None},
    {"a": {"$eq": 1}},
    {"a": {"$ne": 3}},
    {"a": {"$nin": [1, 2]}},
    {"a": {"$in": [1]}},
    {"a": {"$in": []}},
    {"a": {"$in": [1, 2]}},
    {"a": {"$all": [1, 2]}},
    {"a": {"$all": [1]}},
    {"a": {"$gt": 1}, "b": 2},
    {"a": {"$gt": 3, "$lt": 9}},
    {"a": 1, "b": 2, "c": 3},
    {"$and": [{"a": 1}, {"b": 2}]},
    {"$or": [{"a": 1}, {"b": 2}]},
    {"$or": [{"a": 1}]},
    {"$nor": [{"a": 1}, {"b": 2}]},
    {"$nor": [{"a": 1}]},
    {"$nor": [{"a": 1}], "c": 3},
    {"a": {"$type": "string"}},
    {"a": {"$type": ["string", "int"]}},
    {"a": {"$type": "number"}},
    {"a": {"$exists": True}},
    {"a": {"$size": 2}},
    {"a": {"$mod": [4, 0]}},
    {"a": {"$bitsAllSet": 5}},
    {"a": {"$bitsAnyClear": 3}},
    {"a": {"$regex": "x", "$options": "i"}},
    {"a": {"$elemMatch": {"b": 1}}},
    {"a": {"$elemMatch": {"$gt": 1}}},
    {"a": {"$not": {"$eq": 1}}},
    {"a": 1, "$comment": "ignored"},
    {"a": {"$gte": 1, "$lte": 9}, "z": {"$exists": False}},
    {"nested": {"doc": 1}},
    {},
]


@pytest.fixture(scope="module")
def servers(tmp_path_factory):
    rust = _server.RustServer(str(tmp_path_factory.mktemp("rs_explain") / "wt"), 0)
    py = SecantusDBServer(port=0, storage_path=str(tmp_path_factory.mktemp("py_explain")))
    py.start()
    try:
        yield rust, py
    finally:
        rust.stop()
        py.stop()


@pytest.fixture
def dbs(servers):
    rust, py = servers
    clients = []
    out = []
    for host, port in (rust.address, py.address):
        cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
        clients.append(cli)
        db = cli["expl"]
        db.c.drop()
        db.c.insert_many([{"_id": i, "a": i, "b": str(i)} for i in range(3)])
        out.append(db)
    try:
        yield out[0], out[1]
    finally:
        for cli in clients:
            cli.close()


def _parsed_query(db, filter_: dict):
    reply = db.command({"explain": {"find": "c", "filter": filter_}, "verbosity": "queryPlanner"})
    return reply["queryPlanner"]["parsedQuery"]


@pytest.mark.parametrize("filter_", FILTERS, ids=[str(f) for f in FILTERS])
def test_rust_parsed_query_matches_the_python_server(dbs, filter_: dict) -> None:
    rust_db, py_db = dbs
    assert _parsed_query(rust_db, filter_) == _parsed_query(py_db, filter_)


def test_the_normalisation_actually_happens(dbs) -> None:
    """A guard against both servers agreeing by echoing the raw filter.

    Every assertion above compares the two servers to each other, so it would
    stay green if the port were reverted and both echoed the input. These pin
    the normalised SHAPE itself, measured against mongod 8.2.11.
    """
    rust_db, _ = dbs
    assert _parsed_query(rust_db, {"a": 1}) == {"a": {"$eq": 1}}
    assert _parsed_query(rust_db, {"a": {"$ne": 3}}) == {"a": {"$not": {"$eq": 3}}}
    assert _parsed_query(rust_db, {"a": {"$in": [1]}}) == {"a": {"$eq": 1}}
    assert _parsed_query(rust_db, {"a": {"$in": []}}) == {"$alwaysFalse": 1}
    assert _parsed_query(rust_db, {"a": {"$all": [1, 2]}}) == {
        "$and": [{"a": {"$eq": 1}}, {"a": {"$eq": 2}}]
    }
    assert _parsed_query(rust_db, {"a": {"$type": "string"}}) == {"a": {"$type": [2]}}
    assert _parsed_query(rust_db, {"a": {"$bitsAllSet": 5}}) == {"a": {"$bitsAllSet": [0, 2]}}
    assert _parsed_query(rust_db, {"a": 1, "$comment": "ignored"}) == {"a": {"$eq": 1}}
    # The `$and` children come back in mongod's match-type order, so `b`'s
    # equality precedes `a`'s `$gt` -- not the order they were written in.
    assert _parsed_query(rust_db, {"a": {"$gt": 1}, "b": 2}) == {
        "$and": [{"b": {"$eq": 2}}, {"a": {"$gt": 1}}]
    }


# --------------------------------------------------------------------------
# `winningPlan`'s plan-node FIELDS.
#
# mongod reports a fixed set of keys on each plan node, and a client reads them
# to answer real questions: `isUnique` / `isSparse` / `isPartial` say what the
# index can be trusted for, `multiKeyPaths` says which fields made it multikey,
# and a `FETCH` that carries no `filter` is the signal that the index bounds
# covered the whole predicate. The Rust server emitted four of the nine IXSCAN
# keys, echoed the WHOLE filter on `FETCH` (erasing that signal), omitted
# `direction` from `COLLSCAN`, and never set `isCached`.
#
# Still open and filed: the SORT / SKIP / LIMIT / PROJECTION stage tree, which
# needs `sorted_by_index` plumbed through `ExplainPlan`.
# --------------------------------------------------------------------------

IXSCAN_KEYS = [
    "stage",
    "keyPattern",
    "indexName",
    "isMultiKey",
    "multiKeyPaths",
    "isUnique",
    "isSparse",
    "isPartial",
    "indexVersion",
    "direction",
]


@pytest.fixture
def indexed(dbs):
    rust_db, py_db = dbs
    for db in (rust_db, py_db):
        db.c.create_index([("a", 1)], name="a_1")
        db.c.create_index([("b", 1)], name="b_uniq", unique=True)
        db.c.create_index([("s", 1)], name="s_sparse", sparse=True)
    return rust_db, py_db


def _winning(db, body: dict):
    reply = db.command({"explain": {"find": "c", **body}, "verbosity": "queryPlanner"})
    return reply["queryPlanner"]["winningPlan"]


def test_ixscan_carries_mongods_keys_in_mongods_order(indexed) -> None:
    rust_db, _ = indexed
    plan = _winning(rust_db, {"filter": {"a": 7}, "hint": "a_1"})
    assert list(plan["inputStage"]) == IXSCAN_KEYS


@pytest.mark.parametrize(
    "hint,key,expected",
    [
        ("b_uniq", "isUnique", True),
        ("a_1", "isUnique", False),
        ("s_sparse", "isSparse", True),
        ("a_1", "isSparse", False),
        ("a_1", "isPartial", False),
        ("a_1", "indexVersion", 2),
    ],
)
def test_ixscan_index_flags(indexed, hint: str, key: str, expected: object) -> None:
    rust_db, _ = indexed
    plan = _winning(rust_db, {"filter": {"a": 7}, "hint": hint})
    assert plan["inputStage"][key] == expected


def test_fetch_omits_the_filter_when_the_bounds_cover_it(indexed) -> None:
    """A `FETCH` with no `filter` is how a reader tells a fully-index-served
    query from one that re-checks documents. Echoing the whole filter there
    erased the distinction."""
    rust_db, _ = indexed
    covered = _winning(rust_db, {"filter": {"a": 7}, "hint": "a_1"})
    assert "filter" not in covered
    residual = _winning(rust_db, {"filter": {"a": 7, "other": 1}, "hint": "a_1"})
    assert residual["filter"] == {"other": {"$eq": 1}}


def test_collscan_reports_direction_and_omits_an_empty_filter(dbs) -> None:
    rust_db, _ = dbs
    plain = _winning(rust_db, {"filter": {}})
    assert plain["direction"] == "forward"
    assert "filter" not in plain
    backward = _winning(rust_db, {"filter": {}, "hint": {"$natural": -1}})
    assert backward["direction"] == "backward"


def test_is_cached_is_the_first_key_of_the_outermost_node(dbs) -> None:
    """`isCached` is a whole-plan property, so it sits on the OUTERMOST node
    only — and it is that node's first key."""
    rust_db, _ = dbs
    plan = _winning(rust_db, {"filter": {"a": 1}})
    assert next(iter(plan)) == "isCached"
    assert plan["isCached"] is False


def test_plan_nodes_agree_with_the_python_server(indexed) -> None:
    """The two servers must build the same node, key for key."""
    rust_db, py_db = indexed
    for body in (
        {"filter": {"a": 7}, "hint": "a_1"},
        {"filter": {"b": "7"}, "hint": "b_uniq"},
        {"filter": {"s": 7}, "hint": "s_sparse"},
        {"filter": {"a": 7, "other": 1}, "hint": "a_1"},
        {"filter": {}},
        {"filter": {"nope": 1}},
    ):
        assert _winning(rust_db, body) == _winning(py_db, body), body
