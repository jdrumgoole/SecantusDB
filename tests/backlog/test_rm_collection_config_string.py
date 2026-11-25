"""A collection's `storageEngine.wiredTiger.configString` is applied to the
collection's WiredTiger table, as mongod 8.2.11 applies it (measured
2026-11-25): after the table's defaults (`type=file,memory_page_max=10m,
split_pct=90,leaf_value_max=64MB,checksum=on,block_compressor=snappy`) and
before `exclusive=true,key_format=q,app_metadata=(formatVersion=1)` and
`log=(enabled=true)`, so it overrides the defaults and not those. A string
WiredTiger parses badly is refused at `create`, as `==` is.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import mongo_answer, mongod, rust_mongo  # noqa: F401  (fixtures)
from tests.backlog.test_rm_index_config_string import _config, _normal, _se


@pytest.mark.backlog("RM-802")
def test_a_collection_table_is_made_from_its_config_string(rust_mongo: Any, mongod: Any) -> None:  # noqa: F811
    differences = {}
    for config in (
        "block_compressor=zlib,leaf_page_max=8KB",
        "block_compressor=none",
        "value_format=S",
        "key_format=S",
        "app_metadata=(formatVersion=9)",
        "log=(enabled=false)",
        "==",
    ):
        answers = []
        for client in (mongod, rust_mongo):
            db = client["cfg"]
            db.drop_collection("q")
            answer = mongo_answer(
                lambda d=db, c=config: d.create_collection("q", storageEngine=_se(c))
            )
            if answer[0] == "err":
                answers.append(_normal(answer))
                continue
            table = _config(db.command("collStats", "q")["wiredTiger"]["creationString"])
            answers.append(
                {
                    k: table[k]
                    for k in ("block_compressor", "leaf_page_max", "key_format", "value_format")
                }
            )
        if answers[0] != answers[1]:
            differences[config] = {"mongod": answers[0], "secantusd-mdb": answers[1]}
    assert not differences, differences
