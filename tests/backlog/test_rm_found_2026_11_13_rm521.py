"""Gap tests for the sort key in the sub-pipelines of `$unionWith` and
`$facet`, and behind a `$documents`, found working RM-521 (2026-11-13).

Measured against mongod 8.2.11 on the shapes of
`test_rm_found_2026_11_10_rm410.py`, whose documents and ranks they reuse.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer
from tests.backlog.test_rm_found_2026_11_10_rm410 import RANK, _answers

META = {"$addFields": {"m": {"$meta": "sortKey"}}}


@pytest.mark.backlog("RM-624")
def test_a_union_with_sub_pipeline_sort_sets_the_key(rust_mongo: Any, mongod: Any) -> None:
    """In a `$unionWith` pipeline, mongod 8.2.11 ranks by the key the
    sub-pipeline's `$sort` set, and `{$meta: "sortKey"}` reads it; this
    server ties every document and refuses the `$meta` (code 2)."""
    pipelines = {
        "rank": [
            {"$limit": 1},
            {"$unionWith": {"coll": "c", "pipeline": [{"$sort": {"k": 1}}, RANK]}},
        ],
        "$meta": [
            {"$limit": 1},
            {"$unionWith": {"coll": "c", "pipeline": [{"$sort": {"k": 1}}, META]}},
        ],
    }
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog("RM-625")
def test_a_facet_sort_key_is_empty(rust_mongo: Any, mongod: Any) -> None:
    """In a `$facet` pipeline, mongod 8.2.11 gives `{$meta: "sortKey"}` as
    `[]` even behind the facet's own `$sort` (its ranks tie, which this
    server matches); this server refuses the `$meta` (code 2)."""
    pipelines = {"$meta": [{"$facet": {"f": [{"$sort": {"k": 1}}, META]}}]}
    assert _answers(rust_mongo, pipelines) == _answers(mongod, pipelines)


@pytest.mark.backlog("RM-626")
def test_a_sort_behind_documents_sets_no_key(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 refuses a rank behind `$documents` and a `$sort` with
    9973200 "Attempt to get sort key which has not been populated", in a
    collectionless aggregate and in a `$lookup` pipeline alike (cause in
    mongod not traced; RM-523 is the nearest measured shape, a `$sort`
    behind another stage); this server answers both, tied in the first and
    ranked by the `$sort` in the second."""
    docs = {"$documents": [{"n": 2, "k": 1}, {"n": 1, "k": 0}, {"n": 3, "k": 1}]}
    sub = [docs, {"$sort": {"k": 1}}, RANK]

    def code(client: Any) -> dict:
        _answers(client, {})
        db = client.rmfound1110sk
        answers = {
            "collectionless": mongo_answer(lambda: list(db.aggregate(sub))),
            "$lookup": mongo_answer(
                lambda: list(
                    db.c.aggregate([{"$limit": 1}, {"$lookup": {"pipeline": sub, "as": "j"}}])
                )
            ),
        }
        return {k: v[:2] for k, v in answers.items()}

    assert code(rust_mongo) == code(mongod)
