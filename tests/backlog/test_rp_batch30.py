"""Batch 30 of the Rust PG server (2026-10-22): the regression tests of
what it fixed -- RP-072's statements once refused (ALTER TABLE's storage
options, ALTER ROLE ... SET, REASSIGN OWNED, DROP OWNED), and RP-133 ..
RP-142, found while building them -- and the gap test of what it filed
open (RP-137).

As in `test_rp_batch26.py`: each scenario is a list of statements run in
one autocommit connection, and a statement's answer is its rows, its
command tag, or `("ERR", sqlstate)` (with the DETAIL, where the scenario
asks for it). The expected answers were measured on a private, freshly
initdb'd PostgreSQL 15.19 cluster by running this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch30.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _answer, _dsn, _marked


def _answer_detail(conn: Any, sql: str) -> Any:
    """`_answer`, with an error's DETAIL."""
    try:
        cur = conn.execute(sql)
    except psycopg.Error as e:
        return ("ERR", e.sqlstate, e.diag.message_detail)
    if cur.description is None:
        return cur.statusmessage
    return [tuple(str(v) for v in row) for row in cur.fetchall()]


def run_scenario(dsn: str, statements: list[str]) -> list[Any]:
    """Each statement's answer; a statement prefixed `!` reports its error's
    DETAIL too."""
    with psycopg.connect(dsn, autocommit=True) as conn:
        out = [
            _answer_detail(conn, s[1:]) if s.startswith("!") else _answer(conn, s)
            for s in statements
        ]
        if conn.info.transaction_status != psycopg.pq.TransactionStatus.IDLE:
            conn.execute("rollback")
        return out


# (item, open?, statements). `open` scenarios are gap tests: they fail while
# their item is open.
SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------------------- RP-072 statements once refused
    "ALTER TABLE storage options": (
        "RP-072",
        False,
        [
            "create table rp30_t (a int primary key, b text)",
            "alter table rp30_t set unlogged",
            "alter table rp30_t set (fillfactor = 70, autovacuum_enabled = false)",
            "alter table rp30_t alter column b set storage external",
            "alter table rp30_t alter column b set statistics 100",
            "select relpersistence, reloptions::text from pg_class where relname = 'rp30_t'",
            "select attname, attstorage, attstattarget from pg_attribute where attrelid = "
            "'rp30_t'::regclass and attnum > 0 order by attnum",
            "alter table rp30_t set logged",
            "alter table rp30_t reset (fillfactor)",
            "alter table rp30_t alter column b set statistics -1",
            "select relpersistence, reloptions::text from pg_class where relname = 'rp30_t'",
            "alter table rp30_t set (fillfactor = 5)",
            "alter table rp30_t set (nosuch = 1)",
            "alter table rp30_t alter column a set storage external",
            "alter table rp30_t alter column b set statistics -2",
            "drop table rp30_t",
        ],
    ),
    "ALTER ROLE SET": (
        "RP-072",
        False,
        [
            "create role rp30_r login",
            "alter role rp30_r set work_mem = '8MB'",
            "alter role rp30_r in database postgres set search_path = public",
            "select d.datname, r.rolname, s.setconfig::text from pg_db_role_setting s left join "
            "pg_database d on d.oid = s.setdatabase left join pg_roles r on r.oid = s.setrole "
            "order by 1 nulls first, 2 nulls first",
            "alter role rp30_r reset work_mem",
            "alter role rp30_nosuch set work_mem = '1MB'",
            "alter role rp30_r set rp30_nosuch = 1",
            "alter role rp30_r in database rp30_nosuch set work_mem = '1MB'",
            "select count(*) from pg_db_role_setting",
            "drop role rp30_r",
            "create role rp30_r",
            "select count(*) from pg_db_role_setting",
            "drop role rp30_r",
        ],
    ),
    # ------------------------------------------ RP-133 pg_settings units
    "pg_settings in the parameter's unit": (
        "RP-133",
        False,
        [
            "set work_mem = '8MB'",
            "set temp_buffers = '9MB'",
            "set vacuum_cost_delay = '1.5ms'",
            "set statement_timeout = '5s'",
            "select name, setting, unit, source from pg_settings where name in ('work_mem', "
            "'temp_buffers', 'vacuum_cost_delay', 'statement_timeout') order by 1",
            "reset all",
        ],
    ),
    "REASSIGN OWNED and DROP OWNED": (
        "RP-072",
        False,
        [
            "create role rp30_o1",
            "create role rp30_o2",
            "create schema rp30_s",
            "alter schema rp30_s owner to rp30_o1",
            "create table rp30_ot (a serial primary key)",
            "alter table rp30_ot owner to rp30_o1",
            "create view rp30_ov as select * from rp30_ot",
            "alter view rp30_ov owner to rp30_o1",
            "create sequence rp30_oq",
            "alter sequence rp30_oq owner to rp30_o1",
            "create function rp30_of(int, text) returns int language sql as 'select 1'",
            "alter function rp30_of(int, text) owner to rp30_o1",
            "create type rp30_oe as enum ('a')",
            "alter type rp30_oe owner to rp30_o1",
            "create domain rp30_od as int",
            "alter domain rp30_od owner to rp30_o1",
            "create table rp30_og (a int)",
            "grant select on rp30_og to rp30_o1",
            "reassign owned by rp30_o1 to rp30_o2",
            "select relname, relowner::regrole::text from pg_class where relname like 'rp30_o%' "
            "order by 1",
            "select proname, proowner::regrole::text from pg_proc where proname = 'rp30_of'",
            "select typname, typowner::regrole::text from pg_type where typname in "
            "('rp30_oe', '_rp30_oe', 'rp30_od', 'rp30_ot', 'rp30_ov') order by 1",
            "select nspowner::regrole::text from pg_namespace where nspname = 'rp30_s'",
            "!drop role rp30_o1",
            "drop owned by rp30_o1",
            "select relacl::text from pg_class where relname = 'rp30_og'",
            "drop role rp30_o1",
            "create view rp30_dep as select * from rp30_ot",
            "!drop owned by rp30_o2",
            "drop view rp30_dep",
            "drop owned by rp30_o2",
            "select count(*) from pg_class where relname like 'rp30_o%' and relname <> 'rp30_og'",
            "select count(*) from pg_namespace where nspname = 'rp30_s'",
            "select count(*) from pg_type where typname in ('rp30_oe', 'rp30_od')",
            "select count(*) from pg_proc where proname = 'rp30_of'",
            "drop role rp30_o2",
            "drop table rp30_og",
            "reassign owned by rp30_nosuch to postgres",
            "drop owned by rp30_nosuch",
            "reassign owned by postgres to postgres",
            "drop owned by postgres",
        ],
    ),
    "WHERE CURRENT OF a cursor not on a row": (
        "RP-072",
        True,
        [
            "create table rp30_w (a int)",
            "insert into rp30_w values (1)",
            "begin",
            "declare rp30_wc cursor for select * from rp30_w for update",
            "update rp30_w set a = 2 where current of rp30_wc",
            "rollback",
            "begin",
            "declare rp30_wc cursor for select * from rp30_w for update",
            "fetch next from rp30_wc",
            "fetch next from rp30_wc",
            "update rp30_w set a = 2 where current of rp30_wc",
            "rollback",
            "drop table rp30_w",
        ],
    ),
    "REASSIGN OWNED needs both roles' privileges": (
        "RP-072",
        False,
        [
            "create role rp30_p login",
            "set role rp30_p",
            "reassign owned by rp30_p to postgres",
            "drop owned by postgres",
            "reset role",
            "drop role rp30_p",
        ],
    ),
    # ------------------------------------------- RP-134 DROP ROLE checks
    "DROP ROLE of a role something depends on": (
        "RP-134",
        False,
        [
            "create role rp30_d",
            "create table rp30_dt (a int)",
            "alter table rp30_dt owner to rp30_d",
            "!drop role rp30_d",
            "alter table rp30_dt owner to postgres",
            "grant select on rp30_dt to rp30_d",
            "create schema rp30_ds",
            "grant usage on schema rp30_ds to rp30_d",
            "!drop role rp30_d",
            "revoke select on rp30_dt from rp30_d",
            "revoke usage on schema rp30_ds from rp30_d",
            "drop role rp30_d",
            "drop table rp30_dt",
            "drop schema rp30_ds",
        ],
    ),
    # ----------------------------------------- RP-135 owners that follow
    "an index, a row type and an array type follow their owner": (
        "RP-135",
        False,
        [
            "create role rp30_f",
            "create table rp30_ft (a serial primary key)",
            "create index rp30_fi on rp30_ft (a)",
            "alter table rp30_ft owner to rp30_f",
            "create type rp30_fc as (a int)",
            "alter type rp30_fc owner to rp30_f",
            "create view rp30_fv as select 1 as x",
            "alter view rp30_fv owner to rp30_f",
            "select relname, relowner::regrole::text from pg_class where relname like 'rp30_f%' "
            "order by 1",
            "select typname, typowner::regrole::text from pg_type where typname like '%rp30_f%' "
            "order by 1",
            "select count(*) from pg_shdepend where refobjid = 'rp30_f'::regrole",
            "drop view rp30_fv",
            "drop type rp30_fc",
            "drop table rp30_ft",
            "drop role rp30_f",
        ],
    ),
    # ------------------------------------ RP-136 ALTER ... OWNER TO forms
    "ALTER SEQUENCE and ALTER SCHEMA OWNER TO": (
        "RP-136",
        False,
        [
            "create role rp30_a",
            "create sequence rp30_aq",
            "alter sequence rp30_aq owner to rp30_a",
            "alter sequence if exists rp30_nosuch owner to rp30_a",
            "alter sequence rp30_nosuch owner to rp30_a",
            "create table rp30_at (a serial)",
            "!alter sequence rp30_at_a_seq owner to rp30_a",
            "!alter table rp30_at_a_seq owner to rp30_a",
            "create schema rp30_as",
            "alter schema rp30_as owner to rp30_a",
            "alter schema rp30_nosuch owner to rp30_a",
            "select relname, relowner::regrole::text from pg_class where relname like 'rp30_a%' "
            "order by 1",
            "select nspowner::regrole::text from pg_namespace where nspname = 'rp30_as'",
            "drop schema rp30_as",
            "drop table rp30_at",
            "drop sequence rp30_aq",
            "drop role rp30_a",
        ],
    ),
    # --------------------------------------------- RP-137 oids in order
    "DROP ROLE lists dependents in creation order": (
        "RP-137",
        True,
        [
            "create role rp30_c",
            "create sequence rp30_cq",
            "create table rp30_ct (a int)",
            "create view rp30_cv as select 1 as x",
            "alter sequence rp30_cq owner to rp30_c",
            "alter table rp30_ct owner to rp30_c",
            "alter view rp30_cv owner to rp30_c",
            "!drop role rp30_c",
            "drop view rp30_cv",
            "drop table rp30_ct",
            "drop sequence rp30_cq",
            "drop role rp30_c",
        ],
    ),
    # ------------------------------- RP-138, RP-142 routines' privileges
    "GRANT on a routine of the other kind": (
        "RP-138",
        False,
        [
            "create role rp30_g",
            "create procedure rp30_gp() language sql as 'select 1'",
            "create function rp30_gf(int, text) returns int language sql as 'select 1'",
            "grant execute on function rp30_gp() to rp30_g",
            "revoke all on function rp30_gp() from rp30_g",
            "grant execute on procedure rp30_gf(int, text) to rp30_g",
            "grant execute on routine rp30_gp(), rp30_gf(int, text) to rp30_g",
            "drop procedure rp30_gp()",
            "drop function rp30_gf(int, text)",
            "drop role rp30_g",
        ],
    ),
    "a routine's proacl": (
        "RP-142",
        False,
        [
            "create role rp30_h",
            "create function rp30_hf(int, text) returns int language sql as 'select 1'",
            "select proacl::text from pg_proc where proname = 'rp30_hf'",
            "grant execute on function rp30_hf(int, text) to rp30_h",
            "select proacl::text from pg_proc where proname = 'rp30_hf'",
            "revoke execute on function rp30_hf(int, text) from public",
            "grant execute on function rp30_hf(int, text) to public",
            "grant execute on function rp30_hf(int, text) to rp30_h with grant option",
            "select proacl::text from pg_proc where proname = 'rp30_hf'",
            "drop function rp30_hf(int, text)",
            "drop role rp30_h",
        ],
    ),
    # ------------------------------------------- RP-139 sequences in a block
    "a sequence made in the open block is a relation": (
        "RP-139",
        False,
        [
            "begin",
            "create table rp30_bt (a serial)",
            "select last_value, is_called from rp30_bt_a_seq",
            "create sequence rp30_bq",
            "select last_value from rp30_bq",
            "alter table rp30_bq owner to postgres",
            "drop sequence rp30_bq",
            "select last_value from rp30_bq",
            "rollback",
        ],
    ),
    # ------------------------------------------------- RP-140 sequence ACLs
    "a sequence's relacl": (
        "RP-140",
        False,
        [
            "create role rp30_q",
            "create sequence rp30_qq",
            "select relacl from pg_class where relname = 'rp30_qq'",
            "grant select, update on sequence rp30_qq to rp30_q with grant option",
            "grant usage on sequence rp30_qq to public",
            "select relacl::text from pg_class where relname = 'rp30_qq'",
            "revoke all on sequence rp30_qq from rp30_q",
            "select relacl::text from pg_class where relname = 'rp30_qq'",
            "drop sequence rp30_qq",
            "drop role rp30_q",
        ],
    ),
    # ------------------------------------------ RP-143, RP-144 schemas' routines
    "two schemas' routines of one name": (
        "RP-143",
        True,
        [
            "create schema rp30_za",
            "create schema rp30_zb",
            "create function rp30_za.h() returns int language sql as 'select 1'",
            "create function rp30_zb.h() returns int language sql as 'select 2'",
            "select rp30_za.h(), rp30_zb.h()",
            "drop schema rp30_za, rp30_zb cascade",
        ],
    ),
    "DROP SCHEMA drops the routines in it": (
        "RP-144",
        False,
        [
            "create schema rp30_zs",
            "create function rp30_zs.g(int, text) returns int language sql as 'select 3'",
            "!drop schema rp30_zs",
            "drop schema rp30_zs cascade",
            "select count(*) from pg_proc where proname = 'g'",
            "create function g(int, text) returns int language sql as 'select 4'",
            "drop function g(int, text)",
        ],
    ),
    # ------------------------------------------ RP-141 DDL in a PL/pgSQL body
    "a DO block drops a view, then its table": (
        "RP-141",
        False,
        [
            "create table rp30_lt (a int)",
            "create view rp30_lv as select * from rp30_lt",
            "do $$ begin drop view rp30_lv; drop table rp30_lt; end $$",
            "select count(*) from pg_class where relname in ('rp30_lt', 'rp30_lv')",
        ],
    ),
}


def role_setting_sources(dsn: str) -> list[Any]:
    """RP-072: a login takes every role's, the database's, the role's and
    the role's-in-this-database defaults, and pg_settings names each
    layer."""
    setup = (
        "create role rp30_src login",
        "alter role all set work_mem = '5MB'",
        "alter role rp30_src set maintenance_work_mem = '70MB'",
        "alter role rp30_src in database postgres set temp_buffers = '9MB'",
        "alter database postgres set default_statistics_target = 150",
    )
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as conn:
        for s in setup:
            _answer(conn, s)
    try:
        with psycopg.connect(
            dsn.replace("user=postgres", "user=rp30_src") + " dbname=postgres", autocommit=True
        ) as conn:
            seen = [
                _answer(conn, s)
                for s in (
                    "select name, setting, source, reset_val from pg_settings where name in "
                    "('work_mem', 'maintenance_work_mem', 'temp_buffers', "
                    "'default_statistics_target') order by 1",
                    "set work_mem = '6MB'",
                    "select source from pg_settings where name = 'work_mem'",
                    "reset work_mem",
                    "show work_mem",
                )
            ]
    finally:
        with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as conn:
            for s in (
                "alter role all reset all",
                "alter database postgres reset all",
                "drop role rp30_src",
            ):
                _answer(conn, s)
    return seen


def _scenario(name: str, server: Any) -> list[Any]:
    return run_scenario(_dsn(server) + " dbname=postgres", SCENARIOS[name][2])


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(name, marks=_marked(item, is_open), id=name)
        for name, (item, is_open, _) in sorted(SCENARIOS.items())
    ],
)
def test_scenario(name: str, rust_pg: Any) -> None:
    assert _scenario(name, rust_pg) == EXPECTED[name]


@pytest.mark.backlog_fixed("RP-072")
def test_role_setting_sources(rust_pg: Any) -> None:
    assert role_setting_sources(_dsn(rust_pg)) == EXPECTED["role setting sources"]


EXPECTED: dict[str, Any] = {
    "two schemas' routines of one name": [
        "CREATE SCHEMA",
        "CREATE SCHEMA",
        "CREATE FUNCTION",
        "CREATE FUNCTION",
        [("1", "2")],
        "DROP SCHEMA",
    ],
    "DROP SCHEMA drops the routines in it": [
        "CREATE SCHEMA",
        "CREATE FUNCTION",
        ("ERR", "2BP01", "function rp30_zs.g(integer,text) depends on schema rp30_zs"),
        "DROP SCHEMA",
        [("0",)],
        "CREATE FUNCTION",
        "DROP FUNCTION",
    ],
    "WHERE CURRENT OF a cursor not on a row": [
        "CREATE TABLE",
        "INSERT 0 1",
        "BEGIN",
        "DECLARE CURSOR",
        ("ERR", "24000"),
        "ROLLBACK",
        "BEGIN",
        "DECLARE CURSOR",
        [("1",)],
        [],
        ("ERR", "24000"),
        "ROLLBACK",
        "DROP TABLE",
    ],
    "ALTER ROLE SET": [
        "CREATE ROLE",
        "ALTER ROLE",
        "ALTER ROLE",
        [("None", "rp30_r", "{work_mem=8MB}"), ("postgres", "rp30_r", "{search_path=public}")],
        "ALTER ROLE",
        ("ERR", "42704"),
        ("ERR", "42704"),
        ("ERR", "3D000"),
        [("1",)],
        "DROP ROLE",
        "CREATE ROLE",
        [("0",)],
        "DROP ROLE",
    ],
    "ALTER SEQUENCE and ALTER SCHEMA OWNER TO": [
        "CREATE ROLE",
        "CREATE SEQUENCE",
        "ALTER SEQUENCE",
        "ALTER SEQUENCE",
        ("ERR", "42P01"),
        "CREATE TABLE",
        ("ERR", "0A000", 'Sequence "rp30_at_a_seq" is linked to table "rp30_at".'),
        ("ERR", "0A000", 'Sequence "rp30_at_a_seq" is linked to table "rp30_at".'),
        "CREATE SCHEMA",
        "ALTER SCHEMA",
        ("ERR", "3F000"),
        [("rp30_aq", "rp30_a"), ("rp30_at", "postgres"), ("rp30_at_a_seq", "postgres")],
        [("rp30_a",)],
        "DROP SCHEMA",
        "DROP TABLE",
        "DROP SEQUENCE",
        "DROP ROLE",
    ],
    "ALTER TABLE storage options": [
        "CREATE TABLE",
        "ALTER TABLE",
        "ALTER TABLE",
        "ALTER TABLE",
        "ALTER TABLE",
        [("u", "{fillfactor=70,autovacuum_enabled=false}")],
        [("a", "p", "-1"), ("b", "e", "100")],
        "ALTER TABLE",
        "ALTER TABLE",
        "ALTER TABLE",
        [("p", "{autovacuum_enabled=false}")],
        ("ERR", "22023"),
        ("ERR", "22023"),
        ("ERR", "0A000"),
        ("ERR", "22023"),
        "DROP TABLE",
    ],
    "DROP ROLE lists dependents in creation order": [
        "CREATE ROLE",
        "CREATE SEQUENCE",
        "CREATE TABLE",
        "CREATE VIEW",
        "ALTER SEQUENCE",
        "ALTER TABLE",
        "ALTER VIEW",
        (
            "ERR",
            "2BP01",
            "owner of sequence rp30_cq\nowner of table rp30_ct\nowner of view rp30_cv",
        ),
        "DROP VIEW",
        "DROP TABLE",
        "DROP SEQUENCE",
        "DROP ROLE",
    ],
    "DROP ROLE of a role something depends on": [
        "CREATE ROLE",
        "CREATE TABLE",
        "ALTER TABLE",
        ("ERR", "2BP01", "owner of table rp30_dt"),
        "ALTER TABLE",
        "GRANT",
        "CREATE SCHEMA",
        "GRANT",
        ("ERR", "2BP01", "privileges for table rp30_dt\nprivileges for schema rp30_ds"),
        "REVOKE",
        "REVOKE",
        "DROP ROLE",
        "DROP TABLE",
        "DROP SCHEMA",
    ],
    "GRANT on a routine of the other kind": [
        "CREATE ROLE",
        "CREATE PROCEDURE",
        "CREATE FUNCTION",
        ("ERR", "42809"),
        ("ERR", "42809"),
        ("ERR", "42809"),
        "GRANT",
        "DROP PROCEDURE",
        "DROP FUNCTION",
        "DROP ROLE",
    ],
    "REASSIGN OWNED and DROP OWNED": [
        "CREATE ROLE",
        "CREATE ROLE",
        "CREATE SCHEMA",
        "ALTER SCHEMA",
        "CREATE TABLE",
        "ALTER TABLE",
        "CREATE VIEW",
        "ALTER VIEW",
        "CREATE SEQUENCE",
        "ALTER SEQUENCE",
        "CREATE FUNCTION",
        "ALTER FUNCTION",
        "CREATE TYPE",
        "ALTER TYPE",
        "CREATE DOMAIN",
        "ALTER DOMAIN",
        "CREATE TABLE",
        "GRANT",
        "REASSIGN OWNED",
        [
            ("rp30_og", "postgres"),
            ("rp30_oq", "rp30_o2"),
            ("rp30_ot", "rp30_o2"),
            ("rp30_ot_a_seq", "rp30_o2"),
            ("rp30_ot_pkey", "rp30_o2"),
            ("rp30_ov", "rp30_o2"),
        ],
        [("rp30_of", "rp30_o2")],
        [
            ("_rp30_oe", "rp30_o2"),
            ("rp30_od", "rp30_o2"),
            ("rp30_oe", "rp30_o2"),
            ("rp30_ot", "rp30_o2"),
            ("rp30_ov", "rp30_o2"),
        ],
        [("rp30_o2",)],
        ("ERR", "2BP01", "privileges for table rp30_og"),
        "DROP OWNED",
        [("{postgres=arwdDxt/postgres}",)],
        "DROP ROLE",
        "CREATE VIEW",
        ("ERR", "2BP01", "view rp30_dep depends on table rp30_ot"),
        "DROP VIEW",
        "DROP OWNED",
        [("0",)],
        [("0",)],
        [("0",)],
        [("0",)],
        "DROP ROLE",
        "DROP TABLE",
        ("ERR", "42704"),
        ("ERR", "42704"),
        ("ERR", "2BP01"),
        ("ERR", "2BP01"),
    ],
    "REASSIGN OWNED needs both roles' privileges": [
        "CREATE ROLE",
        "SET",
        ("ERR", "42501"),
        ("ERR", "42501"),
        "RESET",
        "DROP ROLE",
    ],
    "a DO block drops a view, then its table": ["CREATE TABLE", "CREATE VIEW", "DO", [("0",)]],
    "a routine's proacl": [
        "CREATE ROLE",
        "CREATE FUNCTION",
        [("None",)],
        "GRANT",
        [("{=X/postgres,postgres=X/postgres,rp30_h=X/postgres}",)],
        "REVOKE",
        "GRANT",
        "GRANT",
        [("{postgres=X/postgres,rp30_h=X*/postgres,=X/postgres}",)],
        "DROP FUNCTION",
        "DROP ROLE",
    ],
    "a sequence made in the open block is a relation": [
        "BEGIN",
        "CREATE TABLE",
        [("1", "False")],
        "CREATE SEQUENCE",
        [("1",)],
        "ALTER TABLE",
        "DROP SEQUENCE",
        ("ERR", "42P01"),
        "ROLLBACK",
    ],
    "a sequence's relacl": [
        "CREATE ROLE",
        "CREATE SEQUENCE",
        [("None",)],
        "GRANT",
        "GRANT",
        [("{postgres=rwU/postgres,rp30_q=r*w*/postgres,=U/postgres}",)],
        "REVOKE",
        [("{postgres=rwU/postgres,=U/postgres}",)],
        "DROP SEQUENCE",
        "DROP ROLE",
    ],
    "an index, a row type and an array type follow their owner": [
        "CREATE ROLE",
        "CREATE TABLE",
        "CREATE INDEX",
        "ALTER TABLE",
        "CREATE TYPE",
        "ALTER TYPE",
        "CREATE VIEW",
        "ALTER VIEW",
        [
            ("rp30_fc", "rp30_f"),
            ("rp30_fi", "rp30_f"),
            ("rp30_ft", "rp30_f"),
            ("rp30_ft_a_seq", "rp30_f"),
            ("rp30_ft_pkey", "rp30_f"),
            ("rp30_fv", "rp30_f"),
        ],
        [
            ("_rp30_fc", "rp30_f"),
            ("_rp30_ft", "rp30_f"),
            ("_rp30_fv", "rp30_f"),
            ("rp30_fc", "rp30_f"),
            ("rp30_ft", "rp30_f"),
            ("rp30_fv", "rp30_f"),
        ],
        [("4",)],
        "DROP VIEW",
        "DROP TYPE",
        "DROP TABLE",
        "DROP ROLE",
    ],
    "pg_settings in the parameter's unit": [
        "SET",
        "SET",
        "SET",
        "SET",
        [
            ("statement_timeout", "5000", "ms", "session"),
            ("temp_buffers", "1152", "8kB", "session"),
            ("vacuum_cost_delay", "1.5", "ms", "session"),
            ("work_mem", "8192", "kB", "session"),
        ],
        "RESET",
    ],
    "role setting sources": [
        [
            ("default_statistics_target", "150", "database", "150"),
            ("maintenance_work_mem", "71680", "user", "71680"),
            ("temp_buffers", "1152", "database user", "1152"),
            ("work_mem", "5120", "global", "5120"),
        ],
        "SET",
        [("session",)],
        "RESET",
        [("5MB",)],
    ],
}


if __name__ == "__main__":
    import pprint

    dsn = sys.argv[1]
    measured = {
        name: run_scenario(dsn + " dbname=postgres", stmts)
        for name, (_, _, stmts) in SCENARIOS.items()
    }
    measured["role setting sources"] = role_setting_sources(dsn)
    print("EXPECTED: dict[str, Any] = " + pprint.pformat(measured, width=100, sort_dicts=True))
