"""Batch 34 of the Rust PG server (2026-10-23): the regression tests of
what it fixed -- RP-072's `CREATE / ALTER / DROP TEXT SEARCH
CONFIGURATION`, and RP-166 (DROP COLLATION ... CASCADE) and RP-167 (a
column default's call printed with its literal types), found while
building it -- and the gap tests of what it filed open (RP-165, RP-168).

A scenario is a list of statements, each run in a named autocommit
connection: `a` unless the statement starts `@name `; `@open name k=v`
opens connection `name` with those options, and `@sleep` waits for a
reload to reach every PostgreSQL backend. An answer is a statement's
rows, its command tag, or `("ERR", sqlstate, message, hint)`; a statement
prefixed `!` adds its error's DETAIL, and one prefixed `~` is answered with
the notices it raised (message, then DETAIL). The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch34.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it
made. No scenario prints an oid, which PostgreSQL draws from a counter this
server starts elsewhere (RP-137).
"""

from __future__ import annotations

import sys
import time
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked


def _answer(conn: Any, sql: str, detail: bool = False) -> Any:
    try:
        cur = conn.execute(sql)
    except psycopg.Error as e:
        answer = ("ERR", e.sqlstate, e.diag.message_primary, e.diag.message_hint)
        return (*answer, e.diag.message_detail) if detail else answer
    if cur.description is None:
        return cur.statusmessage
    return [tuple(str(v) for v in row) for row in cur.fetchall()]


def run_scenario(dsn: str, statements: list[str]) -> list[Any]:
    conns: dict[str, Any] = {}
    notices: list[str] = []

    def conn(name: str, options: str = "") -> Any:
        if name not in conns:
            c = psycopg.connect(f"{dsn} {options}", autocommit=True)
            c.add_notice_handler(
                lambda d: notices.append(
                    d.message_primary
                    + (f"\n{d.message_detail}" if d.message_detail is not None else "")
                )
            )
            conns[name] = c
        return conns[name]

    out: list[Any] = []
    try:
        for s in statements:
            if s == "@sleep":
                time.sleep(0.5)
                continue
            if s.startswith("@open "):
                _, name, *options = s.split(" ", 2)
                conn(name, " ".join(options))
                continue
            name = "a"
            if s.startswith("@"):
                name, s = s[1:].split(" ", 1)
            if s.startswith("!"):
                out.append(_answer(conn(name), s[1:], detail=True))
            elif s.startswith("~"):
                notices.clear()
                answer = _answer(conn(name), s[1:])
                out.append((answer, list(notices)))
            else:
                out.append(_answer(conn(name), s))
    finally:
        for c in conns.values():
            if c.info.transaction_status != psycopg.pq.TransactionStatus.IDLE:
                c.execute("rollback")
            c.close()
    return out


_MAP = (
    "select maptokentype, mapseqno, mapdict::regdictionary::text from pg_ts_config_map "
    "where mapcfg = '{}'::regconfig order by 1, 2"
)

# (item, open?, statements). `open` scenarios are gap tests: they fail while
# their item is open.
SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------------- RP-072 CREATE TEXT SEARCH CONFIGURATION
    "CREATE TEXT SEARCH CONFIGURATION and its map": (
        "RP-072",
        False,
        [
            "create text search configuration rp34_c (copy = english)",
            "select cfgname, cfgnamespace::regnamespace::text, cfgowner::regrole::text, "
            "cfgparser from pg_ts_config where cfgname = 'rp34_c'",
            "select count(*) from pg_ts_config_map where mapcfg = 'rp34_c'::regconfig",
            "select to_tsvector('rp34_c', 'The quick brown foxes jumped')",
            "alter text search configuration rp34_c alter mapping for asciiword with simple",
            "select to_tsvector('rp34_c', 'The quick brown foxes jumped')",
            "alter text search configuration rp34_c alter mapping for asciiword "
            "with english_stem, simple",
            "alter text search configuration rp34_c drop mapping for asciiword",
            "select to_tsvector('rp34_c', 'The quick brown foxes jumped 123')",
            "alter text search configuration rp34_c drop mapping for asciiword",
            "~alter text search configuration rp34_c drop mapping if exists for asciiword",
            "alter text search configuration rp34_c add mapping for asciiword with french_stem",
            "alter text search configuration rp34_c add mapping for asciiword with simple",
            "alter text search configuration rp34_c alter mapping replace french_stem "
            "with german_stem",
            _MAP.format("rp34_c"),
            "alter text search configuration rp34_c add mapping for nosuchtype with simple",
            "alter text search configuration rp34_c alter mapping for asciiword with nosuchdict",
            "create text search configuration rp34_p (parser = default)",
            "select count(*) from pg_ts_config_map where mapcfg = 'rp34_p'::regconfig",
            "select to_tsvector('rp34_p', 'hello world')",
            "create text search configuration rp34_p (parser = default)",
            "create text search configuration rp34_q (parser = nosuch)",
            "create text search configuration rp34_q (copy = nosuch)",
            "create text search configuration rp34_q (parser = default, copy = english)",
            "alter text search configuration rp34_p alter mapping for blank, asciiword with simple",
            "select to_tsvector('rp34_p', 'a b')",
            "select * from ts_debug('rp34_p', 'Hello 42')",
            "alter text search configuration rp34_p add mapping for word with english_stem, simple",
            "select alias, dictionaries::text, dictionary::text, lexemes::text "
            "from ts_debug('rp34_p', 'Hello Grüße')",
            "select to_tsquery('rp34_c', 'Foxes & jumped'), "
            "plainto_tsquery('rp34_c', 'the foxes'), "
            "ts_headline('rp34_c', 'the quick foxes', to_tsquery('rp34_c', 'fox'))",
            "drop text search configuration rp34_c, rp34_p",
        ],
    ),
    "a configuration's name, owner, schema and comment": (
        "RP-072",
        False,
        [
            "create role rp34_o",
            "create text search configuration rp34_e (copy = simple)",
            "alter text search configuration rp34_e owner to rp34_o",
            "select cfgowner::regrole::text from pg_ts_config where cfgname = 'rp34_e'",
            "alter text search configuration rp34_e owner to rp34_nosuch",
            "create schema rp34_s",
            "alter text search configuration rp34_e set schema rp34_s",
            "select 'rp34_s.rp34_e'::regconfig::text, "
            "'rp34_s.rp34_e'::regconfig = (select oid from pg_ts_config where cfgname = 'rp34_e')",
            "select 'rp34_e'::regconfig",
            "set search_path = rp34_s, public",
            "select 'rp34_e'::regconfig::text",
            "set default_text_search_config = rp34_e",
            "show default_text_search_config",
            "select get_current_ts_config()::text",
            "select to_tsvector('foxes')",
            "reset all",
            "alter text search configuration rp34_s.rp34_e rename to rp34_f",
            "alter text search configuration rp34_s.rp34_f rename to rp34_f",
            "create text search configuration rp34_s.rp34_g (copy = rp34_s.rp34_f)",
            "alter text search configuration rp34_s.rp34_g rename to rp34_f",
            "alter text search configuration rp34_nosuch rename to x",
            "alter text search configuration rp34_nosuch_s.c rename to x",
            "comment on text search configuration rp34_s.rp34_f is 'c'",
            "select obj_description('rp34_s.rp34_f'::regconfig, 'pg_ts_config')",
            "select description from pg_description d join pg_ts_config c on c.oid = d.objoid "
            "and d.classoid = 'pg_ts_config'::regclass where c.cfgname = 'rp34_f'",
            "create text search configuration english (copy = simple)",
            "select 'english'::regconfig::oid < 16384, 'public.english'::regconfig::oid > 16384, "
            "'public.english'::regconfig::text",
            "drop text search configuration public.english",
            "~drop schema rp34_s cascade",
            "drop role rp34_o",
        ],
    ),
    "DROP TEXT SEARCH CONFIGURATION and what depends on it": (
        "RP-072",
        False,
        [
            "create text search configuration rp34_d (copy = english)",
            "create table rp34_t (a text, b tsvector default to_tsvector('rp34_d', 'x'), "
            "c text check (to_tsvector('rp34_d', c) is not null), "
            "v tsvector generated always as (to_tsvector('rp34_d', a)) stored)",
            "create index rp34_i on rp34_t using gin (to_tsvector('rp34_d', a))",
            "create view rp34_v as select to_tsvector('rp34_d', a) from rp34_t",
            "create view rp34_v2 as select 'rp34_d'::regconfig as r",
            "!drop text search configuration rp34_d",
            "~drop text search configuration rp34_d cascade",
            "select attname, atthasdef from pg_attribute where attrelid = 'rp34_t'::regclass "
            "and attnum > 0 and not attisdropped order by attnum",
            "select count(*) from pg_constraint where conrelid = 'rp34_t'::regclass",
            "select count(*) from pg_class where relname in ('rp34_i', 'rp34_v', 'rp34_v2')",
            "drop table rp34_t",
            "~drop text search configuration if exists rp34_nosuch",
            "drop text search configuration rp34_nosuch",
        ],
    ),
    "a configuration's dependencies": (
        "RP-072",
        False,
        [
            "create text search configuration rp34_dep (copy = english)",
            "select classid::regclass::text, refclassid::regclass::text, "
            "refobjid::regdictionary::text, deptype from pg_depend "
            "where objid = 'rp34_dep'::regconfig order by 2",
            "drop text search configuration rp34_dep",
        ],
    ),
    # ------------------------------------- RP-166 DROP COLLATION ... CASCADE
    "DROP COLLATION CASCADE drops each column that uses it": (
        "RP-166",
        False,
        [
            "create collation rp34_coll (provider = icu, locale = 'de-x-icu')",
            "create table rp34_ct (a text collate rp34_coll, b int, "
            "c text collate rp34_coll, d text collate rp34_coll)",
            "create table rp34_ct2 (x text collate rp34_coll, y int)",
            "~drop collation rp34_coll cascade",
            "select attname from pg_attribute where attrelid = 'rp34_ct'::regclass "
            "and attnum > 0 and not attisdropped order by attnum",
            "drop table rp34_ct, rp34_ct2",
        ],
    ),
    # ------------------------------- RP-167 a default's call, typed literals
    "a column default's call prints its literals' types": (
        "RP-167",
        False,
        [
            "create table rp34_x (a text default lower('X'), "
            "e tsvector default to_tsvector('english', 'x'), "
            "f tsquery default to_tsquery('simple', 'q'))",
            "select pg_get_expr(adbin, adrelid) from pg_attrdef "
            "where adrelid = 'rp34_x'::regclass order by adnum",
            "drop table rp34_x",
        ],
    ),
    # ------------------------- RP-165 a built-in configuration is changeable
    "a built-in configuration can be altered": (
        "RP-165",
        True,
        [
            "alter text search configuration danish rename to rp34_danish",
            "select 'rp34_danish'::regconfig::text",
            "alter text search configuration rp34_danish rename to danish",
            "select 'danish'::regconfig::oid",
        ],
    ),
    # ------------------------- RP-169 statements still refused with 0A000
    "statements PostgreSQL runs that are refused": (
        "RP-169",
        True,
        [
            "create operator family rp34_of using btree",
            "create operator class rp34_oc for type int using btree as "
            "operator 1 <, function 1 btint4cmp(int4, int4)",
            "create text search dictionary rp34_td (template = simple)",
            "create subscription rp34_sub connection 'dbname=nosuch' publication p "
            "with (connect = false)",
            "alter subscription rp34_sub set (slot_name = none)",
            "drop subscription rp34_sub",
            "create transform for int language sql (from sql with function "
            "prsd_lextype(internal), to sql with function int4recv(internal))",
            "drop transform for int language sql",
            "drop text search dictionary rp34_td",
            "drop operator class rp34_oc using btree",
            "drop operator family rp34_of using btree",
        ],
    ),
    # --------------------------------- RP-168 an "any" argument's literal
    'a default\'s literal for an "any" argument is bare': (
        "RP-168",
        False,
        [
            "create table rp34_y (b text default concat('a', 1))",
            "select pg_get_expr(adbin, adrelid) from pg_attrdef where adrelid = 'rp34_y'::regclass",
            "drop table rp34_y",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a configuration's dependencies": [
        "CREATE TEXT SEARCH CONFIGURATION",
        [
            ("pg_ts_config", "pg_namespace", "2200", "n"),
            ("pg_ts_config", "pg_ts_dict", "english_stem", "n"),
        ],
        "DROP TEXT SEARCH CONFIGURATION",
    ],
    "CREATE TEXT SEARCH CONFIGURATION and its map": [
        "CREATE TEXT SEARCH CONFIGURATION",
        [("rp34_c", "public", "postgres", "3722")],
        [("19",)],
        [("'brown':3 'fox':4 'jump':5 'quick':2",)],
        "ALTER TEXT SEARCH CONFIGURATION",
        [("'brown':3 'foxes':4 'jumped':5 'quick':2 'the':1",)],
        "ALTER TEXT SEARCH CONFIGURATION",
        "ALTER TEXT SEARCH CONFIGURATION",
        [("'123':1",)],
        ("ERR", "42704", 'mapping for token type "asciiword" does not exist', None),
        (
            "ALTER TEXT SEARCH CONFIGURATION",
            ['mapping for token type "asciiword" does not exist, skipping'],
        ),
        "ALTER TEXT SEARCH CONFIGURATION",
        (
            "ERR",
            "23505",
            'duplicate key value violates unique constraint "pg_ts_config_map_index"',
            None,
        ),
        "ALTER TEXT SEARCH CONFIGURATION",
        [
            ("1", "1", "german_stem"),
            ("2", "1", "english_stem"),
            ("3", "1", "simple"),
            ("4", "1", "simple"),
            ("5", "1", "simple"),
            ("6", "1", "simple"),
            ("7", "1", "simple"),
            ("8", "1", "simple"),
            ("9", "1", "simple"),
            ("10", "1", "english_stem"),
            ("11", "1", "english_stem"),
            ("15", "1", "simple"),
            ("16", "1", "english_stem"),
            ("17", "1", "english_stem"),
            ("18", "1", "simple"),
            ("19", "1", "simple"),
            ("20", "1", "simple"),
            ("21", "1", "simple"),
            ("22", "1", "simple"),
        ],
        ("ERR", "22023", 'token type "nosuchtype" does not exist', None),
        ("ERR", "42704", 'text search dictionary "nosuchdict" does not exist', None),
        "CREATE TEXT SEARCH CONFIGURATION",
        [("0",)],
        [("",)],
        (
            "ERR",
            "23505",
            'duplicate key value violates unique constraint "pg_ts_config_cfgname_index"',
            None,
        ),
        ("ERR", "42704", 'text search parser "nosuch" does not exist', None),
        ("ERR", "42704", 'text search configuration "nosuch" does not exist', None),
        ("ERR", "42601", "cannot specify both PARSER and COPY options", None),
        "ALTER TEXT SEARCH CONFIGURATION",
        [("' ':2 'a':1 'b':3",)],
        [
            ("asciiword", "Word, all ASCII", "Hello", "['simple']", "simple", "['hello']"),
            ("blank", "Space symbols", " ", "['simple']", "simple", "[' ']"),
            ("uint", "Unsigned integer", "42", "[]", "None", "None"),
        ],
        "ALTER TEXT SEARCH CONFIGURATION",
        [
            ("asciiword", "{simple}", "simple", "{hello}"),
            ("blank", "{simple}", "simple", '{" "}'),
            ("word", "{english_stem,simple}", "english_stem", "{grüße}"),
        ],
        [("'fox' & 'jumped'", "'the' & 'fox'", "the quick <b>foxes</b>")],
        "DROP TEXT SEARCH CONFIGURATION",
    ],
    "DROP COLLATION CASCADE drops each column that uses it": [
        "CREATE COLLATION",
        "CREATE TABLE",
        "CREATE TABLE",
        (
            "DROP COLLATION",
            [
                "drop cascades to 4 other objects\n"
                "drop cascades to column d of table "
                "rp34_ct\n"
                "drop cascades to column c of table "
                "rp34_ct\n"
                "drop cascades to column a of table "
                "rp34_ct\n"
                "drop cascades to column x of table "
                "rp34_ct2"
            ],
        ),
        [("b",)],
        "DROP TABLE",
    ],
    "DROP TEXT SEARCH CONFIGURATION and what depends on it": [
        "CREATE TEXT SEARCH CONFIGURATION",
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE VIEW",
        "CREATE VIEW",
        (
            "ERR",
            "2BP01",
            "cannot drop text search configuration rp34_d because other objects depend on it",
            "Use DROP ... CASCADE to drop the dependent objects too.",
            "default value for column b of table "
            "rp34_t depends on text search "
            "configuration rp34_d\n"
            "column v of table rp34_t depends on "
            "text search configuration rp34_d\n"
            "constraint rp34_t_c_check on table "
            "rp34_t depends on text search "
            "configuration rp34_d\n"
            "index rp34_i depends on text search "
            "configuration rp34_d\n"
            "view rp34_v depends on text search "
            "configuration rp34_d\n"
            "view rp34_v2 depends on text search "
            "configuration rp34_d",
        ),
        (
            "DROP TEXT SEARCH CONFIGURATION",
            [
                "drop cascades to 6 other objects\n"
                "drop cascades to default value for "
                "column b of table rp34_t\n"
                "drop cascades to column v of table "
                "rp34_t\n"
                "drop cascades to constraint "
                "rp34_t_c_check on table rp34_t\n"
                "drop cascades to index rp34_i\n"
                "drop cascades to view rp34_v\n"
                "drop cascades to view rp34_v2"
            ],
        ),
        [("a", "False"), ("b", "False"), ("c", "False")],
        [("0",)],
        [("0",)],
        "DROP TABLE",
        (
            "DROP TEXT SEARCH CONFIGURATION",
            ['text search configuration "rp34_nosuch" does not exist, skipping'],
        ),
        ("ERR", "42704", 'text search configuration "rp34_nosuch" does not exist', None),
    ],
    "a built-in configuration can be altered": [
        "ALTER TEXT SEARCH CONFIGURATION",
        [("rp34_danish",)],
        "ALTER TEXT SEARCH CONFIGURATION",
        [("13632",)],
    ],
    "a column default's call prints its literals' types": [
        "CREATE TABLE",
        [
            ("lower('X'::text)",),
            ("to_tsvector('english'::regconfig, 'x'::text)",),
            ("to_tsquery('simple'::regconfig, 'q'::text)",),
        ],
        "DROP TABLE",
    ],
    "a configuration's name, owner, schema and comment": [
        "CREATE ROLE",
        "CREATE TEXT SEARCH CONFIGURATION",
        "ALTER TEXT SEARCH CONFIGURATION",
        [("rp34_o",)],
        ("ERR", "42704", 'role "rp34_nosuch" does not exist', None),
        "CREATE SCHEMA",
        "ALTER TEXT SEARCH CONFIGURATION",
        [("rp34_s.rp34_e", "True")],
        ("ERR", "42704", 'text search configuration "rp34_e" does not exist', None),
        "SET",
        [("rp34_e",)],
        "SET",
        [("rp34_s.rp34_e",)],
        [("rp34_e",)],
        [("'foxes':1",)],
        "RESET",
        "ALTER TEXT SEARCH CONFIGURATION",
        (
            "ERR",
            "42710",
            'text search configuration "rp34_f" already exists in schema "rp34_s"',
            None,
        ),
        "CREATE TEXT SEARCH CONFIGURATION",
        (
            "ERR",
            "42710",
            'text search configuration "rp34_f" already exists in schema "rp34_s"',
            None,
        ),
        ("ERR", "42704", 'text search configuration "rp34_nosuch" does not exist', None),
        ("ERR", "3F000", 'schema "rp34_nosuch_s" does not exist', None),
        "COMMENT",
        [("c",)],
        [("c",)],
        "CREATE TEXT SEARCH CONFIGURATION",
        [("True", "True", "public.english")],
        "DROP TEXT SEARCH CONFIGURATION",
        (
            "DROP SCHEMA",
            [
                "drop cascades to 2 other objects\n"
                "drop cascades to text search "
                "configuration rp34_s.rp34_f\n"
                "drop cascades to text search "
                "configuration rp34_s.rp34_g"
            ],
        ),
        "DROP ROLE",
    ],
    'a default\'s literal for an "any" argument is bare': [
        "CREATE TABLE",
        [("concat('a', 1)",)],
        "DROP TABLE",
    ],
    "statements PostgreSQL runs that are refused": [
        "CREATE OPERATOR FAMILY",
        "CREATE OPERATOR CLASS",
        "CREATE TEXT SEARCH DICTIONARY",
        "CREATE SUBSCRIPTION",
        "ALTER SUBSCRIPTION",
        "DROP SUBSCRIPTION",
        "CREATE TRANSFORM",
        "DROP TRANSFORM",
        "DROP TEXT SEARCH DICTIONARY",
        "DROP OPERATOR CLASS",
        "DROP OPERATOR FAMILY",
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
