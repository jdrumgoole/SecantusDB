"""Gap tests for Rust MongoDB server items filed during Phase 5 on
2026-10-16 (measured against mongod 8.2.11): the 7.078 notes that were
not yet items, and `showRecordId` beside RM-083.
"""

from __future__ import annotations

import uuid
from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer


def _db(client: Any, prefix: str) -> Any:
    return client[f"{prefix}_{uuid.uuid4().hex[:8]}"]


def _metrics_keys(client: Any) -> list:
    db = _db(client, "rm170")
    db.c.insert_many([{"_id": i} for i in range(3)])
    found = db.command("find", "c", includeQueryStatsMetrics=True)["cursor"]
    aggregated = db.command(
        "aggregate", "c", pipeline=[], cursor={}, includeQueryStatsMetrics=True
    )["cursor"]
    distinct = db.command("distinct", "c", key="_id", includeQueryStatsMetrics=True)
    return [sorted(r.get("metrics", {})) for r in (found, aggregated, distinct)]


@pytest.mark.backlog("RM-170")
def test_include_query_stats_metrics_adds_a_metrics_document(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers `includeQueryStatsMetrics: true` on `find`,
    `aggregate` and `distinct` with a `metrics` document (`keysExamined`,
    `docsExamined`, `cpuNanos`, ...). Its fields are compared, not their
    values, which count work and time."""
    expected = _metrics_keys(mongod)
    assert all(expected)
    assert _metrics_keys(rust_mongo) == expected


def _distinct_multikey(client: Any) -> Any:
    db = _db(client, "rm171")
    db.m.insert_many([{"_id": 1, "a": [8, [9]]}, {"_id": 2, "a": 7}])
    db.m.create_index("a")
    return mongo_answer(lambda: db.command("distinct", "m", key="a")["values"])


@pytest.mark.backlog("RM-171")
def test_distinct_over_a_multikey_index(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers a `distinct` with no query over a multikey index
    from a DISTINCT_SCAN of the index keys, so a value nested as `[8, [9]]`
    gives `9` (its key `[9]`, unwound): `[7, 8, 9]`. Without the index it is
    `[7, 8, [9]]`."""
    expected = _distinct_multikey(mongod)
    assert expected == ("ok", [7, 8, 9])
    assert _distinct_multikey(rust_mongo) == expected


def _distinct_collated(client: Any) -> Any:
    db = _db(client, "rm172")
    db.c.insert_many(
        [
            {"_id": 1, "s": "b"},
            {"_id": 2, "s": "B"},
            {"_id": 3},
            {"_id": 4, "s": None},
            {"_id": 5, "s": "a"},
        ]
    )
    collation = {"locale": "en", "strength": 2}
    db.c.create_index("s", name="s_en", collation=collation)
    return mongo_answer(lambda: db.command("distinct", "c", key="s", collation=collation)["values"])


@pytest.mark.backlog("RM-172")
def test_distinct_through_a_collated_index_leaves_out_null(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers a `distinct` under the collation of an index on
    the key from a fetching DISTINCT_SCAN: one document per index key, the
    first in index order. The null key's first document lacks the field, so
    `null` is left out: `["a", "b"]`."""
    expected = _distinct_collated(mongod)
    assert expected == ("ok", ["a", "b"])
    assert _distinct_collated(rust_mongo) == expected


def _show_record_id(client: Any) -> Any:
    db = _db(client, "rm083")
    db.c.insert_many([{"_id": i} for i in range(1, 5)])
    db.c.delete_one({"_id": 1})
    return [d.get("$recordId") for d in db.c.find({}, show_record_id=True)]


@pytest.mark.backlog("RM-083")
def test_show_record_id_is_the_record_id(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's `showRecordId` is each document's RecordId -- after
    the first of four is deleted, 2, 3, 4 -- not its position in the reply."""
    expected = _show_record_id(mongod)
    assert expected == [2, 3, 4]
    assert _show_record_id(rust_mongo) == expected
