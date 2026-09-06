"""A descending sort on the RUST server, over values whose keys nest.

The Rust server sorted documents by `sortkey::encode_value_directed`, which
gives a descending column its order by INVERTING the key bytes. That is the
right thing inside a B-tree, where the storage engine sorts by raw bytes and
there is nowhere to put a direction — but it is not a descending comparator,
because **inversion does not reverse a PREFIX relationship**. `""` encodes to a
strict prefix of `"a"`'s key, and a shorter byte string sorts first both before
and after inversion.

So every prefix chain came out ASCENDING inside a descending result (measured
against mongod 8.2.11, 2026-09-06)::

    values  ["", "a", "ab", "abc", "b"]   sort {x: -1}
    mongod  ["b", "abc", "ab", "a", ""]
    before  ["", "b", "a", "ab", "abc"]

It is fixed by applying direction when the keys are COMPARED rather than by
inverting them: prefix-shorter-first is exactly right ascending, and its reverse
is exactly right descending.

The Python server was never affected — its in-memory sort goes through
`ordering.sort_docs`, not the encoder — but the encoder it shares carries the
same hazard, which is now documented at both definitions.

Gated on the `_secantus_server` extension, like `test_rust_server_smoke.py`.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")

from bson import Binary  # noqa: E402

# Every expectation below is mongod 8.2.11's own answer, re-measured 2026-09-06.
PREFIX_CHAIN = ["", "a", "ab", "abc", "b"]


@pytest.fixture(scope="module")
def rs(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("rs_descsort") / "wt"), 0)
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def db(rs):
    host, port = rs.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    d = cli["descsort"]
    d.c.drop()
    try:
        yield d
    finally:
        cli.close()


def _seed(db, values):
    db.c.insert_many([{"_id": i, "x": v} for i, v in enumerate(values)])


def _ids(db, direction, hint=None):
    cur = db.c.find({}).sort([("x", direction), ("_id", 1)])
    if hint:
        cur = cur.hint(hint)
    return [d["_id"] for d in cur]


def test_a_descending_sort_reverses_a_prefix_chain(db):
    _seed(db, PREFIX_CHAIN)
    assert _ids(db, -1) == [4, 3, 2, 1, 0], "b, abc, ab, a, ''"


def test_the_ascending_order_is_unchanged(db):
    _seed(db, PREFIX_CHAIN)
    assert _ids(db, 1) == [0, 1, 2, 3, 4]


@pytest.mark.parametrize("index_direction", [1, -1])
def test_an_index_does_not_change_the_order(db, index_direction):
    """An index must change speed, never results — in either direction."""
    _seed(db, PREFIX_CHAIN)
    name = f"x_{index_direction}"
    db.c.create_index([("x", index_direction)], name=name)
    assert _ids(db, -1) == [4, 3, 2, 1, 0]
    assert _ids(db, -1, hint=name) == [4, 3, 2, 1, 0]
    assert _ids(db, 1, hint=name) == [0, 1, 2, 3, 4]


def test_range_queries_are_unaffected_by_a_descending_index(db):
    """The stored keys stay inverted; only the comparison changed. Each bound
    must return the same documents with and without the index."""
    _seed(db, ["", "a", "ab", "abc", "b", "ba", "z"])
    queries = [
        {"x": {"$gt": ""}},
        {"x": {"$gte": ""}},
        {"x": {"$gt": "a"}},
        {"x": {"$lt": "ab"}},
        {"x": {"$gt": "ab"}},
        {"x": {"$lte": "a"}},
        {"x": ""},
        {"x": "ab"},
        {"x": {"$in": ["", "ab", "z"]}},
    ]
    before = [sorted(d["_id"] for d in db.c.find(q)) for q in queries]
    db.c.create_index([("x", -1)], name="x_desc")
    after = [sorted(d["_id"] for d in db.c.find(q).hint("x_desc")) for q in queries]
    assert before == after


def test_a_compound_sort_applies_direction_per_field(db):
    db.c.insert_many(
        [
            {"_id": 1, "a": "x", "b": ""},
            {"_id": 2, "a": "x", "b": "z"},
            {"_id": 3, "a": "w", "b": ""},
        ]
    )
    got = [d["_id"] for d in db.c.find({}).sort([("a", 1), ("b", -1)])]
    assert got == [3, 2, 1], "a ascending, then b descending with '' last"


def test_binary_and_empty_array_still_sort_correctly(db):
    """Neighbouring behaviour that was already right, so the fix cannot
    over-reach: BinData is length-then-bytes, and `[]` sits between MinKey and
    null."""
    _seed(db, [Binary(b""), Binary(b"\x02"), Binary(b"\x01\x02"), Binary(b"\x01")])
    assert _ids(db, 1) == [0, 3, 1, 2]
    assert _ids(db, -1) == [2, 1, 3, 0]

    db.c.drop()
    _seed(db, [[], None, "s"])
    assert _ids(db, 1) == [0, 1, 2]
    assert _ids(db, -1) == [2, 1, 0]
