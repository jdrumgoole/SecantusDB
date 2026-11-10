"""Gap tests of the divergences found while working RP-825 (a domain's
DEFAULT is the type's, not its column's): a column `CREATE TABLE ... (LIKE
t)` copies from a column of a domain is not one of the domain.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each test runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists l825a, l825b, t825",
    "drop domain if exists d825 cascade",
    "create domain d825 as int default 42 check (value > 0)",
    "create table t825 (k int, d d825)",
]
_TEARDOWN = 2


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:_TEARDOWN]:
            c.execute(q)


def _answers(dsn: str, sqls: list[str]) -> list[object]:
    out: list[object] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        for sql in sqls:
            try:
                cur = c.execute(sql)
                out.append(cur.fetchall() if cur.description else cur.statusmessage)
            except psycopg.Error as e:
                out.append(f"{e.sqlstate} {e.diag.message_primary}")
    return out


# `transformTableLikeClause` copies each column's `atttypid`, so a column of
# a domain stays one of it: an INSERT naming no `d` takes the domain's
# DEFAULT, the domain's CHECK refuses `-1`, and `information_schema` names
# the domain and no `column_default` (the domain's DEFAULT is the type's,
# which INCLUDING DEFAULTS does not copy, as it is no `pg_attrdef` row).
# Here the copy is typed by the domain's base: no DEFAULT without INCLUDING
# DEFAULTS, no CHECK, no `domain_name`, and with INCLUDING DEFAULTS the
# domain's DEFAULT is copied in as the column's own.
@pytest.mark.backlog("RP-839")
def test_a_like_copy_of_a_domain_column_is_one_of_the_domain(dsn: str) -> None:
    got = _answers(
        dsn,
        [
            "create table l825a (like t825)",
            "create table l825b (like t825 including defaults)",
            "insert into l825a (k) values (1)",
            "insert into l825b (k) values (1)",
            "insert into l825a values (2, -1)",
            "select * from l825a order by k",
            "select * from l825b",
            "select table_name, column_default, domain_name"
            " from information_schema.columns where table_name in"
            " ('l825a', 'l825b') and column_name = 'd' order by 1",
        ],
    )
    assert got == [
        "CREATE TABLE",
        "CREATE TABLE",
        "INSERT 0 1",
        "INSERT 0 1",
        '23514 value for domain d825 violates check constraint "d825_check"',
        [(1, 42)],
        [(1, 42)],
        [("l825a", None, "d825"), ("l825b", None, "d825")],
    ]
