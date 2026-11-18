"""Regression tests of divergences found while working RP-750 (a JSON input
error carried no `JSON data, line N:` CONTEXT):

- a `json` / `jsonb` input error on a constant had no statement position
  (RP-952);
- a type's input function called by name (`jsonb_in`, `json_in`, `int4in`)
  was refused 0A000 (RP-953);
- a `numeric(0, s)` type name is not refused (found while working RP-953).

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


@pytest.mark.backlog_fixed("RP-952")
@pytest.mark.parametrize(
    ("query", "position"),
    [
        ("select 't'::jsonb", 8),
        ("select 1, '[1,'::json", 11),
        ("select json_typeof('x')", 20),
    ],
)
def test_json_input_error_has_a_position(query: str, position: int, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c, pytest.raises(psycopg.Error) as info:
        c.execute(query)
    assert (info.value.sqlstate, info.value.diag.statement_position) == ("22P02", str(position))


@pytest.mark.backlog_fixed("RP-953")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        ("""select jsonb_in('{"b":1, "a":2}')::text""", '{"a": 2, "b": 1}'),
        ("""select json_in('{"b":1,  "a":2}')::text""", '{"b":1,  "a":2}'),
        ("select int4in('12')::text", "12"),
    ],
)
def test_type_input_function_is_callable_by_name(query: str, expected: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            answer: object = c.execute(query).fetchone()
        except psycopg.Error as e:
            answer = (e.sqlstate, e.diag.message_primary)
    assert answer == (expected,)


@pytest.mark.backlog_fixed("RP-953")
@pytest.mark.parametrize(
    ("query", "sqlstate", "position"),
    [
        # Read when the call runs: no position.
        ("select int4in('x')", "22P02", None),
        ("select jsonb_in('x')", "22P02", None),
        ("select int2in('70000')", "22003", None),
        # The argument must be `cstring`; `text` has no such function.
        ("select int4in('12'::text)", "42883", "8"),
        # `numeric_in` takes three arguments.
        ("select numeric_in('1.5')", "42883", "8"),
    ],
)
def test_type_input_function_refusals(
    query: str, sqlstate: str, position: str | None, dsn: str
) -> None:
    with psycopg.connect(dsn, autocommit=True) as c, pytest.raises(psycopg.Error) as info:
        c.execute(query)
    assert (info.value.sqlstate, info.value.diag.statement_position) == (sqlstate, position)


@pytest.mark.backlog_fixed("RP-953")
def test_three_argument_input_function_without_typmod(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        assert c.execute("select numeric_in('1.5', 0, -1)::text").fetchone() == ("1.5",)


@pytest.mark.backlog_fixed("RP-953")
def test_input_function_with_a_typmod(dsn: str) -> None:
    """`numeric_in(c, typioparam, typmod)` applied the typmod; it was
    refused 0A000."""
    with psycopg.connect(dsn, autocommit=True) as c, pytest.raises(psycopg.Error) as info:
        c.execute("select numeric_in('1.5', 0, 5)")
    assert (info.value.sqlstate, info.value.diag.message_primary) == (
        "22003",
        "numeric field overflow",
    )


@pytest.mark.backlog_fixed("RP-953")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        # Applied as an assignment: refused, not truncated.
        (
            "select varcharin('abcdef', 0, 7)::text",
            ("22001", "value too long for type character varying(3)"),
        ),
        ("select varcharin('abc', 0, 2)::text", ("abc",)),
        ("select timestamp_in('2020-01-01 1:2:3.456', 0, 1)::text", ("2020-01-01 01:02:03.5",)),
        ("select time_in('1:2:3.456', 0, 0)::text", ("01:02:03",)),
        ("select bit_in('101', 0, 3)::text", ("101",)),
        ("select bit_in('101', 0, 4)", ("22026", "bit string length 3 does not match type bit(4)")),
        ("select varbit_in('101', 0, 2)", ("22001", "bit string too long for type bit varying(2)")),
        (
            "select interval_in('1 day', 0, 524287)",
            ("XX000", "unrecognized interval typmod: 524287"),
        ),
        ("select numeric_in('1.5', 0, 1 + 4)", ("22003", "numeric field overflow")),
        ("select numeric_in('1', 0, null)", (None,)),
        ("select pg_typeof(bit_in('101', 0, 3))::text", ("bit",)),
    ],
)
def test_input_function_typmods(query: str, expected: tuple[object, ...], dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            answer: object = c.execute(query).fetchone()
        except psycopg.Error as e:
            answer = (e.sqlstate, e.diag.message_primary)
    assert answer == expected


@pytest.mark.backlog("RP-1042")
def test_numeric_precision_zero_is_refused(dsn: str) -> None:
    """`numeric(0,1)` is refused when the type is read; here the cast runs
    and overflows."""
    with psycopg.connect(dsn, autocommit=True) as c, pytest.raises(psycopg.Error) as info:
        c.execute("select '1.5'::numeric(0,1)")
    assert (
        info.value.sqlstate,
        info.value.diag.message_primary,
        info.value.diag.statement_position,
    ) == (
        "22023",
        "NUMERIC precision 0 must be between 1 and 1000",
        "15",
    )


@pytest.mark.backlog("RP-1041")
def test_cstring_casts_only_to_strings(dsn: str) -> None:
    """`cstring` has no cast to `integer`; here it reads the value."""
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            answer: object = c.execute("select '12'::cstring::int4").fetchone()
        except psycopg.Error as e:
            answer = (e.sqlstate, e.diag.statement_position)
    assert answer == ("42846", "20")
