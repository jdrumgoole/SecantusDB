"""Gap tests of divergences found while working RP-751 (a RAISE printed a
`money` value as its carried numeric, not through `cash_out`):

- a RAISE below ERROR in a block the PL/pgSQL interpreter runs (one with a
  DECLARE) dropped its DETAIL and HINT (RP-832, fixed);
- a RAISE below ERROR carries no CONTEXT (RP-833).

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


@pytest.mark.backlog("RP-833")
def test_a_notice_carries_its_context(dsn: str) -> None:
    """A RAISE below ERROR carries no CONTEXT here; PostgreSQL 15.19's
    `plpgsql_exec_error_callback` frames every `ereport`, a notice's too."""
    contexts: list[str | None] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.add_notice_handler(lambda d: contexts.append(d.context))
        c.execute("do $$ declare x int; begin raise notice 'm'; end $$")
    assert contexts == ["PL/pgSQL function inline_code_block line 1 at RAISE"]
