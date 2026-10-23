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
import time
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
        False,
        [
            # element_types' UNION, narrowed to its arms' types (the view
            # itself now reads every built-in function).
            "select pg_typeof(n)::text from (select (ss.x).n from (select "
            "information_schema._pg_expandarray(array[7]) as x where false) ss "
            "union all select 1) u",
            "select pg_typeof(n)::text from (select (ss.x).n from (select "
            "information_schema._pg_expandarray(array[7]) as x) ss) t",
        ],
    ),
    "a FROM function over the row to its left": (
        "RP-113",
        False,
        [
            "create temp table rp26_l (a int[])",
            "insert into rp26_l values (array[5, 6])",
            "select * from rp26_l, unnest(a) with ordinality u(x, n)",
            "select * from rp26_l, lateral unnest(a) with ordinality u(x, n)",
            "select (information_schema._pg_expandarray(a)).n from rp26_l",
            "select pg_typeof(n)::text from (select (ss.x).n from (select "
            "information_schema._pg_expandarray(a) as x from rp26_l) ss) t",
            "select pg_typeof(x)::text from (select (ss.x).x from (select "
            "information_schema._pg_expandarray(coalesce(proallargtypes, proargtypes::oid[])) "
            "as x from pg_proc) ss) t limit 1",
        ],
    ),
    "an unknown name in a FROM function beside a table": (
        "RP-114",
        False,
        [
            "create temp table rp26_u (a int[])",
            "select * from rp26_u, unnest(b) u",
            "select * from rp26_u, unnest(b) with ordinality u(x, n)",
            "select * from rp26_u, lateral (select * from unnest(b)) q",
        ],
    ),
    "triggers PostgreSQL makes and their functions": (
        "RP-115",
        False,
        [
            "create table rp28_p (id int primary key)",
            "create table rp28_c (r int references rp28_p on delete cascade)",
            "create function rp28_tf() returns trigger language plpgsql as 'begin return new; end'",
            "create trigger rp28_t before insert on rp28_c for each row execute function rp28_tf()",
            "select tgrelid::regclass::text, regexp_replace(tgname, '[0-9]+$', 'N'), "
            "tgname = 'RI_ConstraintTrigger_' || substr(tgname, 22, 1) || '_' || oid, "
            "tgfoid::regproc::text, tgtype, tgisinternal, tgconstrrelid::regclass::text, "
            "tgconstrindid::regclass::text, (select conname from pg_constraint where oid = "
            "tgconstraint) from pg_trigger where tgrelid in ('rp28_p'::regclass, "
            "'rp28_c'::regclass) order by tgisinternal, 1, 4",
            "drop table rp28_c",
            "drop table rp28_p",
            "drop function rp28_tf()",
        ],
    ),
    "a composite type's relation": (
        "RP-116",
        False,
        [
            "create type rp28_comp as (x int)",
            "select typrelid::regclass::text, (select relkind from pg_class where oid = typrelid) "
            "from pg_type where typname = 'rp28_comp'",
            "select 'rp28_comp'::regclass::text",
            "select * from rp28_comp",
            "insert into rp28_comp values (1)",
            "drop table if exists rp28_comp",
            "drop type rp28_comp",
        ],
    ),
    "a view's row type is a type": (
        "RP-117",
        False,
        [
            "create view rp28_v as select 1 as one",
            "select typarray::regtype::text from pg_type where typname = 'rp28_v'",
            "select '_rp28_v'::regtype::text",
            "select 'rp28_v'::regtype::text",
            "select row(1)::rp28_v::text, ('(2)'::rp28_v).one",
            "select pg_typeof(v)::text from rp28_v v",
            "drop view rp28_v",
        ],
    ),
    "an index created in the open block": (
        "RP-118",
        False,
        [
            "begin",
            "create table rp28_i (a int)",
            "create index rp28_ix on rp28_i (a)",
            "select 'rp28_ix'::regclass::text",
            "select (select oid from pg_class where relname = 'rp28_ix')::regclass::text",
            "rollback",
        ],
    ),
    "regproc names": (
        "RP-119",
        False,
        [
            "select 1644::regproc::text, 'pg_catalog.\"RI_FKey_check_ins\"'::regproc::oid",
            "create schema rp28_s",
            "create function rp28_s.h() returns int language sql as 'select 1'",
            "select 'rp28_s.h'::regproc::text",
            "set search_path = rp28_s, public",
            "select 'h'::regproc::text",
            "reset search_path",
            "drop function rp28_s.h()",
            "drop schema rp28_s",
        ],
    ),
    "the plpgsql language's oid": (
        "RP-120",
        False,
        [
            "select oid, lanplcallfoid, laninline, lanvalidator from pg_language "
            "where lanname = 'plpgsql'",
        ],
    ),
    "a function's user-typed arguments": (
        "RP-121",
        False,
        [
            "create type rp28_m as enum ('a')",
            "create function rp28_f(x rp28_m, y int, z rp28_m[]) returns rp28_m "
            "language sql as 'select x'",
            "select proargtypes::regtype[]::text, prorettype::regtype::text from pg_proc "
            "where proname = 'rp28_f'",
            "drop function rp28_f(rp28_m, int, rp28_m[])",
            "drop type rp28_m",
        ],
    ),
    "a trigger's UPDATE OF columns": (
        "RP-122",
        False,
        [
            "create table rp28_q (a int, b int, c int)",
            "create function rp28_tf2() returns trigger language plpgsql as "
            "'begin return new; end'",
            "create trigger rp28_tu after update of c, a on rp28_q for each row "
            "execute function rp28_tf2()",
            "select tgattr::text from pg_trigger where tgname = 'rp28_tu'",
            "drop table rp28_q",
            "drop function rp28_tf2()",
        ],
    ),
    "a vector as an array": (
        "RP-123",
        False,
        [
            "select '1 2'::int2vector::int2[]::text, '23 25'::oidvector::oid[]::text, "
            "array_lower('1 2'::int2vector::int2[], 1)",
            "select indkey::int2[]::text from pg_index "
            "where indexrelid = 'pg_class_oid_index'::regclass",
        ],
    ),
    "dependencies on a constant's type and a range's functions": (
        "RP-105",
        False,
        [
            "create type rp28_e as enum ('a', 'b')",
            "create table rp28_d (a int, b rp28_e)",
            "create index rp28_dp on rp28_d (a) where b = 'a'",
            "select count(*) from pg_depend where objid = 'rp28_dp'::regclass "
            "and refclassid = 'pg_type'::regclass",
            "create type rp28_r as range (subtype = int4)",
            "select count(*) from pg_depend d join pg_proc p on p.oid = d.objid "
            "where d.classid = 'pg_proc'::regclass "
            "and p.proname in ('rp28_r', 'rp28_r_multirange')",
            "drop type rp28_r",
            "drop table rp28_d",
            "drop type rp28_e",
        ],
    ),
    "a materialized view's rule": (
        "RP-124",
        False,
        [
            "create table rp28_mq (a int)",
            "create materialized view rp28_mm as select a from rp28_mq",
            "select rulename, ev_type, is_instead from pg_rewrite "
            "where ev_class = 'rp28_mm'::regclass",
            "select refobjsubid, deptype from pg_depend d join pg_rewrite r on r.oid = d.objid "
            "where r.ev_class = 'rp28_mm'::regclass and d.refobjid = 'rp28_mq'::regclass",
            "drop materialized view rp28_mm",
            "drop table rp28_mq",
        ],
    ),
    "has_function_privilege by oid": (
        "RP-125",
        False,
        [
            "select has_function_privilege(1644::oid, 'EXECUTE'), "
            "has_function_privilege(999999::oid, 'EXECUTE') is null, "
            "has_function_privilege('public', 1644::oid, 'EXECUTE'), "
            "has_function_privilege('1644', 'EXECUTE')",
        ],
    ),
    "information_schema's namespace": (
        "RP-126",
        False,
        [
            "select oid from pg_namespace where nspname = 'information_schema'",
            "select count(*) from information_schema.routines "
            "where specific_schema = 'information_schema'",
        ],
    ),
    "a view inside a FROM subquery": (
        "RP-128",
        False,
        [
            "select count(*) from (select domain_name from information_schema.domains) x",
            "select count(*) from pg_namespace n where (n.nspname::text, 'a') in "
            "(select domain_schema::text, domain_name::text from information_schema.domains)",
        ],
    ),
    "a long generate_series": (
        "RP-130",
        False,
        [
            # The timestamp series only: the numeric one shares the fix and
            # takes minutes in a debug build (10 million decimal additions).
            "select count(*) from generate_series(timestamp '2000-01-01', "
            "timestamp '2000-01-01' + interval '10000001 seconds', interval '1 second')",
        ],
    ),
    "infinite timestamps in every operator": (
        "RP-060",
        False,
        [
            "create temp table rp29_ti (id int, t timestamp, d date, tz timestamptz)",
            "insert into rp29_ti values (1, 'infinity', 'infinity', 'infinity'), "
            "(2, '2021-01-01 10:00:00.123456', '2021-01-01', '2021-01-01'), "
            "(3, '-infinity', '-infinity', '-infinity'), (4, null, null, null)",
            "select array_agg(id order by id) from rp29_ti where t > '2020-01-01'",
            "select array_agg(id order by id) from rp29_ti where t < '2022-01-01'",
            "select array_agg(id order by id) from rp29_ti where t >= '-infinity'",
            "select array_agg(id order by id) from rp29_ti where t > '-infinity'",
            "select array_agg(id order by id) from rp29_ti where t < 'infinity'",
            "select array_agg(id order by id) from rp29_ti where t <> 'infinity'",
            "select array_agg(id order by id) from rp29_ti where tz > '2000-01-01'",
            "create index rp29_ti_t on rp29_ti (t)",
            "select array_agg(id order by id) from rp29_ti "
            "where t between '-infinity' and 'infinity'",
            "select id, (t + interval '1 day')::text, (d + 1)::text, "
            "(tz - interval '1 hour')::text from rp29_ti order by id",
            "select id, isfinite(t), extract(epoch from t)::text, extract(month from t) is null "
            "from rp29_ti where id < 4 order by id",
            "select id, date_trunc('day', t)::text from rp29_ti where id < 4 order by id",
            "select 'infinity'::timestamp - '2000-01-01'::timestamp",
        ],
    ),
    "ANALYZE computes extended statistics": (
        "RP-099",
        True,
        [
            "create table rp30_e (a int, b int)",
            "insert into rp30_e select g % 4, g % 2 from generate_series(1, 40) g",
            "create statistics rp30_es (ndistinct, dependencies) on a, b from rp30_e",
            "analyze rp30_e",
            "select statistics_name, n_distinct::text, dependencies::text from pg_stats_ext "
            "where tablename = 'rp30_e'",
            "drop table rp30_e",
        ],
    ),
    "ANALYZE computes statistics": (
        "RP-099",
        False,
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
    "built-in objects in the catalogs": (
        "RP-102",
        False,
        [
            "select count(*) from pg_proc where pronamespace = 'pg_catalog'::regnamespace",
            "select count(*) from pg_type where typrelid <> 0 and typnamespace in "
            "('pg_catalog'::regnamespace, 'information_schema'::regnamespace)",
            "select count(*) from pg_constraint where connamespace = 'pg_catalog'::regnamespace",
        ],
    ),
    "schema privileges": (
        "RP-103",
        False,
        [
            "begin",
            "grant create on schema public to public",
            "select nspacl::text from pg_namespace where nspname = 'public'",
            "rollback",
        ],
    ),
    "owners of what another role creates": (
        "RP-104",
        False,
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
        False,
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
        False,
        [
            "select (select max(x) from unnest(a) x) from (values (array[1, 5])) v(b)",
        ],
    ),
    "expression columns' types": (
        "RP-107",
        False,
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
        False,
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
        False,
        [
            "create type rp26_hidden as enum ('a')",
            "select pg_catalog.set_config('search_path', '', false)",
            "select 'rp26_hidden'::regtype",
            "select pg_catalog.to_regtype('rp26_hidden') is null",
            "reset search_path",
            "drop type rp26_hidden",
        ],
    ),
    "a large object's pages": (
        "RP-110",
        False,
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


def other_database_owners(dsn: str) -> list[Any]:
    """RP-104: pg_shdepend is shared, so another database's objects show."""
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as conn:
        for s in ("create role rp26_shd", "create database rp26_shd_db owner rp26_shd"):
            _answer(conn, s)
    with psycopg.connect(dsn + " dbname=rp26_shd_db", autocommit=True) as conn:
        for s in ("create table rp26_shd_t (a int)", "alter table rp26_shd_t owner to rp26_shd"):
            _answer(conn, s)
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as conn:
        seen = _answer(
            conn,
            "select d.datname, s.classid::regclass::text, s.deptype from pg_shdepend s "
            "join pg_database d on d.oid = s.dbid join pg_roles r on r.oid = s.refobjid "
            "where r.rolname = 'rp26_shd' order by 1, 2, 3",
        )
        for s in ("drop database rp26_shd_db", "drop role rp26_shd"):
            _answer(conn, s)
    return seen


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
    "a large object's pages": [[("260011",)], [("",)], [("0", "1"), ("4", "1809")], [("1",)]],
    "a merged inherited column": [
        "CREATE TABLE",
        "CREATE TABLE",
        [("x", "True", "1"), ("y", "True", "0")],
        "DROP TABLE",
    ],
    "a record's fields through a subquery": [[("integer",)], [("integer",)]],
    "a FROM function over the row to its left": [
        "CREATE TABLE",
        "INSERT 0 1",
        [("[5, 6]", "5", "1"), ("[5, 6]", "6", "2")],
        [("[5, 6]", "5", "1"), ("[5, 6]", "6", "2")],
        [("1",), ("2",)],
        [("integer",), ("integer",)],
        [("oid",)],
    ],
    "an unknown name in a FROM function beside a table": [
        "CREATE TABLE",
        ("ERR", "42703"),
        ("ERR", "42703"),
        ("ERR", "42703"),
    ],
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
    "other database owners": [("rp26_shd_db", "pg_class", "o")],
    "triggers PostgreSQL makes and their functions": [
        "CREATE TABLE",
        "CREATE TABLE",
        "CREATE FUNCTION",
        "CREATE TRIGGER",
        [
            ("rp28_c", "rp28_t", "False", "rp28_tf", "7", "False", "-", "-", "None"),
            (
                "rp28_c",
                "RI_ConstraintTrigger_c_N",
                "True",
                '"RI_FKey_check_ins"',
                "5",
                "True",
                "rp28_p",
                "rp28_p_pkey",
                "rp28_c_r_fkey",
            ),
            (
                "rp28_c",
                "RI_ConstraintTrigger_c_N",
                "True",
                '"RI_FKey_check_upd"',
                "17",
                "True",
                "rp28_p",
                "rp28_p_pkey",
                "rp28_c_r_fkey",
            ),
            (
                "rp28_p",
                "RI_ConstraintTrigger_a_N",
                "True",
                '"RI_FKey_cascade_del"',
                "9",
                "True",
                "rp28_c",
                "rp28_p_pkey",
                "rp28_c_r_fkey",
            ),
            (
                "rp28_p",
                "RI_ConstraintTrigger_a_N",
                "True",
                '"RI_FKey_noaction_upd"',
                "17",
                "True",
                "rp28_c",
                "rp28_p_pkey",
                "rp28_c_r_fkey",
            ),
        ],
        "DROP TABLE",
        "DROP TABLE",
        "DROP FUNCTION",
    ],
    "a composite type's relation": [
        "CREATE TYPE",
        [("rp28_comp", "c")],
        [("rp28_comp",)],
        ("ERR", "42809"),
        ("ERR", "42809"),
        ("ERR", "42809"),
        "DROP TYPE",
    ],
    "a view's row type is a type": [
        "CREATE VIEW",
        [("rp28_v[]",)],
        [("rp28_v[]",)],
        [("rp28_v",)],
        [("(1)", "2")],
        [("rp28_v",)],
        "DROP VIEW",
    ],
    "an index created in the open block": [
        "BEGIN",
        "CREATE TABLE",
        "CREATE INDEX",
        [("rp28_ix",)],
        [("rp28_ix",)],
        "ROLLBACK",
    ],
    "regproc names": [
        [('"RI_FKey_check_ins"', "1644")],
        "CREATE SCHEMA",
        "CREATE FUNCTION",
        [("rp28_s.h",)],
        "SET",
        [("h",)],
        "RESET",
        "DROP FUNCTION",
        "DROP SCHEMA",
    ],
    "the plpgsql language's oid": [[("14035", "14032", "14033", "14034")]],
    "a function's user-typed arguments": [
        "CREATE TYPE",
        "CREATE FUNCTION",
        [("[0:2]={rp28_m,integer,rp28_m[]}", "rp28_m")],
        "DROP FUNCTION",
        "DROP TYPE",
    ],
    "a trigger's UPDATE OF columns": [
        "CREATE TABLE",
        "CREATE FUNCTION",
        "CREATE TRIGGER",
        [("3 1",)],
        "DROP TABLE",
        "DROP FUNCTION",
    ],
    "a vector as an array": [[("[0:1]={1,2}", "[0:1]={23,25}", "0")], [("[0:0]={1}",)]],
    "dependencies on a constant's type and a range's functions": [
        "CREATE TYPE",
        "CREATE TABLE",
        "CREATE INDEX",
        [("1",)],
        "CREATE TYPE",
        [("17",)],
        "DROP TYPE",
        "DROP TABLE",
        "DROP TYPE",
    ],
    "a materialized view's rule": [
        "CREATE TABLE",
        "SELECT 0",
        [("_RETURN", "1", "True")],
        [("1", "n")],
        "DROP MATERIALIZED VIEW",
        "DROP TABLE",
    ],
    "has_function_privilege by oid": [[("True", "True", "True", "True")]],
    "information_schema's namespace": [[("13679",)], [("11",)]],
    "a view inside a FROM subquery": [[("5",)], [("0",)]],
    "copy char array": [("{i,o}", "i"), ("{v}", "v")],
    "a long generate_series": [[("10000002",)]],
    "wal properties": [
        [("True",)],
        [("True",)],
        [("000000010000000E00000086",)],
        [("4294967274",)],
        [("True", "True")],
    ],
    "configuration-file views": [
        [
            ("2", "local", "['all']", "['all']", "None", "None", "trust", "None", "None"),
            (
                "3",
                "host",
                "['all']",
                "['all']",
                "127.0.0.1",
                "255.255.255.255",
                "trust",
                "None",
                "None",
            ),
            (
                "4",
                "host",
                "['all']",
                "['all']",
                "::1",
                "ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff",
                "trust",
                "None",
                "None",
            ),
            (
                "5",
                "host",
                "['db one', 'postgres']",
                "['+staff', 'test']",
                "10.0.0.0",
                "255.0.0.0",
                "scram-sha-256",
                "None",
                "None",
            ),
            ("6", "host", "['sameuser']", "['all']", "samehost", "None", "md5", "None", "None"),
            (
                "7",
                "host",
                "['all', 'test']",
                "['all']",
                "0.0.0.0",
                "0.0.0.0",
                "reject",
                "None",
                "None",
            ),
        ],
        [
            (
                "BINDIR,CC,CFLAGS,CFLAGS_SL,CONFIGURE,CPPFLAGS,DOCDIR,HTMLDIR,INCLUDEDIR,INCLUDEDIR-SERVER,LDFLAGS,LDFLAGS_EX,LDFLAGS_SL,LIBDIR,LIBS,LOCALEDIR,MANDIR,PGXS,PKGINCLUDEDIR,PKGLIBDIR,SHAREDIR,SYSCONFDIR,VERSION",
            )
        ],
        [("0",)],
        [("0",)],
    ],
    "infinite timestamps in every operator": [
        "CREATE TABLE",
        "INSERT 0 4",
        [("[1, 2]",)],
        [("[2, 3]",)],
        [("[1, 2, 3]",)],
        [("[1, 2]",)],
        [("[2, 3]",)],
        [("[2, 3]",)],
        [("[1, 2]",)],
        "CREATE INDEX",
        [("[1, 2, 3]",)],
        [
            ("1", "infinity", "infinity", "infinity"),
            ("2", "2021-01-02 10:00:00.123456", "2021-01-02", "2020-12-31 23:00:00+00"),
            ("3", "-infinity", "-infinity", "-infinity"),
            ("4", "None", "None", "None"),
        ],
        [
            ("1", "False", "Infinity", "True"),
            ("2", "True", "1609495200.123456", "False"),
            ("3", "False", "-Infinity", "True"),
        ],
        [("1", "infinity"), ("2", "2021-01-01 00:00:00"), ("3", "-infinity")],
        ("ERR", "22008"),
    ],
    "ANALYZE computes extended statistics": [
        "CREATE TABLE",
        "INSERT 0 40",
        "CREATE STATISTICS",
        "ANALYZE",
        [("rp30_es", '{"1, 2": 4}', '{"1 => 2": 1.000000}')],
        "DROP TABLE",
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


@pytest.mark.backlog_fixed("RP-104")
def test_other_databases_objects_in_pg_shdepend(rust_pg: Any) -> None:
    assert other_database_owners(_dsn(rust_pg)) == EXPECTED["other database owners"]


def copy_char_array(dsn: str) -> list[Any]:
    """RP-127: COPY into a `"char"[]` column stores an array."""
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as conn:
        conn.execute('create temp table rp28_ca (m "char"[])')
        with conn.cursor().copy("copy rp28_ca from stdin") as copy:
            copy.write("{v}\n{i,o}\n")
        return _answer(conn, "select m::text, m[1]::text from rp28_ca order by 1")


@pytest.mark.backlog_fixed("RP-127")
def test_copy_into_a_char_array(rust_pg: Any) -> None:
    assert copy_char_array(_dsn(rust_pg)) == EXPECTED["copy char array"]


def timeout_latency(dsn: str) -> tuple[Any, bool]:
    """RP-129: statement_timeout ends a long statement at the deadline.

    PostgreSQL 15.19 answers 57014 about 0.3 s in (it checks for interrupts
    row by row); the bound below leaves room for a slow machine."""
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as conn:
        conn.execute("set statement_timeout = '300ms'")
        started = time.monotonic()
        answer = _answer(conn, "select count(*) from generate_series(1, 30000000)")
        return answer, time.monotonic() - started < 3.0


@pytest.mark.backlog_fixed("RP-129")
def test_statement_timeout_is_prompt(rust_pg: Any) -> None:
    assert timeout_latency(_dsn(rust_pg)) == (("ERR", "57014"), True)


def wal_properties(dsn: str) -> list[Any]:
    """RP-112, as decided (Joe, 2026-10-22): the log position and relation
    files are WiredTiger's, so what is compared is what PostgreSQL also
    guarantees, not its numbers."""
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as conn:
        conn.execute("create table rp29_w (x int)")
        conn.execute("create view rp29_v as select 1 as one")
        try:
            (before,) = conn.execute("select pg_current_wal_lsn()").fetchone()
            conn.execute("insert into rp29_w select generate_series(1, 100)")
            return [
                _answer(conn, f"select pg_current_wal_lsn() > '{before}'::pg_lsn"),
                _answer(conn, "select pg_walfile_name(pg_current_wal_lsn()) ~ '^[0-9A-F]{24}$'"),
                _answer(conn, "select pg_walfile_name('E/869194F0')"),
                _answer(conn, "select pg_wal_lsn_diff('1/0', '0/16')"),
                _answer(
                    conn,
                    "select pg_relation_filepath('rp29_w') is not null, "
                    "pg_relation_filepath('rp29_v') is null",
                ),
            ]
        finally:
            conn.execute("drop view rp29_v")
            conn.execute("drop table rp29_w")


@pytest.mark.backlog_fixed("RP-112")
def test_wal_and_file_functions(rust_pg: Any) -> None:
    assert wal_properties(_dsn(rust_pg)) == EXPECTED["wal properties"]


RP101_HBA = """\
# TYPE  DATABASE  USER  ADDRESS  METHOD
local   all   all   trust
host    all   all   127.0.0.1/32   trust
host    all   all   ::1/128   trust
host    "db one",postgres   +staff,test   10.0.0.0   255.0.0.0   scram-sha-256
host    sameuser   all   samehost   md5   # a trailing comment
host    all,  test   all   0.0.0.0/0   reject
"""


def config_views(home: pathlib.Path) -> list[Any]:
    """RP-101, as decided (Joe, 2026-10-22): `pg_hba_file_rules` shows the
    file the server was started with as PostgreSQL 15.19 shows it (measured
    with `hba_file` pointed at this text), `pg_config` answers PostgreSQL's
    names, and `pg_file_settings` has no rows, as no postgresql.conf is
    read. The file also needs RP-132's list reading to load at all."""
    from tests.test_rust_pgserver_slice import _Server

    hba = home / "pg_hba.conf"
    hba.write_text(RP101_HBA)
    with (
        _Server(home / "store", args=("--hba-file", str(hba))) as server,
        psycopg.connect(
            f"host=127.0.0.1 port={server.port} dbname=postgres user=postgres",
            autocommit=True,
        ) as conn,
    ):
        return [
            _answer(
                conn,
                "select line_number, type, database, user_name, address, netmask, "
                "auth_method, options, error from pg_hba_file_rules order by 1",
            ),
            _answer(conn, "select string_agg(name, ',' order by name) from pg_config"),
            _answer(conn, "select count(*) from pg_file_settings"),
            _answer(conn, "select count(*) from pg_ident_file_mappings"),
        ]


@pytest.mark.backlog_fixed("RP-101")
def test_configuration_file_views(tmp_path: pathlib.Path) -> None:
    assert config_views(tmp_path) == EXPECTED["configuration-file views"]


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
    measured["wal properties"] = wal_properties(dsn)
    measured["copy char array"] = copy_char_array(dsn)
    measured["other database owners"] = other_database_owners(dsn)
    print("EXPECTED: dict[str, Any] = " + pprint.pformat(measured, width=100, sort_dicts=True))
    port = int(dsn.split("port=")[1].split()[0])
    load_schema(port, _pg15_bin())
    (HERE / "rp071_dump_expected.sql").write_text(pg_dump_text(port, _pg15_bin()))
    drop_schema(port)
