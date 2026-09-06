"""What the RUST server's upsert inserts: the document, and its field order.

Two defects, both measured against mongod 8.2.11 on 2026-09-06 and both
Rust-server-only — the Python server was already right on every shape here:

1. **A dotted equality in the filter was stored as a literal dotted key.**
   ``update({"a.b": 5}, {"$set": {...}}, upsert=True)`` inserted a document with
   a key ``"a.b"`` in it, which mongod cannot produce and which does NOT match
   the query that created it. So running the SAME upsert twice inserted TWO
   documents. The idempotent upsert is the canonical use of the feature, so this
   broke it outright. mongod builds the nesting: ``{a: {b: 5}}``.
2. **The fields the update added were not ordered.** mongod emits ``_id`` first,
   then the query-seeded fields, then the update-added ones sorted by name.
   The Rust path had no ordering at all. BSON field order is on the wire and
   drivers compare raw bytes (mongo-php-library's codec tests do).

Not reproduced, deliberately: mongod's order for the QUERY-SEEDED fields is an
internal hash order that varies between runs for identical input (the same query
gave ``[z, c, m]`` on one run and ``[m, c, z]`` on the next three). Both servers
sort them, which is the approximation the Python server already made; the point
of this file is that the two servers now agree with each other and with mongod
on everything else.

Gated on the ``_secantus_server`` extension, like ``test_rust_server_smoke.py``.
"""

from __future__ import annotations

import pytest

_server = pytest.importorskip("_secantus_server")
pymongo = pytest.importorskip("pymongo")


@pytest.fixture(scope="module")
def rs(tmp_path_factory):
    srv = _server.RustServer(str(tmp_path_factory.mktemp("rs_upsert") / "wt"), 0)
    try:
        yield srv
    finally:
        srv.stop()


@pytest.fixture
def db(rs):
    host, port = rs.address
    cli = pymongo.MongoClient(host, port, directConnection=True, serverSelectionTimeoutMS=5000)
    d = cli["upsertfidelity"]
    d.c.drop()
    try:
        yield d
    finally:
        cli.close()


def _upsert(db, q, u):
    return db.command("update", "c", updates=[{"q": q, "u": u, "upsert": True}])


# --- 1. a dotted equality builds the nesting, and stays idempotent ----------


@pytest.mark.parametrize(
    ("filter_", "nested"),
    [
        ({"a.b": 5}, {"a": {"b": 5}}),
        ({"a.b.c": 5}, {"a": {"b": {"c": 5}}}),
        ({"a.b": 1, "a.c": 2}, {"a": {"b": 1, "c": 2}}),
        ({"p": 1, "a.b": 2}, {"p": 1, "a": {"b": 2}}),
    ],
)
def test_a_dotted_upsert_key_builds_the_nesting(db, filter_, nested):
    _upsert(db, filter_, {"$set": {"z": 1}})
    got = db.c.find_one({}, {"_id": 0})
    for k, v in nested.items():
        assert got[k] == v, got
    assert not any("." in k for k in got), f"stored a literal dotted key: {got}"


@pytest.mark.parametrize(
    "filter_",
    [{"a.b": 5}, {"a.b.c": 5}, {"a.b": 1, "a.c": 2}, {"p": 1, "a.b": 2}],
)
def test_the_same_upsert_twice_inserts_one_document(db, filter_):
    """The upserted document must match the query that created it."""
    _upsert(db, filter_, {"$set": {"z": 1}})
    _upsert(db, filter_, {"$set": {"z": 1}})
    assert db.c.count_documents({}) == 1


# --- 2. field order ---------------------------------------------------------


def test_an_operator_upsert_sorts_the_fields_the_update_added(db):
    _upsert(db, {"c": 1}, {"$set": {"z": 2, "a": 3}})
    assert list(db.c.find_one({}).keys()) == ["_id", "c", "a", "z"]


def test_a_replacement_upsert_keeps_the_documents_own_order(db):
    _upsert(db, {"_id": 9}, {"z": 1, "a": 2})
    got = db.c.find_one({"_id": 9})
    assert list(got.keys()) == ["_id", "z", "a"]
    assert got == {"_id": 9, "z": 1, "a": 2}


def test_setoninsert_fields_are_ordered_too(db):
    _upsert(db, {"c": 1}, {"$setOnInsert": {"z": 1, "a": 2}})
    assert list(db.c.find_one({}).keys()) == ["_id", "c", "a", "z"]
