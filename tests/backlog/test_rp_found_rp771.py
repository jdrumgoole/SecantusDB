"""Gap tests of divergences found while working RP-771 (an array literal
whose upper bound passes `int4` was accepted):

- an array parameter in BINARY format whose upper bound passes `int4` is
  accepted, where `array_recv` runs the same `ArrayCheckBounds` as
  `array_in` and refuses it with 54000.

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


@pytest.mark.backlog("RP-795")
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
