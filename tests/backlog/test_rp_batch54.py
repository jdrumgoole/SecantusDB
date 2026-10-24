"""Batch 54 of the Rust PG server (2026-10-24): the regression tests of
RP-199 (`pg_identify_object`, `pg_identify_object_as_address` and
`pg_get_object_address`), and of RP-217 to RP-223, found while building
them.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch54.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario that changes
anything runs in transactions it rolls back.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

# One object of each kind, named in a schema of its own.
_OBJECTS = [
    "create schema rp54",
    "create table rp54.t (a int default 1 check (a > 0), b text)",
    "create index rp54_i on rp54.t (a)",
    "create view rp54.v as select a from rp54.t",
    "create function rp54.f(int, text) returns int language sql as 'select 1'",
    "create sequence rp54.s",
    "create type rp54.e as enum ('x')",
    "create domain rp54.d as int constraint rp54_dpos check (value > 0)",
    "create function rp54.tf() returns trigger language plpgsql as 'begin return new; end'",
    "create trigger rp54_tg before insert on rp54.t for each row execute function rp54.tf()",
    "create policy rp54_pol on rp54.t using (true)",
    "create foreign data wrapper rp54_fdw",
    "create server rp54_srv foreign data wrapper rp54_fdw",
    "create user mapping for postgres server rp54_srv",
    "create user mapping for public server rp54_srv",
    "alter default privileges in schema rp54 grant select on tables to public",
    "create publication rp54_pub for table rp54.t",
    "create statistics rp54.st on a, b from rp54.t",
]

# `(classid, objid, objsubid)` of each, and of objects PostgreSQL ships.
_ADDRESSES = """
select 'pg_class'::regclass as classid, c.oid as objid, 0 as objsubid from pg_class c
 where relnamespace = 'rp54'::regnamespace
union all select 'pg_class'::regclass, 'rp54.t'::regclass, 2
union all select 'pg_namespace'::regclass, 'rp54'::regnamespace, 0
union all select 'pg_proc'::regclass, oid, 0 from pg_proc where pronamespace = 'rp54'::regnamespace
union all select 'pg_proc'::regclass, 'int4pl'::regproc, 0
union all select 'pg_type'::regclass, t::regtype::oid, 0 from unnest(array['text', '"char"',
 'varchar', '_text', 'bpchar', 'timestamptz', 'name', 'oid', 'regclass', 'bool', 'numeric', 'jsonb',
 'int4range', 'int4']) t
union all select 'pg_type'::regclass, oid, 0 from pg_type
 where typnamespace = 'rp54'::regnamespace and typname not like '\\_%'
union all select 'pg_attrdef'::regclass, oid, 0 from pg_attrdef where adrelid = 'rp54.t'::regclass
union all select 'pg_constraint'::regclass, oid, 0 from pg_constraint
 where connamespace = 'rp54'::regnamespace
union all select 'pg_rewrite'::regclass, oid, 0 from pg_rewrite where ev_class = 'rp54.v'::regclass
union all select 'pg_cast'::regclass, oid, 0 from pg_cast
 where castsource = 'int4'::regtype and casttarget = 'int8'::regtype
union all select 'pg_opclass'::regclass, oid, 0 from pg_opclass
 where opcname = 'int4_ops' and opcmethod = 403
union all select 'pg_opfamily'::regclass, oid, 0 from pg_opfamily
 where opfname = 'integer_ops' and opfmethod = 403
union all select 'pg_am'::regclass, 403, 0
union all select 'pg_amop'::regclass, min(oid), 0 from pg_amop where amopfamily = 1976
union all select 'pg_amproc'::regclass, min(oid), 0 from pg_amproc where amprocfamily = 1976
union all select 'pg_trigger'::regclass, oid, 0 from pg_trigger where tgname = 'rp54_tg'
union all select 'pg_policy'::regclass, oid, 0 from pg_policy where polname = 'rp54_pol'
union all select 'pg_user_mapping'::regclass, oid, 0 from pg_user_mapping
union all select 'pg_foreign_server'::regclass, oid, 0 from pg_foreign_server
union all select 'pg_foreign_data_wrapper'::regclass, oid, 0 from pg_foreign_data_wrapper
 where fdwname = 'rp54_fdw'
union all select 'pg_default_acl'::regclass, oid, 0 from pg_default_acl
union all select 'pg_publication'::regclass, oid, 0 from pg_publication
union all select 'pg_publication_rel'::regclass, oid, 0 from pg_publication_rel
union all select 'pg_statistic_ext'::regclass, oid, 0 from pg_statistic_ext
union all select 'pg_language'::regclass, oid, 0 from pg_language where lanname = 'plpgsql'
union all select 'pg_extension'::regclass, oid, 0 from pg_extension
union all select 'pg_collation'::regclass, oid, 0 from pg_collation where collname = 'C'
union all select 'pg_ts_config'::regclass, oid, 0 from pg_ts_config where cfgname = 'english'
union all select 'pg_ts_dict'::regclass, oid, 0 from pg_ts_dict where dictname = 'simple'
union all select 'pg_ts_parser'::regclass, oid, 0 from pg_ts_parser
union all select 'pg_ts_template'::regclass, oid, 0 from pg_ts_template where tmplname = 'simple'
union all select 'pg_tablespace'::regclass, oid, 0 from pg_tablespace where spcname = 'pg_default'
union all select 'pg_authid'::regclass, 10, 0
union all select 'pg_database'::regclass, oid, 0 from pg_database where datname = 'postgres'
union all select 'pg_operator'::regclass, '+(int4,int4)'::regoperator, 0
union all select 'pg_proc'::regclass, 999999, 0
union all select 'pg_constraint'::regclass, 999999, 0
union all select 'pg_class'::regclass, 999999, 0
"""

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------------------------------- RP-199 the identities
    "every kind of object identified, and found again from its address": (
        "RP-199",
        False,
        [
            "begin",
            *_OBJECTS,
            "select x.classid::regclass::text, i.type, i.schema, i.name, i.identity, "
            "a.object_names::text, a.object_args::text, "
            "(select (g.classid, g.objid, g.objsubid) = (x.classid, x.objid, x.objsubid) "
            "from pg_get_object_address(a.type, a.object_names, a.object_args) g) "
            f"from ({_ADDRESSES}) x, "
            "pg_identify_object(x.classid, x.objid, x.objsubid) i, "
            "pg_identify_object_as_address(x.classid, x.objid, x.objsubid) a "
            "order by 1, 2, 5, 6, 7",
            "rollback",
        ],
    ),
    "pg_identify_object as a value, a row and its fields": (
        "RP-199",
        False,
        [
            "select pg_identify_object('pg_class'::regclass, 'pg_class'::regclass, 0)::text, "
            "pg_identify_object(null, 1, 0) is null",
            "select (pg_identify_object('pg_class'::regclass, 'pg_class'::regclass, 0)).*",
            "select (pg_identify_object(1255, 'int4pl'::regproc, 0)).identity",
            "select i.type, i.identity from pg_identify_object(1259, 1259, 2) i",
            "select * from pg_identify_object(null, 1, 0)",
            "select (pg_identify_object_as_address(1259, 1259, 0)).*",
            "select * from pg_identify_object(1, 2, 0)",
            "select prefix.* from pg_identify_object_as_address(2617, "
            "'-(NONE,int4)'::regoperator, 0) prefix",
        ],
    ),
    "pg_get_object_address refuses as PostgreSQL does": (
        "RP-199",
        False,
        [
            "select * from pg_get_object_address('table', '{nope}', '{}')",
            "select * from pg_get_object_address('index', '{pg_class}', '{}')",
            "select * from pg_get_object_address('bogus', '{x}', '{}')",
            "select * from pg_get_object_address('schema', '{a,b}', '{}')",
            "select * from pg_get_object_address('schema', '{nope}', '{}')",
            "select * from pg_get_object_address('function', '{nope}', '{int4,text}')",
            "select * from pg_get_object_address('type', '{nope}', '{}')",
            "select * from pg_get_object_address('table column', '{pg_class,nope}', '{}')",
            "select * from pg_get_object_address('user mapping', '{postgres}', '{nope}')",
            "select * from pg_get_object_address('operator', '{+}', '{int4,int4}')",
            "select * from pg_get_object_address('cast', '{int4}', '{int8}')",
            "select * from pg_get_object_address('operator of access method', "
            "'{btree,integer_ops,1}', '{int2,int2}')",
            "select * from pg_get_object_address('operator of access method', "
            "'{btree,pg_catalog,integer_ops,9}', '{smallint,smallint}')",
            "select * from pg_get_object_address('function of access method', "
            "'{btree,pg_catalog,integer_ops,9}', '{smallint,smallint}')",
            "select * from pg_get_object_address('parameter ACL', '{work_mem}', '{}')",
            "select * from pg_get_object_address('role membership', '{a}', '{}')",
            "select * from pg_get_object_address('default acl', '{postgres}', '{r}')",
            "select * from pg_get_object_address('default acl', '{postgres}', '{x}')",
        ],
    ),
    # ---------------------- RP-217 a schema's table in a policy and the rest
    "a policy, statistics object and publication on a schema's table": (
        "RP-217",
        False,
        [
            "begin",
            "create schema rp54q",
            "create table rp54q.t (a int, b int)",
            "create policy rp54_p1 on rp54q.t using (true)",
            "create statistics rp54q.st on a, b from rp54q.t",
            "create publication rp54_qp for table rp54q.t",
            "alter policy rp54_p1 on rp54q.t rename to rp54_p2",
            "alter policy rp54_p2 on rp54q.t using (a > 0)",
            "set local search_path = rp54q, public",
            "create policy rp54_p3 on t using (true)",
            "create statistics st2 on a, b from t",
            "alter publication rp54_qp drop table t",
            "reset search_path",
            "select polname::text, polrelid::regclass::text from pg_policy "
            "where polname like 'rp54%' order by 1",
            "select stxname::text, stxrelid::regclass::text from pg_statistic_ext order by 1",
            "select count(*) from pg_publication_rel",
            "rollback",
        ],
    ),
    "a statistics object's schema": (
        "RP-217",
        False,
        [
            "begin",
            "create schema rp54s",
            "create table rp54s.t (a int, b int)",
            "create statistics rp54s.named on a, b from rp54s.t",
            "create statistics bare on a, b from rp54s.t",
            "select stxname::text, stxnamespace::regnamespace::text from pg_statistic_ext "
            "order by 1",
            "rollback",
        ],
    ),
    # ------------------------------- RP-222 the grammar PostgreSQL 16 added
    "syntax PostgreSQL 16 added is refused as 15 refuses it": (
        "RP-222",
        False,
        [
            "begin",
            "create table rp54g (a int, b int)",
            "create statistics on a, b from rp54g",
            "rollback",
            "begin",
            "create table rp54g (a int, b int)",
            "CREATE STATISTICS IF NOT EXISTS ON a, b FROM rp54g",
            "rollback",
            "select 'x' is json",
            "select 'x' IS NOT Json",
        ],
    ),
    # ------------------------------------ RP-220 x.* over a function in FROM
    "a FROM function's columns by its alias": (
        "RP-220",
        False,
        [
            "select x.* from json_each('{\"a\": 1}') x",
            "select o.* from pg_options_to_table('{a=1,b=2}') o",
            "select i.* from pg_identify_object(1259, 1259, 0) i",
            "select g.*, g.g from generate_series(1, 2) g",
        ],
    ),
    # ------------------------- RP-223 pg_options_to_table of a literal array
    "pg_options_to_table of an untyped array literal": (
        "RP-223",
        False,
        [
            "select * from pg_options_to_table('{a=1,b=2}')",
            "select pg_options_to_table('{fillfactor=70}')",
            "select * from aclexplode('{postgres=r/postgres}')",
        ],
    ),
    # ------------------------------------ RP-221 a domain constraint's schema
    "a domain constraint's schema": (
        "RP-221",
        False,
        [
            "begin",
            "create schema rp54d",
            "create domain rp54d.d as int constraint rp54_dc check (value > 0)",
            "select conname::text, connamespace::regnamespace::text, contypid::regtype::text "
            "from pg_constraint where conname = 'rp54_dc'",
            "rollback",
        ],
    ),
    # ----------------------- RP-218 schema-qualified types in a regprocedure
    "a regprocedure's argument types in their schema": (
        "RP-218",
        False,
        [
            "select to_regprocedure('int4pl(integer,pg_catalog.int4)')::oid, "
            "'pg_catalog.textlen(pg_catalog.text)'::regprocedure::oid, "
            "'textlen(\"pg_catalog\".text)'::regprocedure::oid",
        ],
    ),
    # --------------------------------------- RP-219 to_regoper(ator) refused
    "to_regoper and to_regoperator": (
        "RP-219",
        False,
        [
            "select to_regoper('+'), to_regoper('||/')::oid, "
            "to_regoperator('nope(int4,int4)'), to_regoperator('+(int4,int4)')::oid, "
            "to_regoperator('pg_catalog.-(NONE,int4)')::oid",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a FROM function's columns by its alias": [
        [("a", "1")],
        [("a", "1"), ("b", "2")],
        [("table", "pg_catalog", "pg_class", "pg_catalog.pg_class")],
        [("1", "1"), ("2", "2")],
    ],
    "a domain constraint's schema": [
        "BEGIN",
        "CREATE SCHEMA",
        "CREATE DOMAIN",
        [("rp54_dc", "rp54d", "rp54d.d")],
        "ROLLBACK",
    ],
    "a policy, statistics object and publication on a schema's table": [
        "BEGIN",
        "CREATE SCHEMA",
        "CREATE TABLE",
        "CREATE POLICY",
        "CREATE STATISTICS",
        "CREATE PUBLICATION",
        "ALTER POLICY",
        "ALTER POLICY",
        "SET",
        "CREATE POLICY",
        "CREATE STATISTICS",
        "ALTER PUBLICATION",
        "RESET",
        [("rp54_p2", "rp54q.t"), ("rp54_p3", "rp54q.t")],
        [("st", "rp54q.t"), ("st2", "rp54q.t")],
        [("0",)],
        "ROLLBACK",
    ],
    "a regprocedure's argument types in their schema": [[("177", "1257", "1257")]],
    "a statistics object's schema": [
        "BEGIN",
        "CREATE SCHEMA",
        "CREATE TABLE",
        "CREATE STATISTICS",
        "CREATE STATISTICS",
        [("bare", "public"), ("named", "rp54s")],
        "ROLLBACK",
    ],
    "every kind of object identified, and found again from its address": [
        "BEGIN",
        "CREATE SCHEMA",
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE VIEW",
        "CREATE FUNCTION",
        "CREATE SEQUENCE",
        "CREATE TYPE",
        "CREATE DOMAIN",
        "CREATE FUNCTION",
        "CREATE TRIGGER",
        "CREATE POLICY",
        "CREATE FOREIGN DATA WRAPPER",
        "CREATE SERVER",
        "CREATE USER MAPPING",
        "CREATE USER MAPPING",
        "ALTER DEFAULT PRIVILEGES",
        "CREATE PUBLICATION",
        "CREATE STATISTICS",
        [
            ("pg_am", "access method", "None", "btree", "btree", "{btree}", "{}", "True"),
            (
                "pg_amop",
                "operator of access method",
                "None",
                "None",
                "operator 1 (smallint, smallint) of pg_catalog.integer_ops USING btree",
                "{btree,pg_catalog,integer_ops,1}",
                "{smallint,smallint}",
                "True",
            ),
            (
                "pg_amproc",
                "function of access method",
                "None",
                "None",
                "function 1 (smallint, smallint) of pg_catalog.integer_ops USING btree",
                "{btree,pg_catalog,integer_ops,1}",
                "{smallint,smallint}",
                "True",
            ),
            (
                "pg_attrdef",
                "default value",
                "None",
                "None",
                "for rp54.t.a",
                "{rp54,t,a}",
                "{}",
                "True",
            ),
            ("pg_authid", "role", "None", "postgres", "postgres", "{postgres}", "{}", "True"),
            (
                "pg_cast",
                "cast",
                "None",
                "None",
                "(integer AS bigint)",
                "{integer}",
                "{bigint}",
                "True",
            ),
            ("pg_class", "index", "rp54", "rp54_i", "rp54.rp54_i", "{rp54,rp54_i}", "{}", "True"),
            ("pg_class", "relation", "None", "None", "None", "None", "None", "None"),
            ("pg_class", "sequence", "rp54", "s", "rp54.s", "{rp54,s}", "{}", "True"),
            ("pg_class", "table", "rp54", "t", "rp54.t", "{rp54,t}", "{}", "True"),
            ("pg_class", "table column", "rp54", "t", "rp54.t.b", "{rp54,t,b}", "{}", "True"),
            ("pg_class", "view", "rp54", "v", "rp54.v", "{rp54,v}", "{}", "True"),
            (
                "pg_collation",
                "collation",
                "pg_catalog",
                '"C"',
                'pg_catalog."C"',
                "{pg_catalog,C}",
                "{}",
                "True",
            ),
            ("pg_constraint", "constraint", "None", "None", "None", "None", "None", "None"),
            (
                "pg_constraint",
                "domain constraint",
                "rp54",
                "None",
                "rp54_dpos on rp54.d",
                "{rp54.d}",
                "{rp54_dpos}",
                "True",
            ),
            (
                "pg_constraint",
                "table constraint",
                "rp54",
                "None",
                "t_a_check on rp54.t",
                "{rp54,t,t_a_check}",
                "{}",
                "True",
            ),
            ("pg_database", "database", "None", "postgres", "postgres", "{postgres}", "{}", "True"),
            (
                "pg_default_acl",
                "default acl",
                "None",
                "None",
                "for role postgres in schema rp54 on tables",
                "{postgres,rp54}",
                "{r}",
                "True",
            ),
            ("pg_extension", "extension", "None", "plpgsql", "plpgsql", "{plpgsql}", "{}", "True"),
            (
                "pg_foreign_data_wrapper",
                "foreign-data wrapper",
                "None",
                "rp54_fdw",
                "rp54_fdw",
                "{rp54_fdw}",
                "{}",
                "True",
            ),
            (
                "pg_foreign_server",
                "server",
                "None",
                "rp54_srv",
                "rp54_srv",
                "{rp54_srv}",
                "{}",
                "True",
            ),
            ("pg_language", "language", "None", "plpgsql", "plpgsql", "{plpgsql}", "{}", "True"),
            ("pg_namespace", "schema", "None", "rp54", "rp54", "{rp54}", "{}", "True"),
            (
                "pg_opclass",
                "operator class",
                "pg_catalog",
                "int4_ops",
                "pg_catalog.int4_ops USING btree",
                "{btree,pg_catalog,int4_ops}",
                "{}",
                "True",
            ),
            (
                "pg_operator",
                "operator",
                "pg_catalog",
                "None",
                "pg_catalog.+(integer,integer)",
                "{pg_catalog,+}",
                "{integer,integer}",
                "True",
            ),
            (
                "pg_opfamily",
                "operator family",
                "pg_catalog",
                "integer_ops",
                "pg_catalog.integer_ops USING btree",
                "{btree,pg_catalog,integer_ops}",
                "{}",
                "True",
            ),
            (
                "pg_policy",
                "policy",
                "None",
                "None",
                "rp54_pol on rp54.t",
                "{rp54,t,rp54_pol}",
                "{}",
                "True",
            ),
            (
                "pg_proc",
                "function",
                "pg_catalog",
                "None",
                "pg_catalog.int4pl(integer,integer)",
                "{pg_catalog,int4pl}",
                "{integer,integer}",
                "True",
            ),
            (
                "pg_proc",
                "function",
                "rp54",
                "None",
                "rp54.f(integer,pg_catalog.text)",
                "{rp54,f}",
                "{integer,pg_catalog.text}",
                "True",
            ),
            ("pg_proc", "function", "rp54", "None", "rp54.tf()", "{rp54,tf}", "{}", "True"),
            ("pg_proc", "routine", "None", "None", "None", "None", "None", "None"),
            (
                "pg_publication",
                "publication",
                "None",
                "rp54_pub",
                "rp54_pub",
                "{rp54_pub}",
                "{}",
                "True",
            ),
            (
                "pg_publication_rel",
                "publication relation",
                "None",
                "None",
                "rp54.t in publication rp54_pub",
                "{rp54,t}",
                "{rp54_pub}",
                "True",
            ),
            (
                "pg_rewrite",
                "rule",
                "None",
                "None",
                '"_RETURN" on rp54.v',
                "{rp54,v,_RETURN}",
                "{}",
                "True",
            ),
            (
                "pg_statistic_ext",
                "statistics object",
                "rp54",
                "st",
                "rp54.st",
                "{rp54,st}",
                "{}",
                "True",
            ),
            (
                "pg_tablespace",
                "tablespace",
                "None",
                "pg_default",
                "pg_default",
                "{pg_default}",
                "{}",
                "True",
            ),
            (
                "pg_trigger",
                "trigger",
                "None",
                "None",
                "rp54_tg on rp54.t",
                "{rp54,t,rp54_tg}",
                "{}",
                "True",
            ),
            (
                "pg_ts_config",
                "text search configuration",
                "pg_catalog",
                "english",
                "pg_catalog.english",
                "{pg_catalog,english}",
                "{}",
                "True",
            ),
            (
                "pg_ts_dict",
                "text search dictionary",
                "pg_catalog",
                "simple",
                "pg_catalog.simple",
                "{pg_catalog,simple}",
                "{}",
                "True",
            ),
            (
                "pg_ts_parser",
                "text search parser",
                "pg_catalog",
                '"default"',
                'pg_catalog."default"',
                "{pg_catalog,default}",
                "{}",
                "True",
            ),
            (
                "pg_ts_template",
                "text search template",
                "pg_catalog",
                "simple",
                "pg_catalog.simple",
                "{pg_catalog,simple}",
                "{}",
                "True",
            ),
            ("pg_type", "type", "pg_catalog", "bool", "boolean", "{boolean}", "{}", "True"),
            ("pg_type", "type", "pg_catalog", "bpchar", "character", "{character}", "{}", "True"),
            (
                "pg_type",
                "type",
                "pg_catalog",
                '"varchar"',
                "character varying",
                '{"character varying"}',
                "{}",
                "True",
            ),
            ("pg_type", "type", "pg_catalog", "int4", "integer", "{integer}", "{}", "True"),
            ("pg_type", "type", "pg_catalog", '"numeric"', "numeric", "{numeric}", "{}", "True"),
            (
                "pg_type",
                "type",
                "pg_catalog",
                '"char"',
                'pg_catalog."char"',
                '{"pg_catalog.\\"char\\""}',
                "{}",
                "True",
            ),
            (
                "pg_type",
                "type",
                "pg_catalog",
                "int4range",
                "pg_catalog.int4range",
                "{pg_catalog.int4range}",
                "{}",
                "True",
            ),
            (
                "pg_type",
                "type",
                "pg_catalog",
                "jsonb",
                "pg_catalog.jsonb",
                "{pg_catalog.jsonb}",
                "{}",
                "True",
            ),
            (
                "pg_type",
                "type",
                "pg_catalog",
                "name",
                "pg_catalog.name",
                "{pg_catalog.name}",
                "{}",
                "True",
            ),
            (
                "pg_type",
                "type",
                "pg_catalog",
                "oid",
                "pg_catalog.oid",
                "{pg_catalog.oid}",
                "{}",
                "True",
            ),
            (
                "pg_type",
                "type",
                "pg_catalog",
                "regclass",
                "pg_catalog.regclass",
                "{pg_catalog.regclass}",
                "{}",
                "True",
            ),
            (
                "pg_type",
                "type",
                "pg_catalog",
                "text",
                "pg_catalog.text",
                "{pg_catalog.text}",
                "{}",
                "True",
            ),
            (
                "pg_type",
                "type",
                "pg_catalog",
                "_text",
                "pg_catalog.text[]",
                "{pg_catalog.text[]}",
                "{}",
                "True",
            ),
            ("pg_type", "type", "rp54", "d", "rp54.d", "{rp54.d}", "{}", "True"),
            ("pg_type", "type", "rp54", "e", "rp54.e", "{rp54.e}", "{}", "True"),
            ("pg_type", "type", "rp54", "t", "rp54.t", "{rp54.t}", "{}", "True"),
            ("pg_type", "type", "rp54", "v", "rp54.v", "{rp54.v}", "{}", "True"),
            (
                "pg_type",
                "type",
                "pg_catalog",
                "timestamptz",
                "timestamp with time zone",
                '{"timestamp with time zone"}',
                "{}",
                "True",
            ),
            (
                "pg_user_mapping",
                "user mapping",
                "None",
                "None",
                "postgres on server rp54_srv",
                "{postgres}",
                "{rp54_srv}",
                "True",
            ),
            (
                "pg_user_mapping",
                "user mapping",
                "None",
                "None",
                "public on server rp54_srv",
                "{public}",
                "{rp54_srv}",
                "True",
            ),
        ],
        "ROLLBACK",
    ],
    "pg_get_object_address refuses as PostgreSQL does": [
        ("ERR", "42P01", 'relation "nope" does not exist', None),
        ("ERR", "42809", '"pg_class" is not an index', None),
        ("ERR", "22023", 'unrecognized object type "bogus"', None),
        ("ERR", "22023", "name list length must be exactly 1", None),
        ("ERR", "3F000", 'schema "nope" does not exist', None),
        ("ERR", "42883", "function nope(integer, text) does not exist", None),
        ("ERR", "42704", 'type "nope" does not exist', None),
        ("ERR", "42703", 'column "nope" of relation "pg_class" does not exist', None),
        ("ERR", "42704", 'server "nope" does not exist', None),
        [("2617", "551", "0")],
        [("2605", "10010", "0")],
        [("2602", "10000", "0")],
        (
            "ERR",
            "42704",
            "operator 9 (pg_catalog.int2, "
            "pg_catalog.int2) of operator family "
            "integer_ops for access method btree does "
            "not exist",
            None,
        ),
        (
            "ERR",
            "42704",
            "function 9 (pg_catalog.int2, "
            "pg_catalog.int2) of operator family "
            "integer_ops for access method btree does "
            "not exist",
            None,
        ),
        ("ERR", "42704", 'parameter ACL "work_mem" does not exist', None),
        ("ERR", "22023", 'unrecognized object type "role membership"', None),
        ("ERR", "42704", 'default ACL for user "postgres" on tables does not exist', None),
        (
            "ERR",
            "22023",
            'unrecognized default ACL object type "x"',
            'Valid object types are "r", "S", "f", "T", "n".',
        ),
    ],
    "pg_identify_object as a value, a row and its fields": [
        [("(table,pg_catalog,pg_class,pg_catalog.pg_class)", "True")],
        [("table", "pg_catalog", "pg_class", "pg_catalog.pg_class")],
        [("pg_catalog.int4pl(integer,integer)",)],
        [("table column", "pg_catalog.pg_class.relname")],
        [("None", "None", "None", "None")],
        [("table", "['pg_catalog', 'pg_class']", "[]")],
        ("ERR", "XX000", "unrecognized object class: 1", None),
        [("operator", "['pg_catalog', '-']", "['integer']")],
    ],
    "pg_options_to_table of an untyped array literal": [
        [("a", "1"), ("b", "2")],
        [("('fillfactor', '70')",)],
        [("10", "10", "SELECT", "False")],
    ],
    "syntax PostgreSQL 16 added is refused as 15 refuses it": [
        "BEGIN",
        "CREATE TABLE",
        ("ERR", "42601", 'syntax error at or near "on"', None),
        "ROLLBACK",
        "BEGIN",
        "CREATE TABLE",
        ("ERR", "42601", 'syntax error at or near "ON"', None),
        "ROLLBACK",
        ("ERR", "42601", 'syntax error at or near "json"', None),
        ("ERR", "42601", 'syntax error at or near "Json"', None),
    ],
    "to_regoper and to_regoperator": [[("None", "597", "None", "551", "558")]],
}


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


if __name__ == "__main__":
    import pprint

    dsn = sys.argv[1]
    measured = {
        name: run_scenario(dsn + " dbname=postgres", stmts)
        for name, (_, _, stmts) in SCENARIOS.items()
    }
    print("EXPECTED: dict[str, Any] = " + pprint.pformat(measured, width=100, sort_dicts=True))
