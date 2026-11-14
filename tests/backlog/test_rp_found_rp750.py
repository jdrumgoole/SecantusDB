"""Gap tests of divergences found while working RP-750 (a JSON input error
carried no `JSON data, line N:` CONTEXT):

- a `json` / `jsonb` input error on a constant has no statement position;
- a type's input function called by name (`jsonb_in`, `json_in`, `int4in`)
  is refused 0A000.

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


@pytest.mark.backlog("RP-952")
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


@pytest.mark.backlog("RP-953")
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
