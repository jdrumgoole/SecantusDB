"""A `$lookup` without `from` (RM-413): mongod 8.2.11 runs one whose
pipeline starts with `$documents`, reading no collection, and reads every
`$lookup` -- at any depth -- before it checks any stage's fields: `from`,
then `pipeline`, then, with no `from`, a pipeline that makes its own
documents. Measured on 8.2.11, 2026-11-10.
"""

from __future__ import annotations

from typing import Any

import pytest
from bson import SON
from bson.codec_options import CodecOptions

from tests.mongod_live.conftest import mongo_answer, same

#: A fixed name: the cases' ids carry it, and every xdist worker must
#: collect the same ids. Each worker starts servers of its own.
_DB = "rm413_collectionless_lookup"

DOCS = [{"$documents": [{"b": 1}, {"b": 2}, {"b": 1}]}]

PIPELINES: dict[str, list[dict]] = {
    "documents": [{"$lookup": {"pipeline": DOCS, "as": "z"}}],
    "documents_with_let": [
        {
            "$lookup": {
                "pipeline": [{"$documents": [{"y": "$$a"}, {"y": 9}]}],
                "let": {"a": "$a"},
                "as": "z",
            }
        }
    ],
    "documents_then_match_on_let": [
        {
            "$lookup": {
                "pipeline": [*DOCS, {"$match": {"$expr": {"$eq": ["$b", "$$x"]}}}],
                "let": {"x": "$a"},
                "as": "z",
            }
        }
    ],
    # The join's match comes straight after `$documents`, before the `$limit`.
    "join_fields_then_limit": [
        {
            "$lookup": {
                "pipeline": [*DOCS, {"$limit": 1}],
                "localField": "a",
                "foreignField": "b",
                "as": "z",
            }
        }
    ],
    "join_on_an_array": [
        {
            "$lookup": {
                "pipeline": [{"$documents": [{"b": 1}, {"b": [2, 3]}, {"b": 4}]}],
                "localField": "arr",
                "foreignField": "b",
                "as": "z",
            }
        }
    ],
    "no_documents": [{"$lookup": {"pipeline": [{"$documents": []}], "as": "z"}}],
    "a_collection_lookup_inside": [
        {
            "$lookup": {
                "pipeline": [
                    *DOCS,
                    {"$lookup": {"from": "c", "localField": "b", "foreignField": "a", "as": "w"}},
                ],
                "as": "z",
            }
        }
    ],
    "inside_a_union_with": [
        {"$match": {"_id": 1}},
        {
            "$unionWith": {
                "pipeline": [
                    {"$documents": [{"q": 1}]},
                    {"$lookup": {"pipeline": DOCS, "as": "z"}},
                ]
            }
        },
    ],
    # Refusals, each before the field checks that would otherwise come first.
    "documents_second": [{"$lookup": {"pipeline": [{"$match": {}}, *DOCS], "as": "z"}}],
    "empty_pipeline": [{"$lookup": {"pipeline": [], "as": "z"}}],
    "no_as": [{"$lookup": {"pipeline": [{"$match": {}}]}}],
    "unknown_field": [{"$lookup": {"bogus": 1, "pipeline": [{"$match": {}}], "as": "z"}}],
    "no_pipeline": [{"$lookup": {"bogus": 1}}],
    "pipeline_not_an_array": [{"$lookup": {"bogus": 1, "pipeline": 5}}],
    "pipeline_element_not_a_document": [{"$lookup": {"pipeline": [5], "as": "z"}}],
    "from_null": [{"$lookup": {"from": None, "pipeline": DOCS, "as": "z"}}],
    "from_int_after_an_unknown_field": [{"$lookup": {"bogus": 1, "from": 5, "pipeline": 5}}],
    "from_empty": [{"$lookup": {"bogus": 1, "from": "", "pipeline": DOCS, "as": "z"}}],
    "from_int_after_a_bad_match": [{"$match": 5}, {"$lookup": {"from": 5, "as": "z"}}],
    "from_int_before_an_unknown_stage": [{"$lookup": {"from": 5, "as": "z"}}, {"$bogus": 1}],
    "nested_in_a_lookup": [
        {
            "$lookup": {
                "from": "c",
                "as": "z",
                "pipeline": [{"$lookup": {"pipeline": [], "as": "y"}}],
            }
        }
    ],
    "nested_in_a_facet": [
        {"$facet": {"f": [{"$lookup": {"pipeline": [{"$match": {}}], "as": "y"}}]}}
    ],
    "nested_in_a_union_with": [
        {"$unionWith": {"coll": "c", "pipeline": [{"$lookup": {"pipeline": [], "as": "y"}}]}}
    ],
}


def _seed(client: Any) -> Any:
    client.drop_database(_DB)
    db = client.get_database(_DB, codec_options=CodecOptions(document_class=SON))
    db.c.insert_many([{"_id": 1, "a": 1, "arr": [1, 3]}, {"_id": 2, "a": 2, "arr": []}])
    return db


@pytest.mark.parametrize("case", sorted(PIPELINES))
def test_a_lookup_without_from(rust_mongo: Any, mongod: Any, case: str) -> None:
    def answer(client: Any) -> Any:
        db = _seed(client)
        return mongo_answer(lambda: list(db.c.aggregate(PIPELINES[case])))

    same(answer(rust_mongo), answer(mongod))


def test_aggregate_1_runs_a_lookup_without_from(rust_mongo: Any, mongod: Any) -> None:
    def answer(client: Any) -> Any:
        db = _seed(client)
        return mongo_answer(
            lambda: list(db.aggregate([{"$documents": [{"x": 1}]}, *PIPELINES["documents"]]))
        )

    same(answer(rust_mongo), answer(mongod))


def test_explain_names_what_it_reads(rust_mongo: Any, mongod: Any) -> None:
    """mongod shows the `$lookup` as reading `$cmd.aggregate`, its fields in
    its own order, whatever order they were given in."""

    def answer(client: Any) -> Any:
        db = _seed(client)
        spec = SON(
            [
                ("as", "z"),
                ("pipeline", DOCS),
                ("foreignField", "b"),
                ("localField", "a"),
            ]
        )
        reply = db.command(
            SON(
                [
                    (
                        "explain",
                        SON(
                            [
                                ("aggregate", "c"),
                                ("pipeline", [{"$lookup": spec}]),
                                ("cursor", {}),
                            ]
                        ),
                    ),
                    ("verbosity", "queryPlanner"),
                ]
            )
        )
        return reply["stages"][1]

    same(answer(rust_mongo), answer(mongod))
