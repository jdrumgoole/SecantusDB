"""Gap tests of the divergences found while working RP-575 (an array of
`int2vector` or `oidvector` was a two-dimensional int array): RP-601 (the
array built-ins refuse a vector), RP-602 (a two-dimensional `int2vector`
prints as text), RP-603 (`oidvector`'s `<` compared element by element, fixed),
RP-604 (a parameter declared `int2vector[]` has no type) and RP-605 (an
`int2vector` input's out-of-range error quotes only its number).

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


_ONE_COLUMN_INDEX = "from pg_index where indexrelid = 'pg_class_oid_index'::regclass"

# A vector is a true array type to the polymorphic resolution, subscripted
# from 0.
ARRAY_BUILTINS: dict[str, list[tuple[object, ...]]] = {
    "select cardinality('1 2'::int2vector)": [(2,)],
    "select array_dims('1 2'::int2vector)": [("[0:1]",)],
    "select array_to_string('1 2'::oidvector, ',')": [("1,2",)],
    f"select array_dims(indkey), array_upper(indkey, 1) {_ONE_COLUMN_INDEX}": [("[0:0]", 0)],
    "select cardinality(array_agg(v)) from (values ('1 2'::int2vector)) s(v)": [(2,)],
    "select ('1 2'::int2vector || '3'::int2vector)::text": [("[0:2]={1,2,3}",)],
}


@pytest.mark.backlog("RP-601")
@pytest.mark.parametrize("query", list(ARRAY_BUILTINS))
def test_array_builtins_over_a_vector(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        assert pg_answer(c, query) == ("ok", ARRAY_BUILTINS[query])


# `array_agg` over vectors holds two dimensions under the type `int2vector`,
# which `int2vectorout` refuses wherever the value is printed.
TWO_DIMENSIONAL_TEXT = [
    "select array_agg(v)::text from (values ('1 2'::int2vector)) s(v)",
    "select concat(array_agg(v)) from (values ('1 2'::int2vector)) s(v)",
]


@pytest.mark.backlog("RP-602")
@pytest.mark.parametrize("query", TWO_DIMENSIONAL_TEXT)
def test_two_dimensional_vector_text(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        assert pg_answer(c, query) == ("err", "42804", "array is not a valid int2vector")


@pytest.mark.backlog_fixed("RP-603")
def test_oidvector_orders_by_length_first(dsn: str) -> None:
    """`oidvector`'s `<` and `>` compared its vectors as int arrays, element
    by element, where `btoidvectorcmp` orders the shorter vector first."""
    query = (
        "select '1 2'::oidvector < '3'::oidvector, '3'::oidvector < '1 2'::oidvector, "
        "'1 3'::oidvector > '2 2'::oidvector"
    )
    with psycopg.connect(dsn, autocommit=True) as c:
        assert pg_answer(c, query) == ("ok", [(False, True, False)])


# Each answer measured on PostgreSQL 15.19 over this table: the shorter
# vector first, then oid by oid, each unsigned (4294967295 after 5).
_VECTORS = "(1, '1 2'), (2, '3'), (3, '2 1'), (4, ''), (5, '1 1 1'), (6, '4294967295'), (7, '5')"
OIDVECTOR_COLUMN: dict[str, list[tuple[object, ...]]] = {
    "select k from t603 order by v, k": [(4,), (2,), (7,), (6,), (1,), (3,), (5,)],
    "select k from t603 order by v desc, k": [(5,), (3,), (1,), (6,), (7,), (2,), (4,)],
    "select k from t603 where v < '2 0' order by k": [(1,), (2,), (4,), (6,), (7,)],
    "select k from t603 where v > '3' order by k": [(1,), (3,), (5,), (6,), (7,)],
    "select k from t603 where v between '3' and '1 1' order by k": [(2,), (6,), (7,)],
    "select k from t603 where v not between '3' and '1 1' order by k": [
        (1,),
        (3,),
        (4,),
        (5,),
    ],
    "select k from t603 where '3' < v order by k": [(1,), (3,), (5,), (6,), (7,)],
    "select k, v < '3' from t603 order by k": [
        (1, False),
        (2, False),
        (3, False),
        (4, True),
        (5, False),
        (6, False),
        (7, False),
    ],
    "select k from t603 where v < any(array['2 0'::oidvector]) order by k": [
        (1,),
        (2,),
        (4,),
        (6,),
        (7,),
    ],
    "select k, rank() over (order by v) from t603 order by k": [
        (1, 5),
        (2, 2),
        (3, 6),
        (4, 1),
        (5, 7),
        (6, 4),
        (7, 3),
    ],
    "select greatest(v, '2 0')::text from t603 where k in (3, 5) order by k": [
        ("2 1",),
        ("1 1 1",),
    ],
    "select least(v, '1')::text from t603 where k in (1, 4) order by k": [("1",), ("",)],
    # `max` / `min` are `array_larger` / `array_smaller` over anyarray: an
    # array's order, element by element, on PostgreSQL too.
    "select max(v)::text, min(v)::text from t603": [("4294967295", "")],
}


@pytest.mark.backlog_fixed("RP-603")
@pytest.mark.parametrize("query", list(OIDVECTOR_COLUMN))
def test_oidvector_column_orders_by_length_first(query: str, dsn: str) -> None:
    """A column's sort, comparisons, BETWEEN, ANY and `greatest` / `least`
    ordered as int arrays too, with or without an index on it."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists t603")
        c.execute("create table t603 (k int, v oidvector)")
        c.execute(f"insert into t603 values {_VECTORS}")
        assert pg_answer(c, query) == ("ok", OIDVECTOR_COLUMN[query])
        c.execute("create index on t603 (v)")
        assert pg_answer(c, query) == ("ok", OIDVECTOR_COLUMN[query])


def _int2vector(nums: list[int]) -> bytes:
    """`int2vectorsend`: one dimension, lower bound 0."""
    out = (1).to_bytes(4, "big") + (0).to_bytes(4, "big") + (21).to_bytes(4, "big")
    out += len(nums).to_bytes(4, "big") + (0).to_bytes(4, "big")
    for n in nums:
        out += (2).to_bytes(4, "big") + n.to_bytes(2, "big")
    return out


def _int2vector_array(vectors: list[list[int]]) -> bytes:
    """`array_send` of `int2vector`s."""
    out = (1).to_bytes(4, "big") + (0).to_bytes(4, "big") + (22).to_bytes(4, "big")
    out += len(vectors).to_bytes(4, "big") + (1).to_bytes(4, "big")
    for v in vectors:
        element = _int2vector(v)
        out += len(element).to_bytes(4, "big") + element
    return out


@pytest.mark.backlog("RP-604")
@pytest.mark.parametrize(
    ("value", "fmt"),
    [(b'{"1 2",3}', 0), (_int2vector_array([[1, 2], [3]]), 1)],
    ids=["text", "binary"],
)
def test_vector_array_parameter(value: bytes, fmt: int, dsn: str) -> None:
    expected = {
        b"select pg_typeof($1)::text": b"int2vector[]",
        b"select ($1)[2]::text": b"3",
        b"select cardinality($1)": b"2",
    }
    got = {}
    with psycopg.connect(dsn, autocommit=True) as c:
        for sql in expected:
            r = c.pgconn.exec_params(sql, [value], [1006], [fmt])
            got[sql] = r.get_value(0, 0) if r.ntuples else r.error_message
    assert got == expected


@pytest.mark.backlog("RP-605")
@pytest.mark.parametrize("literal", ["70000 1", "1 70000 1"])
def test_int2vector_range_error_quotes_the_rest(literal: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        assert pg_answer(c, f"select '{literal}'::int2vector") == (
            "err",
            "22003",
            'value "70000 1" is out of range for type smallint',
        )
