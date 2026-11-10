"""What working RP-794 (an ORDER BY over an unsized `bpchar` column) found:

- an untyped literal resolved to `bpchar` by a VALUES list or a set
  operation beside a `char(n)` lost its trailing blanks (RP-804, fixed);
- `string_agg` over an unsized `bpchar` column kept the blanks the cast
  to its `text` argument drops (RP-805, fixed).

Measured on the shared PostgreSQL 15.19 reference (port 5415); the
statements create nothing.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _answer(dsn: str, query: str) -> object:
    """The rows, or the error's SQLSTATE and first line."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            return [tuple(r) for r in c.execute(query).fetchall()]
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


@pytest.mark.backlog_fixed("RP-804")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("select 'a'::char(3) union all select 'b '", [("a  ",), ("b ",)]),
        (
            "select f, octet_length(f) from (select 'a'::char(3) f union all select 'b ') s",
            [("a  ", 3), ("b ", 2)],
        ),
        (
            "select f, octet_length(f) from (values ('a'::char(3)), ('b ')) s(f)",
            [("a  ", 3), ("b ", 2)],
        ),
        (
            "select f, octet_length(f) from (values ('b '), ('a'::char(3))) s(f)",
            [("b ", 2), ("a  ", 3)],
        ),
        (
            "select f, octet_length(f) from (select 'b ' f union all select 'a'::char(3)) s",
            [("b ", 2), ("a  ", 3)],
        ),
        (
            "select f, octet_length(f) from (select x f from"
            " (select 'a'::char(3) x from generate_series(1, 1)) t union all select 'b ') s",
            [("a  ", 3), ("b ", 2)],
        ),
        (
            "select octet_length(f) from (values ('a'::char(3)), ('c ')) s(f) where f = 'c'",
            [(2,)],
        ),
        (
            "select f, octet_length(f) from (select 'a'::char(3) f intersect select 'a ') s",
            [("a  ", 3)],
        ),
    ],
)
def test_an_untyped_literal_resolved_to_bpchar_keeps_its_blanks(
    dsn: str, query: str, expected: object
) -> None:
    """The column's type is `bpchar` with no modifier (`'b '` is `unknown`,
    resolved to the `char(3)` member's type, whose modifier the other
    member does not share), so `'b '` keeps its blank: `octet_length` is 2
    on PostgreSQL 15.19, on either side and under a filter that compares
    it as `bpchar`. It read back `b`, 1 byte: the literal went through the
    `bpchar` cast that stores a `char(n)` value trimmed."""
    assert _answer(dsn, query) == expected


@pytest.mark.backlog_fixed("RP-805")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        (
            "select string_agg(f, '|') from (values ('a  '::char(3)), ('b'::char(2))) s(f)",
            [("a|b",)],
        ),
        (
            "select string_agg(f, '|' order by f) from"
            " (values ('a  '::char(3)), (E'a\\t'), ('a'::char(2))) s(f)",
            [("a|a|a\t",)],
        ),
        (
            "select string_agg(f, '|' order by f desc) from"
            " (values ('a  '::char(3)), (E'a\\t'), ('a'::char(2))) s(f)",
            [("a\t|a|a",)],
        ),
        (
            "select string_agg(distinct f, '|') from"
            " (values ('a  '::char(3)), ('a'::char(2)), ('b'::char(2))) s(f)",
            [("a|b",)],
        ),
        (
            "select string_agg(f, '|') over (order by f) from"
            " (values ('a  '::char(3)), ('b'::char(2))) s(f)",
            [("a",), ("a|b",)],
        ),
        (
            "select string_agg(f, '|') from (values (' a '::char(3)), ('b'::char(2))) s(f)",
            [(" a|b",)],
        ),
    ],
)
def test_string_agg_over_an_unsized_bpchar_drops_its_blanks(
    dsn: str, query: str, expected: object
) -> None:
    """`string_agg(text, text)` takes a `bpchar` argument through the
    implicit `bpchar` -> `text` cast, `rtrim1`, which drops the trailing
    blanks: `a|b` on PostgreSQL 15.19. Over an unsized column (its members'
    modifiers differ) the cells kept their blanks here, and the aggregate
    joined them as they were: `a  |b `. The cast comes before DISTINCT and
    applies in a window too; a leading blank and a tab stay. A `char(3)`
    column of both members already agreed."""
    assert _answer(dsn, query) == expected
