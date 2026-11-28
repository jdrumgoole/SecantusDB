"""`client_min_messages` decides which messages below ERROR reach the
client, at the moment each is raised (`should_output_to_client` in elog.c;
RP-1092). Every expected answer was measured on PostgreSQL 15.19 (the
shared reference on port 5415)."""

from __future__ import annotations

from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest

from tests.test_rust_pgserver_slice import BINARY, _Server

pytestmark = pytest.mark.skipif(not BINARY.exists(), reason="secantusd-pg not built")


@pytest.fixture
def conn(tmp_path: Path) -> Iterator[Any]:
    with _Server(tmp_path) as server, server.connect() as c:
        c.autocommit = True
        yield c


def _seen(conn: Any, *statements: str) -> list[tuple[str, str]]:
    """The (severity, message) of each notice the statements send."""
    seen: list[tuple[str, str]] = []
    conn.add_notice_handler(lambda d: seen.append((d.severity or "", d.message_primary or "")))
    for s in statements:
        conn.execute(s)
    return seen


def test_warning_withholds_a_notice_and_sends_info(conn: Any) -> None:
    assert _seen(
        conn,
        "set client_min_messages = warning",
        "drop table if exists cmm_missing",
        "drop database if exists cmm_nodb",
        "do $$ begin raise notice 'n'; raise warning 'w'; raise info 'i'; end $$",
    ) == [("WARNING", "w"), ("INFO", "i")]


def test_error_withholds_a_warning_but_never_an_info(conn: Any) -> None:
    assert _seen(
        conn,
        "set client_min_messages = error",
        "do $$ begin raise warning 'w'; raise info 'i'; end $$",
    ) == [("INFO", "i")]


def test_debug_and_log_are_sent_when_the_level_allows(conn: Any) -> None:
    body = "do $$ begin raise debug 'd'; raise log 'l'; raise notice 'n'; end $$"
    assert _seen(conn, "set client_min_messages = debug1", body) == [
        ("DEBUG", "d"),
        ("LOG", "l"),
        ("NOTICE", "n"),
    ]


def test_log_sits_below_notice_for_the_client(conn: Any) -> None:
    body = "do $$ begin raise debug 'd'; raise log 'l'; raise notice 'n'; end $$"
    assert _seen(conn, "set client_min_messages = log", body) == [("LOG", "l"), ("NOTICE", "n")]


def test_the_setting_in_force_at_the_raise_decides(conn: Any) -> None:
    # Changed part-way through a DO block: what came before is sent.
    assert _seen(
        conn,
        "do $$ begin raise notice 'before'; set client_min_messages = warning;"
        " raise notice 'after'; end $$",
    ) == [("NOTICE", "before")]
    assert conn.execute("show client_min_messages").fetchone() == ("warning",)


def test_a_functions_own_set_clause_withholds_its_notices(conn: Any) -> None:
    conn.execute(
        "create function cmm_f() returns int language plpgsql"
        " set client_min_messages = warning"
        " as $$ begin raise notice 'inside'; return 1; end $$"
    )
    assert _seen(conn, "select cmm_f()") == []
    # A function without one runs under the session's level.
    conn.execute(
        "create function cmm_g() returns int language plpgsql"
        " as $$ begin raise debug 'dd'; raise notice 'nn'; return 1; end $$"
    )
    assert _seen(conn, "set client_min_messages = debug1", "select cmm_g()") == [
        ("DEBUG", "dd"),
        ("NOTICE", "nn"),
    ]


def test_the_hidden_level_spellings(conn: Any) -> None:
    conn.execute("set client_min_messages = debug")
    assert conn.execute("show client_min_messages").fetchone() == ("debug2",)
    conn.execute("set client_min_messages = info")
    assert conn.execute("show client_min_messages").fetchone() == ("info",)
    # `info` withholds LOG, and still sends a NOTICE.
    assert _seen(conn, "do $$ begin raise log 'l'; raise notice 'n'; end $$") == [("NOTICE", "n")]
    conn.execute("set log_min_messages = debug")
    assert conn.execute("show log_min_messages").fetchone() == ("debug2",)
    conn.execute("set log_min_error_statement = debug")
    assert conn.execute("show log_min_error_statement").fetchone() == ("debug2",)


def test_a_planner_notice_is_withheld_too(conn: Any) -> None:
    # The stop-word notice text search raises while it plans.
    query = "select to_tsquery('english', 'the')"
    assert _seen(conn, "set client_min_messages = warning", query) == []
    conn.execute("reset client_min_messages")
    assert _seen(conn, query) == [
        ("NOTICE", "text-search query contains only stop words or doesn't contain lexemes, ignored")
    ]
