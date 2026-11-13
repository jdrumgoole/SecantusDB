"""Gap tests found 2026-11-13 while closing RM-506 (an SBE sort spills rows
of values), by comparing the profile's sort counts with mongod 8.2.11 over
pipelines the RM-506 test does not reach.

Each test lowers the sort budget on both servers, runs the same SBE sort
(one a `$group` is pushed down behind) over the same documents and compares
what mongod reports.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.test_rm_found_2026_10_26 import _profiled, _spill_db

SORT_BUDGET = {"internalQueryMaxBlockingSortMemoryUsageBytes": 50_000}


def _root_row(client: Any) -> dict:
    db = _spill_db(client)
    entry = _profiled(
        db,
        [{"$sort": {"n": 1}}, {"$group": {"_id": "$k", "f": {"$first": "$$ROOT"}}}],
        SORT_BUDGET,
        "rm506-root-row",
    )
    return {k: entry.get(k) for k in ("sortSpills", "sortSpilledRecords", "sortSpilledBytes")}


@pytest.mark.backlog("RM-615")
def test_a_whole_document_row_keeps_the_fields_read_beside_it(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11's SBE sort in front of a `$group` that reads `$$ROOT`
    and `$k` spills rows of the whole document and `k`: 956700 bytes over
    900 records. This server's row holds the document alone, 5 bytes a
    record fewer (952200)."""
    assert _root_row(rust_mongo) == _root_row(mongod)


def _missing_field(client: Any) -> dict:
    client.drop_database("rm506missing")
    db = client.rm506missing
    db.c.insert_many(
        [
            {"_id": i, "k": i % 7, "s": "x" * 1000, "n": i, **({"t": "ab"} if i % 2 == 0 else {})}
            for i in range(300)
        ]
    )
    entry = _profiled(
        db,
        [
            {"$sort": {"n": 1}},
            {"$group": {"_id": "$k", "f": {"$first": "$t"}, "g": {"$first": "$s"}}},
        ],
        SORT_BUDGET,
        "rm506-missing-field",
    )
    return {k: entry.get(k) for k in ("sortSpills", "sortSpilledBytes", "sortTotalDataSizeBytes")}


@pytest.mark.backlog("RM-616")
def test_a_missing_field_counts_its_slot(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 counts an SBE sort row's slot for a field a document
    lacks (`Nothing`) in the memory the sort holds, as it counts any value
    (`getApproximateSize`: its tag and value, 9 bytes); this server counts
    nothing for it: with `t` on half of 300 documents, mongod reports a
    `sortTotalDataSizeBytes` of 276834, this server 275673 (129 slots of
    9)."""
    assert _missing_field(rust_mongo) == _missing_field(mongod)
