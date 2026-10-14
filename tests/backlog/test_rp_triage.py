"""Gap tests for `tasks/backlog-rust-postgres.md`, "Open from the 2026-08-26 triage"."""

from __future__ import annotations

import threading
import time
from typing import Any

import pytest

from tests.backlog.conftest import pg_answer


@pytest.mark.backlog("RP-001")
def test_set_transaction_outside_a_block_warns(pg: Any) -> None:
    """PostgreSQL 15.19: `WARNING: SET TRANSACTION can only be used in
    transaction blocks` (sent as a notice, the statement succeeds)."""
    notices: list[str] = []
    pg.add_notice_handler(lambda d: notices.append(f"{d.severity}: {d.message_primary}"))
    pg.execute("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
    assert notices == ["WARNING: SET TRANSACTION can only be used in transaction blocks"]


@pytest.mark.backlog("RP-002")
def test_cancel_during_parse_is_honoured_at_once(pg: Any) -> None:
    """PostgreSQL 15.19: a cancel sent 0.5 s into this 1.2 MB statement
    answers 57014 at 0.515 s, during the parse (it takes ~2 s to parse
    here in a debug build, and the cancel waits for it)."""
    sql = "select pg_sleep(3), 1 in (" + ",".join(str(i) for i in range(200_000)) + ")"
    delay = 0.5
    timer = threading.Timer(delay, pg.cancel)
    t0 = time.monotonic()
    timer.start()
    answer = pg_answer(pg, sql)
    elapsed = time.monotonic() - t0
    timer.join()
    assert answer[:2] == ("err", "57014")
    assert elapsed - delay < 0.5, f"cancel honoured {elapsed - delay:.2f} s after it was sent"


@pytest.mark.backlog("RP-003")
def test_national_literal_under_nonconforming_strings(pg: Any) -> None:
    """PostgreSQL 15.19: with `standard_conforming_strings = off`, `N'a\\\\b'`
    reads its backslash as an escape, like a plain literal: `a\\b`."""
    pg.execute("set standard_conforming_strings = off")
    assert pg_answer(pg, r"select N'a\\b'") == ("ok", [("a\\b",)])


@pytest.mark.backlog("RP-003")
def test_backslash_quote_off_refuses_backslash_quote(pg: Any) -> None:
    """PostgreSQL 15.19: with `standard_conforming_strings = off` and
    `backslash_quote = off`, `'a\\'b'` is 22P06 `unsafe use of \\' in a
    string literal`."""
    pg.execute("set standard_conforming_strings = off")
    pg.execute("set backslash_quote = off")
    assert pg_answer(pg, r"select 'a\'b'") == (
        "err",
        "22P06",
        "unsafe use of \\' in a string literal",
    )


@pytest.fixture(scope="module")
def _revoked_abs(rust_pg: Any) -> str:
    """A role with no EXECUTE on `abs(int)`, and a view, a DEFAULT and a SQL
    function that call it. Returns the role's name."""
    with rust_pg.connect() as conn:
        for sql in (
            "create role rp004_r",
            "revoke execute on function abs(int) from public",
            "create table rp004_t (a int default abs(-1), b int)",
            "create view rp004_v as select abs(-1) as x",
            "grant select, insert on rp004_t to rp004_r",
            "grant select on rp004_v to rp004_r",
        ):
            conn.execute(sql)
    return "rp004_r"


@pytest.mark.backlog("RP-004")
@pytest.mark.parametrize(
    "sql",
    ["select x from rp004_v", "insert into rp004_t (b) values (1)"],
    ids=["view", "column-default"],
)
def test_revoked_builtin_refused_through_view_and_default(
    pg: Any, _revoked_abs: str, sql: str
) -> None:
    """PostgreSQL 15.19 (measured in a rolled-back transaction): both are
    42501 `permission denied for function abs` for the restricted role."""
    pg.execute(f"set role {_revoked_abs}")
    assert pg_answer(pg, sql) == ("err", "42501", "permission denied for function abs")


@pytest.mark.backlog("RP-005")
def test_pg_collation_lists_locale_icu_collations(pg: Any) -> None:
    """PostgreSQL 15.19 (907 ICU collations here) lists `de-x-icu`,
    `fr-FR-x-icu` and `sv-x-icu` beside `en-x-icu` and `und-x-icu`."""
    sql = (
        "select collname::text from pg_collation where collprovider = 'i' and collname in "
        "('de-x-icu', 'en-x-icu', 'fr-FR-x-icu', 'sv-x-icu', 'und-x-icu') order by 1"
    )
    assert pg_answer(pg, sql) == (
        "ok",
        [("de-x-icu",), ("en-x-icu",), ("fr-FR-x-icu",), ("sv-x-icu",), ("und-x-icu",)],
    )
