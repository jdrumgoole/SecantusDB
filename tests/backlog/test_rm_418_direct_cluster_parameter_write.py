"""RM-418: a write made to `config.clusterParameters` directly.

mongod 8.2.11 (measured 2026-11-28, a one-member replica set): its op
observer reloads the cluster parameters on a write to
`config.clusterParameters`, so deleting the `querySettings` document
directly lifts a `reject: true` query setting at once: the next `find` of
that shape answers its document. This server (0.5.3-beta.199) lists no
setting after the delete (`$querySettings` reads the collection) but still
rejects the query (code 411) until the next start.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer

_Q = {"find": "a", "filter": {"x": 1}, "$db": "rm418"}


def _find_after_a_direct_delete(client: Any) -> Any:
    db = client["rm418"]
    db.drop_collection("a")
    db.a.insert_many([{"_id": i, "x": i} for i in range(5)])
    client.admin.command({"setQuerySettings": _Q, "settings": {"reject": True}})
    try:
        before = mongo_answer(lambda: list(db.a.find({"x": 1})))
        client["config"]["clusterParameters"].delete_many({"_id": "querySettings"})
        after = mongo_answer(lambda: list(db.a.find({"x": 1})))
    finally:
        for entry in list(client.admin.aggregate([{"$querySettings": {}}])):
            client.admin.command({"removeQuerySettings": entry["queryShapeHash"]})
    return before[:2], after


@pytest.mark.backlog("RM-418")
def test_a_direct_delete_of_the_query_settings_lifts_them(rust_mongo: Any, mongod_rs: Any) -> None:
    expected = _find_after_a_direct_delete(mongod_rs)
    assert expected == (("err", 411), ("ok", [{"_id": 1, "x": 1}])), expected
    assert _find_after_a_direct_delete(rust_mongo) == expected
