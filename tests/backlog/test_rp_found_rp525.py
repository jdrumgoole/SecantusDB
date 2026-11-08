"""Regression tests of the divergence found while working RP-525 (a
materialized view is a dependent of the relations it reads): a DROP of the
wrong kind of relation (RP-528, fixed).

PostgreSQL's `RemoveRelations` looks every name of a DROP TABLE, VIEW,
MATERIALIZED VIEW, INDEX, SEQUENCE or FOREIGN TABLE up before it drops any,
and `DropErrorMsgWrongType` refuses one of another kind with 42809, hinting
at the DROP that fits; IF EXISTS does not excuse it. The server answered
`drop table` of a materialized view by dropping its relation and leaving its
`pg_matviews` row behind. The expected answers were measured on a private,
freshly initdb'd PostgreSQL 15.19 cluster.
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


@pytest.mark.backlog_fixed("RP-528")
@pytest.mark.parametrize("drop", list(WRONG_KIND))
def test_drop_of_the_wrong_relation_kind(drop: str, rust_pg: object) -> None:
    dsn = _dsn(rust_pg) + " dbname=postgres"
    assert wrong_kind_answer(dsn, drop) == WRONG_KIND[drop]


# Every DROP verb against every relation kind, IF EXISTS, missing names and
# lists of names. Each case runs against a fresh set of relations; the third
# field is what the statement dropped (`relname:relkind`), the second the
# NOTICEs it raised.
_SETUP = [
    "create foreign data wrapper rq528w",
    "create server rq528s foreign data wrapper rq528w",
    "create foreign table rq528f(a int) server rq528s",
    "create table rq528t(a int)",
    "create table rq528p(a int) partition by range (a)",
    "create index rq528i on rq528t(a)",
    "create materialized view rq528m as select a from rq528t",
    "create view rq528v as select 1 a",
    "create sequence rq528s",
    "create type rq528c as (a int)",
]
_TEARDOWN = [
    "drop materialized view if exists rq528m",
    "drop view if exists rq528v",
    "drop table if exists rq528t, rq528p",
    "drop sequence if exists rq528s",
    "drop type if exists rq528c",
    "drop foreign data wrapper if exists rq528w cascade",
]
_RELATIONS = {
    "rq528c:c",
    "rq528f:f",
    "rq528i:i",
    "rq528m:m",
    "rq528p:p",
    "rq528s:S",
    "rq528t:r",
    "rq528v:v",
}

_USE_CASCADE = "Use DROP ... CASCADE to drop the dependent objects too."
_USE_FOREIGN_TABLE = "Use DROP FOREIGN TABLE to remove a foreign table."
_USE_INDEX = "Use DROP INDEX to remove an index."
_USE_MATERIALIZED_VIEW = "Use DROP MATERIALIZED VIEW to remove a materialized view."
_USE_SEQUENCE = "Use DROP SEQUENCE to remove a sequence."
_USE_TABLE = "Use DROP TABLE to remove a table."
_USE_TYPE = "Use DROP TYPE to remove a type."
_USE_VIEW = "Use DROP VIEW to remove a view."

MATRIX = {
    "drop foreign table if exists rq528i": (
        ("42809", '"rq528i" is not a foreign table', _USE_INDEX),
        [],
        "",
    ),
    "drop foreign table if exists rq528m": (
        ("42809", '"rq528m" is not a foreign table', _USE_MATERIALIZED_VIEW),
        [],
        "",
    ),
    "drop foreign table if exists rq528v": (
        ("42809", '"rq528v" is not a foreign table', _USE_VIEW),
        [],
        "",
    ),
    "drop foreign table rq528c": (("42809", '"rq528c" is not a foreign table', _USE_TYPE), [], ""),
    "drop foreign table rq528f": ("ok", [], "rq528f:f"),
    "drop foreign table rq528i": (("42809", '"rq528i" is not a foreign table', _USE_INDEX), [], ""),
    "drop foreign table rq528m": (
        ("42809", '"rq528m" is not a foreign table', _USE_MATERIALIZED_VIEW),
        [],
        "",
    ),
    "drop foreign table rq528nosuch": (
        ("42704", 'foreign table "rq528nosuch" does not exist', None),
        [],
        "",
    ),
    "drop foreign table rq528p": (("42809", '"rq528p" is not a foreign table', _USE_TABLE), [], ""),
    "drop foreign table rq528s": (
        ("42809", '"rq528s" is not a foreign table', _USE_SEQUENCE),
        [],
        "",
    ),
    "drop foreign table rq528t": (("42809", '"rq528t" is not a foreign table', _USE_TABLE), [], ""),
    "drop foreign table rq528v": (("42809", '"rq528v" is not a foreign table', _USE_VIEW), [], ""),
    "drop index if exists rq528i": ("ok", [], "rq528i:i"),
    "drop index if exists rq528m": (
        ("42809", '"rq528m" is not an index', _USE_MATERIALIZED_VIEW),
        [],
        "",
    ),
    "drop index if exists rq528v": (("42809", '"rq528v" is not an index', _USE_VIEW), [], ""),
    "drop index rq528c": (("42809", '"rq528c" is not an index', _USE_TYPE), [], ""),
    "drop index rq528f": (("42809", '"rq528f" is not an index', _USE_FOREIGN_TABLE), [], ""),
    "drop index rq528i": ("ok", [], "rq528i:i"),
    "drop index rq528i, rq528t": (("42809", '"rq528t" is not an index', _USE_TABLE), [], ""),
    "drop index rq528m": (("42809", '"rq528m" is not an index', _USE_MATERIALIZED_VIEW), [], ""),
    "drop index rq528nosuch": (("42704", 'index "rq528nosuch" does not exist', None), [], ""),
    "drop index rq528p": (("42809", '"rq528p" is not an index', _USE_TABLE), [], ""),
    "drop index rq528s": (("42809", '"rq528s" is not an index', _USE_SEQUENCE), [], ""),
    "drop index rq528t": (("42809", '"rq528t" is not an index', _USE_TABLE), [], ""),
    "drop index rq528v": (("42809", '"rq528v" is not an index', _USE_VIEW), [], ""),
    "drop materialized view if exists rq528i": (
        ("42809", '"rq528i" is not a materialized view', _USE_INDEX),
        [],
        "",
    ),
    "drop materialized view if exists rq528m": ("ok", [], "rq528m:m"),
    "drop materialized view if exists rq528v": (
        ("42809", '"rq528v" is not a materialized view', _USE_VIEW),
        [],
        "",
    ),
    "drop materialized view rq528c": (
        ("42809", '"rq528c" is not a materialized view', _USE_TYPE),
        [],
        "",
    ),
    "drop materialized view rq528f": (
        ("42809", '"rq528f" is not a materialized view', _USE_FOREIGN_TABLE),
        [],
        "",
    ),
    "drop materialized view rq528i": (
        ("42809", '"rq528i" is not a materialized view', _USE_INDEX),
        [],
        "",
    ),
    "drop materialized view rq528m": ("ok", [], "rq528m:m"),
    "drop materialized view rq528m, rq528v": (
        ("42809", '"rq528v" is not a materialized view', _USE_VIEW),
        [],
        "",
    ),
    "drop materialized view rq528p": (
        ("42809", '"rq528p" is not a materialized view', _USE_TABLE),
        [],
        "",
    ),
    "drop materialized view rq528s": (
        ("42809", '"rq528s" is not a materialized view', _USE_SEQUENCE),
        [],
        "",
    ),
    "drop materialized view rq528t": (
        ("42809", '"rq528t" is not a materialized view', _USE_TABLE),
        [],
        "",
    ),
    "drop materialized view rq528v": (
        ("42809", '"rq528v" is not a materialized view', _USE_VIEW),
        [],
        "",
    ),
    "drop sequence if exists rq528i": (("42809", '"rq528i" is not a sequence', _USE_INDEX), [], ""),
    "drop sequence if exists rq528m": (
        ("42809", '"rq528m" is not a sequence', _USE_MATERIALIZED_VIEW),
        [],
        "",
    ),
    "drop sequence if exists rq528v": (("42809", '"rq528v" is not a sequence', _USE_VIEW), [], ""),
    "drop sequence rq528c": (("42809", '"rq528c" is not a sequence', _USE_TYPE), [], ""),
    "drop sequence rq528f": (("42809", '"rq528f" is not a sequence', _USE_FOREIGN_TABLE), [], ""),
    "drop sequence rq528i": (("42809", '"rq528i" is not a sequence', _USE_INDEX), [], ""),
    "drop sequence rq528m": (
        ("42809", '"rq528m" is not a sequence', _USE_MATERIALIZED_VIEW),
        [],
        "",
    ),
    "drop sequence rq528p": (("42809", '"rq528p" is not a sequence', _USE_TABLE), [], ""),
    "drop sequence rq528s": ("ok", [], "rq528s:S"),
    "drop sequence rq528t": (("42809", '"rq528t" is not a sequence', _USE_TABLE), [], ""),
    "drop sequence rq528v": (("42809", '"rq528v" is not a sequence', _USE_VIEW), [], ""),
    "drop table if exists rq528i": (("42809", '"rq528i" is not a table', _USE_INDEX), [], ""),
    "drop table if exists rq528m": (
        ("42809", '"rq528m" is not a table', _USE_MATERIALIZED_VIEW),
        [],
        "",
    ),
    "drop table if exists rq528nosuch, rq528m": (
        ("42809", '"rq528m" is not a table', _USE_MATERIALIZED_VIEW),
        ['table "rq528nosuch" does not exist, skipping'],
        "",
    ),
    "drop table if exists rq528t, rq528nosuch cascade": (
        "ok",
        [
            'table "rq528nosuch" does not exist, skipping',
            "drop cascades to materialized view rq528m",
        ],
        "rq528i:i,rq528m:m,rq528t:r",
    ),
    "drop table if exists rq528v": (("42809", '"rq528v" is not a table', _USE_VIEW), [], ""),
    "drop table rq528c": (("42809", '"rq528c" is not a table', _USE_TYPE), [], ""),
    "drop table rq528f": (("42809", '"rq528f" is not a table', _USE_FOREIGN_TABLE), [], ""),
    "drop table rq528i": (("42809", '"rq528i" is not a table', _USE_INDEX), [], ""),
    "drop table rq528m": (("42809", '"rq528m" is not a table', _USE_MATERIALIZED_VIEW), [], ""),
    "drop table rq528nosuch": (("42P01", 'table "rq528nosuch" does not exist', None), [], ""),
    "drop table rq528nosuch, rq528m": (
        ("42P01", 'table "rq528nosuch" does not exist', None),
        [],
        "",
    ),
    "drop table rq528p": ("ok", [], "rq528p:p"),
    "drop table rq528s": (("42809", '"rq528s" is not a table', _USE_SEQUENCE), [], ""),
    "drop table rq528t": (
        ("2BP01", "cannot drop table rq528t because other objects depend on it", _USE_CASCADE),
        [],
        "",
    ),
    "drop table rq528t, rq528m": (
        ("42809", '"rq528m" is not a table', _USE_MATERIALIZED_VIEW),
        [],
        "",
    ),
    "drop table rq528t, rq528nosuch": (
        ("42P01", 'table "rq528nosuch" does not exist', None),
        [],
        "",
    ),
    "drop table rq528v": (("42809", '"rq528v" is not a table', _USE_VIEW), [], ""),
    "drop view if exists rq528i": (("42809", '"rq528i" is not a view', _USE_INDEX), [], ""),
    "drop view if exists rq528m": (
        ("42809", '"rq528m" is not a view', _USE_MATERIALIZED_VIEW),
        [],
        "",
    ),
    "drop view if exists rq528v": ("ok", [], "rq528v:v"),
    "drop view rq528c": (("42809", '"rq528c" is not a view', _USE_TYPE), [], ""),
    "drop view rq528f": (("42809", '"rq528f" is not a view', _USE_FOREIGN_TABLE), [], ""),
    "drop view rq528i": (("42809", '"rq528i" is not a view', _USE_INDEX), [], ""),
    "drop view rq528m": (("42809", '"rq528m" is not a view', _USE_MATERIALIZED_VIEW), [], ""),
    "drop view rq528p": (("42809", '"rq528p" is not a view', _USE_TABLE), [], ""),
    "drop view rq528s": (("42809", '"rq528s" is not a view', _USE_SEQUENCE), [], ""),
    "drop view rq528t": (("42809", '"rq528t" is not a view', _USE_TABLE), [], ""),
    "drop view rq528v": ("ok", [], "rq528v:v"),
}


def drop_answer(dsn: str, drop: str) -> Any:
    notes: list[str] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.add_notice_handler(lambda d: notes.append(d.message_primary))
        for s in _TEARDOWN + _SETUP:
            c.execute(s)
        notes.clear()
        try:
            c.execute(drop)
            result: Any = "ok"
        except psycopg.Error as e:
            result = (e.sqlstate, e.diag.message_primary, e.diag.message_hint)
        finally:
            said = list(notes)
            left = c.execute(
                "select string_agg(relname || ':' || relkind::text, ',') from pg_class "
                "where relname like 'rq528%' and relname <> 'rq528m_a_idx'"
            ).fetchone()[0]
            for s in _TEARDOWN:
                c.execute(s)
    dropped = ",".join(sorted(_RELATIONS - set((left or "").split(","))))
    return (result, said, dropped)


@pytest.mark.backlog_fixed("RP-528")
@pytest.mark.parametrize("drop", list(MATRIX))
def test_drop_relation_kind_matrix(drop: str, rust_pg: object) -> None:
    dsn = _dsn(rust_pg) + " dbname=postgres"
    assert drop_answer(dsn, drop) == MATRIX[drop]
