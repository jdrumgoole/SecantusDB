"""RM-431 and RM-432: required fields mongod's parser finds missing, which
this server answered otherwise (measured 8.2.11, 2026-11-08). RM-431 is
fixed; RM-432 is open."""

from __future__ import annotations

from typing import Any

import pytest
from bson import SON

from tests.backlog.conftest import mongo_answer


def _answer(client: Any, command: list) -> tuple:
    db = client["rm431"]
    db.x.replace_one({"_id": 1}, {"_id": 1}, upsert=True)
    answer = mongo_answer(lambda: db.command(SON(command)))
    return answer if answer[0] == "err" else ("ok",)


_NO_PIPELINE = [
    [("aggregate", "x")],
    [("aggregate", "x"), ("cursor", {})],
    [("aggregate", 1)],
    [("aggregate", 1), ("cursor", {})],
]


@pytest.mark.backlog_fixed("RM-431")
@pytest.mark.parametrize("command", _NO_PIPELINE, ids=[repr(c) for c in _NO_PIPELINE])
def test_an_aggregate_without_a_pipeline_is_refused(
    rust_mongo: Any, mongod: Any, command: list
) -> None:
    """`pipeline` is a required field: mongod refuses an `aggregate` without
    one (40414) before it looks at `cursor`, where this server asked for the
    cursor (9) or, given one, ran an empty pipeline."""
    assert _answer(rust_mongo, command) == _answer(mongod, command)


# mongod's parse is one pass over the fields in DOCUMENT order, the first bad
# field winning; then the missing `pipeline` (40414); then the namespace; then
# the missing `cursor` (9). The server used to ask for the cursor first.
_ORDER = [
    [("aggregate", "x"), ("let", 5)],
    [("aggregate", "x"), ("bogus", 1)],
    [("aggregate", "x"), ("hint", 5)],
    [("aggregate", "x"), ("allowDiskUse", 5)],
    [("aggregate", "x"), ("pipeline", 5)],
    [("aggregate", "x"), ("pipeline", [5])],
    [("aggregate", "x"), ("let", 5), ("bogus", 1)],
    [("aggregate", "x"), ("bogus", 1), ("let", 5)],
    [("aggregate", "x"), ("let", 5), ("explain", 1)],
    [("aggregate", "x"), ("explain", 1), ("let", 5)],
    [("aggregate", "x"), ("bogus", 1), ("pipeline", [5])],
    [("aggregate", "x"), ("pipeline", [5]), ("bogus", 1)],
    [("aggregate", "x"), ("let", None), ("cursor", {})],
    [("aggregate", ""), ("pipeline", [])],
    [("aggregate", 2), ("pipeline", [{}])],
    [("aggregate", "x"), ("pipeline", [{}])],
    [("explain", SON([("aggregate", "x"), ("cursor", {})]))],
]


@pytest.mark.backlog_fixed("RM-431")
@pytest.mark.parametrize("command", _ORDER, ids=[repr(c) for c in _ORDER])
def test_aggregate_checks_its_fields_in_mongods_order(
    rust_mongo: Any, mongod: Any, command: list
) -> None:
    """A malformed `aggregate` gets the error mongod's parse finds first: the
    first bad field in document order, then a missing `pipeline`, then the
    namespace, then a missing `cursor` -- not the cursor error every one of
    these used to get (measured 8.2.11, 2026-11-09)."""
    assert _answer(rust_mongo, command) == _answer(mongod, command)


_EXPLAINED = [
    [("explain", {"insert": "x"})],
    [("explain", {"update": 1})],
    [("explain", {"delete": 1})],
]


@pytest.mark.backlog("RM-432")
@pytest.mark.parametrize("command", _EXPLAINED, ids=[repr(c) for c in _EXPLAINED])
def test_explain_parses_the_write_it_explains(rust_mongo: Any, mongod: Any, command: list) -> None:
    """mongod parses an explained write in full before it explains it, and
    finds a required field missing before the namespace's type: an
    `insert` without `documents` is 40414, not "Cannot explain cmd" (20), and
    `{update: 1}` names its missing `updates`, not the namespace (73)."""
    assert _answer(rust_mongo, command) == _answer(mongod, command)
