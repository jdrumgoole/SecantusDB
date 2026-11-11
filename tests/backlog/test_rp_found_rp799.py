"""Gap tests of the divergences found while working RP-799 (a column ADD
COLUMN adds of a domain is typed and tagged as CREATE TABLE types one): a
column retyped off its domain still enforces it; a domain's DEFAULT is
listed as its column's; a domain over a domain does not take the inner
domain's DEFAULT; `information_schema.columns.data_type` of a domain over a
domain names its base. And RP-799's own regression test.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`, which each test runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop table if exists t799",
    "drop domain if exists dx2_799 cascade",
    "drop domain if exists dx799 cascade",
    "drop domain if exists dnd799 cascade",
    "drop domain if exists dpos799 cascade",
    "drop domain if exists dch799 cascade",
    "drop domain if exists dt799 cascade",
    "create domain dx799 as int default 42",
    "create domain dx2_799 as dx799",
    "create domain dnd799 as int not null default 5",
    "create domain dpos799 as int check (value > 0)",
    "create domain dch799 as char(3)",
    'create domain dt799 as text collate "C"',
]
_TEARDOWN = 7


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for q in _SETUP[:_TEARDOWN]:
            c.execute(q)


def _connect(dsn: str) -> psycopg.Connection:
    c = psycopg.connect(dsn, autocommit=True)
    for q in _SETUP:
        c.execute(q)
    return c


def _answer(c: psycopg.Connection, sql: str) -> object:
    try:
        cur = c.execute(sql)
        return cur.fetchall() if cur.description else cur.statusmessage
    except psycopg.Error as e:
        return f"{e.sqlstate} {e.diag.message_primary}"


def _answers(dsn: str, sqls: list[str]) -> list[object]:
    with _connect(dsn) as c:
        return [_answer(c, sql) for sql in sqls]


# `ATExecAddColumn` coerces the new column's DEFAULT, or NULL, into its
# domain for every row already there, and the column is otherwise one of the
# domain as CREATE TABLE declares it: the domain's DEFAULT fills the rows, a
# `char(n)` domain pads, a CHECK refuses a write, `information_schema` names
# the domain and a function or aggregate over the base takes it. Here the
# column was typed by the domain's name and tagged with nothing: no DEFAULT,
# no NOT NULL, no padding, and `string_agg` was 42883. Fixed by RP-799.
@pytest.mark.backlog_fixed("RP-799")
def test_an_added_domain_column_is_one_of_the_domain(dsn: str) -> None:
    got = _answers(
        dsn,
        [
            "create table t799 (k int)",
            "insert into t799 values (1), (2)",
            "alter table t799 add column n dnd799 default null",
            "alter table t799 add column a dt799",
            "alter table t799 add column d dx799",
            "alter table t799 add column e dch799",
            "alter table t799 add column p dpos799",
            "alter table t799 add column q dpos799 default 0",
            "alter table t799 add column f dnd799",
            "update t799 set a = 'x' || k, e = 'y'",
            "update t799 set p = -1",
            "update t799 set f = null",
            "select k, d, e, octet_length(e), f, pg_typeof(f) from t799 order by k",
            "select string_agg(a, ',' order by a), sum(d), max(e) from t799",
            "select column_name, data_type, domain_name, character_maximum_length,"
            " collation_name from information_schema.columns"
            " where table_name = 't799' order by ordinal_position",
            "alter table t799 alter column d type int",
            "drop table t799",
            "create table t799 (k int)",
            "alter table t799 add column f dnd799 default null",
            "insert into t799 values (1)",
            "insert into t799 values (1, 2)",
            "select * from t799",
        ],
    )
    assert got == [
        "CREATE TABLE",
        "INSERT 0 2",
        "23502 domain dnd799 does not allow null values",
        "ALTER TABLE",
        "ALTER TABLE",
        "ALTER TABLE",
        "ALTER TABLE",
        '23514 value for domain dpos799 violates check constraint "dpos799_check"',
        "ALTER TABLE",
        "UPDATE 2",
        '23514 value for domain dpos799 violates check constraint "dpos799_check"',
        "23502 domain dnd799 does not allow null values",
        [(1, 42, "y  ", 3, 5, "dnd799"), (2, 42, "y  ", 3, 5, "dnd799")],
        [("x1,x2", 84, "y  ")],
        [
            ("k", "integer", None, None, None),
            ("a", "text", "dt799", None, "C"),
            ("d", "integer", "dx799", None, None),
            ("e", "character", "dch799", 3, None),
            ("p", "integer", "dpos799", None, None),
            ("f", "integer", "dnd799", None, None),
        ],
        "ALTER TABLE",
        "DROP TABLE",
        "CREATE TABLE",
        "ALTER TABLE",
        "23502 domain dnd799 does not allow null values",
        "INSERT 0 1",
        [(1, 2)],
    ]


# `ATExecAlterColumnType` gives the column the new type outright: nothing of
# the domain it had is left, so its CHECK no longer applies and the domain
# can be dropped. Here the column kept its `domain_type` tag. Fixed by
# RP-824.
@pytest.mark.backlog_fixed("RP-824")
def test_a_column_retyped_off_its_domain_drops_it(dsn: str) -> None:
    got = _answers(
        dsn,
        [
            "create table t799 (a dpos799)",
            "alter table t799 alter column a type int",
            "insert into t799 values (-1)",
            "select a, pg_typeof(a) from t799",
            "select data_type, domain_name from information_schema.columns"
            " where table_name = 't799'",
            "drop domain dpos799",
            "drop table t799",
        ],
    )
    assert got == [
        "CREATE TABLE",
        "ALTER TABLE",
        "INSERT 0 1",
        [(-1, "integer")],
        [("integer", None)],
        "DROP DOMAIN",
        "DROP TABLE",
    ]


# `ATExecAlterColumnType` also takes the column's new collation from
# `GetColumnDefCollation`: the statement's COLLATE, else the new type's,
# checked before the cast. A domain's DEFAULT was the type's, so it goes
# with the domain; a DEFAULT the column declared stays. Here the column kept
# the domain's DEFAULT and the collation it had, its own or its domain's,
# and the COLLATE clause was ignored. Fixed by RP-824.
@pytest.mark.backlog_fixed("RP-824")
def test_a_retyped_column_takes_the_new_types_collation_and_default(dsn: str) -> None:
    got = _answers(
        dsn,
        [
            "create table t799 (k int, d dx799, n dnd799 default 7, a dt799,"
            ' c text collate "C", e text)',
            "alter table t799 alter column d type bigint, alter column n type int,"
            " alter column a type text, alter column c type text,"
            ' alter column e type text collate "C"',
            "insert into t799 (k) values (1)",
            "select * from t799",
            "select column_name, data_type, domain_name, column_default,"
            " collation_name from information_schema.columns"
            " where table_name = 't799' order by ordinal_position",
            'alter table t799 alter column k type text collate "C",'
            ' alter column e type int collate "C"',
            "drop domain dnd799",
            "drop domain dt799",
            "drop table t799",
        ],
    )
    assert got == [
        "CREATE TABLE",
        "ALTER TABLE",
        "INSERT 0 1",
        [(1, None, 7, None, None, None)],
        [
            ("k", "integer", None, None, None),
            ("d", "bigint", None, None, None),
            ("n", "integer", None, "7", None),
            ("a", "text", None, None, None),
            ("c", "text", None, None, None),
            ("e", "text", None, None, "C"),
        ],
        "42804 collations are not supported by type integer",
        "DROP DOMAIN",
        "DROP DOMAIN",
        "DROP TABLE",
    ]


# A domain's DEFAULT is the type's (`typdefault`), which
# `build_column_default` falls back to at INSERT; the column has no
# `pg_attrdef` row of its own. Here the domain's DEFAULT was copied into
# the column, and listed as its default. Fixed by RP-825.
@pytest.mark.backlog_fixed("RP-825")
def test_a_domains_default_is_not_its_columns(dsn: str) -> None:
    got = _answers(
        dsn,
        [
            "create table t799 (k int, d dx799)",
            "alter table t799 add column f dnd799",
            "insert into t799 (k) values (1)",
            "select * from t799",
            "select attname, pg_get_expr(adbin, adrelid) from pg_attribute"
            " left join pg_attrdef on adrelid = attrelid and adnum = attnum"
            " where attrelid = 't799'::regclass and attnum > 0 order by attnum",
            "select column_name, column_default from information_schema.columns"
            " where table_name = 't799' order by ordinal_position",
            "drop table t799",
        ],
    )
    assert got == [
        "CREATE TABLE",
        "ALTER TABLE",
        "INSERT 0 1",
        [(1, 42, 5)],
        [("k", None), ("d", None), ("f", None)],
        [("k", None), ("d", None), ("f", None)],
        "DROP TABLE",
    ]


# A column's own DEFAULT (`pg_attrdef`) is the one an INSERT takes, and an
# ALTER DOMAIN does not move it; DROP DEFAULT removes only it, so the
# domain's DEFAULT applies again, as it stands then. Here SET DEFAULT left
# the column following the domain (`3` took 9), and DROP DEFAULT left it
# with none (`4` was NULL). Measured on a private PostgreSQL 15.19 cluster;
# fixed by RP-825.
@pytest.mark.backlog_fixed("RP-825")
def test_a_columns_drop_default_leaves_its_domains(dsn: str) -> None:
    attrdef = (
        "select attname, atthasdef, pg_get_expr(adbin, adrelid) from pg_attribute"
        " left join pg_attrdef on adrelid = attrelid and adnum = attnum"
        " where attrelid = 't799'::regclass and attnum > 0 order by attnum"
    )
    got = _answers(
        dsn,
        [
            "create table t799 (k int, d dx799)",
            "insert into t799 (k) values (1)",
            "alter table t799 alter d set default 7",
            "insert into t799 (k) values (2)",
            attrdef,
            "alter domain dx799 set default 9",
            "insert into t799 (k) values (3)",
            "alter table t799 alter d drop default",
            "insert into t799 (k) values (4)",
            attrdef,
            "alter domain dx799 set default 11",
            "insert into t799 (k) values (5)",
            "select * from t799 order by k",
            "drop table t799",
        ],
    )
    assert got == [
        "CREATE TABLE",
        "INSERT 0 1",
        "ALTER TABLE",
        "INSERT 0 1",
        [("k", False, None), ("d", True, "7")],
        "ALTER DOMAIN",
        "INSERT 0 1",
        "ALTER TABLE",
        "INSERT 0 1",
        [("k", False, None), ("d", False, None)],
        "ALTER DOMAIN",
        "INSERT 0 1",
        [(1, 42), (2, 7), (3, 7), (4, 9), (5, 11)],
        "DROP TABLE",
    ]


# `DefineDomain` copies the base type's `typdefaultbin` into a domain over
# a domain that declares no DEFAULT of its own. Here only the outer
# domain's own DEFAULT was read, and the column was NULL. Fixed by RP-826.
@pytest.mark.backlog_fixed("RP-826")
def test_a_domain_over_a_domain_takes_its_default(dsn: str) -> None:
    got = _answers(
        dsn,
        [
            "create table t799 (k int, g dx2_799)",
            "insert into t799 (k) values (1)",
            "alter table t799 add column h dx2_799",
            "select * from t799",
            "drop table t799",
        ],
    )
    assert got == [
        "CREATE TABLE",
        "INSERT 0 1",
        "ALTER TABLE",
        [(1, 42, 42)],
        "DROP TABLE",
    ]


# RP-826's regression test. The inherited default is a COPY taken at CREATE
# DOMAIN: a later ALTER DOMAIN of the base does not move it, a domain over
# that domain copies it again, and the outer domain's own DEFAULT -- even
# `DEFAULT NULL` -- replaces it. `information_schema.domains` lists the copy
# as the domain's default (measured on a private PostgreSQL 15.19 cluster).
@pytest.mark.backlog_fixed("RP-826")
def test_a_domain_over_a_domain_copies_its_default_at_create(dsn: str) -> None:
    drops = [
        "drop table if exists t799",
        "drop domain if exists dx4_799",
        "drop domain if exists dx3_799",
        "drop domain if exists dn799",
    ]
    got = _answers(
        dsn,
        [
            *drops,
            "create domain dx3_799 as dx799 default 9",
            "create domain dn799 as dx799 default null",
            "alter domain dx799 set default 7",
            "create domain dx4_799 as dx2_799",
            "create table t799 (k int, a dx2_799, b dx3_799, c dn799, d dx799, e dx4_799)",
            "insert into t799 (k) values (1)",
            "select * from t799",
            "select domain_name, domain_default from information_schema.domains"
            " where domain_name in ('dx2_799', 'dx3_799', 'dx4_799') order by 1",
            *drops,
        ],
    )
    assert got[len(drops) :] == [
        "CREATE DOMAIN",
        "CREATE DOMAIN",
        "ALTER DOMAIN",
        "CREATE DOMAIN",
        "CREATE TABLE",
        "INSERT 0 1",
        [(1, 42, 9, None, 7, 42)],
        [("dx2_799", "42"), ("dx3_799", "9"), ("dx4_799", "42")],
        "DROP TABLE",
        "DROP DOMAIN",
        "DROP DOMAIN",
        "DROP DOMAIN",
    ]


# `information_schema.columns.data_type` of a domain column is its
# domain's IMMEDIATE base type's, which for a domain over a domain is a
# domain itself: `USER-DEFINED`. Here it named the ultimate base,
# `integer`. Fixed by RP-827.
@pytest.mark.backlog_fixed("RP-827")
def test_data_type_of_a_domain_over_a_domain_is_user_defined(dsn: str) -> None:
    got = _answers(
        dsn,
        [
            "create table t799 (g dx2_799)",
            "select data_type, domain_name from information_schema.columns"
            " where table_name = 't799'",
            "drop table t799",
        ],
    )
    assert got == [
        "CREATE TABLE",
        [("USER-DEFINED", "dx2_799")],
        "DROP TABLE",
    ]


# RP-827's regression test. Every type-derived column of
# `information_schema.columns` reads the domain's immediate base
# (`typbasetype`): for a domain over a domain, `udt_name` is the inner
# domain and the lengths and precisions are NULL, whatever the inner
# domain is over; a domain over a built-in keeps its base's. Measured on a
# private PostgreSQL 15.19 cluster.
@pytest.mark.backlog_fixed("RP-827")
def test_a_domain_over_a_domain_column_is_typed_by_the_inner_domain(dsn: str) -> None:
    domains = [
        ("dch827", "char(3)"),
        ("dch2_827", "dch827"),
        ("dn827", "numeric(5,2)"),
        ("dn2_827", "dn827"),
        ("darr827", "int[]"),
        ("darr2_827", "darr827"),
        ("dts827", "timestamp(3)"),
        ("dts2_827", "dts827"),
        ("dt827", 'text collate "C"'),
        ("dt2_827", "dt827"),
    ]
    drops = [
        "drop table if exists t799",
        *(f"drop domain if exists {n}" for n, _ in reversed(domains)),
    ]
    got = _answers(
        dsn,
        [
            *drops,
            *(f"create domain {n} as {b}" for n, b in domains),
            "create table t799 (a dx2_799, b dch2_827, c dn2_827, d darr2_827,"
            " f dts2_827, h dt2_827, i dx799, j dch827)",
            "alter table t799 add column k dx2_799",
            "select column_name, data_type, udt_schema, udt_name,"
            " character_maximum_length, character_octet_length, numeric_precision,"
            " numeric_precision_radix, numeric_scale, datetime_precision,"
            " collation_name, domain_name from information_schema.columns"
            " where table_name = 't799' order by ordinal_position",
            *drops,
        ],
    )
    n = len(drops) + len(domains) + 2
    assert got[n] == [
        ("a", "USER-DEFINED", "public", "dx799", *[None] * 7, "dx2_799"),
        ("b", "USER-DEFINED", "public", "dch827", *[None] * 7, "dch2_827"),
        ("c", "USER-DEFINED", "public", "dn827", *[None] * 7, "dn2_827"),
        ("d", "USER-DEFINED", "public", "darr827", *[None] * 7, "darr2_827"),
        ("f", "USER-DEFINED", "public", "dts827", *[None] * 7, "dts2_827"),
        ("h", "USER-DEFINED", "public", "dt827", *[None] * 6, "C", "dt2_827"),
        ("i", "integer", "pg_catalog", "int4", None, None, 32, 2, 0, None, None, "dx799"),
        ("j", "character", "pg_catalog", "bpchar", 3, 12, *[None] * 5, "dch827"),
        ("k", "USER-DEFINED", "public", "dx799", *[None] * 7, "dx2_799"),
    ]
