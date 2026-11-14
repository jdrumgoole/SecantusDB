"""Gap test found working RM-417 (2026-11-10): how `setParameter` coerces
a double at the edge of an int parameter's range.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

EDGES = [2147483647.0, 2147483647.9, -2147483648.5]


def _edges(client: Any) -> list:
    out = []
    for value in EDGES:
        out.append(
            mongo_answer(
                lambda v=value: client.admin.command(
                    {"setParameter": 1, "transactionLifetimeLimitSeconds": v}
                )["ok"]
            )
        )
        client.admin.command({"setParameter": 1, "transactionLifetimeLimitSeconds": 60})
    return out


@pytest.mark.backlog_fixed("RM-541")
def test_an_int_parameter_bounds_the_double_before_truncating(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 took `2147483647.0` for an int parameter and refused
    `2147483647.9` and `-2147483648.5` as out of bounds, rather than
    truncating them into range first."""
    assert _edges(rust_mongo) == _edges(mongod)
