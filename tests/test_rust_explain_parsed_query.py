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
