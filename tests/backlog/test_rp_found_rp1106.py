"""Gap tests of the divergences found while working RP-1106 (a LEFT JOIN
nothing reads, to a view, a CTE or a subquery, is removed).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.20 cluster (2026-11-27). The build before RP-1106 answered
as the one after it.
"""

from __future__ import annotations

from typing import Any

import pytest

from tests.backlog.conftest import pg_answer


@pytest.mark.backlog("RP-1155")
def test_create_view_with_check_option_refuses_a_row_it_cannot_show(pg: Any) -> None:
    """PostgreSQL reads a view's `WITH (check_option = local)` as `WITH
    LOCAL CHECK OPTION`, so a row the view cannot show is refused
    (`44000`). `secantusd-pg` drops the options CREATE VIEW is given, and
    inserts the row."""
    pg.execute("drop view if exists rp1155v")
    pg.execute("drop table if exists rp1155t")
    pg.execute("create table rp1155t (a int)")
    pg.execute(
        "create view rp1155v with (check_option = local) as select * from rp1155t where a > 0"
    )
    assert pg_answer(pg, "insert into rp1155v values (-1)") == (
        "err",
        "44000",
        'new row violates check option for view "rp1155v"',
    )


@pytest.mark.backlog("RP-1156")
def test_a_views_options_are_its_reloptions(pg: Any) -> None:
    """PostgreSQL shows a view's options in `pg_class.reloptions`:
    `{security_barrier=true}` after `ALTER VIEW ... SET
    (security_barrier)`. `secantusd-pg` answers NULL for every user view."""
    pg.execute("drop view if exists rp1156w")
    pg.execute("drop table if exists rp1156t")
    pg.execute("create table rp1156t (a int)")
    pg.execute("create view rp1156w as select * from rp1156t")
    pg.execute("alter view rp1156w set (security_barrier)")
    assert pg_answer(pg, "select reloptions from pg_class where relname = 'rp1156w'") == (
        "ok",
        [(["security_barrier=true"],)],
    )
