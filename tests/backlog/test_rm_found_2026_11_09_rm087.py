"""Gap tests for the Rust MongoDB server items found on 2026-11-09 while
working RM-087.

Each test asks mongod 8.2.11 live and asserts the Rust server gives the same
answer: the reply, reduced to a value or an error code, and the documents
the write left behind.
"""

from __future__ import annotations

import uuid
from typing import Any

import bson
import pytest
from bson import SON

from tests.backlog.conftest import mongo_answer


def _fresh(client: Any, prefix: str) -> Any:
    db = client[f"{prefix}_{uuid.uuid4().hex[:8]}"]
    db.c.insert_many([{"_id": 1}, {"_id": 2}])
    return db


@pytest.mark.backlog_fixed("RM-461")
def test_a_mistyped_update_statement_applies_none(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 parses every statement of an `update` before it runs
    one: a `multi: 1` in the second refuses the command (14) and the first
    is not applied. The Rust server applied the first, then refused."""

    def run(client: Any) -> tuple:
        db = _fresh(client, "rm461")
        command = SON(
            [
                ("update", "c"),
                (
                    "updates",
                    [
                        {"q": {"_id": 1}, "u": {"$set": {"z": 1}}},
                        {"q": {"_id": 2}, "u": {"$set": {"z": 2}}, "multi": 1},
                    ],
                ),
            ]
        )
        answer = mongo_answer(lambda: db.command(command))
        return answer[:2], list(db.c.find({}, sort=[("_id", 1)]))

    got, ref = run(rust_mongo), run(mongod)
    assert ref == (("err", 14), [{"_id": 1}, {"_id": 2}])
    assert got == ref


@pytest.mark.backlog("RM-460")
def test_a_write_refuses_fields_mongod_does_not_know(rust_mongo: Any, mongod: Any) -> None:
    """mongod 8.2.11 answers 40415 to `originalQuery` and `originalCollation`
    on `insert`, `update` and `delete`, and to a command-level `sampleId` on
    `update`, and writes nothing; the Rust server runs the write."""
    sample = bson.Binary(uuid.UUID(int=5).bytes, 4)

    def run(client: Any) -> tuple:
        db = _fresh(client, "rm460")
        commands = [
            SON([("insert", "c"), ("documents", [{"_id": 3}]), ("originalQuery", {"a": 1})]),
            SON(
                [
                    ("insert", "c"),
                    ("documents", [{"_id": 4}]),
                    ("originalCollation", {"locale": "fr"}),
                ]
            ),
            SON(
                [
                    ("update", "c"),
                    ("updates", [{"q": {"_id": 1}, "u": {"$set": {"a": 1}}}]),
                    ("originalQuery", {"a": 1}),
                ]
            ),
            SON(
                [
                    ("update", "c"),
                    ("updates", [{"q": {"_id": 1}, "u": {"$set": {"b": 1}}}]),
                    ("sampleId", sample),
                ]
            ),
            SON(
                [
                    ("delete", "c"),
                    ("deletes", [{"q": {"_id": 2}, "limit": 1}]),
                    ("originalQuery", {"a": 1}),
                ]
            ),
        ]
        answers = [mongo_answer(lambda c=c: db.command(c))[:2] for c in commands]
        return answers, list(db.c.find({}, sort=[("_id", 1)]))

    got, ref = run(rust_mongo), run(mongod)
    assert ref == ([("err", 40415)] * 5, [{"_id": 1}, {"_id": 2}])
    assert got == ref
