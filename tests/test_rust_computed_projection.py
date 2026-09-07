"""Computed projections on the RUST server.

`find` and `findAndModify` answered `2 BadValue: projection is not supported by
the Rust server` for every projection whose value was an EXPRESSION rather than
an include/exclude flag -- `{x: "$a"}`, `{x: {$add: [...]}}`, a literal constant.
The engine deferred those shapes to the pure evaluator, and a defer has no
Python behind it on this server, so the refusal reached the client.

Every expectation below is mongod 8.2.11's own answer, measured 2026-09-07 via
`scratchpad/proj_probe.py` (42 shapes, 0 divergences). They are written as
literals rather than compared against the Python engine on purpose: parity with
the other engine is equally satisfied by both being wrong, which has happened in
this codebase before.

Gated on the `_secantus_server` extension, like `test_rust_server_smoke.py`.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

from bson.decimal128 import Decimal128  # noqa: E402

DOC = {
    "_id": 1,
    "a": 2,
    "b": 3,
    "s": "hi",
    "n": {"p": 1, "q": 2},
    "arr": [1, 2, 3],
    "nul": None,
}


@pytest.fixture(scope="module")
def rs(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("rs_proj") / "wt"), 0)
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def db(rs):
    host, port = rs.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    d = cli["projtest"]
    d.c.drop()
    d.c.insert_one(dict(DOC))
    try:
        yield d
    finally:
        cli.close()


# (name, projection, expected document) -- mongod 8.2.11.
CASES = [
    ("literal", {"x": {"$literal": 5}}, {"_id": 1, "x": 5}),
    # `{$literal: 0}` yields the VALUE 0; it is not an exclusion.
    ("literal_zero", {"x": {"$literal": 0}}, {"_id": 1, "x": 0}),
    ("literal_false", {"x": {"$literal": False}}, {"_id": 1, "x": False}),
    ("add", {"x": {"$add": ["$a", "$b"]}}, {"_id": 1, "x": 5}),
    ("concat", {"x": {"$concat": ["$s", "!"]}}, {"_id": 1, "x": "hi!"}),
    ("cond", {"x": {"$cond": [{"$gt": ["$b", "$a"]}, "big", "small"]}}, {"_id": 1, "x": "big"}),
    ("size", {"x": {"$size": "$arr"}}, {"_id": 1, "x": 3}),
    ("rename", {"x": "$a"}, {"_id": 1, "x": 2}),
    ("rename_dotted", {"x": "$n.p"}, {"_id": 1, "x": 1}),
    # A STRING is an expression, never a flag: a non-path string is a literal.
    ("string_literal", {"x": "plain"}, {"_id": 1, "x": "plain"}),
    ("computed_with_inclusion", {"x": {"$add": ["$a", 1]}, "b": 1}, {"_id": 1, "b": 3, "x": 3}),
    ("id_excluded", {"_id": 0, "x": {"$literal": 7}}, {"x": 7}),
    ("computed_id", {"_id": {"$literal": 99}}, {"_id": 99}),
    ("dotted_target", {"o.x": {"$literal": 9}}, {"_id": 1, "o": {"x": 9}}),
    # A sub-document is classified PER LEAF: `p` includes, `z` computes.
    ("leaf_mix", {"n": {"p": 1, "z": "$b"}}, {"_id": 1, "n": {"p": 1, "z": 3}}),
    ("two_computed", {"x": "$a", "y": "$b"}, {"_id": 1, "x": 2, "y": 3}),
    # A `Decimal128` is a BSON number, so it is a FLAG, not a literal.
    ("decimal_includes", {"a": Decimal128("1.5")}, {"_id": 1, "a": 2}),
]


@pytest.mark.parametrize("name,proj,want", CASES, ids=[c[0] for c in CASES])
def test_computed_projection_matches_mongod(db, name, proj, want):
    got = list(db.c.find({}, proj))
    assert got == [want]
    # Field ORDER is what a driver renders, and `==` on a dict cannot see it.
    assert list(got[0]) == list(want)


def test_a_bare_reference_to_a_missing_field_omits_the_key(db):
    """`{x: "$absent"}` gives no `x` at all -- distinct from the next test."""
    assert list(db.c.find({}, {"x": "$nope"})) == [{"_id": 1}]


def test_an_expression_over_a_missing_field_yields_null(db):
    """The rule that looks identical to the one above and is not."""
    assert list(db.c.find({}, {"x": {"$add": ["$nope", 1]}})) == [{"_id": 1, "x": None}]


def test_decimal_zero_excludes(db):
    """`Decimal128("0")` is a falsy flag, so this is an exclusion projection."""
    got = list(db.c.find({}, {"a": Decimal128("0")}))[0]
    assert "a" not in got
    assert got["b"] == 3


def test_false_is_a_flag_not_a_literal(db):
    """`{x: false}` excludes a field that is not there -- the whole doc survives."""
    assert list(db.c.find({}, {"x": False})) == [DOC]


def test_computed_plus_exclusion_is_31254(db):
    """A computed field forces inclusion mode, so `b: 0` is the mix mongod rejects."""
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        list(db.c.find({}, {"x": {"$add": ["$a", 1]}, "b": 0}))
    assert e.value.code == 31254
    assert "Cannot do exclusion on field b in inclusion projection" in str(e.value)


def test_empty_sub_projection_is_51270(db):
    with pytest.raises(pymongo.errors.OperationFailure) as e:
        list(db.c.find({}, {"o": {}}))
    assert e.value.code == 51270
    assert "Invalid empty sub-projection: o" in str(e.value)


def test_find_and_modify_takes_computed_projections_too(db):
    """`findAndModify` shares the engine, and had the same refusal."""
    r = db.command(
        {
            "findAndModify": "c",
            "query": {"_id": 1},
            "update": {"$set": {"t": 1}},
            "fields": {"x": {"$add": ["$a", "$b"]}},
        }
    )
    assert r["value"] == {"_id": 1, "x": 5}


def test_plain_inclusion_and_exclusion_still_work(db):
    """The flag paths this change routes around must be untouched."""
    assert list(db.c.find({}, {"a": 1})) == [{"_id": 1, "a": 2}]
    got = list(db.c.find({}, {"a": 0}))[0]
    assert "a" not in got and got["b"] == 3
