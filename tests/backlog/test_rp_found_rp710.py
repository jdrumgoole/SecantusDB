"""Every write path of an array column fitting its elements (RP-710), and
tests of divergences found while working it:

- COPY FROM read a field with no regard to its column's modifier, scalar
  or array: an overlong `varchar(3)` value was stored, a `numeric(4,1)` one
  kept its digits, where PostgreSQL's `CopyFrom` reads each field with its
  input function AT the column's modifier (fixed, RP-718);
- a `char(n)[]` column's elements were stored unpadded, where PostgreSQL's
  element coercion (`bpchar(text, typmod)`, or `bpcharin` at the modifier
  for a literal or a COPY field) pads each one to the width (fixed by
  RP-719).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with each test's setup.
"""

from __future__ import annotations

from collections.abc import Iterator
from decimal import Decimal

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t710",
    "drop table if exists s710",
    "create table t710 (k int primary key, a varchar(3)[], c char(3)[],"
    " v varbit(2)[], n numeric(4,1)[])",
    "create table s710 (x text[], y text)",
    "insert into s710 values (array['abcdef'], 'abcdef'), (array['ab'], 'ab')",
    "insert into t710 (k, a) values (1, array['ab'])",
]
_VARCHAR3 = ("error", "22001", "value too long for type character varying(3)")


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])
        c.execute(_SETUP[1])
        c.execute("drop table if exists cp710")


def _answer(dsn: str, query: str, params: tuple | None = None) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            cur = c.execute(query, params)
            return ("ok", cur.fetchall() if cur.description else None)
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


# `transformAssignedExpr` / `transformAssignmentSubscripts` coerce the value
# to the column's element type and modifier by the assignment rule, on every
# path a row is written by: an INSERT's SELECT, an UPDATE's expression over
# the row, ON CONFLICT DO UPDATE, a subscripted SET, and a bound parameter.
WRITE_PATHS = {
    "insert into t710 (k, a) select 9, x from s710 where y = 'abcdef'": _VARCHAR3,
    "update t710 set a = array[a[1] || 'xyzw']": _VARCHAR3,
    "update t710 set a = array[a[1] || '  '] returning a": ("ok", [(["ab "],)]),
    "insert into t710 (k, a) values (1, array['zz'])"
    " on conflict (k) do update set a = array['abcdef']": _VARCHAR3,
    "insert into t710 (k, a) values (1, array['zz'])"
    " on conflict (k) do update set a = excluded.a || array['qqqq']": _VARCHAR3,
    "update t710 set a[1] = 'abcdef'": _VARCHAR3,
    "update t710 set a[2] = 'abc  ' returning a": ("ok", [(["ab", "abc"],)]),
    "insert into t710 (k, a) values (5, array[['ab', 'abcd']])": _VARCHAR3,
    "insert into t710 (k, a) values (5, array[null, 'abcd'])": _VARCHAR3,
    "insert into t710 (k, a) values (5, '[2:2]={abcd}')": _VARCHAR3,
    "insert into t710 (k, v) values (5, array[B'101'])": (
        "error",
        "22001",
        "bit string too long for type bit varying(2)",
    ),
    "insert into t710 (k, n) values (5, array[1.25, 2.35]) returning n": (
        "ok",
        [([Decimal("1.3"), Decimal("2.4")],)],
    ),
    "insert into t710 (k, n) values (5, array[12345.0])": (
        "error",
        "22003",
        "numeric field overflow",
    ),
}


@pytest.mark.backlog_fixed("RP-710")
@pytest.mark.parametrize("query", list(WRITE_PATHS))
def test_every_write_path_fits_an_array_columns_elements(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == WRITE_PATHS[query]


@pytest.mark.backlog_fixed("RP-710")
@pytest.mark.parametrize("cast", ["", "::text[]"])
def test_a_bound_array_parameter_is_fitted_to_the_column(cast: str, dsn: str) -> None:
    query = f"insert into t710 (k, a) values (6, %s{cast})"
    assert _answer(dsn, query, (["abcdef"],)) == _VARCHAR3


# `CopyFrom` reads each field with `InputFunctionCall(..., typmod)`, the
# column's modifier: `varchar_input` refuses an overlong value (blanks past
# the length drop), `numeric_in` rounds, and `array_in` passes the modifier
# to each element's input function.
COPY_FIELDS = {
    ("s", "abcdef"): _VARCHAR3,
    ("s", "abc   "): ("ok", [("abc",)]),
    ("n", "1.25"): ("ok", [("1.3",)]),
    ("a", "{abcdef}"): _VARCHAR3,
    ("a", '{"abc  "}'): ("ok", [("{abc}",)]),
    ("na", "{1.25}"): ("ok", [("{1.3}",)]),
}


@pytest.mark.backlog_fixed("RP-718")
@pytest.mark.parametrize("column, field", list(COPY_FIELDS))
def test_copy_reads_a_field_at_its_columns_modifier(column: str, field: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists cp710")
        c.execute(
            "create table cp710 (s varchar(3), n numeric(4,1), a varchar(3)[], na numeric(4,1)[])"
        )
        try:
            with c.cursor().copy(f"copy cp710 ({column}) from stdin") as cp:
                cp.write(field + "\n")
            got: object = ("ok", c.execute(f"select {column}::text from cp710").fetchall())
        except psycopg.Error as e:
            got = ("error", e.sqlstate, str(e).splitlines()[0])
    assert got == COPY_FIELDS[(column, field)]


# The other modifiers an input function reads, and the CONTEXT that names
# the field (`CopyFromErrorCallback`): measured on a private, freshly
# initdb'd PostgreSQL 15.19 cluster.
_COPY_MORE_SETUP = (
    "create table cp718 (c char(3), b bit(3), vb varbit(2), ts timestamp(0),"
    " tm time(1), na numeric(4,1)[])"
)
COPY_MORE = {
    ("c", "abcd"): (
        "error",
        "22001",
        "value too long for type character(3)",
        'COPY cp718, line 1, column c: "abcd"',
    ),
    ("b", "1"): (
        "error",
        "22026",
        "bit string length 1 does not match type bit(3)",
        'COPY cp718, line 1, column b: "1"',
    ),
    ("vb", "101"): (
        "error",
        "22001",
        "bit string too long for type bit varying(2)",
        'COPY cp718, line 1, column vb: "101"',
    ),
    ("na", "{12345}"): (
        "error",
        "22003",
        "numeric field overflow",
        'COPY cp718, line 1, column na: "{12345}"',
    ),
    ("ts", "2020-01-01 00:00:00.6"): ("ok", [("2020-01-01 00:00:01",)]),
    ("tm", "10:00:00.26"): ("ok", [("10:00:00.3",)]),
}


def _copy(dsn: str, column: str, binary: bool, write) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists cp718")
        c.execute(
            _COPY_MORE_SETUP if not binary else "create table cp718 (s varchar(3), n numeric(4,1))"
        )
        try:
            fmt = " (format binary)" if binary else ""
            with c.cursor().copy(f"copy cp718 ({column}) from stdin{fmt}") as cp:
                write(cp)
            return ("ok", c.execute(f"select {column}::text from cp718").fetchall())
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0], e.diag.context)
        finally:
            c.execute("drop table if exists cp718")


@pytest.mark.backlog_fixed("RP-718")
@pytest.mark.parametrize("column, field", list(COPY_MORE))
def test_copy_reads_every_modifier_and_names_the_field(column: str, field: str, dsn: str) -> None:
    got = _copy(dsn, column, False, lambda cp: cp.write(field + "\n"))
    assert got == COPY_MORE[(column, field)]


# `CopyReadBinaryAttribute` calls `ReceiveFunctionCall(..., typmod)`, and a
# binary field's CONTEXT names its line and column with no text.
COPY_BINARY = {
    ("s", "text", "abcdef"): (
        "error",
        "22001",
        "value too long for type character varying(3)",
        "COPY cp718, line 1, column s",
    ),
    ("s", "text", "abc  "): ("ok", [("abc",)]),
    ("n", "numeric", Decimal("1.25")): ("ok", [("1.3",)]),
}


@pytest.mark.backlog_fixed("RP-718")
@pytest.mark.parametrize("column, ty, value", list(COPY_BINARY))
def test_binary_copy_reads_a_field_at_its_columns_modifier(
    column: str, ty: str, value: object, dsn: str
) -> None:
    def write(cp: psycopg.Copy) -> None:
        cp.set_types([ty])
        cp.write_row([value])

    assert _copy(dsn, column, True, write) == COPY_BINARY[(column, ty, value)]


# A `char(3)[]` element is padded to the width: `bpchar(text, 7, false)` over
# each element of an ARRAY[], `bpcharin` at the modifier for a literal. The
# elements were stored and shown unpadded until RP-719; the wider matrix is
# in `test_rp_found_rp719.py`.
CHAR_ARRAY = {
    "insert into t710 (k, c) values (5, array['ab']) returning c, octet_length(c[1])": (
        "ok",
        [(["ab "], 3)],
    ),
    "insert into t710 (k, c) values (5, '{ab}') returning c": ("ok", [(["ab "],)]),
}


@pytest.mark.backlog_fixed("RP-719")
@pytest.mark.parametrize("query", list(CHAR_ARRAY))
def test_a_char_n_array_element_is_padded_to_its_width(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == CHAR_ARRAY[query]
