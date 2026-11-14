"""Gap tests of the divergences found while working RP-612 (`oidvector`'s
comparison functions were refused): its hash functions and
`oidvectortypes` are refused too (RP-957).

The expected answers were measured on PostgreSQL 15.19; none needs DDL.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.conftest import pg_answer
from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


@pytest.mark.backlog("RP-957")
def test_oidvector_hash_and_types(dsn: str) -> None:
    query = (
        "select hashoidvector('1 2'), hashoidvectorextended('1 2', 0), "
        "oidvectortypes('23 25'), oidvectortypes('')"
    )
    with psycopg.connect(dsn, autocommit=True) as c:
        assert pg_answer(c, query) == (
            "ok",
            [(456323500, 4561117610811519404, "integer, text", "")],
        )
