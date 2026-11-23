"""Gap tests found 2026-11-18 while closing RM-613 (a spilled `$median` is
mongod's t-digest): the answer an approximate `$median` gives once mongod's
t-digest interpolates, measured against mongod 8.2.11.
"""

from __future__ import annotations

from typing import Any

import pytest


def _medians(client: Any) -> list:
    client.drop_database("rm613digest")
    db = client.rm613digest
    db.c.insert_many([{"n": (i * 7919) % 30011 * 0.37} for i in range(20_000)])
    out = []
    for limit in (5999, 20_000):
        (row,) = db.c.aggregate(
            [
                {"$sort": {"_id": 1}},
                {"$limit": limit},
                {
                    "$group": {
                        "_id": None,
                        "m": {"$median": {"input": "$n", "method": "approximate"}},
                    }
                },
            ]
        )
        out.append(row["m"])
    return out


@pytest.mark.backlog_fixed("RM-720")
def test_an_approximate_median_is_mongods_tdigest(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers an approximate `$median` from its t-digest,
    interpolating between centroids: 5552.219999999999 over 5999 values and
    5551.609969919785 over 20000. This server ranked the exact values
    (5552.22, 5551.48) until it ported the t-digest."""
    assert _medians(rust_mongo) == _medians(mongod)
