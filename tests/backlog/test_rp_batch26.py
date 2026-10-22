"""Batch 26 of the Rust PG server (2026-10-22): the regression tests of what
it fixed (RP-070's catalogs and system views, RP-071's client tools, and
RP-092..RP-097, found while making pg_dump's output and restore match), and
the gap tests of what it filed (RP-098..RP-112).

Each scenario is a list of statements run in one autocommit connection; a
statement's answer is its rows, its command tag, or `("ERR", sqlstate)`.
The expected answers are literals measured on PostgreSQL 15.19. The
scenarios create roles and databases, so they were measured on a private,
freshly initdb'd 15.19 cluster (`initdb --locale=C.UTF-8 -U postgres`, TCP
only) by running this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch26.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys
from typing import Any

import psycopg
import pytest

HERE = pathlib.Path(__file__).parent

# (item, open?, statements). `open` scenarios are gap tests: they fail while
# their item is open.
SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ----------------------------------------------------- RP-070 catalogs
    "built-in catalog rows": (
        "RP-070",
        False,
        [
            "select count(*) from pg_aggregate where aggfnoid::oid < 16384",
            "select count(*) from pg_amop",
            "select count(*) from pg_amproc",
            "select count(*) from pg_opfamily",
            "select count(*) from pg_conversion",
            "select count(*) from information_schema.sql_features",
            "select initprivs::text from pg_init_privs where objoid = 2200",
            "select attacl::text from pg_attribute where attrelid = 'pg_subscription'::regclass "
            "and attname = 'subname'",
        ],
    ),
    "empty catalogs": (
        "RP-070",
        False,
        [
            "select count(*) from pg_transform",
            "select count(*) from pg_seclabel",
            "select count(*) from pg_shseclabel",
            "select count(*) from pg_subscription",
            "select count(*) from pg_replication_slots",
            "select count(*) from pg_stat_replication",
            "select count(*) from pg_stat_wal_receiver",
        ],
    ),
    "shared comments": (
        "RP-070",
        False,
        [
            "select description from pg_shdescription d join pg_database b on b.oid = d.objoid "
            "where b.datname = 'template0'",
            "create role rp26_commented",
            "comment on role rp26_commented is 'a role'",
            "select description from pg_shdescription d join pg_roles r on r.oid = d.objoid "
            "where r.rolname = 'rp26_commented'",
            "comment on database rp26_nosuch is 'x'",
            "comment on role rp26_nosuch is 'x'",
            "drop role rp26_commented",
            "select description from pg_description where objoid = 2200",
            "select nspacl::text from pg_namespace where nspname in ('public', 'pg_catalog') "
            "order by 1",
        ],
    ),
    "predefined roles": (
        "RP-070",
        False,
        [
            "select rolname from pg_roles where oid < 16384 order by oid",
            "select r.rolname, m.rolname from pg_auth_members a "
            "join pg_roles r on r.oid = a.roleid "
            "join pg_roles m on m.oid = a.member order by 1",
            "create role pg_rp26",
            "alter role pg_monitor login",
            "drop role pg_monitor",
            "select pg_has_role(6171::oid, 'USAGE'), pg_has_role(99999::oid, 'MEMBER')",
            "select role_name from information_schema.applicable_roles order by 1",
        ],
    ),
    "information_schema views": (
        "RP-070",
        False,
        [
            "create domain rp26_dom as int check (value > 0)",
            "create table rp26_p (id int primary key, n varchar(20) unique, d rp26_dom)",
            "create table rp26_c (id serial primary key, pid int references rp26_p (id) "
            "deferrable initially deferred, s smallint not null)",
            "select table_name, table_type, is_insertable_into, is_typed from "
            "information_schema.tables where table_name like 'rp26%' order by 1",
            "select regexp_replace(constraint_name, '^[0-9]+_[0-9]+_', 'N_'), constraint_type, "
            "is_deferrable, initially_deferred, enforced, nulls_distinct from "
            "information_schema.table_constraints where table_name = 'rp26_c' order by 1",
            "select constraint_name, column_name, ordinal_position, position_in_unique_constraint "
            "from information_schema.key_column_usage where table_name like 'rp26%' order by 1, 2",
            "select sequence_name, data_type, numeric_precision, numeric_precision_radix, "
            "numeric_scale from information_schema.sequences where sequence_name like 'rp26%'",
            "select domain_name, data_type, numeric_precision, udt_name, dtd_identifier from "
            "information_schema.domains where domain_name = 'rp26_dom'",
            "select constraint_name, check_clause from information_schema.check_constraints "
            "where constraint_name = 'rp26_dom_check'",
            "select table_name, column_name from information_schema.constraint_column_usage "
            "where table_name like 'rp26%' order by 1, 2",
            "select schema_name from information_schema.schemata where schema_name = 'public'",
            "select information_schema._pg_char_max_length(1043, 14), "
            "information_schema._pg_numeric_precision(1700, 655366)",
            "drop table rp26_c, rp26_p",
            "drop domain rp26_dom",
        ],
    ),
    "available extensions": (
        "RP-070",
        False,
        [
            "select name, default_version from pg_available_extensions where name in "
            "('btree_gist', 'citext', 'plpgsql') order by 1",
            # Each extension's default version: PostgreSQL lists every
            # version script it ships, and this server installs only that one.
            "select v.name, v.version, v.superuser, v.trusted, v.relocatable, v.schema from "
            "pg_available_extension_versions v join pg_available_extensions e on e.name = v.name "
            "and e.default_version = v.version where v.name in ('hstore', 'plpgsql') order by 1",
        ],
    ),
    "stats views on a primary": (
        "RP-070",
        False,
        [
            "select gss_authenticated, principal, encrypted from pg_stat_gssapi "
            "where pid = pg_backend_pid()",
            "select confl_lock, confl_deadlock from pg_stat_database_conflicts "
            "where datname = current_database()",
            "select prefetch, block_distance from pg_stat_recovery_prefetch",
        ],
    ),
    "the internals are refused": (
        "RP-070",
        False,
        [
            "select * from pg_stat_bgwriter",
            "select * from pg_backend_memory_contexts",
        ],
    ),
    # ----------------------------------------------- RP-071 client tools
    "name subscripts": (
        "RP-071",
        False,
        [
            "select ('abc'::name)[0], ('abc'::name)[2], ('abc'::name)[3] = '', "
            "('abc'::name)[64] is null, pg_typeof(('abc'::name)[0])",
            "select ('abc'::name)[0:1]",
        ],
    ),
    "FROM functions of the outer row": (
        "RP-071",
        False,
        [
            "select (select max(x) from unnest(a) x) from (values (array[1, 5])) v(a)",
            "select array(select x * 2 from unnest(a) as t(x)) from (values (array[1, 2])) v(a)",
        ],
    ),
    "names under an empty search_path": (
        "RP-071",
        False,
        [
            "create type rp26_mood as enum ('a', 'b')",
            "create table rp26_q (id serial primary key, m rp26_mood default 'a')",
            "create view rp26_v as select id from rp26_q",
            "select pg_catalog.set_config('search_path', '', false)",
            "select pg_catalog.format_type('public.rp26_mood'::regtype, null)",
            "select pg_catalog.pg_get_expr(adbin, adrelid) from pg_catalog.pg_attrdef "
            "where adrelid = 'public.rp26_q'::regclass order by adnum",
            "select pg_catalog.pg_get_viewdef('public.rp26_v'::regclass)",
            "select deptype from pg_catalog.pg_depend where classid = 'pg_class'::regclass "
            "and refobjid = 'public.rp26_q'::regclass and objid = 'public.rp26_q_id_seq'::regclass",
            "reset search_path",
            "drop view rp26_v",
            "drop table rp26_q",
            "drop type rp26_mood",
        ],
    ),
    "identity added by ALTER": (
        "RP-071",
        False,
        [
            "create table rp26_i (a int, b int not null, c int not null default 1, "
            "d text not null)",
            "alter table rp26_i alter column a add generated always as identity",
            "alter table rp26_i alter column c add generated always as identity",
            "alter table rp26_i alter column d add generated always as identity",
            "alter table rp26_i alter column b add generated by default as identity "
            "(start 10 sequence name rp26_i_custom)",
            "select pg_get_serial_sequence('rp26_i', 'b'), attidentity from pg_attribute "
            "where attrelid = 'rp26_i'::regclass and attname = 'b'",
            "insert into rp26_i (b, c, d) values (default, 1, 'x') returning b",
            "drop table rp26_i",
        ],
    ),
    "type owners": (
        "RP-071",
        False,
        [
            "create role rp26_owner",
            "create type rp26_t as enum ('x')",
            "alter type rp26_t owner to rp26_owner",
            "select typowner::regrole::text from pg_type where typname = 'rp26_t'",
            "alter type rp26_nosuch owner to rp26_owner",
            "drop type rp26_t",
            "drop role rp26_owner",
        ],
    ),
    # ------------------------------------------- RP-092..RP-097, fixed here
    "a renamed table keeps its row type": (
        "RP-093",
        False,
        [
            "create table rp26_r (a int)",
            "alter table rp26_r rename to rp26_r2",
            "select 'rp26_r2'::regclass::text, to_regclass('rp26_r') is null",
            "select typname from pg_type where typname like 'rp26_r%' and typtype = 'c'",
            "begin",
            "alter table rp26_r2 rename to rp26_r3",
            "rollback",
            "select 'rp26_r2'::regclass::text",
            "drop table rp26_r2",
        ],
    ),
    "set operations resolve one type": (
        "RP-095",
        False,
        [
            "select x, pg_typeof(x) from (select 1::oid as x union all "
            "select 'pg_class'::regclass) s",
            "select x::text, pg_typeof(x) from (select 'pg_class'::regclass as x union all "
            "select 1::oid) s",
            "select x::text, pg_typeof(x) from (select 1::int4 as x union all "
            "select 'pg_class'::regclass) s",
            "select 1.5::numeric union all select 1::oid",
            "select array[1] union all select array['a']",
            "select x, length(x) from (select 'a'::text as x union all select 'a'::char(2)) s",
            "select x, pg_typeof(x) from (select 1::int4 as x union select 2::int8) s order by 1",
        ],
    ),
    "materialized views": (
        "RP-096",
        False,
        [
            "create materialized view rp26_m as select 1 as x with no data",
            "select * from rp26_m",
            "comment on materialized view rp26_m is 'mv'",
            "select obj_description('rp26_m'::regclass, 'pg_class')",
            "select pg_catalog.set_config('search_path', '', false)",
            "refresh materialized view public.rp26_m",
            "reset search_path",
            "alter materialized view rp26_m rename to rp26_m2",
            "select * from rp26_m2",
            "drop materialized view rp26_m2",
        ],
    ),
    "ALTER TABLE on a sequence": (
        "RP-097",
        False,
        [
            "create sequence rp26_s",
            "alter table rp26_s owner to current_user",
            "alter table rp26_s add column x int",
            "select relkind from pg_class where relname = 'rp26_s'",
            "select nextval('rp26_s')",
            "drop sequence rp26_s",
        ],
    ),
    # ---------------------------------------------------- filed, still open
    "a record's fields through a subquery": (
        "RP-098",
        True,
        [
            "select count(*) >= 0 from information_schema.element_types",
            "select pg_typeof(n)::text from (select (ss.x).n from (select "
            "information_schema._pg_expandarray(array[7]) as x) ss) t",
        ],
    ),
    "ANALYZE computes statistics": (
        "RP-099",
        True,
        [
            "create table rp26_a (x int)",
            "insert into rp26_a values (1), (1), (null)",
            "analyze rp26_a",
            "select attname, null_frac, n_distinct, most_common_vals::text from pg_stats "
            "where tablename = 'rp26_a'",
            "drop table rp26_a",
        ],
    ),
    "progress views": (
        "RP-100",
        True,
        [
            "select count(*) from pg_stat_progress_vacuum",
            "select count(*) from pg_stat_progress_copy",
        ],
    ),
    "configuration-file views": (
        "RP-101",
        True,
        [
            "select count(*) > 0 from pg_hba_file_rules",
            "select count(*) >= 0 from pg_ident_file_mappings",
            "select count(*) > 0 from pg_config",
            "select count(*) > 0 from pg_file_settings",
        ],
    ),
    "built-in objects in the catalogs": (
        "RP-102",
        True,
        [
            "select count(*) from pg_proc where pronamespace = 'pg_catalog'::regnamespace",
            "select count(*) from pg_type where typrelid <> 0 and typnamespace in "
            "('pg_catalog'::regnamespace, 'information_schema'::regnamespace)",
            "select count(*) from pg_constraint where connamespace = 'pg_catalog'::regnamespace",
        ],
    ),
    "schema privileges": (
        "RP-103",
        True,
        [
            "begin",
            "grant create on schema public to public",
            "select nspacl::text from pg_namespace where nspname = 'public'",
            "rollback",
        ],
    ),
    "owners of what another role creates": (
        "RP-104",
        True,
        [
            "create role rp26_maker",
            "create schema rp26_sch authorization rp26_maker",
            "select nspowner::regrole::text from pg_namespace where nspname = 'rp26_sch'",
            "grant create on schema public to rp26_maker",
            "set role rp26_maker",
            "create type rp26_made as enum ('x')",
            "reset role",
            "select typowner::regrole::text from pg_type where typname = 'rp26_made'",
            "drop type rp26_made",
            "revoke create on schema public from rp26_maker",
            "drop schema rp26_sch",
            "drop role rp26_maker",
        ],
    ),
    "dependencies beyond relations": (
        "RP-105",
        True,
        [
            "create table rp26_d (a int check (a > 0), b int)",
            "create index rp26_d_expr on rp26_d ((a + b))",
            "select count(*) from pg_depend where classid = 'pg_constraint'::regclass "
            "and refobjid = 'rp26_d'::regclass",
            "select refobjsubid from pg_depend where objid = 'rp26_d_expr'::regclass order by 1",
            "drop table rp26_d",
        ],
    ),
    "an unknown name in a FROM function": (
        "RP-106",
        True,
        [
            "select (select max(x) from unnest(a) x) from (values (array[1, 5])) v(b)",
        ],
    ),
    "expression columns' types": (
        "RP-107",
        True,
        [
            "create table rp26_e (a int default 1)",
            "select pg_typeof(adbin)::text from pg_attrdef where adrelid = 'rp26_e'::regclass",
            "select pg_typeof(attmissingval)::text from pg_attribute "
            "where attrelid = 'rp26_e'::regclass and attname = 'a'",
            "drop table rp26_e",
        ],
    ),
    "the server's version": (
        "RP-108",
        False,
        [
            "select current_setting('server_version_num')",
            "select split_part(current_setting('server_version'), ' ', 1)",
        ],
    ),
    "a merged inherited column": (
        "RP-109",
        True,
        [
            "create table rp26_b (x int)",
            "create table rp26_k (x int, y int) inherits (rp26_b)",
            "select attname, attislocal, attinhcount from pg_attribute where attrelid = "
            "'rp26_k'::regclass and attnum > 0 order by attnum",
            "drop table rp26_k, rp26_b",
        ],
    ),
    "a type off the search path": (
        "RP-111",
        True,
        [
            "create type rp26_hidden as enum ('a')",
            "select pg_catalog.set_config('search_path', '', false)",
            "select 'rp26_hidden'::regtype",
            "select pg_catalog.to_regtype('rp26_hidden') is null",
            "reset search_path",
            "drop type rp26_hidden",
        ],
    ),
    "WAL and file functions": (
        "RP-112",
        True,
        [
            "select pg_current_wal_lsn() is not null",
            "select pg_relation_filepath('pg_class')",
        ],
    ),
    "a large object's pages": (
        "RP-110",
        True,
        [
            "select lo_from_bytea(260011, '\\x01')",
            "select lo_put(260011, 10000, '\\x02')",
            "select pageno, length(data) from pg_largeobject where loid = 260011 order by 1",
            "select lo_unlink(260011)",
        ],
    ),
}


def _answer(conn: Any, sql: str) -> Any:
    try:
        cur = conn.execute(sql)
    except psycopg.Error as e:
        return ("ERR", e.sqlstate)
    if cur.description is None:
        return cur.statusmessage
    return [tuple(str(v) for v in row) for row in cur.fetchall()]


def run_scenario(dsn: str, statements: list[str]) -> list[Any]:
    with psycopg.connect(dsn, autocommit=True) as conn:
        out = [_answer(conn, s) for s in statements]
        if conn.info.transaction_status != psycopg.pq.TransactionStatus.IDLE:
            conn.execute("rollback")
        return out


def memberships(dsn: str) -> list[Any]:
    """RP-092: a role membership is the cluster's, seen from every database."""
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as conn:
        setup = [
            _answer(conn, s)
            for s in (
                "create role rp26_group",
                "create role rp26_member",
                "grant rp26_group to rp26_member",
                "create database rp26_other",
            )
        ]
    with psycopg.connect(dsn + " dbname=rp26_other", autocommit=True) as conn:
        seen = _answer(
            conn,
            "select pg_has_role('rp26_member', 'rp26_group', 'MEMBER'), (select count(*) from "
            "pg_auth_members a join pg_roles r on r.oid = a.roleid where r.rolname = 'rp26_group')",
        )
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as conn:
        for s in ("drop database rp26_other", "drop role rp26_member", "drop role rp26_group"):
            _answer(conn, s)
    return [setup, seen]


def copy_table(dsn: str) -> list[str]:
    """RP-094: a table's COPY TO writes what a SELECT of it shows."""
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as conn:
        conn.execute("set timezone = 'UTC'")
        conn.execute("create temp table rp26_copy (t timestamptz, u timestamp, i interval)")
        conn.execute(
            "insert into rp26_copy values ('2020-01-01 00:00+00', '2020-01-01 01:00', '1 day')"
        )
        out = []
        for sql in ("copy rp26_copy to stdout", "copy rp26_copy to stdout (format csv)"):
            with conn.cursor().copy(sql) as copy:
                out.append("".join(bytes(b).decode() for b in copy))
        return out


SCHEMA = HERE / "rp071_dump_schema.sql"


def load_schema(port: int, pg15_bin: pathlib.Path) -> None:
    """The schema into a fresh `rp26_dump`, through psql (it has `$$` bodies)."""
    with psycopg.connect(
        f"host=127.0.0.1 port={port} user=postgres dbname=postgres", autocommit=True
    ) as conn:
        conn.execute("create database rp26_dump")
    subprocess.run(
        [
            str(pg15_bin / "psql"),
            "-h",
            "127.0.0.1",
            "-p",
            str(port),
            "-U",
            "postgres",
            "-X",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-f",
            str(SCHEMA),
            "rp26_dump",
        ],
        capture_output=True,
        check=True,
    )


def drop_schema(port: int) -> None:
    with psycopg.connect(
        f"host=127.0.0.1 port={port} user=postgres dbname=postgres", autocommit=True
    ) as conn:
        conn.execute("drop database if exists rp26_dump")
        conn.execute("drop role if exists rp26_reader")


def pg_dump_text(port: int, pg15_bin: pathlib.Path) -> str:
    """RP-071: pg_dump of a schema with most kinds of object, header line and
    restrict key dropped."""
    out = subprocess.run(
        [
            str(pg15_bin / "pg_dump"),
            "-h",
            "127.0.0.1",
            "-p",
            str(port),
            "-U",
            "postgres",
            "rp26_dump",
        ],
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    return "".join(
        line
        for line in out.splitlines(keepends=True)
        if not line.startswith(("\\restrict", "\\unrestrict", "-- Dumped from", "-- Dumped by"))
    )


def _pg15_bin() -> pathlib.Path:
    from tests.backlog.test_rp_found_2026_10_20 import _pg15_bin as find

    return find()


# Decided (Joe, 2026-10-22): the server's internals views are refused by
# name (0A000), so that one answer is the decision, not PostgreSQL's rows.
EXPECTED: dict[str, Any] = {
    "ALTER TABLE on a sequence": [
        "CREATE SEQUENCE",
        "ALTER TABLE",
        ("ERR", "42809"),
        [("S",)],
        [("1",)],
        "DROP SEQUENCE",
    ],
    "ANALYZE computes statistics": [
        "CREATE TABLE",
        "INSERT 0 3",
        "ANALYZE",
        [("x", "0.33333334", "-0.33333334", "{1}")],
        "DROP TABLE",
    ],
    "FROM functions of the outer row": [[("5",)], [("[2, 4]",)]],
    "WAL and file functions": [[("True",)], [("base/5/1259",)]],
    "a large object's pages": [[("260011",)], [("",)], [("0", "1"), ("4", "1809")], [("1",)]],
    "a merged inherited column": [
        "CREATE TABLE",
        "CREATE TABLE",
        [("x", "True", "1"), ("y", "True", "0")],
        "DROP TABLE",
    ],
    "a record's fields through a subquery": [[("True",)], [("integer",)]],
    "a renamed table keeps its row type": [
        "CREATE TABLE",
        "ALTER TABLE",
        [("rp26_r2", "True")],
        [("rp26_r2",)],
        "BEGIN",
        "ALTER TABLE",
        "ROLLBACK",
        [("rp26_r2",)],
        "DROP TABLE",
    ],
    "a type off the search path": [
        "CREATE TYPE",
        [("",)],
        ("ERR", "42704"),
        [("True",)],
        "RESET",
        "DROP TYPE",
    ],
    "an unknown name in a FROM function": [("ERR", "42703")],
    "available extensions": [
        [("btree_gist", "1.7"), ("citext", "1.6"), ("plpgsql", "1.0")],
        [
            ("hstore", "1.8", "True", "True", "True", "None"),
            ("plpgsql", "1.0", "True", "True", "False", "pg_catalog"),
        ],
    ],
    "built-in catalog rows": [
        [("148",)],
        [("945",)],
        [("696",)],
        [("146",)],
        [("128",)],
        [("714",)],
        [("{pg_database_owner=UC/pg_database_owner,=U/pg_database_owner}",)],
        [("{=r/postgres}",)],
    ],
    "built-in objects in the catalogs": [[("3233",)], [("208",)], [("109",)]],
    "configuration-file views": [[("True",)], [("True",)], [("True",)], [("True",)]],
    "copy table": [
        "2020-01-01 00:00:00+00\t2020-01-01 01:00:00\t1 day\n",
        "2020-01-01 00:00:00+00,2020-01-01 01:00:00,1 day\n",
    ],
    "dependencies beyond relations": [
        "CREATE TABLE",
        "CREATE INDEX",
        [("2",)],
        [("0",), ("1",), ("2",)],
        "DROP TABLE",
    ],
    "empty catalogs": [[("0",)], [("0",)], [("0",)], [("0",)], [("0",)], [("0",)], [("0",)]],
    "expression columns' types": [
        "CREATE TABLE",
        [("pg_node_tree",)],
        [("anyarray",)],
        "DROP TABLE",
    ],
    "identity added by ALTER": [
        "CREATE TABLE",
        ("ERR", "55000"),
        ("ERR", "55000"),
        ("ERR", "22023"),
        "ALTER TABLE",
        [("public.rp26_i_custom", "d")],
        [("10",)],
        "DROP TABLE",
    ],
    "information_schema views": [
        "CREATE DOMAIN",
        "CREATE TABLE",
        "CREATE TABLE",
        [("rp26_c", "BASE TABLE", "YES", "NO"), ("rp26_p", "BASE TABLE", "YES", "NO")],
        [
            ("N_1_not_null", "CHECK", "NO", "NO", "YES", "None"),
            ("N_3_not_null", "CHECK", "NO", "NO", "YES", "None"),
            ("rp26_c_pid_fkey", "FOREIGN KEY", "YES", "YES", "YES", "None"),
            ("rp26_c_pkey", "PRIMARY KEY", "NO", "NO", "YES", "None"),
        ],
        [
            ("rp26_c_pid_fkey", "pid", "1", "1"),
            ("rp26_c_pkey", "id", "1", "None"),
            ("rp26_p_n_key", "n", "1", "None"),
            ("rp26_p_pkey", "id", "1", "None"),
        ],
        [("rp26_c_id_seq", "integer", "32", "2", "0")],
        [("rp26_dom", "integer", "32", "int4", "1")],
        [("rp26_dom_check", "((VALUE > 0))")],
        [("rp26_c", "id"), ("rp26_p", "id"), ("rp26_p", "id"), ("rp26_p", "n")],
        [("public",)],
        [("10", "10")],
        "DROP TABLE",
        "DROP DOMAIN",
    ],
    "materialized views": [
        "CREATE MATERIALIZED VIEW",
        ("ERR", "55000"),
        "COMMENT",
        [("mv",)],
        [("",)],
        "REFRESH MATERIALIZED VIEW",
        "RESET",
        "ALTER MATERIALIZED VIEW",
        [("1",)],
        "DROP MATERIALIZED VIEW",
    ],
    "memberships": [
        ["CREATE ROLE", "CREATE ROLE", "GRANT ROLE", "CREATE DATABASE"],
        [("True", "1")],
    ],
    "name subscripts": [[("a", "c", "True", "True", '"char"')], ("ERR", "0A000")],
    "names under an empty search_path": [
        "CREATE TYPE",
        "CREATE TABLE",
        "CREATE VIEW",
        [("",)],
        [("public.rp26_mood",)],
        [("nextval('public.rp26_q_id_seq'::regclass)",), ("'a'::public.rp26_mood",)],
        [(" SELECT rp26_q.id\n   FROM public.rp26_q;",)],
        [("a",)],
        "RESET",
        "DROP VIEW",
        "DROP TABLE",
        "DROP TYPE",
    ],
    "owners of what another role creates": [
        "CREATE ROLE",
        "CREATE SCHEMA",
        [("rp26_maker",)],
        "GRANT",
        "SET",
        "CREATE TYPE",
        "RESET",
        [("rp26_maker",)],
        "DROP TYPE",
        "REVOKE",
        "DROP SCHEMA",
        "DROP ROLE",
    ],
    "predefined roles": [
        [
            ("postgres",),
            ("pg_monitor",),
            ("pg_read_all_settings",),
            ("pg_read_all_stats",),
            ("pg_stat_scan_tables",),
            ("pg_signal_backend",),
            ("pg_checkpoint",),
            ("pg_read_server_files",),
            ("pg_write_server_files",),
            ("pg_execute_server_program",),
            ("pg_database_owner",),
            ("pg_read_all_data",),
            ("pg_write_all_data",),
        ],
        [
            ("pg_read_all_settings", "pg_monitor"),
            ("pg_read_all_stats", "pg_monitor"),
            ("pg_stat_scan_tables", "pg_monitor"),
        ],
        ("ERR", "42939"),
        ("ERR", "42939"),
        ("ERR", "2BP01"),
        [("True", "True")],
        [
            ("pg_database_owner",),
            ("pg_read_all_settings",),
            ("pg_read_all_stats",),
            ("pg_stat_scan_tables",),
        ],
    ],
    "progress views": [[("0",)], [("0",)]],
    "schema privileges": [
        "BEGIN",
        "GRANT",
        [("{pg_database_owner=UC/pg_database_owner,=UC/pg_database_owner}",)],
        "ROLLBACK",
    ],
    "set operations resolve one type": [
        [("1", "oid"), ("1259", "oid")],
        [("pg_class", "regclass"), ("1", "regclass")],
        [("1", "regclass"), ("pg_class", "regclass")],
        ("ERR", "42846"),
        ("ERR", "42846"),
        [("a", "1"), ("a", "1")],
        [("1", "bigint"), ("2", "bigint")],
    ],
    "shared comments": [
        [("unmodifiable empty database",)],
        "CREATE ROLE",
        "COMMENT",
        [("a role",)],
        "COMMENT",
        ("ERR", "42704"),
        "DROP ROLE",
        [("standard public schema",)],
        [
            ("{pg_database_owner=UC/pg_database_owner,=U/pg_database_owner}",),
            ("{postgres=UC/postgres,=U/postgres}",),
        ],
    ],
    "stats views on a primary": [[("False", "None", "False")], [("0", "0")], [("0", "1")]],
    "the internals are refused": [("ERR", "0A000"), ("ERR", "0A000")],
    "the server's version": [[("150019",)], [("15.19",)]],
    "type owners": [
        "CREATE ROLE",
        "CREATE TYPE",
        "ALTER TYPE",
        [("rp26_owner",)],
        ("ERR", "42704"),
        "DROP TYPE",
        "DROP ROLE",
    ],
}

EXPECTED_DUMP = (
    (HERE / "rp071_dump_expected.sql").read_text()
    if (HERE / "rp071_dump_expected.sql").exists()
    else ""
)


def _dsn(server: Any) -> str:
    return f"host=127.0.0.1 port={server.port} user=postgres"


def _marked(item: str, is_open: bool) -> Any:
    return pytest.mark.backlog(item) if is_open else pytest.mark.backlog_fixed(item)


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(name, marks=_marked(item, is_open), id=name)
        for name, (item, is_open, _) in sorted(SCENARIOS.items())
    ],
)
def test_scenario(name: str, rust_pg: Any) -> None:
    got = run_scenario(_dsn(rust_pg) + " dbname=postgres", SCENARIOS[name][2])
    assert got == EXPECTED[name]


@pytest.mark.backlog_fixed("RP-092")
def test_memberships_are_the_clusters(rust_pg: Any) -> None:
    assert memberships(_dsn(rust_pg)) == EXPECTED["memberships"]


@pytest.mark.backlog_fixed("RP-094")
def test_copy_of_a_table(rust_pg: Any) -> None:
    assert copy_table(_dsn(rust_pg)) == EXPECTED["copy table"]


@pytest.mark.backlog_fixed("RP-071")
def test_pg_dump_matches(rust_pg: Any) -> None:
    """PostgreSQL 15's pg_dump of the same schema, byte for byte."""
    load_schema(rust_pg.port, _pg15_bin())
    try:
        assert pg_dump_text(rust_pg.port, _pg15_bin()) == EXPECTED_DUMP
    finally:
        drop_schema(rust_pg.port)


if __name__ == "__main__":
    import pprint

    dsn = sys.argv[1]
    measured = {
        name: run_scenario(dsn + " dbname=postgres", stmts)
        for name, (_, _, stmts) in SCENARIOS.items()
    }
    measured["memberships"] = memberships(dsn)
    measured["copy table"] = copy_table(dsn)
    print("EXPECTED: dict[str, Any] = " + pprint.pformat(measured, width=100, sort_dicts=True))
    port = int(dsn.split("port=")[1].split()[0])
    load_schema(port, _pg15_bin())
    (HERE / "rp071_dump_expected.sql").write_text(pg_dump_text(port, _pg15_bin()))
    drop_schema(port)
