"""Gap tests of the divergences found while working RP-782 (`CREATE
DOMAIN`'s COLLATE clause is kept): a cast to a collated domain does not
take its collation; `information_schema`'s `character_data` columns are not
collated "C"; `pg_get_viewdef` of a system view is NULL.

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster (locale C.UTF-8, built with ICU) with `_SETUP`,
which each statement runs after.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_SETUP = [
    "drop domain if exists xu782f cascade",
    "drop domain if exists xc782f cascade",
    'create domain xu782f as text collate "und-x-icu"',
    'create domain xc782f as text collate "C"',
]
_TEARDOWN = 2


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


# `exprCollation` of a cast to a domain is the domain's `typcollation`, an
# IMPLICIT collation: two different ones meeting in a comparison are 42P22.
# A cast written over a `COLLATE` keeps the clause, EXPLICIT, as
# `coerce_type` pushes the coercion beneath it. Here a cast to a collated
# domain carried no collation, so it compared bytewise; fixed by RP-796.
@pytest.mark.backlog_fixed("RP-796")
def test_a_cast_to_a_collated_domain_takes_its_collation(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                "select 'a'::xu782f < 'B'::xu782f, 'a'::xc782f < 'B'::xc782f",
                "select v from (values ('a'::xu782f), ('B'::xu782f)) s(v) order by v",
                "select max(v) from (values ('a'::xu782f), ('B'::xu782f)) s(v)",
                "select 'a'::xu782f < 'B'::text",
                "select 'a'::xu782f < 'B'::xc782f",
                "select ('a' collate \"C\")::xu782f < 'B',"
                " (('a' collate \"und-x-icu\")::xc782f) < 'B'",
                "select ('a' collate \"und-x-icu\")::text < 'B'",
                "select v from (select 'a'::xu782f union all select 'B') s(v) order by v",
                "select a from (values ('a'), ('B'), ('c'), ('D')) t(a)"
                " where a::xu782f < 'C' order by a::xu782f",
            ]
        ]
    assert got == [
        [(True, False)],
        [("a",), ("B",)],
        [("B",)],
        [(True,)],
        "42P22 could not determine which collation to use for string comparison",
        [(False, True)],
        [(True,)],
        [("a",), ("B",)],
        [("a",), ("B",), ("c",)],
    ]


# `information_schema.sql` declares `character_data` and `yes_or_no`
# `COLLATE "C"`, and its views' text columns are of those domains: they
# were "default" here until RP-797.
@pytest.mark.backlog_fixed("RP-797")
def test_information_schema_character_data_columns_are_collated_c(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                "select collation for (data_type), collation for (is_nullable),"
                " collation for (table_name) from information_schema.columns limit 1",
                "select collation for (is_insertable_into) from information_schema.tables limit 1",
            ]
        ]
    assert got == [[('"C"', '"C"', '"C"')], [('"C"',)]]


# `pg_get_viewdef` deparses a system view's `_RETURN` rule as it does a
# user's: a `pg_catalog` view's was NULL here until RP-798.
@pytest.mark.backlog_fixed("RP-798")
def test_pg_get_viewdef_of_a_system_view(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [_answer(c, "select pg_get_viewdef('pg_tables'::regclass)")]
    assert got == [
        [
            (
                " SELECT n.nspname AS schemaname,\n"
                "    c.relname AS tablename,\n"
                "    pg_get_userbyid(c.relowner) AS tableowner,\n"
                "    t.spcname AS tablespace,\n"
                "    c.relhasindex AS hasindexes,\n"
                "    c.relhasrules AS hasrules,\n"
                "    c.relhastriggers AS hastriggers,\n"
                "    c.relrowsecurity AS rowsecurity\n"
                "   FROM ((pg_class c\n"
                "     LEFT JOIN pg_namespace n ON ((n.oid = c.relnamespace)))\n"
                "     LEFT JOIN pg_tablespace t ON ((t.oid = c.reltablespace)))\n"
                "  WHERE (c.relkind = ANY (ARRAY['r'::\"char\", 'p'::\"char\"]));",
            )
        ],
    ]


# `pg_stat_database` reads `SELECT 0 AS oid ... UNION ALL SELECT oid ...`:
# `select_common_type` takes oid for an integer and an oid, as an integer
# coerces implicitly to oid and oid only by assignment to an integer. The
# printer refused the meet, so the view was NULL (RP-798). The md5s are of
# PostgreSQL 15.19's text in each print mode.
@pytest.mark.backlog_fixed("RP-798")
def test_pg_get_viewdef_of_pg_stat_database(dsn: str) -> None:
    with _connect(dsn) as c:
        got = _answer(
            c,
            "select md5(pg_get_viewdef('pg_stat_database'::regclass, false)),"
            " md5(pg_get_viewdef('pg_stat_database'::regclass, true))",
        )
    assert got == [("e53a1cc7dce4f6f7beda4a5f96ad5ee5", "ae875e0fc37768b28af9bd4745f95520")]


# An integer and an oid meet at oid in a set operation, the constant left
# as it is written.
@pytest.mark.backlog_fixed("RP-798")
def test_pg_get_viewdef_of_a_union_of_an_integer_and_an_oid(dsn: str) -> None:
    with _connect(dsn) as c:
        c.execute(
            "create or replace view v798u as select 0 as oid union all select oid from pg_database"
        )
        got = _answer(c, "select pg_get_viewdef('v798u'::regclass)")
        c.execute("drop view v798u")
    assert got == [(" SELECT 0 AS oid\nUNION ALL\n SELECT pg_database.oid\n   FROM pg_database;",)]


# `information_schema` views print like PostgreSQL 15.19's (md5 of each
# print mode): a cast to one of its domains is a `CoerceToDomain`, printed
# qualified over a literal folded to the base type, and a relation of the
# view's own schema is qualified. These were NULL (RP-798).
INFORMATION_SCHEMA_VIEWDEF_MD5 = {
    "applicable_roles": ("7b2228d7129e68f9ef8ac3bcf3e5a997", "7a916288707a88eeab8e34cec869ac26"),
    "foreign_data_wrappers": (
        "dc66fbbcc5b07bc3fd21a7d29d2e9ac4",
        "e2a26391087a52ac3eea9e6366c9ad17",
    ),
    "information_schema_catalog_name": (
        "98db718db18460a9db827a806a8f2b21",
        "d7a78c3875c8ac1227eae2d030adc766",
    ),
    "schemata": ("582f055df269ae7d14b87133470c182e", "b755dd39bfb4cc0f52f667684a4bd191"),
}


@pytest.mark.backlog_fixed("RP-798")
@pytest.mark.parametrize("view", list(INFORMATION_SCHEMA_VIEWDEF_MD5))
def test_pg_get_viewdef_of_an_information_schema_view(view: str, dsn: str) -> None:
    with _connect(dsn) as c:
        got = _answer(
            c,
            f"select md5(pg_get_viewdef('information_schema.{view}'::regclass, false)),"
            f" md5(pg_get_viewdef('information_schema.{view}'::regclass, true))",
        )
    assert got == [INFORMATION_SCHEMA_VIEWDEF_MD5[view]]


# Measured on PostgreSQL 15.19.
@pytest.mark.backlog_fixed("RP-798")
def test_pg_get_viewdef_of_casts_to_information_schema_domains(dsn: str) -> None:
    with _connect(dsn) as c:
        c.execute(
            "create or replace view v798d as select 'YES'::information_schema.yes_or_no a,"
            " 5::information_schema.cardinal_number d,"
            " null::information_schema.sql_identifier e,"
            " relname::information_schema.character_data f from pg_class"
        )
        got = _answer(c, "select pg_get_viewdef('v798d'::regclass), pg_get_viewdef('v798d', true)")
        c.execute("drop view v798d")
    assert got == [
        (
            " SELECT ('YES'::character varying)::information_schema.yes_or_no AS a,\n"
            "    (5)::information_schema.cardinal_number AS d,\n"
            "    (NULL::name)::information_schema.sql_identifier AS e,\n"
            "    (pg_class.relname)::information_schema.character_data AS f\n"
            "   FROM pg_class;",
            " SELECT 'YES'::character varying::information_schema.yes_or_no AS a,\n"
            "    5::information_schema.cardinal_number AS d,\n"
            "    NULL::name::information_schema.sql_identifier AS e,\n"
            "    pg_class.relname::information_schema.character_data AS f\n"
            "   FROM pg_class;",
        )
    ]


# A set operation's later branches print their own column names inside a
# subquery (no result descriptor), the view's names at the top. Measured on
# PostgreSQL 15.19; the printer named every branch by the leftmost one.
@pytest.mark.backlog_fixed("RP-798")
def test_pg_get_viewdef_names_set_operation_branches(dsn: str) -> None:
    with _connect(dsn) as c:
        c.execute(
            "create or replace view v798s as select m.a from (select relname as a from pg_class"
            " union select relname as q from pg_class) m"
        )
        c.execute("create or replace view v798t as select 1 as a union select 2 as b")
        got = _answer(c, "select pg_get_viewdef('v798s'::regclass), pg_get_viewdef('v798t')")
        c.execute("drop view v798s, v798t")
    assert got == [
        (
            " SELECT m.a\n"
            "   FROM ( SELECT pg_class.relname AS a\n"
            "           FROM pg_class\n"
            "        UNION\n"
            "         SELECT pg_class.relname AS q\n"
            "           FROM pg_class) m;",
            " SELECT 1 AS a\nUNION\n SELECT 2 AS a;",
        )
    ]


# The system views the printer does not reproduce are still NULL: an
# `information_schema` view (its domain casts), and `pg_catalog` views
# such as `pg_stats_ext`.
@pytest.mark.backlog("RP-798")
def test_pg_get_viewdef_of_the_remaining_system_views(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                "select pg_get_viewdef('information_schema.domains'::regclass) is not null",
                "select pg_get_viewdef('pg_stats_ext'::regclass) is not null",
            ]
        ]
    assert got == [[(True,)], [(True,)]]


# A built-in taking IN arguments and returning a record of OUT parameters
# prints with its column list; one whose arguments name a sibling FROM
# item prints LATERAL (`transformRangeFunction`); a cast to the type a
# column already has prints no AS. These views were NULL (RP-798),
# measured on PostgreSQL 15.19.
@pytest.mark.backlog_fixed("RP-798")
def test_pg_get_viewdef_of_views_over_functions_with_in_arguments(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, f"select pg_get_viewdef('{v}'::regclass)")
            for v in ("pg_stat_subscription_stats", "pg_stat_gssapi")
        ]
        analyze = _answer(c, "select pg_get_viewdef('pg_stat_progress_analyze'::regclass)")
    assert got == [
        [
            (
                " SELECT ss.subid,\n    s.subname,\n    ss.apply_error_count,\n"
                "    ss.sync_error_count,\n    ss.stats_reset\n   FROM pg_subscription s,\n"
                "    LATERAL pg_stat_get_subscription_stats(s.oid)"
                " ss(subid, apply_error_count, sync_error_count, stats_reset);",
            )
        ],
        [
            (
                " SELECT s.pid,\n    s.gss_auth AS gss_authenticated,\n"
                "    s.gss_princ AS principal,\n    s.gss_enc AS encrypted\n"
                "   FROM pg_stat_get_activity(NULL::integer) s(datid, pid, usesysid,"
                " application_name, state, query, wait_event_type, wait_event, xact_start,"
                " query_start, backend_start, state_change, client_addr, client_hostname,"
                " client_port, backend_xid, backend_xmin, backend_type, ssl, sslversion,"
                " sslcipher, sslbits, ssl_client_dn, ssl_client_serial, ssl_issuer_dn,"
                " gss_auth, gss_princ, gss_enc, leader_pid, query_id)\n"
                "  WHERE (s.client_port IS NOT NULL);",
            )
        ],
    ]
    assert analyze[0][0].startswith(
        " SELECT s.pid,\n    s.datid,\n    d.datname,\n    s.relid,\n        CASE s.param1\n"
    )


# A function in FROM returning a record of OUT parameters prints with its
# column list, as ruleutils expands it. It was NULL: the printer refused
# `SELECT * FROM f() a` over a record; fixed by naming the RTE's columns
# from the built-in's OUT parameters (RP-798).
@pytest.mark.backlog_fixed("RP-798")
def test_pg_get_viewdef_of_a_view_over_a_function_in_from(dsn: str) -> None:
    with _connect(dsn) as c:
        got = _answer(c, "select pg_get_viewdef('pg_settings'::regclass)")
    assert got == [
        (
            " SELECT a.name,\n"
            "    a.setting,\n"
            "    a.unit,\n"
            "    a.category,\n"
            "    a.short_desc,\n"
            "    a.extra_desc,\n"
            "    a.context,\n"
            "    a.vartype,\n"
            "    a.source,\n"
            "    a.min_val,\n"
            "    a.max_val,\n"
            "    a.enumvals,\n"
            "    a.boot_val,\n"
            "    a.reset_val,\n"
            "    a.sourcefile,\n"
            "    a.sourceline,\n"
            "    a.pending_restart\n"
            "   FROM pg_show_all_settings() a(name, setting, unit, category, short_desc,"
            " extra_desc, context, vartype, source, min_val, max_val, enumvals, boot_val,"
            " reset_val, sourcefile, sourceline, pending_restart);",
        )
    ]


# `ATExecAddColumn` types an added column of a domain as it does one CREATE
# TABLE declares: an aggregate over the base type takes it. Here the column
# was typed by the domain's name, so `string_agg` was 42883; fixed by
# RP-799.
@pytest.mark.backlog_fixed("RP-799")
def test_an_added_domain_column_reaches_its_base_types_aggregates(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                "drop table if exists ta782f",
                "create table ta782f (k int)",
                "insert into ta782f values (1), (2)",
                "alter table ta782f add column a xc782f",
                "update ta782f set a = 'x' || k",
                "select string_agg(a, ',' order by a) from ta782f",
                "drop table ta782f",
            ]
        ]
    assert got == [
        "DROP TABLE",
        "CREATE TABLE",
        "INSERT 0 2",
        "ALTER TABLE",
        "UPDATE 2",
        [("x1,x2",)],
        "DROP TABLE",
    ]


# A CASE whose ELSE NULL takes the type of a `pg_` function's result: the
# printer could not type `pg_sequence_last_value(oid)` (overload selection
# leaves the `pg_` functions alone), so `pg_sequences` was NULL; fixed by
# reading the result from PG 15's `pg_proc` when every overload of that
# arity agrees (RP-798).
@pytest.mark.backlog_fixed("RP-798")
def test_pg_get_viewdef_of_pg_sequences(dsn: str) -> None:
    with _connect(dsn) as c:
        got = _answer(c, "select pg_get_viewdef('pg_sequences'::regclass)")
    assert got == [
        (
            " SELECT n.nspname AS schemaname,\n"
            "    c.relname AS sequencename,\n"
            "    pg_get_userbyid(c.relowner) AS sequenceowner,\n"
            "    (s.seqtypid)::regtype AS data_type,\n"
            "    s.seqstart AS start_value,\n"
            "    s.seqmin AS min_value,\n"
            "    s.seqmax AS max_value,\n"
            "    s.seqincrement AS increment_by,\n"
            "    s.seqcycle AS cycle,\n"
            "    s.seqcache AS cache_size,\n"
            "        CASE\n"
            "            WHEN has_sequence_privilege(c.oid, 'SELECT,USAGE'::text)"
            " THEN pg_sequence_last_value((c.oid)::regclass)\n"
            "            ELSE NULL::bigint\n"
            "        END AS last_value\n"
            "   FROM ((pg_sequence s\n"
            "     JOIN pg_class c ON ((c.oid = s.seqrelid)))\n"
            "     LEFT JOIN pg_namespace n ON ((n.oid = c.relnamespace)))\n"
            "  WHERE ((NOT pg_is_other_temp_schema(n.oid)) AND (c.relkind = 'S'::\"char\"));",
        )
    ]


# `name` keeps its type under NOT IN and `!~`: it has `namene` and
# `nameregexne`. The printer cast it to text, so the `pg_stat*_user_*` /
# `_sys_*` views would have printed wrong once their base views printed
# (RP-798).
@pytest.mark.backlog_fixed("RP-798")
def test_pg_get_viewdef_keeps_name_under_in_and_regex(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, "select pg_get_viewdef('pg_statio_user_sequences'::regclass)"),
            _answer(c, "drop view if exists vn782f"),
            _answer(
                c,
                "create view vn782f as select relname from pg_class where relname not in "
                "('a','b') and relname in ('c','d') and relname ~~ 'x%' and relname ~* 'y' "
                "and relname !~~* 'z'",
            ),
            _answer(c, "select pg_get_viewdef('vn782f'::regclass)"),
            _answer(c, "drop view vn782f"),
        ]
    assert got[0] == [
        (
            " SELECT pg_statio_all_sequences.relid,\n"
            "    pg_statio_all_sequences.schemaname,\n"
            "    pg_statio_all_sequences.relname,\n"
            "    pg_statio_all_sequences.blks_read,\n"
            "    pg_statio_all_sequences.blks_hit\n"
            "   FROM pg_statio_all_sequences\n"
            "  WHERE ((pg_statio_all_sequences.schemaname <> ALL (ARRAY['pg_catalog'::name,"
            " 'information_schema'::name])) AND (pg_statio_all_sequences.schemaname !~"
            " '^pg_toast'::text));",
        )
    ]
    assert got[3] == [
        (
            " SELECT pg_class.relname\n"
            "   FROM pg_class\n"
            "  WHERE ((pg_class.relname <> ALL (ARRAY['a'::name, 'b'::name]))"
            " AND (pg_class.relname = ANY (ARRAY['c'::name, 'd'::name]))"
            " AND (pg_class.relname ~~ 'x%'::text) AND (pg_class.relname ~* 'y'::text)"
            " AND (pg_class.relname !~~* 'z'::text));",
        )
    ]


# `select_common_type` over `name` and `text` keeps `name`: each coerces
# implicitly to the other, so the first type stands.
@pytest.mark.backlog("RP-1045")
def test_pg_get_viewdef_of_a_name_in_list_with_a_text_element(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, "drop view if exists vt782f"),
            _answer(
                c,
                "create view vt782f as select relname from pg_class"
                " where relname in ('g'::text, 'h')",
            ),
            _answer(c, "select pg_get_viewdef('vt782f'::regclass)"),
            _answer(c, "drop view vt782f"),
        ]
    assert got[2] == [
        (
            " SELECT pg_class.relname\n"
            "   FROM pg_class\n"
            "  WHERE (pg_class.relname = ANY (ARRAY[('g'::text)::name, 'h'::name]));",
        )
    ]


def _viewdef_of(dsn: str, name: str, query: str) -> object:
    with _connect(dsn) as c:
        c.execute(f"create or replace view {name} as {query}")
        got = _answer(c, f"select pg_get_viewdef('{name}'::regclass)")
        c.execute(f"drop view {name}")
    return got


# `FigureColnameInternal` names a CASE by its ELSE result (RP-1046).
@pytest.mark.backlog("RP-1046")
def test_pg_get_viewdef_names_a_case_column_by_its_else_column(dsn: str) -> None:
    got = _viewdef_of(
        dsn, "v1046", "select case when c.oid > 5 then 0 else c.relnatts end from pg_class c"
    )
    assert got == [
        (
            " SELECT\n"
            "        CASE\n"
            "            WHEN (c.oid > (5)::oid) THEN 0\n"
            "            ELSE (c.relnatts)::integer\n"
            "        END AS relnatts\n"
            "   FROM pg_class c;",
        )
    ]


# `transformAExprIn` ORs an `=` for each element holding a Var (RP-1047).
@pytest.mark.backlog("RP-1047")
def test_pg_get_viewdef_of_an_in_list_holding_a_column(dsn: str) -> None:
    got = _viewdef_of(dsn, "v1047", "select c.relnatts in (1, c.relam::int) from pg_class c")
    assert got == [
        (
            ' SELECT ((c.relnatts = 1) OR (c.relnatts = (c.relam)::integer)) AS "?column?"\n'
            "   FROM pg_class c;",
        )
    ]
