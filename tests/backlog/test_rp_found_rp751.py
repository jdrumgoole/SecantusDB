"""Gap tests of divergences found while working RP-751 (a RAISE printed a
`money` value as its carried numeric, not through `cash_out`):

- a RAISE below ERROR in a block the PL/pgSQL interpreter runs (one with a
  DECLARE) dropped its DETAIL and HINT (RP-832, fixed);
- a RAISE below ERROR carried no CONTEXT (RP-833, fixed).

The expected answers were measured on PostgreSQL 15.19. Only the nested
CONTEXT test creates an object, a function in its own session's `pg_temp`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _notices(dsn: str, query: str) -> list[tuple[object, ...]]:
    notes: list[tuple[object, ...]] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.add_notice_handler(
            lambda d: notes.append(
                (
                    d.severity,
                    d.sqlstate,
                    d.message_primary,
                    d.message_detail,
                    d.message_hint,
                )
            )
        )
        c.execute(query)
    return notes


@pytest.mark.backlog_fixed("RP-832")
@pytest.mark.parametrize(
    ("query", "expected"),
    [
        (
            "do $$ declare x text := 'q'; begin "
            "raise notice 'm' using detail = x, hint = x; end $$",
            ("NOTICE", "00000", "m", "q", "q"),
        ),
        (
            "do $$ declare x text := 'q'; begin raise warning using "
            "message = x, detail = 'd', hint = 'h', errcode = '22012'; end $$",
            ("WARNING", "22012", "q", "d", "h"),
        ),
        (
            "do $$ declare x int; begin raise info 'i %', x using detail = 'dd'; end $$",
            ("INFO", "00000", "i <NULL>", "dd", None),
        ),
        # Printed by `cash_out`, as the message's parameters are (RP-751).
        (
            "do $$ declare x money := 5.25; begin "
            "raise notice 'm' using detail = x, hint = x; end $$",
            ("NOTICE", "00000", "m", "$5.25", "$5.25"),
        ),
    ],
)
def test_a_notice_keeps_its_detail_and_hint(
    query: str, expected: tuple[object, ...], dsn: str
) -> None:
    """A RAISE below ERROR in a block the interpreter runs dropped its
    DETAIL and HINT; `exec_stmt_raise` passes both to `ereport` at every
    level."""
    assert _notices(dsn, query) == [expected]


@pytest.mark.backlog_fixed("RP-833")
def test_a_notice_carries_its_context(dsn: str) -> None:
    """A RAISE below ERROR carried no CONTEXT here; PostgreSQL 15.19's
    `plpgsql_exec_error_callback` frames every `ereport`, a notice's too."""
    contexts: list[str | None] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.add_notice_handler(lambda d: contexts.append(d.context))
        c.execute("do $$ declare x int; begin raise notice 'm'; end $$")
    assert contexts == ["PL/pgSQL function inline_code_block line 1 at RAISE"]


@pytest.mark.backlog_fixed("RP-833")
@pytest.mark.parametrize(
    ("body", "expected"),
    [
        (
            "begin perform pg_temp.f833(); end",
            "PL/pgSQL function {f} line 1 at RAISE\n"
            'SQL statement "SELECT pg_temp.f833()"\n'
            "PL/pgSQL function inline_code_block line 1 at PERFORM",
        ),
        (
            "declare x int; begin x := pg_temp.f833(); end",
            "PL/pgSQL function {f} line 1 at RAISE\n"
            "PL/pgSQL function inline_code_block line 1 at assignment",
        ),
        (
            "begin for i in 1..1 loop raise warning 'w'; end loop; end",
            "PL/pgSQL function inline_code_block line 1 at RAISE",
        ),
    ],
)
def test_a_notice_carries_its_callers_frames(dsn: str, body: str, expected: str) -> None:
    """A notice was framed by each caller's running statement, innermost
    first, and by only the innermost statement of each function, as
    PostgreSQL 15.19's `error_context_stack` leaves it. The function's own
    label (`pg_temp_3.f833()` there) is a separate divergence, RP-1018."""
    contexts: list[str | None] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute(
            "create function pg_temp.f833() returns int language plpgsql "
            "as $$ begin raise notice 'in'; return 1; end $$"
        )
        c.add_notice_handler(lambda d: contexts.append(d.context))
        c.execute(f"do $$ {body} $$")
    assert len(contexts) == 1
    label = contexts[0].split(" line ", 1)[0].removeprefix("PL/pgSQL function ")  # type: ignore[union-attr]
    assert contexts == [expected.format(f=label)]
