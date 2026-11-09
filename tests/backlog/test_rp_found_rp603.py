"""Gap tests of the divergences found while working RP-603 (`oidvector`'s
ordering operators compared as int arrays): the network types' BETWEEN
compared their text (RP-610, fixed), an array of `oidvector`s ordered its
elements as int arrays (RP-611, fixed: the tests of both are now regression
tests), and `oidvector`'s comparison functions are refused (RP-612).

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


# `network_cmp` orders 10.0.0.10 after 10.0.0.2; text order puts it first.
NETWORK_BETWEEN: dict[str, list[tuple[object, ...]]] = {
    "select '10.0.0.10'::inet between '10.0.0.2' and '10.0.0.20'": [(True,)],
    "select '10.0.0.10'::inet not between '10.0.0.2' and '10.0.0.20'": [(False,)],
    "select '10.0.0.10/32'::cidr between '10.0.0.2/32' and '10.0.0.20/32'": [(True,)],
    "select a::text from (values ('10.0.0.10'::inet), ('9.0.0.1')) s(a) "
    "where a between '10.0.0.2' and '10.0.0.20'": [("10.0.0.10/32",)],
    "select '10.0.0.10'::inet between symmetric '10.0.0.20' and '10.0.0.2'": [(True,)],
    "select '10.0.0.10'::inet not between symmetric '10.0.0.20' and '10.0.0.2'": [(False,)],
}


@pytest.mark.backlog_fixed("RP-610")
@pytest.mark.parametrize("query", list(NETWORK_BETWEEN))
def test_network_between_orders_by_network_cmp(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        assert pg_answer(c, query) == ("ok", NETWORK_BETWEEN[query])


# `array_cmp` compares elements with the element type's B-tree support
# function, `btoidvectorcmp`: the shorter vector first. A NULL element sorts
# after every other, and the array whose elements run out first sorts first.
# `max` / `min` are `array_larger` / `array_smaller`, which go through
# `array_cmp` too.
OIDVECTOR_ARRAYS: dict[str, list[tuple[object, ...]]] = {
    "select array['1 2'::oidvector] < array['3'::oidvector]": [(False,)],
    "select vs < array['3'::oidvector] from (select array['1 2'::oidvector] vs) s": [(False,)],
    "select '1 2'::oidvector < any(vs) from (select array['3'::oidvector] vs) s": [(False,)],
    "select '1 2'::oidvector < all(vs), '1 2'::oidvector >= all(vs) "
    "from (select array['3'::oidvector, '4'] vs) s": [(False, True)],
    "select '{\"1 2\"}'::oidvector[] > '{3}'::oidvector[]": [(True,)],
    "select array['1 2'::oidvector] between array['0'::oidvector] and array['5'::oidvector]": [
        (False,)
    ],
    "select array[array['1 2'::oidvector], array['3'::oidvector]] "
    "< array[array['3'::oidvector], array['1 2'::oidvector]]": [(False,)],
    "select array['1 2'::oidvector, null] < array['1 2'::oidvector, '3']": [(False,)],
    "select '{{3}}'::oidvector[] < '{3,\"1 2\"}'::oidvector[]": [(True,)],
    "select v::text t from (values (array['1 2'::oidvector]), (array['3'::oidvector])) s(v) "
    "order by v": [("{3}",), ('{"1 2"}',)],
    "select v::text t from (values ('1 2'::oidvector), ('3')) s(v) order by v": [
        ("3",),
        ("1 2",),
    ],
    "select greatest(array['1 2'::oidvector], array['3'::oidvector])::text t, "
    "least(array['1 2'::oidvector], array['3'::oidvector])::text u": [('{"1 2"}', "{3}")],
    "select max(v)::text t, min(v)::text u "
    "from (values (array['1 2'::oidvector]), (null), (array['3'::oidvector])) s(v)": [
        ('{"1 2"}', "{3}")
    ],
    "select array_larger(array['1 2'::oidvector], array['3'::oidvector])::text t, "
    "array_smaller(array['1 2'::oidvector], array['3'::oidvector])::text u": [('{"1 2"}', "{3}")],
}


@pytest.mark.backlog_fixed("RP-611")
@pytest.mark.parametrize("query", list(OIDVECTOR_ARRAYS))
def test_oidvector_array_orders_by_btoidvectorcmp(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        assert pg_answer(c, query) == ("ok", OIDVECTOR_ARRAYS[query])


@pytest.mark.backlog("RP-612")
def test_oidvector_comparison_functions(dsn: str) -> None:
    query = (
        "select btoidvectorcmp('1 2', '3'), oidvectorlt('1 2', '3'), oidvectorle('1 2', '3'), "
        "oidvectorgt('1 2', '3'), oidvectorge('1 2', '3'), oidvectoreq('1 2', '1 2'), "
        "oidvectorne('1 2', '3')"
    )
    with psycopg.connect(dsn, autocommit=True) as c:
        assert pg_answer(c, query) == ("ok", [(1, False, False, True, True, True, True)])
