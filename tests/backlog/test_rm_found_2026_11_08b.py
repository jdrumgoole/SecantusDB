"""Gap tests for what working RM-215 (`$queryStats`) found on 2026-11-08,
comparing with mongod 8.2.11 (a one-member replica set, macOS arm64).
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_057_084 import mongod_rs  # noqa: F401 - fixture

# ------------------------------------------ a collectionless $lookup --


def _collectionless_lookups(client: Any) -> dict:
    client.drop_database("rm215l")
    db = client.rm215l
    db.c.insert_many([{"_id": 1, "a": 1}, {"_id": 2, "a": 2}])
    return {
        "aggregate_1": mongo_answer(
            lambda: list(
                client.admin.aggregate(
                    [
                        {"$documents": [{"x": 1}]},
                        {
                            "$lookup": {
                                "pipeline": [{"$documents": [{"y": 2}, {"y": 3}]}],
                                "as": "z",
                            }
                        },
                    ]
                )
            )
        ),
        "over_a_collection": mongo_answer(
            lambda: list(
                db.c.aggregate(
                    [
                        {
                            "$lookup": {
                                "pipeline": [{"$documents": [{"y": "$$a"}]}],
                                "let": {"a": "$a"},
                                "as": "z",
                            }
                        }
                    ]
                )
            )
        ),
        "without_documents": mongo_answer(
            lambda: list(db.c.aggregate([{"$lookup": {"pipeline": [{"$match": {}}], "as": "z"}}]))
        ),
    }


@pytest.mark.backlog("RM-413")
def test_a_lookup_without_from_runs_its_documents(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 runs a `$lookup` with no `from` whose pipeline starts
    with `$documents`, and refuses one whose pipeline does not (9, "$lookup
    stage without explicit collection must have a pipeline with $documents
    as first stage"). This server answers 2 "$lookup requires 'from'
    (string)" to both."""
    assert _collectionless_lookups(rust_mongo) == _collectionless_lookups(mongod_rs)


# ------------------------------- a collectionless aggregate's explain --


def _collectionless_explain(client: Any) -> Any:
    def stages() -> list:
        reply = client.admin.command(
            "explain",
            {
                "aggregate": 1,
                "pipeline": [{"$documents": [{"a": 1}, {"a": 2}]}, {"$match": {"a": 1}}],
                "cursor": {},
            },
            verbosity="executionStats",
        )
        # The time each stage took is the machine's, not the server's.
        return [
            {k: v for k, v in s.items() if k != "executionTimeMillisEstimate"}
            for s in reply["stages"]
        ]

    return mongo_answer(stages)


@pytest.mark.backlog("RM-414")
def test_a_collectionless_explain_counts_each_stage(rust_mongo: Any, mongod_rs: Any) -> None:  # noqa: F811
    """mongod 8.2.11 explains `{aggregate: 1}` over `$documents` with each
    stage's `nReturned` (and `executionTimeMillisEstimate`), a `$queue`
    that shows no documents, and the `$match` moved before the
    `$replaceRoot` and rewritten over `_tempDocumentsField`. This server
    gives the stages without counts, `$queue: [{}]`, and the `$match` last
    as written."""
    assert _collectionless_explain(rust_mongo) == _collectionless_explain(mongod_rs)
