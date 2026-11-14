"""Gap tests of what was found while working RP-573 (a PL/pgSQL DECLARE of
a type that does not exist was created).

Each answer is `"ok"`, or `(sqlstate, message, position, context)` for an
error. The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster, each in a new session.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _compiled(dsn: str, queries: list[str]) -> list[Any]:
    """Each CREATE's answer, in a session of its own, the routine dropped."""
    out: list[Any] = []
    for query in queries:
        with psycopg.connect(dsn, autocommit=True) as c:
            try:
                c.execute(query)
                out.append("ok")
            except psycopg.Error as e:
                d = e.diag
                out.append((e.sqlstate, d.message_primary, d.statement_position, d.context))
            c.execute("drop function if exists e573")
            c.execute("drop function if exists e573(int)")
            c.execute("drop function if exists e573(pg_temp.nosuch)")
    return out


_F = "create function e573() returns int language plpgsql as $$ {} $$"

# `read_datatype` hands a declaration's text to `parse_datatype` unless it
# is a `%TYPE` of a variable or column it finds, and a text that is not one
# type name is a syntax error there, at the token, with the frame `invalid
# type name "..."`.
NOT_ONE_TYPE_NAME = [
    _F.format("declare x nosuch573%type; begin return 1; end"),
    _F.format("declare x int; y nosuch573.x%type; begin return 1; end"),
    _F.format("declare x int(3); begin return 1; end"),
    _F.format("declare x varchar(nosuch); begin return 1; end"),
]

# `plpgsql_build_variable` refuses a variable whose type is a pseudo-type
# (`record` is a record variable), with no position.
PSEUDO_TYPE = [
    _F.format("declare x anyelement; begin return 1; end"),
    "create function e573() returns int language plpgsql as $$\n"
    "declare\n  y int;\n  x void; begin return 1; end $$",
    _F.format("declare x record; begin return 1; end"),
    _F.format('declare x pg_catalog."any"; begin return 1; end'),
]

# The compiler reads the body once, from the start: a declaration's missing
# type is found before a syntax error later in the body, in the DECLARE
# section or a nested block's.
TYPE_BEFORE_SYNTAX = [
    _F.format("declare x nosuch573; begin return 1 +; end"),
    _F.format("declare x nosuch573; y int := 1 +; begin return 1; end"),
    _F.format("begin declare x nosuch573; begin null; end; return 1 +; end"),
]

# `LookupExplicitNamespace("pg_temp")` in a session with no temporary
# schema yet: 3F000, for a signature's type (no position) and a
# declaration's (at the type).
PG_TEMP_UNMADE = [
    "create function e573(a pg_temp.nosuch) returns int language plpgsql"
    " as $$ begin return 1; end $$",
    _F.format("declare x pg_temp.nosuch; begin return 1; end"),
]

# The compiler's own errors in a CREATE FUNCTION's body are placed at their
# token, as a syntax error already is.
COMPILE_ERROR_POSITION = [
    _F.format("declare x int; begin y := 1; return 1; end"),
    "create function e573(int) returns int language plpgsql as $$"
    " declare x alias for $2; begin return 1; end $$",
]


@pytest.mark.backlog_fixed("RP-617")
def test_declare_of_text_that_is_not_one_type_name(dsn: str) -> None:
    """A DECLARE whose type text was not one type name was created;
    `typeStringToTypeName` refuses it with the grammar's syntax error."""
    assert _compiled(dsn, NOT_ONE_TYPE_NAME) == [
        ("42601", 'syntax error at or near "%"', "78", 'invalid type name "nosuch573%type"'),
        ("42601", 'syntax error at or near "%"', "87", 'invalid type name "nosuch573.x%type"'),
        ("42601", 'syntax error at or near "("', "72", 'invalid type name "int(3)"'),
        ("42601", 'syntax error at or near "nosuch"', "77", 'invalid type name "varchar(nosuch)"'),
    ]


@pytest.mark.backlog_fixed("RP-618")
def test_declare_of_a_pseudo_type(dsn: str) -> None:
    """A variable of a pseudo-type was created; `plpgsql_build_variable`
    refuses one. `record` is a record variable, and `record[]` is refused."""
    assert _compiled(dsn, PSEUDO_TYPE) == [
        (
            "0A000",
            'variable "x" has pseudo-type anyelement',
            None,
            'compilation of PL/pgSQL function "e573" near line 1',
        ),
        (
            "0A000",
            'variable "x" has pseudo-type void',
            None,
            'compilation of PL/pgSQL function "e573" near line 4',
        ),
        "ok",
        (
            "0A000",
            'variable "x" has pseudo-type "any"',
            None,
            'compilation of PL/pgSQL function "e573" near line 1',
        ),
    ]


@pytest.mark.backlog_fixed("RP-619")
def test_declare_type_found_before_a_later_syntax_error(dsn: str) -> None:
    """A declaration's missing type was reported after a later syntax error:
    the whole body was parsed before any type was looked up."""
    assert _compiled(dsn, TYPE_BEFORE_SYNTAX) == [
        ("42704", 'type "nosuch573" does not exist', "69", None),
        ("42704", 'type "nosuch573" does not exist', "69", None),
        ("42704", 'type "nosuch573" does not exist', "75", None),
    ]


@pytest.mark.backlog("RP-620")
def test_pg_temp_type_in_a_session_with_no_temp_schema(dsn: str) -> None:
    assert _compiled(dsn, PG_TEMP_UNMADE) == [
        ("3F000", 'schema "pg_temp" does not exist', None, None),
        ("3F000", 'schema "pg_temp" does not exist', "69", None),
    ]


@pytest.mark.backlog("RP-621")
def test_plpgsql_compile_error_in_create_has_a_position(dsn: str) -> None:
    assert _compiled(dsn, COMPILE_ERROR_POSITION) == [
        ("42601", '"y" is not a known variable', "80", None),
        ("42704", 'variable "$2" does not exist', "82", None),
    ]
