"""Regression tests of the divergences found while working RP-600 (an ARRAY
subquery was typed from its values): RP-575 (an array of `int2vector` or
`oidvector` was a two-dimensional int array).

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


@pytest.mark.backlog_fixed("RP-575")
@pytest.mark.parametrize("query", list(VECTOR_ARRAYS))
def test_vector_array_elements(query: str, dsn: str) -> None:
    type_name, rows = VECTOR_ARRAYS[query]
    with psycopg.connect(dsn, autocommit=True) as c:
        assert pg_answer(c, query) == ("ok", rows)
        assert pg_answer(c, f"select pg_typeof(({query[7:]}))::text") == ("ok", [(type_name,)])


# What reads the elements of such an array -- its shape, a subscript,
# `unnest`, a comparison with each, the array built-ins, `to_json` -- sees
# one vector per element, as PostgreSQL 15.19 does. `array_agg` over vectors
# is `array_agg(anyarray)`: it accumulates them as arrays, so two of
# different lengths are 2202E.
VECTOR_ARRAY_READS: dict[str, tuple] = {
    """select '{"1 2","3"}'::oidvector[]::text""": ("ok", [('{"1 2",3}',)]),
    """select '{{"1 2"},{"3"}}'::int2vector[]::text""": ("ok", [('{{"1 2"},{3}}',)]),
    "select array[''::int2vector]::text": ("ok", [('{""}',)]),
    "select array[array['1 2'::int2vector]]::text": ("ok", [('{{"1 2"}}',)]),
    "select array['1 2'::int2vector, null]::text": ("ok", [('{"1 2",NULL}',)]),
    "select array(select v from (values ('1'::int2vector), ('2 3')) s(v))::text": (
        "ok",
        [('{1,"2 3"}',)],
    ),
    "select array_dims(array['1 2'::int2vector])": ("ok", [("[1:1]",)]),
    "select array_ndims(array['1 2'::int2vector]), cardinality(array['1 2'::int2vector, '3'])": (
        "ok",
        [(1, 2)],
    ),
    "select (array['1 2'::int2vector, '3'])[1]::text": ("ok", [("1 2",)]),
    "select (array['1 2'::int2vector])[1] = '1 2'::int2vector": ("ok", [(True,)]),
    "select ((array['1 2'::int2vector])[1])[0]": ("ok", [(1,)]),
    "select u::text from unnest(array['1 2'::int2vector, '3']) u": ("ok", [("1 2",), ("3",)]),
    "select u = '3'::int2vector from unnest(array['1 2'::int2vector, '3']) u": (
        "ok",
        [(False,), (True,)],
    ),
    "select '3'::int2vector = any(array['1 2'::int2vector, '3'])": ("ok", [(True,)]),
    "select array_position(array['1 2'::int2vector, '3'], '3'::int2vector)": ("ok", [(2,)]),
    "select array_append(array['1 2'::int2vector], '3'::int2vector)::text": (
        "ok",
        [('{"1 2",3}',)],
    ),
    "select array_to_string(array['1 2'::int2vector, '3'], ',')": ("ok", [("1 2,3",)]),
    "select to_json(array['1 2'::int2vector])::text": ("ok", [("[[1,2]]",)]),
    "select array_agg(v) from (values ('1 2'::int2vector), ('3')) s(v)": (
        "err",
        "2202E",
        "cannot accumulate arrays of different dimensionality",
    ),
    "select pg_typeof(array_agg(v))::text from (values ('1 2'::oidvector)) s(v)": (
        "ok",
        [("oidvector",)],
    ),
    # Typed `int2vector` but two-dimensional: `int2vectorout` refuses it.
    "select array_agg(v) from (values ('1 2'::int2vector)) s(v)": (
        "err",
        "42804",
        "array is not a valid int2vector",
    ),
}


@pytest.mark.backlog_fixed("RP-575")
@pytest.mark.parametrize("query", list(VECTOR_ARRAY_READS))
def test_vector_array_reads(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        assert pg_answer(c, query) == VECTOR_ARRAY_READS[query]


def _vector(oid: int, nums: list[int]) -> bytes:
    """`int2vectorsend` / `oidvectorsend`: one dimension, lower bound 0."""
    width = 2 if oid == 21 else 4
    out = (1).to_bytes(4, "big") + (0).to_bytes(4, "big") + oid.to_bytes(4, "big")
    out += len(nums).to_bytes(4, "big") + (0).to_bytes(4, "big")
    for n in nums:
        out += width.to_bytes(4, "big") + n.to_bytes(width, "big")
    return out


# In binary, an array of vectors is `array_send` over each vector's own
# send function, and `array_agg` over vectors sends the accumulated
# two-dimensional array (outer lower bound 1, the vectors' 0). psycopg has
# no binary loader for a vector, so each one comes back as its bytes. The
# bytes were measured on PostgreSQL 15.19.
VECTOR_ARRAYS_BINARY: dict[str, tuple[int, object]] = {
    "select array['1 2'::int2vector, '3']": (1006, [_vector(21, [1, 2]), _vector(21, [3])]),
    """select '{"1 2"}'::oidvector[]""": (1013, [_vector(26, [1, 2])]),
    "select array[''::int2vector, null]": (1006, [_vector(21, []), None]),
    "select array_agg(v) from (values ('1 2'::oidvector), ('3 4')) s(v)": (
        30,
        bytes.fromhex("00000002 00000000 0000001a 00000002 00000001 00000002 00000000")
        + b"".join((4).to_bytes(4, "big") + n.to_bytes(4, "big") for n in (1, 2, 3, 4)),
    ),
}


@pytest.mark.backlog_fixed("RP-575")
@pytest.mark.parametrize("query", list(VECTOR_ARRAYS_BINARY))
def test_vector_array_binary(query: str, dsn: str) -> None:
    oid, value = VECTOR_ARRAYS_BINARY[query]
    with psycopg.connect(dsn, autocommit=True) as c:
        cur = c.cursor(binary=True)
        cur.execute(query)
        assert cur.description is not None
        assert cur.description[0].type_code == oid
        assert cur.fetchall() == [(value,)]
