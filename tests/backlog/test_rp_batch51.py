"""Batch 51 of the Rust PG server (2026-10-23): the regression tests of
RP-169's text-search dictionaries (`CREATE / ALTER / DROP TEXT SEARCH
DICTIONARY` over the `simple`, `snowball` and `synonym` templates, and
`ts_lexize` and the configurations lexing through them), operator families
and classes, subscriptions and transforms; RP-201's `shobj_description`;
and the gap tests of what is left of RP-169, of RP-199 (the object-address
functions) and of RP-200 (a block's own index definitions).

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch51.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario runs in a
transaction it rolls back (an error inside one rolls back to a savepoint),
except two that must commit -- a comment on a database, a second database
-- and put back what they changed.
An error whose message names PostgreSQL's installation directory (a
dictionary file it cannot open) is compared by its SQLSTATE alone, which a
DO block raises as a notice.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario


def _sqlstate_of(sql: str) -> str:
    """`~` statement: the SQLSTATE `sql` fails with, as a notice."""
    return "~do $$ begin " + sql + "; exception when others then raise notice '%', sqlstate; end $$"


def _guarded(sql: str) -> list[str]:
    """`sql`, which fails, inside a savepoint the scenario goes on after."""
    return ["savepoint e", sql, "rollback to savepoint e"]


SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "a simple, a snowball and a synonym dictionary": (
        "RP-169",
        False,
        [
            "begin",
            "create text search dictionary rp51_d "
            "(template = simple, StopWords = English, ACCEPT = true)",
            "select dictinitoption from pg_ts_dict where dictname = 'rp51_d'",
            "alter text search dictionary rp51_d (accept = false, stopwords = french)",
            "select dictinitoption from pg_ts_dict where dictname = 'rp51_d'",
            "select ts_lexize('rp51_d', 'Le'), ts_lexize('rp51_d', 'Chat'), "
            "ts_lexize('rp51_d', 'The')",
            "alter text search dictionary rp51_d (accept)",
            "select dictinitoption from pg_ts_dict where dictname = 'rp51_d'",
            "create text search dictionary rp51_sn "
            "(template = snowball, language = english, stopwords = english)",
            "select ts_lexize('rp51_sn', 'Running'), ts_lexize('rp51_sn', 'the')",
            "create text search dictionary rp51_sy (template = synonym, synonyms = synonym_sample)",
            "select ts_lexize('rp51_sy', 'Postgres'), ts_lexize('rp51_sy', 'indices'), "
            "ts_lexize('rp51_sy', 'x')",
            "select dictname, dicttemplate::int, dictinitoption from pg_ts_dict "
            "where dictname like 'rp51_%' order by 1",
            "rollback",
        ],
    ),
    "a configuration lexes through user dictionaries in order": (
        "RP-169",
        False,
        [
            "begin",
            "create text search dictionary rp51_sy (template = synonym, synonyms = synonym_sample)",
            "create text search dictionary rp51_st (template = simple, stopwords = english, "
            "accept = false)",
            "create text search dictionary rp51_sn (template = snowball, language = english)",
            "create text search configuration rp51_c (copy = simple)",
            "alter text search configuration rp51_c alter mapping for asciiword "
            "with rp51_sy, rp51_st, rp51_sn",
            "select to_tsvector('rp51_c', 'The cats running Postgres')",
            "select alias, dictionaries::text, dictionary::text, lexemes::text "
            "from ts_debug('rp51_c', 'The Postgres cats') where alias = 'asciiword'",
            "~drop text search dictionary rp51_sn cascade",
            "select count(*) from pg_ts_config where cfgname = 'rp51_c'",
            "rollback",
        ],
    ),
    "a dictionary's errors": (
        "RP-169",
        False,
        [
            "begin",
            *_guarded("create text search dictionary rp51_e (template = snowball)"),
            *_guarded(
                "create text search dictionary rp51_e (template = snowball, language = klingon)"
            ),
            *_guarded("create text search dictionary rp51_e (template = simple, foo = 1)"),
            *_guarded("create text search dictionary rp51_e (template = simple, accept = maybe)"),
            *_guarded("create text search dictionary rp51_e (template = nosuch)"),
            *_guarded("create text search dictionary rp51_e (stopwords = english)"),
            *_guarded(
                "create text search dictionary rp51_e (template = simple, stopwords = 'English')"
            ),
            *_guarded(
                "create text search dictionary rp51_e (template = snowball, language = english, "
                "foo = 1)"
            ),
            _sqlstate_of(
                "create text search dictionary rp51_e (template = simple, stopwords = nosuch)"
            ),
            _sqlstate_of(
                "create text search dictionary rp51_e (template = synonym, synonyms = nosuch)"
            ),
            "create text search dictionary rp51_q (template = simple)",
            *_guarded("alter text search dictionary rp51_q (template = snowball)"),
            *_guarded("create text search dictionary rp51_q (template = simple)"),
            *_guarded("alter text search dictionary nosuch (accept = true)"),
            "~drop text search dictionary if exists nosuch",
            *_guarded("drop text search dictionary simple"),
            "!savepoint e",
            "!drop text search dictionary english_stem",
            "rollback to savepoint e",
            "rollback",
        ],
    ),
    "rename, owner, schema and comment": (
        "RP-169",
        False,
        [
            "begin",
            "create text search dictionary rp51_r (template = pg_catalog.simple)",
            "alter text search dictionary rp51_r rename to rp51_r2",
            "alter text search dictionary rp51_r2 owner to postgres",
            "comment on text search dictionary rp51_r2 is 'a dictionary'",
            "select obj_description('rp51_r2'::regdictionary, 'pg_ts_dict')",
            "select 'rp51_r2'::regdictionary::text",
            "create schema rp51_s",
            "alter text search dictionary rp51_r2 set schema rp51_s",
            "select 'rp51_s.rp51_r2'::regdictionary::text, dictnamespace::regnamespace::text "
            "from pg_ts_dict where dictname = 'rp51_r2'",
            "drop text search dictionary rp51_s.rp51_r2",
            "select count(*) from pg_ts_dict where dictname like 'rp51_r%'",
            "rollback",
        ],
    ),
    "a built-in dictionary changed": (
        "RP-169",
        False,
        [
            "begin",
            "alter text search dictionary english_stem (stopwords = french)",
            "select dictinitoption from pg_ts_dict where dictname = 'english_stem'",
            "select ts_lexize('english_stem', 'le'), ts_lexize('english_stem', 'the')",
            "select to_tsvector('english', 'le chat and the cats')",
            "rollback",
        ],
    ),
    # ------------------------------------- operator families and classes
    "an operator family and class in the catalogs": (
        "RP-169",
        False,
        [
            "begin",
            "create function rp51_cmp(int, int) returns int immutable language sql "
            "as 'select btint4cmp($1, $2)'",
            "create operator family rp51_f using btree",
            "create operator class rp51_c for type int using btree family rp51_f as "
            "operator 1 <, operator 3 =, function 1 rp51_cmp(int, int)",
            "select opfname, opfmethod::int, opfnamespace::regnamespace::text, "
            "opfowner::regrole::text from pg_opfamily where opfname = 'rp51_f'",
            "select opcname, opcmethod::int, (select opfname from pg_opfamily f "
            "where f.oid = opcfamily), opcintype::regtype::text, opcdefault, opckeytype::int "
            "from pg_opclass where opcname = 'rp51_c'",
            "select amopstrategy, amoplefttype::regtype::text, amoprighttype::regtype::text, "
            "amopopr::regoperator::text, amoppurpose, amopmethod::int, amopsortfamily::int "
            "from pg_amop a join pg_opfamily f on f.oid = a.amopfamily "
            "where f.opfname = 'rp51_f' order by 1",
            "select amprocnum, amproclefttype::regtype::text, amprocrighttype::regtype::text, "
            "amproc::regprocedure::text from pg_amproc a join pg_opfamily f "
            "on f.oid = a.amprocfamily where f.opfname = 'rp51_f' order by 1",
            "create table rp51_t (a int, b text)",
            "insert into rp51_t values (2, 'x'), (1, 'y')",
            "create index rp51_i on rp51_t (a rp51_c)",
            "create index rp51_i2 on rp51_t (b text_pattern_ops)",
            "select indexdef from pg_indexes where indexname in ('rp51_i', 'rp51_i2') order by 1",
            "select c.relname, o.opcname from pg_index i join pg_class c on c.oid = i.indexrelid "
            "join pg_opclass o on o.oid = i.indclass[0] "
            "where c.relname in ('rp51_i', 'rp51_i2') order by 1",
            "select a from rp51_t where a < 2",
            "rollback",
        ],
    ),
    "an implicit family, a hash class and a storage type": (
        "RP-169",
        False,
        [
            "begin",
            "create operator class rp51_h for type int using hash as "
            "operator 1 =, function 1 hashint4(int)",
            "select opcname, (select opfname from pg_opfamily f where f.oid = opcfamily), "
            "(select opfmethod::int from pg_opfamily f where f.oid = opcfamily) "
            "from pg_opclass where opcname = 'rp51_h'",
            "select amprocnum, amproclefttype::regtype::text, amprocrighttype::regtype::text "
            "from pg_amproc a join pg_opfamily f on f.oid = a.amprocfamily "
            "where f.opfname = 'rp51_h'",
            "create operator class rp51_s for type int using btree as "
            "operator 1 <, function 1 btint4cmp(int, int), storage int",
            "select opckeytype::int from pg_opclass where opcname = 'rp51_s'",
            "create operator family rp51_g using btree",
            "create operator class rp51_s2 for type int using btree family rp51_g as operator 1 <",
            "create operator class rp51_s3 for type int8 using btree family rp51_g as operator 1 <",
            "select opcname, opcintype::regtype::text from pg_opclass c join pg_opfamily f "
            "on f.oid = c.opcfamily where f.opfname = 'rp51_g' order by 1",
            "create table rp51_h_t (a int)",
            "create index rp51_hi on rp51_h_t using hash (a rp51_h)",
            "select indexdef from pg_indexes where indexname = 'rp51_hi'",
            "rollback",
        ],
    ),
    "an operator class's errors": (
        "RP-169",
        False,
        [
            "begin",
            *_guarded("create operator class rp51_e for type int using nosuch as operator 1 <"),
            *_guarded(
                "create operator class rp51_e for type int using btree family nosuch as "
                "operator 1 <"
            ),
            "!savepoint e",
            "!create operator class rp51_e default for type int using btree as operator 1 <",
            "rollback to savepoint e",
            *_guarded("create operator class rp51_e for type int using btree as operator 9 <"),
            *_guarded(
                "create operator class rp51_e for type int using btree as "
                "function 6 btint4cmp(int, int)"
            ),
            *_guarded("create operator class rp51_e for type int using btree as operator 1 <<<"),
            *_guarded("create operator class rp51_e for type int using btree as operator 1 +"),
            *_guarded(
                "create operator class rp51_e for type int using btree as "
                "function 1 nosuch(int, int)"
            ),
            *_guarded(
                "create operator class rp51_e for type int using btree as "
                "function 1 int4eq(int, int)"
            ),
            *_guarded(
                "create operator class rp51_e for type int using btree as function 1 int4abs(int)"
            ),
            *_guarded(
                "create operator class rp51_e for type int using hash as "
                "function 1 btint4cmp(int, int)"
            ),
            *_guarded(
                "create operator class rp51_e for type int using btree as "
                "operator 1 <, operator 1 <"
            ),
            *_guarded(
                "create operator class rp51_e for type int using btree as "
                "operator 1 <, storage text"
            ),
            *_guarded(
                "create operator class rp51_e for type int using btree as "
                "operator 1 <, storage int, storage int"
            ),
            *_guarded("create operator class rp51_e for type nosuch using btree as operator 1 <"),
            *_guarded("create operator family rp51_e using nosuch"),
            "create operator class rp51_e for type int using btree as operator 1 <",
            *_guarded("create operator class rp51_e for type int using btree as operator 1 <"),
            *_guarded("create operator family rp51_e using btree"),
            "create role rp51_r",
            "set role rp51_r",
            *_guarded("create operator family rp51_x using btree"),
            *_guarded("create operator class rp51_x for type int using btree as operator 1 <"),
            *_guarded("alter operator family rp51_e using btree add operator 2 <= (int, int)"),
            "reset role",
            *_guarded("create table rp51_et (a int); create index on rp51_et (a nosuch_ops)"),
            *_guarded("create table rp51_et (a text); create index on rp51_et (a rp51_e)"),
            "rollback",
        ],
    ),
    "alter operator family": (
        "RP-169",
        False,
        [
            "begin",
            "create operator family rp51_f using btree",
            "alter operator family rp51_f using btree add operator 1 < (int, int), "
            "operator 1 < (int, int8), function 1 (int, int8) btint48cmp(int, int8), "
            "function 1 btint4cmp(int, int)",
            "select amopstrategy, amoplefttype::regtype::text, amoprighttype::regtype::text, "
            "amopopr::regoperator::text from pg_amop a join pg_opfamily f "
            "on f.oid = a.amopfamily where f.opfname = 'rp51_f' order by 2, 3",
            "select amprocnum, amproclefttype::regtype::text, amprocrighttype::regtype::text, "
            "amproc::regprocedure::text from pg_amproc a join pg_opfamily f "
            "on f.oid = a.amprocfamily where f.opfname = 'rp51_f' order by 2, 3",
            *_guarded("alter operator family rp51_f using btree add operator 1 < (int, int)"),
            *_guarded(
                "alter operator family rp51_f using btree add function 1 btint4cmp(int, int)"
            ),
            *_guarded("alter operator family rp51_f using btree add operator 2 <="),
            *_guarded("alter operator family rp51_f using btree drop operator 2 (int, int)"),
            *_guarded("alter operator family rp51_f using btree drop function 2 (int, int)"),
            *_guarded("alter operator family rp51_f using btree add storage int"),
            *_guarded("alter operator family rp51_f using nosuch add operator 1 < (int, int)"),
            *_guarded("alter operator family nosuch using btree add operator 1 < (int, int)"),
            *_guarded("alter operator family rp51_f using btree add operator 9 < (int, int)"),
            "alter operator family rp51_f using btree drop operator 1 (int, int8), "
            "function 1 (int, int8)",
            "select count(*) from pg_amop a join pg_opfamily f on f.oid = a.amopfamily "
            "where f.opfname = 'rp51_f'",
            "create operator class rp51_c for type int using btree family rp51_f as operator 3 =",
            "select count(*) from pg_amop a join pg_opfamily f on f.oid = a.amopfamily "
            "where f.opfname = 'rp51_f'",
            "drop operator class rp51_c using btree",
            "select amopstrategy from pg_amop a join pg_opfamily f on f.oid = a.amopfamily "
            "where f.opfname = 'rp51_f'",
            "drop operator family rp51_f using btree",
            "select count(*) from pg_amop a where not exists "
            "(select from pg_opfamily f where f.oid = a.amopfamily)",
            "rollback",
        ],
    ),
    "rename, owner, schema, comment and drop of a class": (
        "RP-169",
        False,
        [
            "begin",
            "create operator family rp51_f using btree",
            "create operator class rp51_c for type int using btree family rp51_f as "
            "operator 1 <, function 1 btint4cmp(int, int)",
            "create operator class rp51_c3 for type int using btree family rp51_f as operator 3 =",
            "alter operator family rp51_f using btree rename to rp51_f2",
            "alter operator class rp51_c using btree rename to rp51_c2",
            *_guarded("alter operator class rp51_c2 using btree rename to rp51_c3"),
            *_guarded("alter operator family rp51_f2 using btree rename to rp51_f2"),
            "create role rp51_o",
            "alter operator family rp51_f2 using btree owner to rp51_o",
            "alter operator class rp51_c2 using btree owner to rp51_o",
            "create schema rp51_sc",
            "alter operator class rp51_c2 using btree set schema rp51_sc",
            "alter operator family rp51_f2 using btree set schema rp51_sc",
            "select opcname, opcnamespace::regnamespace::text, opcowner::regrole::text "
            "from pg_opclass where opcname like 'rp51%' order by 1",
            "select opfname, opfnamespace::regnamespace::text, opfowner::regrole::text "
            "from pg_opfamily where opfname like 'rp51%' order by 1",
            "comment on operator class rp51_sc.rp51_c2 using btree is 'cc'",
            "comment on operator family rp51_sc.rp51_f2 using btree is 'ff'",
            "select obj_description(oid, 'pg_opclass') from pg_opclass where opcname = 'rp51_c2'",
            "select obj_description(oid, 'pg_opfamily') from pg_opfamily where opfname = 'rp51_f2'",
            "create table rp51_t (a int)",
            "create operator class rp51_m for type int using btree as operator 1 <",
            *_guarded("create index on rp51_t (a rp51_m)"),
            "create index rp51_i on rp51_t (a rp51_sc.rp51_c2)",
            "select indexdef from pg_indexes where indexname = 'rp51_i'",
            "!savepoint e",
            "!drop operator class rp51_sc.rp51_c2 using btree",
            "rollback to savepoint e",
            "!savepoint e",
            "!drop operator family rp51_sc.rp51_f2 using btree",
            "rollback to savepoint e",
            "~drop operator family rp51_sc.rp51_f2 using btree cascade",
            "select count(*) from pg_opclass where opcname like 'rp51_c%'",
            "select count(*) from pg_class where relname = 'rp51_i'",
            "~drop operator class if exists nosuch using btree",
            "~drop operator family if exists nosuch using btree",
            *_guarded("drop operator class nosuch using btree"),
            *_guarded("drop operator family nosuch using hash"),
            *_guarded("drop operator class int4_ops using btree"),
            "rollback",
        ],
    ),
    "a class's functions and operators are its dependencies": (
        "RP-169",
        False,
        [
            "begin",
            "create function rp51_cmp(int, int) returns int immutable language sql "
            "as 'select btint4cmp($1, $2)'",
            "create function rp51_cmp2(int, int) returns int immutable language sql "
            "as 'select btint4cmp($1, $2)'",
            "create operator class rp51_c for type int using btree as operator 1 <, "
            "function 1 rp51_cmp(int, int)",
            "!savepoint e",
            "!drop function rp51_cmp(int, int)",
            "rollback to savepoint e",
            "~drop function rp51_cmp(int, int) cascade",
            "select count(*) from pg_opclass where opcname = 'rp51_c'",
            "select count(*) from pg_opfamily where opfname = 'rp51_c'",
            "create operator family rp51_g using btree",
            "alter operator family rp51_g using btree add function 1 (int, int) "
            "rp51_cmp2(int, int)",
            "~drop function rp51_cmp2(int, int)",
            "select count(*) from pg_amproc a join pg_opfamily f on f.oid = a.amprocfamily "
            "where f.opfname = 'rp51_g'",
            "rollback",
        ],
    ),
    # ------------------------------------- RP-199 the object-address functions
    "pg_describe_object": (
        "RP-199",
        False,
        [
            "begin",
            "select pg_describe_object('pg_class'::regclass, 'pg_class'::regclass, 0), "
            "pg_describe_object('pg_class'::regclass, 'pg_class'::regclass, 2), "
            "pg_describe_object('pg_type'::regclass, 'int4'::regtype, 0), "
            "pg_describe_object('pg_proc'::regclass, 'now'::regproc, 0), "
            "pg_describe_object('pg_namespace'::regclass, 'public'::regnamespace, 0)",
            "create operator family rp51_f using btree",
            "create operator class rp51_c for type int using btree family rp51_f as "
            "operator 1 <, function 1 btint4cmp(int, int)",
            "select pg_describe_object('pg_opclass'::regclass, oid, 0) from pg_opclass "
            "where opcname = 'rp51_c'",
            "select pg_describe_object('pg_opfamily'::regclass, oid, 0) from pg_opfamily "
            "where opfname = 'rp51_f'",
            "select pg_describe_object('pg_amop'::regclass, a.oid, 0) from pg_amop a "
            "join pg_opfamily f on f.oid = a.amopfamily where f.opfname = 'rp51_f'",
            "select pg_describe_object('pg_amproc'::regclass, a.oid, 0) from pg_amproc a "
            "join pg_opfamily f on f.oid = a.amprocfamily where f.opfname = 'rp51_f'",
            "create transform for int language sql (from sql with function prsd_lextype(internal))",
            "select pg_describe_object('pg_transform'::regclass, oid, 0) from pg_transform",
            "create subscription rp51_s connection 'dbname=x' publication p with (connect = false)",
            "select pg_describe_object('pg_subscription'::regclass, oid, 0) from pg_subscription",
            "create schema rp51_ds",
            "create table rp51_ds.t (a int default 1, constraint c check (a > 0))",
            "create view rp51_ds.v as select a from rp51_ds.t",
            "create sequence rp51_ds.s",
            "create index rp51_di on rp51_ds.t (a)",
            "select pg_describe_object('pg_attrdef'::regclass, oid, 0) from pg_attrdef "
            "where adrelid = 'rp51_ds.t'::regclass",
            "select pg_describe_object('pg_constraint'::regclass, oid, 0) from pg_constraint "
            "where conrelid = 'rp51_ds.t'::regclass",
            "select pg_describe_object('pg_rewrite'::regclass, oid, 0) from pg_rewrite "
            "where ev_class = 'rp51_ds.v'::regclass",
            "select pg_describe_object('pg_class'::regclass, 'rp51_ds.t'::regclass, 1), "
            "pg_describe_object('pg_type'::regclass, 'rp51_ds.t'::regtype, 0)",
            "select pg_describe_object('pg_class'::regclass, oid, 0) from pg_class "
            "where relnamespace = 'rp51_ds'::regnamespace order by 1",
            *_guarded("select pg_describe_object(0, 1, 0)"),
            *_guarded("select pg_describe_object('pg_roles'::regclass, 1, 0)"),
            "select pg_describe_object('pg_class'::regclass, 1, 0)",
            "select pg_describe_object(1255, oid, 0) from pg_proc where proname = 'btint4cmp'",
            "select pg_describe_object(2605, oid, 0) from pg_cast "
            "where castsource = 'int4'::regtype and casttarget = 'int8'::regtype",
            "select pg_describe_object(1262, oid, 0) from pg_database where datname = 'postgres'",
            "select pg_describe_object(3079, oid, 0) from pg_extension",
            "select 71::regtype::text, format_type(210, null), 1263::regtype::text",
            "rollback",
        ],
    ),
    "pg_identify_object and its siblings": (
        "RP-199",
        False,
        [
            "select pg_identify_object('pg_class'::regclass, 'pg_class'::regclass, 0)::text, "
            "pg_identify_object_as_address('pg_class'::regclass, 'pg_class'::regclass, 0)::text",
            "select * from pg_get_object_address('table', '{pg_class}', '{}')",
        ],
    ),
    # ----------------------------------- RP-203 the system views' rules
    "pg_rewrite lists the system views' rules": (
        "RP-211",
        False,
        [
            "select count(*) from pg_rewrite where ev_class = 'pg_roles'::regclass",
            "select pg_describe_object('pg_rewrite'::regclass, oid, 0) from pg_rewrite "
            "where ev_class = 'pg_roles'::regclass",
            "select count(*) from pg_rewrite",
        ],
    ),
    "a rule's node trees": (
        "RP-203",
        True,
        [
            "select ev_qual::text from pg_rewrite where ev_class = 'pg_roles'::regclass",
            "select left(ev_action::text, 20) from pg_rewrite "
            "where ev_class = 'pg_roles'::regclass",
        ],
    ),
    "pg_get_indexdef of an index the block created": (
        "RP-200",
        False,
        [
            "begin",
            "create table rp51_g (a int, b text)",
            "create index rp51_gi on rp51_g (a)",
            "alter table rp51_g add constraint rp51_gc check (a > 0)",
            "select pg_get_indexdef('rp51_gi'::regclass), "
            "pg_get_indexdef('rp51_gi'::regclass, 1, false)",
            "select pg_get_constraintdef(oid) from pg_constraint where conname = 'rp51_gc'",
            "rollback",
        ],
    ),
    "a GiST operator class": (
        "RP-169",
        True,
        [
            "begin",
            "create operator class rp51_gb for type box using gist as operator 3 &&, "
            "function 1 gist_box_consistent(internal, box, smallint, oid, internal), "
            "function 2 gist_box_union(internal, internal), "
            "function 5 gist_box_penalty(internal, internal, internal), "
            "function 6 gist_box_picksplit(internal, internal), "
            "function 7 gist_box_same(box, box, internal)",
            "select opcname, opcmethod::int, opcintype::regtype::text from pg_opclass "
            "where opcname = 'rp51_gb'",
            "rollback",
        ],
    ),
    "ispell and thesaurus dictionaries": (
        "RP-169",
        True,
        [
            "begin",
            "create text search dictionary rp51_is "
            "(template = ispell, dictfile = ispell_sample, afffile = ispell_sample)",
            "select ts_lexize('rp51_is', 'skies'), ts_lexize('rp51_is', 'bookings')",
            "create text search dictionary rp51_th "
            "(template = thesaurus, dictfile = thesaurus_sample, dictionary = english_stem)",
            "select ts_lexize('rp51_th', 'one')",
            "rollback",
        ],
    ),
    "pg_subscription lists every database's subscriptions": (
        "RP-169",
        False,
        # Committed, then dropped: CREATE DATABASE cannot run in a block.
        [
            "drop database if exists rp51_db",
            "create database rp51_db",
            "@open b dbname=rp51_db",
            "@b create subscription rp51_other connection 'dbname=x' publication p "
            "with (connect = false, slot_name = none)",
            "select subname, subdbid = (select oid from pg_database where datname = 'rp51_db') "
            "from pg_subscription",
            "@b drop subscription rp51_other",
            "@b select 1",
        ],
    ),
    "shobj_description reads a shared object's comment": (
        "RP-201",
        False,
        # Committed, then put back (batch 52's scenario reads one inside a
        # block).
        [
            "comment on database postgres is 'a database'",
            "create role rp51_cr",
            "comment on role rp51_cr is 'a role'",
            "select shobj_description(oid, 'pg_database'), obj_description(oid, 'pg_database') "
            "from pg_database where datname = 'postgres'",
            "select shobj_description(oid, 'pg_authid'), shobj_description(oid, 'pg_database') "
            "from pg_roles where rolname = 'rp51_cr'",
            "drop role rp51_cr",
            "comment on database postgres is 'default administrative connection database'",
        ],
    ),
    # ------------------------------------------------------- subscriptions
    "a subscription created without connecting": (
        "RP-169",
        False,
        [
            "begin",
            "~create subscription rp51_s connection 'dbname=nosuch' publication p1, p2 "
            "with (connect = false)",
            "select subname, subowner::regrole::text, subenabled, subbinary, substream, "
            "subtwophasestate, subdisableonerr, subconninfo, subslotname::text, subsynccommit, "
            "subpublications::text, subskiplsn::text, "
            "subdbid = (select oid from pg_database where datname = current_database()) "
            "from pg_subscription",
            "select roname = 'pg_' || (select oid from pg_subscription)::text "
            "from pg_replication_origin",
            "select count(*) from pg_subscription_rel",
            "select subname, pid, relid, received_lsn::text from pg_stat_subscription",
            "select subname, apply_error_count, sync_error_count, stats_reset "
            "from pg_stat_subscription_stats",
            "alter subscription rp51_s set (slot_name = none)",
            "alter subscription rp51_s set (synchronous_commit = 'local', binary = true, "
            "streaming = true, disable_on_error = true)",
            "alter subscription rp51_s set publication p3 with (refresh = false)",
            "alter subscription rp51_s add publication p4, p5 with (refresh = false)",
            "alter subscription rp51_s drop publication p3 with (refresh = false)",
            "alter subscription rp51_s connection 'dbname=other'",
            "alter subscription rp51_s disable",
            "select subenabled, subbinary, substream, subdisableonerr, subconninfo, "
            "subslotname::text, subsynccommit, subpublications::text from pg_subscription",
            "alter subscription rp51_s rename to rp51_s2",
            "comment on subscription rp51_s2 is 'ss'",
            "select shobj_description(oid, 'pg_subscription'), "
            "obj_description(oid, 'pg_subscription') from pg_subscription",
            "create role rp51_o",
            "!savepoint e",
            "!alter subscription rp51_s2 owner to rp51_o",
            "rollback to savepoint e",
            "create role rp51_su superuser",
            "alter subscription rp51_s2 owner to rp51_su",
            "select subowner::regrole::text from pg_subscription",
            "drop subscription rp51_s2",
            "select count(*) from pg_subscription",
            "select count(*) from pg_replication_origin",
            "rollback",
        ],
    ),
    "a subscription's errors": (
        "RP-169",
        False,
        [
            "begin",
            "create subscription rp51_s connection 'dbname=nosuch' publication p "
            "with (connect = false)",
            *_guarded(
                "create subscription rp51_s connection 'dbname=nosuch' publication p "
                "with (connect = false)"
            ),
            *_guarded(
                "create subscription rp51_e connection 'dbname=x' publication p "
                "with (connect = false, enabled = true)"
            ),
            *_guarded(
                "create subscription rp51_e connection 'dbname=x' publication p "
                "with (connect = false, create_slot = true)"
            ),
            *_guarded(
                "create subscription rp51_e connection 'dbname=x' publication p "
                "with (connect = false, copy_data = true)"
            ),
            *_guarded(
                "create subscription rp51_e connection 'dbname=x' publication p "
                "with (connect = false, foo = 1)"
            ),
            *_guarded(
                "create subscription rp51_e connection 'dbname=x' publication p "
                "with (slot_name = none)"
            ),
            *_guarded(
                "create subscription rp51_e connection 'dbname=x' publication p "
                "with (slot_name = none, enabled = false)"
            ),
            *_guarded(
                "create subscription rp51_e connection 'dbname=x' publication p "
                "with (connect = false, binary = 'x')"
            ),
            "!savepoint e",
            "!create subscription rp51_e connection 'dbname=x' publication p "
            "with (connect = false, synchronous_commit = 'bogus')",
            "rollback to savepoint e",
            *_guarded(
                "create subscription rp51_e connection 'dbname=x' publication p "
                "with (connect = false, connect = false)"
            ),
            *_guarded(
                "create subscription rp51_e connection 'dbname=x' publication p "
                "with (connect = false, slot_name = 'Bad-Name')"
            ),
            *_guarded(
                "create subscription rp51_e connection 'bogus' publication p with (connect = false)"
            ),
            *_guarded(
                "create subscription rp51_e connection 'dbname=x foo=1' publication p "
                "with (connect = false)"
            ),
            *_guarded(
                "create subscription rp51_e connection 'dbname=x' publication p, p "
                "with (connect = false)"
            ),
            *_guarded("alter subscription nosuch disable"),
            *_guarded("alter subscription rp51_s set (connect = false)"),
            *_guarded("alter subscription rp51_s set (two_phase = true)"),
            *_guarded("alter subscription rp51_s refresh publication"),
            "!savepoint e",
            "!alter subscription rp51_s set publication p2",
            "rollback to savepoint e",
            "!savepoint e",
            "!alter subscription rp51_s add publication p",
            "rollback to savepoint e",
            "!savepoint e",
            "!alter subscription rp51_s add publication p with (refresh = false)",
            "rollback to savepoint e",
            "!savepoint e",
            "!alter subscription rp51_s drop publication p with (refresh = false)",
            "rollback to savepoint e",
            *_guarded("alter subscription rp51_s drop publication nosuch with (refresh = false)"),
            *_guarded("drop subscription rp51_s"),
            "alter subscription rp51_s set (slot_name = none)",
            *_guarded("alter subscription rp51_s enable"),
            *_guarded("drop subscription nosuch"),
            "~drop subscription if exists nosuch",
            "create role rp51_r",
            "set role rp51_r",
            *_guarded(
                "create subscription rp51_e connection 'dbname=x' publication p "
                "with (connect = false)"
            ),
            *_guarded("alter subscription rp51_s disable"),
            *_guarded("drop subscription rp51_s"),
            "reset role",
            "rollback",
        ],
    ),
    # ---------------------------------------------------------- transforms
    "a transform": (
        "RP-169",
        False,
        [
            "begin",
            "create transform for int language sql (from sql with function "
            "prsd_lextype(internal), to sql with function int4recv(internal))",
            "select trftype::regtype::text, (select lanname from pg_language l "
            "where l.oid = trflang), trffromsql::text, trftosql::text from pg_transform",
            *_guarded(
                "create transform for int language sql (from sql with function "
                "prsd_lextype(internal))"
            ),
            "create or replace transform for int language sql (from sql with function "
            "prsd_lextype(internal))",
            "select trffromsql::text, trftosql::text from pg_transform",
            "comment on transform for int language sql is 'tt'",
            "select obj_description(oid, 'pg_transform') from pg_transform",
            "create transform for bigint language plpgsql (to sql with function "
            "int8recv(internal))",
            "select trftype::regtype::text, trffromsql::text, trftosql::text from pg_transform "
            "order by 1",
            "drop transform for int language sql",
            "drop transform for bigint language plpgsql",
            "~drop transform if exists for int language sql",
            *_guarded("drop transform for int language sql"),
            "rollback",
        ],
    ),
    "a transform's errors": (
        "RP-169",
        False,
        [
            "begin",
            *_guarded(
                "create transform for int language nosuch (from sql with function "
                "prsd_lextype(internal))"
            ),
            *_guarded(
                "create transform for nosuch language sql (from sql with function "
                "prsd_lextype(internal))"
            ),
            *_guarded(
                "create transform for internal language sql (from sql with function "
                "prsd_lextype(internal))"
            ),
            "create domain rp51_dom as int",
            *_guarded(
                "create transform for rp51_dom language sql (from sql with function "
                "prsd_lextype(internal))"
            ),
            *_guarded(
                "create transform for int language sql (from sql with function int4recv(internal))"
            ),
            *_guarded(
                "create transform for int language sql (to sql with function "
                "prsd_lextype(internal))"
            ),
            *_guarded(
                "create transform for int language sql (from sql with function int4abs(int))"
            ),
            *_guarded(
                "create transform for int language sql (from sql with function nosuch(internal))"
            ),
            *_guarded(
                "create transform for int language sql (from sql with function "
                "pg_stop_making_pinned_objects())"
            ),
            *_guarded("drop transform for int language nosuch"),
            "create role rp51_r",
            "set role rp51_r",
            *_guarded(
                "create transform for int language sql (from sql with function "
                "prsd_lextype(internal))"
            ),
            "reset role",
            "create type rp51_en as enum ('a')",
            "create transform for rp51_en language sql (from sql with function "
            "prsd_lextype(internal))",
            "!savepoint e",
            "!drop type rp51_en",
            "rollback to savepoint e",
            "~drop type rp51_en cascade",
            "select count(*) from pg_transform",
            "rollback",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a GiST operator class": [
        "BEGIN",
        "CREATE OPERATOR CLASS",
        [("rp51_gb", "783", "box")],
        "ROLLBACK",
    ],
    "a built-in dictionary changed": [
        "BEGIN",
        "ALTER TEXT SEARCH DICTIONARY",
        [("language = 'english', stopwords = 'french'",)],
        [("[]", "['the']")],
        [("'and':3 'cat':5 'chat':2 'the':4",)],
        "ROLLBACK",
    ],
    "a class's functions and operators are its dependencies": [
        "BEGIN",
        "CREATE FUNCTION",
        "CREATE FUNCTION",
        "CREATE OPERATOR CLASS",
        "SAVEPOINT",
        (
            "ERR",
            "2BP01",
            "cannot drop function rp51_cmp(integer,integer) because other objects depend on it",
            "Use DROP ... CASCADE to drop the dependent objects too.",
            "operator class rp51_c for access "
            "method btree depends on function "
            "rp51_cmp(integer,integer)",
        ),
        "ROLLBACK",
        ("DROP FUNCTION", ["drop cascades to operator class rp51_c for access method btree"]),
        [("0",)],
        [("1",)],
        "CREATE OPERATOR FAMILY",
        "ALTER OPERATOR FAMILY",
        ("DROP FUNCTION", []),
        [("0",)],
        "ROLLBACK",
    ],
    "a configuration lexes through user dictionaries in order": [
        "BEGIN",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE TEXT SEARCH CONFIGURATION",
        "ALTER TEXT SEARCH CONFIGURATION",
        [("'cat':2 'pgsql':4 'run':3",)],
        [
            ("asciiword", "{rp51_sy,rp51_st,rp51_sn}", "rp51_st", "{}"),
            ("asciiword", "{rp51_sy,rp51_st,rp51_sn}", "rp51_sy", "{pgsql}"),
            ("asciiword", "{rp51_sy,rp51_st,rp51_sn}", "rp51_sn", "{cat}"),
        ],
        ("DROP TEXT SEARCH DICTIONARY", ["drop cascades to text search configuration rp51_c"]),
        [("0",)],
        "ROLLBACK",
    ],
    "a dictionary's errors": [
        "BEGIN",
        "SAVEPOINT",
        ("ERR", "22023", "missing Language parameter", None),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42704",
            'no Snowball stemmer available for language "klingon" and encoding "UTF8"',
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", 'unrecognized simple dictionary parameter: "foo"', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42601", "accept requires a Boolean value", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'text search template "nosuch" does not exist', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "text search template is required", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", 'invalid text search configuration file name "English"', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "22023", 'unrecognized Snowball parameter: "foo"', None),
        "ROLLBACK",
        ("DO", ["F0000"]),
        ("DO", ["F0000"]),
        "CREATE TEXT SEARCH DICTIONARY",
        "SAVEPOINT",
        ("ERR", "22023", 'unrecognized simple dictionary parameter: "template"', None),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "23505",
            'duplicate key value violates unique constraint "pg_ts_dict_dictname_index"',
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'text search dictionary "nosuch" does not exist', None),
        "ROLLBACK",
        (
            "DROP TEXT SEARCH DICTIONARY",
            ['text search dictionary "nosuch" does not exist, skipping'],
        ),
        "SAVEPOINT",
        (
            "ERR",
            "2BP01",
            "cannot drop text search dictionary simple because it is required by "
            "the database system",
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "2BP01",
            "cannot drop text search dictionary english_stem because other objects depend on it",
            "Use DROP ... CASCADE to drop the dependent objects too.",
            "text search configuration english depends on text search dictionary "
            "english_stem\n"
            "text search configuration hindi depends on text search dictionary "
            "english_stem\n"
            "text search configuration russian depends on text search dictionary "
            "english_stem",
        ),
        "ROLLBACK",
        "ROLLBACK",
    ],
    "a rule's node trees": [[("<>",)], [("({QUERY :commandType",)]],
    "a simple, a snowball and a synonym dictionary": [
        "BEGIN",
        "CREATE TEXT SEARCH DICTIONARY",
        [("stopwords = 'english', accept = 'true'",)],
        "ALTER TEXT SEARCH DICTIONARY",
        [("accept = 'false', stopwords = 'french'",)],
        [("[]", "None", "None")],
        "ALTER TEXT SEARCH DICTIONARY",
        [("stopwords = 'french'",)],
        "CREATE TEXT SEARCH DICTIONARY",
        [("['run']", "[]")],
        "CREATE TEXT SEARCH DICTIONARY",
        [("['pgsql']", "['index']", "None")],
        [
            ("rp51_d", "3727", "stopwords = 'french'"),
            ("rp51_sn", "13622", "language = 'english', stopwords = 'english'"),
            ("rp51_sy", "3730", "synonyms = 'synonym_sample'"),
        ],
        "ROLLBACK",
    ],
    "a subscription created without connecting": [
        "BEGIN",
        (
            "CREATE SUBSCRIPTION",
            [
                "tables were not subscribed, you will have to run "
                "ALTER SUBSCRIPTION ... REFRESH PUBLICATION to "
                "subscribe the tables"
            ],
        ),
        [
            (
                "rp51_s",
                "postgres",
                "False",
                "False",
                "False",
                "d",
                "False",
                "dbname=nosuch",
                "rp51_s",
                "off",
                "{p1,p2}",
                "0/0",
                "True",
            )
        ],
        [("True",)],
        [("0",)],
        [("rp51_s", "None", "None", "None")],
        [("rp51_s", "0", "0", "None")],
        "ALTER SUBSCRIPTION",
        "ALTER SUBSCRIPTION",
        "ALTER SUBSCRIPTION",
        "ALTER SUBSCRIPTION",
        "ALTER SUBSCRIPTION",
        "ALTER SUBSCRIPTION",
        "ALTER SUBSCRIPTION",
        [("False", "True", "True", "True", "dbname=other", "None", "local", "{p4,p5}")],
        "ALTER SUBSCRIPTION",
        "COMMENT",
        [("None", "ss")],
        "CREATE ROLE",
        "SAVEPOINT",
        (
            "ERR",
            "42501",
            'permission denied to change owner of subscription "rp51_s2"',
            "The owner of a subscription must be a superuser.",
            None,
        ),
        "ROLLBACK",
        "CREATE ROLE",
        "ALTER SUBSCRIPTION",
        [("rp51_su",)],
        "DROP SUBSCRIPTION",
        [("0",)],
        [("0",)],
        "ROLLBACK",
    ],
    "a subscription's errors": [
        "BEGIN",
        "CREATE SUBSCRIPTION",
        "SAVEPOINT",
        ("ERR", "42710", 'subscription "rp51_s" already exists', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42601", "connect = false and enabled = true are mutually exclusive options", None),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42601",
            "connect = false and create_slot = true are mutually exclusive options",
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42601",
            "connect = false and copy_data = true are mutually exclusive options",
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42601", 'unrecognized subscription parameter: "foo"', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42601", "subscription with slot_name = NONE must also set enabled = false", None),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42601",
            "subscription with slot_name = NONE must also set create_slot = false",
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42601", "binary requires a Boolean value", None),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "22023",
            'invalid value for parameter "synchronous_commit": "bogus"',
            "Available values: local, remote_write, remote_apply, on, off.",
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42601", "conflicting or redundant options", None),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42602",
            'replication slot name "Bad-Name" contains invalid character',
            "Replication slot names may only contain lower case letters, "
            "numbers, and the underscore character.",
        ),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42601",
            'invalid connection string syntax: missing "=" after "bogus" in '
            "connection info string\n",
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42601",
            'invalid connection string syntax: invalid connection option "foo"\n',
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42710", 'publication name "p" used more than once', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'subscription "nosuch" does not exist', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42601", 'unrecognized subscription parameter: "connect"', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42601", 'unrecognized subscription parameter: "two_phase"', None),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "55000",
            "ALTER SUBSCRIPTION ... REFRESH is not allowed for disabled subscriptions",
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "55000",
            "ALTER SUBSCRIPTION with refresh is not allowed for disabled subscriptions",
            "Use ALTER SUBSCRIPTION ... SET PUBLICATION ... WITH (refresh = false).",
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42710", 'publication "p" is already in subscription "rp51_s"', None, None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42710", 'publication "p" is already in subscription "rp51_s"', None, None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "cannot drop all the publications from a subscription", None, None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", 'publication "nosuch" is not in subscription "rp51_s"', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "25001", "DROP SUBSCRIPTION cannot run inside a transaction block", None),
        "ROLLBACK",
        "ALTER SUBSCRIPTION",
        "SAVEPOINT",
        ("ERR", "55000", "cannot enable subscription that does not have a slot name", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'subscription "nosuch" does not exist', None),
        "ROLLBACK",
        ("DROP SUBSCRIPTION", ['subscription "nosuch" does not exist, skipping']),
        "CREATE ROLE",
        "SET",
        "SAVEPOINT",
        ("ERR", "42501", "must be superuser to create subscriptions", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42501", "must be owner of subscription rp51_s", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42501", "must be owner of subscription rp51_s", None),
        "ROLLBACK",
        "RESET",
        "ROLLBACK",
    ],
    "a transform": [
        "BEGIN",
        "CREATE TRANSFORM",
        [("integer", "sql", "prsd_lextype", "int4recv")],
        "SAVEPOINT",
        ("ERR", "42710", 'transform for type integer language "sql" already exists', None),
        "ROLLBACK",
        "CREATE TRANSFORM",
        [("prsd_lextype", "-")],
        "COMMENT",
        [("tt",)],
        "CREATE TRANSFORM",
        [("bigint", "-", "int8recv"), ("integer", "prsd_lextype", "-")],
        "DROP TRANSFORM",
        "DROP TRANSFORM",
        (
            "DROP TRANSFORM",
            ['transform for type pg_catalog.int4 language "sql" does not exist, skipping'],
        ),
        "SAVEPOINT",
        ("ERR", "42704", 'transform for type integer language "sql" does not exist', None),
        "ROLLBACK",
        "ROLLBACK",
    ],
    "a transform's errors": [
        "BEGIN",
        "SAVEPOINT",
        ("ERR", "42704", 'language "nosuch" does not exist', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'type "nosuch" does not exist', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42809", "data type internal is a pseudo-type", None),
        "ROLLBACK",
        "CREATE DOMAIN",
        "SAVEPOINT",
        ("ERR", "42809", "data type rp51_dom is a domain", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "return data type of FROM SQL function must be internal", None),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42P17",
            "return data type of TO SQL function must be the transform data type",
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "return data type of FROM SQL function must be internal", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42883", "function nosuch(internal) does not exist", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "return data type of FROM SQL function must be internal", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'language "nosuch" does not exist', None),
        "ROLLBACK",
        "CREATE ROLE",
        "SET",
        "SAVEPOINT",
        ("ERR", "42501", "must be owner of type integer", None),
        "ROLLBACK",
        "RESET",
        "CREATE TYPE",
        "CREATE TRANSFORM",
        "SAVEPOINT",
        (
            "ERR",
            "2BP01",
            "cannot drop type rp51_en because other objects depend on it",
            "Use DROP ... CASCADE to drop the dependent objects too.",
            "transform for rp51_en language sql depends on type rp51_en",
        ),
        "ROLLBACK",
        ("DROP TYPE", ["drop cascades to transform for rp51_en language sql"]),
        [("0",)],
        "ROLLBACK",
    ],
    "alter operator family": [
        "BEGIN",
        "CREATE OPERATOR FAMILY",
        "ALTER OPERATOR FAMILY",
        [
            ("1", "integer", "bigint", "<(integer,bigint)"),
            ("1", "integer", "integer", "<(integer,integer)"),
        ],
        [
            ("1", "integer", "bigint", "btint48cmp(integer,bigint)"),
            ("1", "integer", "integer", "btint4cmp(integer,integer)"),
        ],
        "SAVEPOINT",
        (
            "ERR",
            "42710",
            'operator 1(integer,integer) already exists in operator family "rp51_f"',
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42710",
            'function 1(integer,integer) already exists in operator family "rp51_f"',
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42601",
            "operator argument types must be specified in ALTER OPERATOR FAMILY",
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42704",
            'operator 2(integer,integer) does not exist in operator family "rp51_f"',
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42704",
            'function 2(integer,integer) does not exist in operator family "rp51_f"',
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42601", "STORAGE cannot be specified in ALTER OPERATOR FAMILY", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'access method "nosuch" does not exist', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'operator family "nosuch" does not exist for access method "btree"', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "invalid operator number 9, must be between 1 and 5", None),
        "ROLLBACK",
        "ALTER OPERATOR FAMILY",
        [("1",)],
        "CREATE OPERATOR CLASS",
        [("2",)],
        "DROP OPERATOR CLASS",
        [("1",)],
        "DROP OPERATOR FAMILY",
        [("0",)],
        "ROLLBACK",
    ],
    "an implicit family, a hash class and a storage type": [
        "BEGIN",
        "CREATE OPERATOR CLASS",
        [("rp51_h", "rp51_h", "405")],
        [("1", "integer", "integer")],
        "CREATE OPERATOR CLASS",
        [("0",)],
        "CREATE OPERATOR FAMILY",
        "CREATE OPERATOR CLASS",
        "CREATE OPERATOR CLASS",
        [("rp51_s2", "integer"), ("rp51_s3", "bigint")],
        "CREATE TABLE",
        "CREATE INDEX",
        [("CREATE INDEX rp51_hi ON public.rp51_h_t USING hash (a rp51_h)",)],
        "ROLLBACK",
    ],
    "an operator class's errors": [
        "BEGIN",
        "SAVEPOINT",
        ("ERR", "42704", 'access method "nosuch" does not exist', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'operator family "nosuch" does not exist for access method "btree"', None),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42710",
            'could not make operator class "rp51_e" be default for type pg_catalog.int4',
            None,
            'Operator class "int4_ops" already is the default.',
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "invalid operator number 9, must be between 1 and 5", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "invalid function number 6, must be between 1 and 5", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42883", "operator does not exist: integer <<< integer", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "index search operators must return boolean", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42883", "function nosuch(integer, integer) does not exist", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "btree comparison functions must return integer", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "btree comparison functions must have two arguments", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "hash function 1 must have one argument", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "operator number 1 for (integer,integer) appears more than once", None),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42P17",
            'storage type cannot be different from data type for access method "btree"',
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", "storage type specified more than once", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'type "nosuch" does not exist', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'access method "nosuch" does not exist', None),
        "ROLLBACK",
        "CREATE OPERATOR CLASS",
        "SAVEPOINT",
        ("ERR", "42710", 'operator class "rp51_e" for access method "btree" already exists', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42710", 'operator family "rp51_e" for access method "btree" already exists', None),
        "ROLLBACK",
        "CREATE ROLE",
        "SET",
        "SAVEPOINT",
        ("ERR", "42501", "permission denied for schema public", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42501", "permission denied for schema public", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42501", "must be superuser to alter an operator family", None),
        "ROLLBACK",
        "RESET",
        "SAVEPOINT",
        (
            "ERR",
            "42704",
            'operator class "nosuch_ops" does not exist for access method "btree"',
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42804", 'operator class "rp51_e" does not accept data type text', None),
        "ROLLBACK",
        "ROLLBACK",
    ],
    "an operator family and class in the catalogs": [
        "BEGIN",
        "CREATE FUNCTION",
        "CREATE OPERATOR FAMILY",
        "CREATE OPERATOR CLASS",
        [("rp51_f", "403", "public", "postgres")],
        [("rp51_c", "403", "rp51_f", "integer", "False", "0")],
        [
            ("1", "integer", "integer", "<(integer,integer)", "s", "403", "0"),
            ("3", "integer", "integer", "=(integer,integer)", "s", "403", "0"),
        ],
        [("1", "integer", "integer", "rp51_cmp(integer,integer)")],
        "CREATE TABLE",
        "INSERT 0 2",
        "CREATE INDEX",
        "CREATE INDEX",
        [
            ("CREATE INDEX rp51_i ON public.rp51_t USING btree (a rp51_c)",),
            ("CREATE INDEX rp51_i2 ON public.rp51_t USING btree (b text_pattern_ops)",),
        ],
        [("rp51_i", "rp51_c"), ("rp51_i2", "text_pattern_ops")],
        [("1",)],
        "ROLLBACK",
    ],
    "ispell and thesaurus dictionaries": [
        "BEGIN",
        "CREATE TEXT SEARCH DICTIONARY",
        [("['sky']", "['booking', 'book']")],
        "CREATE TEXT SEARCH DICTIONARY",
        [("['1']",)],
        "ROLLBACK",
    ],
    "pg_describe_object": [
        "BEGIN",
        [
            (
                "table pg_class",
                "column relname of table pg_class",
                "type integer",
                "function now()",
                "schema public",
            )
        ],
        "CREATE OPERATOR FAMILY",
        "CREATE OPERATOR CLASS",
        [("operator class rp51_c for access method btree",)],
        [("operator family rp51_f for access method btree",)],
        [
            (
                "operator 1 (integer, integer) of operator family rp51_f for access "
                "method btree: <(integer,integer)",
            )
        ],
        [
            (
                "function 1 (integer, integer) of operator family rp51_f for access "
                "method btree: btint4cmp(integer,integer)",
            )
        ],
        "CREATE TRANSFORM",
        [("transform for integer language sql",)],
        "CREATE SUBSCRIPTION",
        [("subscription rp51_s",)],
        "CREATE SCHEMA",
        "CREATE TABLE",
        "CREATE VIEW",
        "CREATE SEQUENCE",
        "CREATE INDEX",
        [("default value for column a of table rp51_ds.t",)],
        [("constraint c on table rp51_ds.t",)],
        [("rule _RETURN on view rp51_ds.v",)],
        [("column a of table rp51_ds.t", "type rp51_ds.t")],
        [
            ("index rp51_ds.rp51_di",),
            ("sequence rp51_ds.s",),
            ("table rp51_ds.t",),
            ("view rp51_ds.v",),
        ],
        "SAVEPOINT",
        ("ERR", "XX000", "unrecognized object class: 0", None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "XX000", "unrecognized object class: 12000", None),
        "ROLLBACK",
        [("None",)],
        [("function btint4cmp(integer,integer)",)],
        [("cast from integer to bigint",)],
        [("database postgres",)],
        [("extension plpgsql",)],
        [("pg_type", "pg_type[]", "cstring[]")],
        "ROLLBACK",
    ],
    "pg_get_indexdef of an index the block created": [
        "BEGIN",
        "CREATE TABLE",
        "CREATE INDEX",
        "ALTER TABLE",
        [("CREATE INDEX rp51_gi ON public.rp51_g USING btree (a)", "a")],
        [("CHECK ((a > 0))",)],
        "ROLLBACK",
    ],
    "pg_identify_object and its siblings": [
        [("(table,pg_catalog,pg_class,pg_catalog.pg_class)", '(table,"{pg_catalog,pg_class}",{})')],
        [("1259", "1259", "0")],
    ],
    "pg_rewrite lists the system views' rules": [
        [("1",)],
        [("rule _RETURN on view pg_roles",)],
        [("142",)],
    ],
    "pg_subscription lists every database's subscriptions": [
        "DROP DATABASE",
        "CREATE DATABASE",
        "CREATE SUBSCRIPTION",
        [("rp51_other", "True")],
        "DROP SUBSCRIPTION",
        [("1",)],
    ],
    "rename, owner, schema and comment": [
        "BEGIN",
        "CREATE TEXT SEARCH DICTIONARY",
        "ALTER TEXT SEARCH DICTIONARY",
        "ALTER TEXT SEARCH DICTIONARY",
        "COMMENT",
        [("a dictionary",)],
        [("rp51_r2",)],
        "CREATE SCHEMA",
        "ALTER TEXT SEARCH DICTIONARY",
        [("rp51_s.rp51_r2", "rp51_s")],
        "DROP TEXT SEARCH DICTIONARY",
        [("0",)],
        "ROLLBACK",
    ],
    "rename, owner, schema, comment and drop of a class": [
        "BEGIN",
        "CREATE OPERATOR FAMILY",
        "CREATE OPERATOR CLASS",
        "CREATE OPERATOR CLASS",
        "ALTER OPERATOR FAMILY",
        "ALTER OPERATOR CLASS",
        "SAVEPOINT",
        (
            "ERR",
            "42710",
            'operator class "rp51_c3" for access method "btree" already exists in schema "public"',
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "42710",
            'operator family "rp51_f2" for access method "btree" already exists in schema "public"',
            None,
        ),
        "ROLLBACK",
        "CREATE ROLE",
        "ALTER OPERATOR FAMILY",
        "ALTER OPERATOR CLASS",
        "CREATE SCHEMA",
        "ALTER OPERATOR CLASS",
        "ALTER OPERATOR FAMILY",
        [("rp51_c2", "rp51_sc", "rp51_o"), ("rp51_c3", "public", "postgres")],
        [("rp51_f2", "rp51_sc", "rp51_o")],
        "COMMENT",
        "COMMENT",
        [("cc",)],
        [("ff",)],
        "CREATE TABLE",
        "CREATE OPERATOR CLASS",
        "SAVEPOINT",
        (
            "ERR",
            "XX000",
            'missing support function 1 for attribute 1 of index "rp51_t_a_idx"',
            None,
        ),
        "ROLLBACK",
        "CREATE INDEX",
        [("CREATE INDEX rp51_i ON public.rp51_t USING btree (a rp51_sc.rp51_c2)",)],
        "SAVEPOINT",
        (
            "ERR",
            "2BP01",
            "cannot drop operator class "
            "rp51_sc.rp51_c2 for access method btree "
            "because other objects depend on it",
            "Use DROP ... CASCADE to drop the dependent objects too.",
            "index rp51_i depends on operator class rp51_sc.rp51_c2 for access method btree",
        ),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "2BP01",
            "cannot drop operator family "
            "rp51_sc.rp51_f2 for access method btree "
            "because other objects depend on it",
            "Use DROP ... CASCADE to drop the dependent objects too.",
            "index rp51_i depends on operator class rp51_sc.rp51_c2 for access method btree",
        ),
        "ROLLBACK",
        ("DROP OPERATOR FAMILY", ["drop cascades to index rp51_i"]),
        [("0",)],
        [("0",)],
        (
            "DROP OPERATOR CLASS",
            ['operator class "nosuch" does not exist for access method "btree", skipping'],
        ),
        (
            "DROP OPERATOR FAMILY",
            ['operator family "nosuch" does not exist for access method "btree", skipping'],
        ),
        "SAVEPOINT",
        ("ERR", "42704", 'operator class "nosuch" does not exist for access method "btree"', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42704", 'operator family "nosuch" does not exist for access method "hash"', None),
        "ROLLBACK",
        "SAVEPOINT",
        (
            "ERR",
            "2BP01",
            "cannot drop operator class int4_ops for "
            "access method btree because it is "
            "required by the database system",
            None,
        ),
        "ROLLBACK",
        "ROLLBACK",
    ],
    "shobj_description reads a shared object's comment": [
        "COMMENT",
        "CREATE ROLE",
        "COMMENT",
        [("a database", "None")],
        [("a role", "None")],
        "DROP ROLE",
        "COMMENT",
    ],
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
