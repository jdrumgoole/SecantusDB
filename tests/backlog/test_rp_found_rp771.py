"""Gap tests of divergences found while working RP-771 (an array literal
whose upper bound passes `int4` was accepted):

- an array parameter in BINARY format whose upper bound passes `int4` was
  accepted, where `array_recv` runs the same `ArrayCheckBounds` as
  `array_in` and refuses it with 54000 (RP-795, fixed: `binary_recv.rs`'s
  `recv_array` now runs `ArrayGetNItems` and `ArrayCheckBounds` on the
  dimensions before it reads an element, as `array_recv` does).

The expected answers were measured on PostgreSQL 15.19 (the shared
reference; the statement creates nothing).
"""

from __future__ import annotations

import struct
from collections.abc import Iterator

import psycopg
import pytest
from psycopg.adapt import Dumper
from psycopg.pq import Format

from tests.backlog.test_rp_batch26 import _dsn


class _RawArray:
    """A binary `int4[]` wire value, sent as written."""

    def __init__(self, data: bytes) -> None:
        self.data = data


class _RawArrayDumper(Dumper):
    format = Format.BINARY
    oid = 1007  # int4[]

    def dump(self, obj: _RawArray) -> bytes:
        return obj.data


def _int4_array(lower: int, values: list[int]) -> _RawArray:
    """`array_send`'s layout: ndim, has-null flag, element oid, one
    (length, lower bound) pair, then each element length-prefixed."""
    data = struct.pack(">iiI", 1, 0, 23) + struct.pack(">ii", len(values), lower)
    for v in values:
        data += struct.pack(">ii", 4, v)
    return _RawArray(data)


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


# `array_recv` checks each dimension's `lb + length` against `int4`
# (`ArrayCheckBounds`) before it reads an element.
BINARY_BOUNDS = {
    (2147483647, (1,)): ("error", "54000", "array lower bound is too large: 2147483647"),
    (2147483646, (1, 2)): ("error", "54000", "array lower bound is too large: 2147483646"),
    (2147483646, (1,)): ("ok", [("[2147483646:2147483646]={1}",)]),
}

# `ArrayGetNItems` runs first: a negative dimension, or one past
# `MaxArraySize`, is refused before any element is read.
SIZE = "array size exceeds the maximum allowed (134217727)"
BINARY_DIMENSIONS = {
    "past MaxArraySize": ((2147483647,), (1,), ("error", "54000", SIZE)),
    "negative": ((-1,), (1,), ("error", "54000", SIZE)),
    "empty, the bound at int4's top": ((0,), (2147483647,), ("ok", [("{}",)])),
}


def _array(dims: tuple[int, ...], lbs: tuple[int, ...]) -> _RawArray:
    """An `int4[]` header of `dims` and `lbs`, with no element."""
    data = struct.pack(">iiI", len(dims), 0, 23)
    for d, lb in zip(dims, lbs, strict=True):
        data += struct.pack(">ii", d, lb)
    return _RawArray(data)


@pytest.mark.backlog_fixed("RP-795")
@pytest.mark.parametrize("case", list(BINARY_BOUNDS), ids=str)
def test_binary_array_refuses_an_upper_bound_past_int4(
    case: tuple[int, tuple[int, ...]], dsn: str
) -> None:
    lower, values = case
    with psycopg.connect(dsn, autocommit=True) as c:
        c.adapters.register_dumper(_RawArray, _RawArrayDumper)
        try:
            rows = c.execute("select %b::text", [_int4_array(lower, list(values))]).fetchall()
            answer: tuple[object, ...] = ("ok", rows)
        except psycopg.Error as e:
            answer = ("error", e.sqlstate, str(e).splitlines()[0])
    assert answer == BINARY_BOUNDS[case]


@pytest.mark.backlog_fixed("RP-795")
@pytest.mark.parametrize("case", list(BINARY_DIMENSIONS))
def test_binary_array_counts_its_elements_before_reading_one(case: str, dsn: str) -> None:
    dims, lbs, want = BINARY_DIMENSIONS[case]
    with psycopg.connect(dsn, autocommit=True) as c:
        c.adapters.register_dumper(_RawArray, _RawArrayDumper)
        try:
            rows = c.execute("select %b::text", [_array(dims, lbs)]).fetchall()
            answer: tuple[object, ...] = ("ok", rows)
        except psycopg.Error as e:
            answer = ("error", e.sqlstate, str(e).splitlines()[0])
    assert answer == want
