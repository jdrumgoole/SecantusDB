"""A numeric path component over an array, e.g. `{"v.0": 1}`.

mongod reads such a component BOTH ways and matches on either:

* the **element at that index** — and having spent the path step on the index,
  it does *not* then re-apply the implicit array traversal, so `{"v.0": 1}`
  does **not** match `{v: [[1, 2]]}` even though `1` is inside `v.0`;
* the **field of that name** in each element, so `{"v.0": 9}` *does* match
  `{v: [{"0": 9}]}`.

Both servers had both halves wrong, in opposite directions: they applied
membership after the index (matching documents mongod does not) and never tried
the field reading (missing documents mongod matches). Either way the query
simply returned the wrong set, with no error.

Measured against mongod 8.2.11 on 2026-09-08 over 22 filters × a 14-document
corpus: **11 of 22 diverged**, now 0.
"""

from __future__ import annotations

import pytest
from pymongo import MongoClient

from secantus import SecantusDBServer

DOCS = [
    {"_id": "nested", "v": [[1, 2]]},
    {"_id": "nested2", "v": [[1, 2], [3, 4]]},
    {"_id": "deep", "v": [[[1]]]},
    {"_id": "flat", "v": [1, 2]},
    {"_id": "flat3", "v": [7, 8, 9]},
    {"_id": "doc0", "v": {"0": 1}},
    {"_id": "doc1", "v": {"1": 5}},
    {"_id": "arrdoc", "v": [{"0": 9}]},
    {"_id": "arrdoc2", "v": [{"0": 9}, {"0": 10}]},
    {"_id": "arrdocmix", "v": [{"0": 1}, {"1": 2}]},
    {"_id": "aad", "v": [[{"k": 1}]]},
    {"_id": "scalar", "v": 1},
    {"_id": "absent", "w": 1},
    {"_id": "docarrval", "v": {"0": [1, 2]}},
]


@pytest.fixture(scope="module")
def coll(tmp_path_factory):
    home = tmp_path_factory.mktemp("pospath") / "wt"
    with SecantusDBServer(port=0, storage_path=str(home)) as srv:
        client = MongoClient(srv.uri, serverSelectionTimeoutMS=5000)
        c = client["pospath"]["c"]
        c.insert_many(DOCS)
        try:
            yield c
        finally:
            client.close()


def ids(coll, flt):
    return sorted(d["_id"] for d in coll.find(flt, {"_id": 1}))


#: Every expectation is mongod 8.2.11's own answer.
CASES = [
    # The INDEX reading, with no membership after it. `nested`/`nested2` have
    # `v.0 == [1, 2]`, which is not equal to 1 or 2 -- these matched before.
    ({"v.0": 1}, ["arrdocmix", "doc0", "docarrval", "flat"]),
    ({"v.0": 2}, ["docarrval"]),
    ({"v.0": 7}, ["flat3"]),
    # ... but whole-array equality against the indexed element still works.
    ({"v.0": [1, 2]}, ["docarrval", "nested", "nested2"]),
    ({"v.1": [3, 4]}, ["nested2"]),
    # The FIELD reading, which was missing entirely.
    ({"v.0": 9}, ["arrdoc", "arrdoc2"]),
    ({"v.1": 2}, ["arrdocmix", "flat"]),
    ({"v.1": 5}, ["doc1"]),
    # Two positional steps: no membership at either.
    ({"v.0.0": 1}, ["arrdocmix", "docarrval", "nested", "nested2"]),
    ({"v.0.1": 2}, ["docarrval", "nested", "nested2"]),
    # A non-numeric component after a positional one still traverses normally.
    ({"v.0.k": 1}, ["aad"]),
    # Operators see the same candidate set.
    ({"v.0": {"$gt": 0}}, ["arrdoc", "arrdoc2", "arrdocmix", "doc0", "docarrval", "flat", "flat3"]),
    (
        {"v.0": {"$lt": 10}},
        ["arrdoc", "arrdoc2", "arrdocmix", "doc0", "docarrval", "flat", "flat3"],
    ),
    (
        {"v.0": {"$type": "int"}},
        ["arrdoc", "arrdoc2", "arrdocmix", "doc0", "docarrval", "flat", "flat3"],
    ),
    ({"v.0": {"$type": "array"}}, ["aad", "deep", "docarrval", "nested", "nested2"]),
    ({"v.0": {"$in": [1, 9]}}, ["arrdoc", "arrdoc2", "arrdocmix", "doc0", "docarrval", "flat"]),
    ({"v.0": {"$size": 2}}, ["docarrval", "nested", "nested2"]),
    ({"v.0": {"$elemMatch": {"$gt": 1}}}, ["docarrval", "nested", "nested2"]),
    (
        {"v.0": {"$exists": True}},
        [
            "aad",
            "arrdoc",
            "arrdoc2",
            "arrdocmix",
            "deep",
            "doc0",
            "docarrval",
            "flat",
            "flat3",
            "nested",
            "nested2",
        ],
    ),
    ({"v.1": {"$exists": True}}, ["arrdoc2", "arrdocmix", "doc1", "flat", "flat3", "nested2"]),
    # The NEGATIVE operators are why the two readings cannot simply be OR-ed:
    # `$ne` is "no candidate equals", over the combined set.
    (
        {"v.0": {"$ne": 1}},
        [
            "aad",
            "absent",
            "arrdoc",
            "arrdoc2",
            "deep",
            "doc1",
            "flat3",
            "nested",
            "nested2",
            "scalar",
        ],
    ),
    # A missing field in one element contributes a MISSING candidate, which
    # matches null.
    ({"v.0": None}, ["absent", "arrdocmix", "doc1", "scalar"]),
]


@pytest.mark.parametrize("flt,expected", CASES, ids=[str(f) for f, _ in CASES])
def test_matches_mongod(coll, flt, expected):
    assert ids(coll, flt) == expected
