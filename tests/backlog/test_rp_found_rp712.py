"""Gap tests of divergences found while working RP-712 (PL/pgSQL assigned
a value through an explicit-only cast, where PostgreSQL converts it through
the types' text I/O):

- a `numeric` out of `int4`'s range is 22003 `integer out of range` on
  PostgreSQL, with no value quoted; here the message quotes the value;
- a `jsonb` input error carries the `JSON data, line 1: ...` CONTEXT line;
- a `money` variable prints through `cash_out` in a RAISE (`$5.25`);
- an assignment's 22001 for an overlong `varchar(n)` value carries the
  `PL/pgSQL assignment "..."` frame;
- an assignment's input error on a literal carries the assignment's text
  as its internal query.

The expected answers were measured on PostgreSQL 15.19. None of the
statements creates an object.
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
    notes: list[str] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.add_notice_handler(lambda d: notes.append(d.message_primary))
        try:
            c.execute(query)
            return ("ok", notes)
        except psycopg.Error as e:
            d = e.diag
            return ("error", e.sqlstate, d.message_primary, d.context, d.internal_query)


_AT_ASSIGNMENT = "PL/pgSQL function inline_code_block line 1 at assignment"


@pytest.mark.backlog("RP-749")
@pytest.mark.parametrize("query", ["select 1e20::numeric::int", "select (-3e9)::numeric::int4"])
def test_numeric_out_of_int4_range_quotes_no_value(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == ("error", "22003", "integer out of range", None, None)


@pytest.mark.backlog("RP-750")
def test_jsonb_input_error_carries_its_json_data_context(dsn: str) -> None:
    assert _answer(dsn, "select 't'::jsonb") == (
        "error",
        "22P02",
        "invalid input syntax for type json",
        "JSON data, line 1: t",
        None,
    )


@pytest.mark.backlog("RP-751")
@pytest.mark.parametrize(
    "query",
    [
        "do $$ declare x money; begin x := 5.25; raise notice '%', x; end $$",
        # float8 -> money has no cast: `float8out`, then `cash_in`.
        "do $$ declare x money; begin x := 5.25::float8; raise notice '%', x; end $$",
    ],
)
def test_money_variable_prints_through_cash_out(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == ("ok", ["$5.25"])


@pytest.mark.backlog("RP-752")
def test_overlong_varchar_assignment_is_framed_by_the_assignment(dsn: str) -> None:
    query = "do $$ declare x varchar(3); begin x := 'abcdef'::text; end $$"
    assert _answer(dsn, query) == (
        "error",
        "22001",
        "value too long for type character varying(3)",
        "PL/pgSQL assignment \"x := 'abcdef'::text\"\n" + _AT_ASSIGNMENT,
        None,
    )


@pytest.mark.backlog("RP-753")
def test_literal_input_error_in_an_assignment_carries_its_query(dsn: str) -> None:
    query = "do $$ declare x int; begin x := 'abc'; end $$"
    assert _answer(dsn, query) == (
        "error",
        "22P02",
        'invalid input syntax for type integer: "abc"',
        _AT_ASSIGNMENT,
        "x := 'abc'",
    )
