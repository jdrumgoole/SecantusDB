"""Gap test of the divergence found while working RP-525 (a materialized
view is a dependent of the relations it reads): a DROP of the wrong kind of
relation (RP-528).

PostgreSQL's `DropErrorMsgWrongType` refuses a DROP TABLE, DROP VIEW or
DROP MATERIALIZED VIEW that names a relation of another kind with 42809,
hinting at the DROP that fits. The expected answers were measured on a
private, freshly initdb'd PostgreSQL 15.19 cluster.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


def wrong_kind_answer(dsn: str, drop: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop materialized view if exists rq528m")
        c.execute("drop view if exists rq528v")
        c.execute("create materialized view rq528m as select 1 a")
        c.execute("create view rq528v as select 1 a")
        try:
            c.execute(drop)
            result: Any = "ok"
        except psycopg.Error as e:
            result = (e.sqlstate, e.diag.message_primary, e.diag.message_hint)
        finally:
            left = c.execute(
                "select to_regclass('rq528m')::text, "
                "(select count(*) from pg_matviews where matviewname = 'rq528m')"
            ).fetchone()
            c.execute("drop materialized view if exists rq528m")
            c.execute("drop view if exists rq528v")
        return (result, left)


_KEPT = ("rq528m", 1)
WRONG_KIND = {
    "drop table rq528m": (
        (
            "42809",
            '"rq528m" is not a table',
            "Use DROP MATERIALIZED VIEW to remove a materialized view.",
        ),
        _KEPT,
    ),
    "drop view rq528m": (
        (
            "42809",
            '"rq528m" is not a view',
            "Use DROP MATERIALIZED VIEW to remove a materialized view.",
        ),
        _KEPT,
    ),
    "drop materialized view rq528v": (
        ("42809", '"rq528v" is not a materialized view', "Use DROP VIEW to remove a view."),
        _KEPT,
    ),
}


@pytest.mark.backlog("RP-528")
@pytest.mark.parametrize("drop", list(WRONG_KIND))
def test_drop_of_the_wrong_relation_kind(drop: str, rust_pg: object) -> None:
    dsn = _dsn(rust_pg) + " dbname=postgres"
    assert wrong_kind_answer(dsn, drop) == WRONG_KIND[drop]
