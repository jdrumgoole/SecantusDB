"""Gap tests of the divergences found while working RP-522 (a whole row
compared with a record runs `record_eq`'s field checks): RP-529 (a domain
column's whole row was typed by its base type, fixed: a table's row type
records the column under its domain), RP-530 (a record compared
with a scalar is 0A000, not 42883), RP-531 (NULLIF, GREATEST and LEAST over
a whole row, fixed: they compare by `record_eq` / `record_cmp`, and
`select_common_type` types a whole row and a `ROW(...)`) and RP-532 (GROUP
BY a whole row not in the select list).

An answer is the rows, or `(sqlstate, message, hint, position)` for an
error. The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop view if exists vfs",
    "drop table if exists fs, fd, ft, fp, fc cascade",
    "drop domain if exists dint, pint, dc3 cascade",
    "create domain dint as int",
    "create domain pint as int check (value > 0)",
    "create domain dc3 as char(3)",
    "create table fs (a int, c text)",
    "create table fd (a dint)",
    "create table ft (a int, c text)",
    "create table fp (a pint, c text)",
    "create table fc (a dc3, b int)",
    "insert into fs values (1, 'y')",
    "insert into fd values (1)",
    "insert into ft values (1, 'y')",
    "insert into fp values (1, 'x')",
    "insert into fc values ('ab', 1)",
    "create view vfs as select * from fs",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:3]:
            c.execute(q)


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            cur = c.execute(query)
        except psycopg.Error as e:
            return (
                e.sqlstate,
                e.diag.message_primary,
                e.diag.message_hint,
                e.diag.statement_position,
            )
        return cur.fetchall()


# A whole row's field of a domain type is the DOMAIN to `record_eq`: its
# tuple descriptor carries the domain's oid, so a row of the base type is
# 42804 whatever the values (the type is checked before the value). Each
# answered here, as the row type recorded the base type; and a cast to the
# row type skipped the domain's CHECK.
_DINT_INT = (
    "42804",
    "cannot compare dissimilar column types dint and integer at record column 1",
    None,
    None,
)
DOMAIN_FIELD = {
    "select fd = row(1) from fd": (
        "42804",
        "cannot compare dissimilar column types dint and integer at record column 1",
        None,
        None,
    ),
    "select fd = row(2) from fd": (
        "42804",
        "cannot compare dissimilar column types dint and integer at record column 1",
        None,
        None,
    ),
    "select fd = row(1::dint) from fd": [(True,)],
    "select fd < row(2) from fd": _DINT_INT,
    "select fd is distinct from row(1) from fd": _DINT_INT,
    "select fd = any(array[row(1)]) from fd": _DINT_INT,
    "select nullif(fd, row(1)) from fd": _DINT_INT,
    "select row(1)::fd = row(1)": _DINT_INT,
    "select row(1)::fd = fd from fd": [(True,)],
    "select fd = fd from fd": [(True,)],
    "select fp = row(1,'x'::text) from fp": (
        "42804",
        "cannot compare dissimilar column types pint and integer at record column 1",
        None,
        None,
    ),
    "select pg_typeof((fd).a) from fd": [("dint",)],
    "select pg_typeof((fp).a) from fp": [("pint",)],
    "select row(0,'x')::fp": (
        "23514",
        'value for domain pint violates check constraint "pint_check"',
        None,
        None,
    ),
    "select row(1,'x')::fp": [("(1,x)",)],
    # A domain over char(3) still pads its field to the width.
    "select fc from fc": [('("ab ",1)',)],
    "select row('ab', 1)::fc": [('("ab ",1)',)],
    "select '(ab,1)'::fc": [('("ab ",1)',)],
    "select row('abcd', 1)::fc": [("(abc,1)",)],
    "select '(abcd,1)'::fc": ("22001", "value too long for type character(3)", None, "8"),
}


@pytest.mark.backlog_fixed("RP-529")
@pytest.mark.parametrize("query", list(DOMAIN_FIELD))
def test_domain_field_of_a_whole_row(query: str, dsn: str) -> None:
    assert answer(dsn, query) == DOMAIN_FIELD[query]


_NO_OPERATOR_HINT = (
    "No operator matches the given name and argument types. "
    "You might need to add explicit type casts."
)

# A record against a scalar has no operator: 42883 naming both types, at
# the operator.
RECORD_AGAINST_SCALAR = {
    "select fs = (1) from fs": (
        "42883",
        "operator does not exist: fs = integer",
        _NO_OPERATOR_HINT,
        "11",
    ),
    "select (1) = fs from fs": (
        "42883",
        "operator does not exist: integer = fs",
        _NO_OPERATOR_HINT,
        "12",
    ),
    "select fs::text = (1,'y') from fs": (
        "42883",
        "operator does not exist: text = record",
        _NO_OPERATOR_HINT,
        "17",
    ),
}


@pytest.mark.backlog("RP-530")
@pytest.mark.parametrize("query", list(RECORD_AGAINST_SCALAR))
def test_record_against_scalar(query: str, dsn: str) -> None:
    assert answer(dsn, query) == RECORD_AGAINST_SCALAR[query]


# NULLIF compared its arguments with `record_eq`; GREATEST and LEAST
# resolved one common type for their arguments (`select_common_type`) and
# ordered them with `record_cmp`. Each of the four answered `(1,y)`.
RECORD_CONDITIONALS = {
    "select nullif(fs, (1,'y'::text)) from fs": [(None,)],
    "select nullif(fs, row(2,'y')::fs) from fs": [("(1,y)",)],
    "select greatest(fs, (2,'a'::text)) from fs": (
        "42804",
        "GREATEST types fs and record cannot be matched",
        None,
        "21",
    ),
    "select least(fs, row(0,'a')::fs) from fs": [("(0,a)",)],
}


@pytest.mark.backlog_fixed("RP-531")
@pytest.mark.parametrize("query", list(RECORD_CONDITIONALS))
def test_record_conditionals(query: str, dsn: str) -> None:
    assert answer(dsn, query) == RECORD_CONDITIONALS[query]


def _mismatch(label: str, a: str, b: str, position: str) -> tuple:
    return ("42804", f"{label} types {a} and {b} cannot be matched", None, position)


def _no_convert(label: str, a: str, b: str, position: str) -> tuple:
    return ("42846", f"{label} could not convert type {a} to {b}", None, position)


def _dissimilar(a: str, b: str, column: int) -> tuple:
    return (
        "42804",
        f"cannot compare dissimilar column types {a} and {b} at record column {column}",
        None,
        None,
    )


# The rest of the family, measured with RP-531 on the same cluster. A
# table's or a view's whole row is of its own composite type (category C),
# a `ROW(...)` or a subquery's or CTE's whole row of `record` (category P);
# the category pass runs over every part before any is coerced, and a
# composite has no cast to another. A `ROW(...)` cast to a composite is
# reported at its `::`.
RECORD_CONDITIONALS_MORE = {
    "select nullif(fs, (1,'y')) from fs": _dissimilar("text", "unknown", 2),
    "select nullif(fs, (1,2)) from fs": _dissimilar("text", "integer", 2),
    "select nullif(row(1,'y'::text), fs) from fs": [(None,)],
    "select nullif(fs, ft) from fs, ft": [(None,)],
    "select greatest(fs, row(0,'a')::fs) from fs": [("(1,y)",)],
    "select greatest(fs, row(2,null)::fs) from fs": [("(2,)",)],
    "select least(fs, row(1,null)::fs) from fs": [("(1,y)",)],
    "select greatest(fs, null) from fs": [("(1,y)",)],
    "select greatest(row(1,'a'::text), row(2,'a'::text), row(2,'b'::text))": [(("2", "b"),)],
    "select least(row(1,'a'::text), row(2,'a'::text))": [(("1", "a"),)],
    "select greatest(row(1,'a'::text), row(1,2))": _dissimilar("text", "integer", 2),
    "select greatest(row(1,'a'::text), row(1))": (
        "42804",
        "cannot compare record types with different numbers of columns",
        None,
        None,
    ),
    "select greatest(row(1), 1)": _mismatch("GREATEST", "record", "integer", "25"),
    "select greatest(fs, 1) from fs": _mismatch("GREATEST", "fs", "integer", "21"),
    "select greatest(1, fs) from fs": _mismatch("GREATEST", "integer", "fs", "20"),
    "select greatest(t, row(0,'a')::fs) from fs t": [("(1,y)",)],
    "select greatest(fs.*, (0,'a'::text)) from fs": _mismatch("GREATEST", "fs", "record", "23"),
    "select greatest(fs, ft) from fs, ft": _no_convert("GREATEST", "ft", "fs", "21"),
    "select greatest(fs, ft, 1) from fs, ft": _mismatch("GREATEST", "fs", "integer", "25"),
    "select greatest(fs, row(0,'a')::fs, (1,'b'::text)) from fs": _mismatch(
        "GREATEST", "fs", "record", "37"
    ),
    "select greatest(null, fs, (1,'b'::text)) from fs": _mismatch("GREATEST", "fs", "record", "27"),
    "select greatest((1,'b'::text), fs) from fs": _mismatch("GREATEST", "record", "fs", "32"),
    "select coalesce(fs, (2,'a'::text)) from fs": _mismatch("COALESCE", "fs", "record", "21"),
    "select coalesce(fs, row(2,'a')::fs) from fs": [("(1,y)",)],
    "select coalesce(fs, ft) from fs, ft": _no_convert("COALESCE", "ft", "fs", "21"),
    "select case when true then fs else (2,'a'::text) end from fs": _mismatch(
        "CASE", "record", "fs", "28"
    ),
    "select case when false then fs when true then (2,'a'::text) end from fs": (
        _mismatch("CASE", "fs", "record", "47")
    ),
    "select greatest(vfs, row(0,'a')::fs) from vfs": _no_convert("GREATEST", "fs", "vfs", "32"),
    "select greatest(vfs, (0,'a'::text)) from vfs": _mismatch("GREATEST", "vfs", "record", "22"),
    "select greatest(s, row(0,'a')::fs) from (select 1 a, 'x'::text c) s": (
        _mismatch("GREATEST", "record", "fs", "30")
    ),
    "with w as (select * from fs) select greatest(w, row(0,'a')::fs) from w": (
        _mismatch("GREATEST", "record", "fs", "59")
    ),
    "select greatest(row(1,'a'::text)::fs, (2,'a'))": _mismatch("GREATEST", "fs", "record", "39"),
    "select least(fs, (2,'a')::fs) from fs": [("(1,y)",)],
    "select greatest(a, 2) from fs": [(2,)],
}


@pytest.mark.backlog_fixed("RP-531")
@pytest.mark.parametrize("query", list(RECORD_CONDITIONALS_MORE))
def test_record_conditionals_more(query: str, dsn: str) -> None:
    assert answer(dsn, query) == RECORD_CONDITIONALS_MORE[query]


# A GROUP BY item need not be in the select list, a whole row included,
# written `fs`, `fs.*` or by its alias, and HAVING reads it too. A field of
# it, `(fs).a`, is the column (`ParseComplexProjection`), so it is not
# grouped by the row, and is reported where `fs` is.
GROUP_BY_WHOLE_ROW = {
    "select count(*) from fs group by fs": [(1,)],
    "select fs, count(*) from fs group by fs": [("(1,y)", 1)],
    "select count(*) from fs x group by x": [(1,)],
    "select count(*) from fs group by fs.*": [(1,)],
    "select count(*) from fs group by (fs), a": [(1,)],
    "select count(*) from fs group by fs having count(*) > 0": [(1,)],
    "select count(*) from fs group by fs having fs is not null": [(1,)],
    "select count(*) from (select * from fs) s group by s": [(1,)],
    "select count(*) from fs group by fs order by fs": [(1,)],
    "select a, count(*) from fs group by fs": (
        "42803",
        'column "fs.a" must appear in the GROUP BY clause or be used in an aggregate function',
        None,
        "8",
    ),
    "select (fs).a, count(*) from fs group by fs": (
        "42803",
        'column "fs.a" must appear in the GROUP BY clause or be used in an aggregate function',
        None,
        "9",
    ),
}


@pytest.mark.backlog_fixed("RP-532")
@pytest.mark.parametrize("query", list(GROUP_BY_WHOLE_ROW))
def test_group_by_whole_row(query: str, dsn: str) -> None:
    """GROUP BY a whole row not in the select list was 42703 `column "fs"
    does not exist`; HAVING refused the row's name, and `(fs).a` was
    answered as though grouped."""
    assert answer(dsn, query) == GROUP_BY_WHOLE_ROW[query]
