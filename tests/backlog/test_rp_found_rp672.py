"""A `char(n)` value through a set operation whose result has no width
(RP-672), and what was found while working it.

PostgreSQL keeps a `char(n)` value padded. A set operation whose sides are
`char(n)` and `text`, `varchar` or another width resolves to `bpchar` with
no modifier (`select_common_typmod` is -1), and its rows carry each side's
values as they are: padded. This server stores a `char(n)` value trimmed and
pads it on the way out to its column's width, so such a column lost its
blanks. And a `bpchar` value assigned to a `text`, `varchar` or `name`
column drops its trailing blanks (`rtrim1`, `bpchar_name`).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t672c, t672t, t672v, t672ctas, t672n cascade",
    "create table t672c (a char(3))",
    "insert into t672c values ('xy')",
    "create table t672t (s text)",
    "create table t672v (v varchar(5), n name)",
    "create table t672n (x numeric)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _connect(dsn: str) -> psycopg.Connection:
    c = psycopg.connect(dsn, autocommit=True)
    for q in _SETUP:
        c.execute(q)
    return c


def _rows(c: psycopg.Connection, query: str) -> list[tuple[object, ...]]:
    return [tuple(r) for r in c.execute(query).fetchall()]


@pytest.mark.backlog_fixed("RP-672")
def test_char_n_through_an_unsized_set_operation_keeps_its_blanks(dsn: str) -> None:
    """A `char(3)` column UNIONed with a `text`, a `varchar` or a wider
    `char(n)` kept its blanks on PostgreSQL 15.19 and lost them here: in
    the rows, in an ARRAY subquery, and in a FROM subquery or WITH over
    the set operation. The padded values still compare as `bpchar`, which
    ignores trailing blanks, in UNION, INTERSECT, EXCEPT and ORDER BY."""
    with _connect(dsn) as c:
        assert _rows(c, "select a from t672c union all select 'e'::text") == [("xy ",), ("e",)]
        assert _rows(c, "select a from t672c union all select 'e'::varchar(5)") == [
            ("xy ",),
            ("e",),
        ]
        assert _rows(c, "select a from t672c union all select 'e'::char(5)") == [
            ("xy ",),
            ("e    ",),
        ]
        assert _rows(c, "select array(select a from t672c union all select 'e'::text)") == [
            (["xy ", "e"],)
        ]
        assert _rows(
            c,
            "select a, octet_length(a) from"
            " (select a from t672c union all select 'e'::text) s order by 1",
        ) == [("e", 1), ("xy ", 3)]
        assert _rows(
            c,
            "with w as (select a from t672c union all select 'e'::text)"
            " select a, octet_length(a), a || '|', a = 'xy' from w order by 1",
        ) == [("e", 1, "e|", False), ("xy ", 3, "xy|", True)]
        assert _rows(c, "select a from t672c union select 'xy'::text") == [("xy ",)]
        assert _rows(c, "select a from t672c union select 'xy'::char(5)") == [("xy ",)]
        # Over two inputs of one estimated row each the left one comes
        # first, so its padded value is the one kept (RP-692 is a table's).
        assert _rows(
            c, "select a from (values ('xy'::char(3))) v(a) intersect select 'xy'::text"
        ) == [("xy ",)]
        assert _rows(c, "select a from t672c except select 'xy'::text") == []
        assert _rows(c, "select a from t672c except select 'q'::text") == [("xy ",)]
        assert _rows(c, "select a from t672c union all select E'xy\\t'::text order by 1") == [
            ("xy ",),
            ("xy\t",),
        ]


@pytest.mark.backlog_fixed("RP-672")
def test_bpchar_assigned_to_a_string_column_drops_its_blanks(dsn: str) -> None:
    """A `bpchar` value written to a `text`, `varchar` or `name` column
    dropped its trailing blanks on PostgreSQL 15.19 (`rtrim1`,
    `bpchar_name`) and kept them here: `'xy '::bpchar` inserted into a
    `text` column was `xy ` (3 bytes)."""
    with _connect(dsn) as c:
        c.execute("insert into t672t select 'xy '::bpchar")
        c.execute("insert into t672t select 'x'::char(3)")
        c.execute("insert into t672t values ('q '::bpchar)")
        c.execute("insert into t672t select a from t672c union all select 'e'::text")
        assert _rows(c, "select s, octet_length(s) from t672t order by 1") == [
            ("e", 1),
            ("q", 1),
            ("x", 1),
            ("xy", 2),
            ("xy", 2),
        ]
        c.execute("update t672t set s = 'w '::bpchar where s = 'q'")
        c.execute("update t672t set s = a from t672c where s = 'x'")
        assert _rows(c, "select s, octet_length(s) from t672t order by 1") == [
            ("e", 1),
            ("w", 1),
            ("xy", 2),
            ("xy", 2),
            ("xy", 2),
        ]
        c.execute("insert into t672v select 'ab '::bpchar, 'cd '::bpchar")
        c.execute("insert into t672v select a, a from t672c union all select 'e', 'f'")
        assert _rows(
            c, "select v, octet_length(v), n::text, octet_length(n::text) from t672v order by 1"
        ) == [("ab", 2, "cd", 2), ("e", 1, "f", 1), ("xy", 2, "xy", 2)]


@pytest.mark.backlog("RP-693")
def test_create_table_as_of_an_unsized_bpchar_keeps_its_blanks(dsn: str) -> None:
    """`create table ... as select 'xy '::bpchar a` makes a `bpchar` column
    with no modifier on PostgreSQL 15.19 and stores the value as it is:
    `octet_length(a)` is 3. So does one over a `char(3)` column UNIONed
    with `text`. Here the column holds `xy`, 2 bytes, in both."""
    with _connect(dsn) as c:
        c.execute("create table t672ctas as select 'xy '::bpchar a")
        c.execute("insert into t672ctas select a from t672c union all select 'e'::text")
        got = _rows(c, "select a, octet_length(a) from t672ctas order by 1, 2")
    assert got == [("e", 1), ("xy ", 3), ("xy ", 3)]


@pytest.mark.backlog_fixed("RP-692")
def test_intersect_keeps_the_smaller_inputs_value(dsn: str) -> None:
    """PostgreSQL 15.19 plans an INTERSECT with the input it estimates
    smaller first (`generate_nonunion_paths`), and a group's value is the
    one its first input gave. Over a table (estimated 1850 rows unanalysed)
    and a one-row constant, the constant comes first: `select a from t672c
    intersect select 'xy'::text` is `xy`, unpadded. Here the left side's
    value was always kept, `xy ` with its blank."""
    with _connect(dsn) as c:
        got = _rows(
            c,
            "select a, octet_length(a) from (select a from t672c intersect select 'xy'::text) s",
        )
    assert got == [("xy", 2)]


def _text(c: psycopg.Connection, query: str) -> list[str | None]:
    return [None if r[0] is None else str(r[0]) for r in c.execute(query).fetchall()]


@pytest.mark.backlog_fixed("RP-692")
def test_a_set_operations_group_is_its_first_inputs_first_row(dsn: str) -> None:
    """A SetOp returns each group's first row from the input listed first,
    as often as the group is output (`nodeSetOp.c`), measured on a private
    PostgreSQL 15.19 cluster. INTERSECT ALL and EXCEPT ALL repeated that
    row where this server returned each matching row; a VALUES list of more
    rows than the other side's came second; and a sorted SetOp
    (`enable_hashagg` off) returns the groups in its quicksort's order,
    each one's row the first among its ties."""
    with _connect(dsn) as c:
        c.execute("insert into t672n values (1.0), (1.00), (2.0)")
        assert _text(c, "select x from t672n intersect select 1.000") == ["1.000"]
        assert _text(
            c,
            "select x from (values (1.0), (1.00)) v(x)"
            " intersect all select x from (values (1), (1)) w(x)",
        ) == ["1.0", "1.0"]
        assert _text(
            c, "select x from (values (1.0), (1.00), (1.000)) v(x) except all select 1"
        ) == ["1.0", "1.0"]
        assert _text(
            c,
            "select x from (values (1.0), (1.00), (1.000)) v(x)"
            " intersect select x from (values (1), (1)) w(x)",
        ) == ["1"]
        c.execute("set enable_hashagg = off")
        assert _text(
            c,
            "select x from (values (2.0), (1.0), (1.00), (3)) v(x)"
            " intersect all select x from (values (1), (1), (3.0), (2)) w(x)",
        ) == ["1", "1", "2", "3.0"]
        assert _text(
            c,
            "select x from (values (5), (4), (3), (2), (1), (1.0), (1.00), (2.0), (3.0)) v(x)"
            " intersect select x from (values (1.000), (2.00), (3.00), (4.0)) w(x)",
        ) == ["1.000", "2.0", "3.0", "4"]
        assert _text(
            c,
            "select x from (values (null::numeric), (1.0)) v(x)"
            " intersect select x from (values (null::numeric), (1)) w(x)",
        ) == ["1.0", None]
