"""Gap tests of the divergences found while working RP-571 (an ARRAY
subquery was typed from its values): RP-575 (an array of `int2vector` or
`oidvector` is a two-dimensional int array).

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


# An `int2vector` or `oidvector` element of an array is ONE value, written
# as its own space-separated text (`array_out` over `int2vectorout`), so the
# array is one-dimensional: `{"1 2"}`, read back by psycopg as `['1 2']`,
# and two vectors of different lengths are two elements of it.
VECTOR_ARRAYS: dict[str, tuple[str, list[tuple[object, ...]]]] = {
    """select '{"1 2"}'::int2vector[]""": ("int2vector[]", [(["1 2"],)]),
    "select array['1 2'::int2vector]": ("int2vector[]", [(["1 2"],)]),
    "select array(select '1 2'::int2vector)": ("int2vector[]", [(["1 2"],)]),
    "select array['1 2'::oidvector, '3'::oidvector]": ("oidvector[]", [(["1 2", "3"],)]),
    "select array(select '1 2'::oidvector)": ("oidvector[]", [(["1 2"],)]),
}


@pytest.mark.backlog("RP-575")
@pytest.mark.parametrize("query", list(VECTOR_ARRAYS))
def test_vector_array_elements(query: str, dsn: str) -> None:
    type_name, rows = VECTOR_ARRAYS[query]
    with psycopg.connect(dsn, autocommit=True) as c:
        assert pg_answer(c, query) == ("ok", rows)
        assert pg_answer(c, f"select pg_typeof(({query[7:]}))::text") == ("ok", [(type_name,)])
