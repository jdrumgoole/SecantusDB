"""RP-343 / RP-344: database DDL waits for other sessions to leave.

PostgreSQL 15.19's ``CountOtherDBBackends`` polls up to 50 times, 100 ms
apart, before it raises 55006 ``... is being accessed by other users``: a
session that is closing (its client has hung up, its backend has not yet
exited) does not fail the statement. Measured on 15.19: a session on the
template that closes 1 s after ``create database ... template`` starts lets
it succeed, after about 1.3 s.
"""

from __future__ import annotations

import threading
from typing import Any

import psycopg
import pytest


@pytest.mark.backlog_fixed("RP-343")
def test_create_database_waits_for_a_closing_template_session(rust_pg: Any) -> None:
    """A session on the template that left within 5 s no longer failed it."""
    dsn = f"host=127.0.0.1 port={rust_pg.port} user=postgres"
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as c:
        c.execute("create database rp343_src")
        holder = psycopg.connect(dsn + " dbname=rp343_src", autocommit=True)
        closer = threading.Timer(1.0, holder.close)
        closer.start()
        try:
            c.execute("create database rp343_dst template rp343_src")
            got = "ok"
        except psycopg.Error as e:
            got = e.sqlstate or "?"
        finally:
            closer.join()
        assert got == "ok"


@pytest.mark.backlog_fixed("RP-343")
def test_alter_database_rename_waits_for_a_closing_session(rust_pg: Any) -> None:
    """ALTER DATABASE ... RENAME waited for a closing session, as CREATE did."""
    dsn = f"host=127.0.0.1 port={rust_pg.port} user=postgres"
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as c:
        c.execute("create database rp343_old")
        holder = psycopg.connect(dsn + " dbname=rp343_old", autocommit=True)
        closer = threading.Timer(1.0, holder.close)
        closer.start()
        try:
            c.execute("alter database rp343_old rename to rp343_new")
            got = "ok"
        except psycopg.Error as e:
            got = e.sqlstate or "?"
        finally:
            closer.join()
        assert got == "ok"


@pytest.mark.backlog("RP-344")
def test_drop_database_refuses_while_another_session_is_on_it(rust_pg: Any) -> None:
    """A session idle on the database makes DROP DATABASE 55006 (15.19)."""
    dsn = f"host=127.0.0.1 port={rust_pg.port} user=postgres"
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as c:
        c.execute("create database rp344_d")
        with psycopg.connect(dsn + " dbname=rp344_d", autocommit=True):
            try:
                c.execute("drop database rp344_d")
                got: tuple[str | None, ...] = ("ok",)
            except psycopg.Error as e:
                got = (e.sqlstate, e.diag.message_primary, e.diag.message_detail)
        assert got == (
            "55006",
            'database "rp344_d" is being accessed by other users',
            "There is 1 other session using the database.",
        )
