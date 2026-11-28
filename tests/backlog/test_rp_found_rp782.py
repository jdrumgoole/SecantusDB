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
    # An operator over a domain resolves on its base type, the operand
    # relabelled (`(x.is_grantable)::text = 'YES'::text`); an `oid` meets a
    # `regclass` constant printed by `regclassout`
    # (`('pg_proc'::regclass)::oid`); `(aclexplode(...)).*` expands to one
    # field selection per OUT column; a set operation's untyped literal
    # takes the column's type (`'PUBLIC'::name`); the integer bitwise
    # operators; `getdatabaseencoding()`; a PARTITION BY item parenthesised
    # as an ORDER BY one is. These were NULL (RP-798), measured on
    # PostgreSQL 15.20.
    "administrable_role_authorizations": (
        "3b2dd83acf16e21582d04ddbf0e8805c",
        "0d71cf2b28aeb95f6aa35f1bf6cbc770",
    ),
    "character_sets": ("f7dfd03a976d9dd99e4cbb221b6c518b", "d5e7b95d5931db65e5f9afe2e5670530"),
    "check_constraint_routine_usage": (
        "bbd2c5f1c9c62890e8b1047b45656222",
        "a14206f1d37d9d8de6acd5fd49de3d4a",
    ),
    "collation_character_set_applicability": (
        "bc1156543daef9fa9e43707bd07d099a",
        "b245894cfa52c42d6841d2b924f0c2e3",
    ),
    "column_column_usage": ("aa714ec98f05775a96a6b98888e3f0ed", "d3509537311773f97d6f563e7f5abacb"),
    "column_options": ("ef2af50013889fe314343d93a6054e64", "ec02b13db0fde6c40fbc25dbe80e4dd2"),
    "column_privileges": ("7236f9885917742caff34fd0f8278cb0", "06c6b686553fb5a85b2e76e236ae9adc"),
    "constraint_column_usage": (
        "96bb72948cd97bb9b9e254c72a156641",
        "6e43c9b98ffc19600fa9c31871d9d86c",
    ),
    "foreign_data_wrapper_options": (
        "089cd585c682bbeb670e56cbb22b65e8",
        "42ad09f5a05724570f3e9d40fc19ed55",
    ),
    "foreign_server_options": (
        "916a920cd0196bec0d212efb88634b74",
        "695ee10f5379d47ead84fc503838361b",
    ),
    "foreign_table_options": (
        "3e9d62b401e799912e38fdc4d6d5370a",
        "0dc9d90ffe2721da4b4dc432ea09d302",
    ),
    "referential_constraints": (
        "80a6a77b048c51ae3a15e8bb08a0331b",
        "85d236ab4f1d1d3ad96e40d3c268d7cb",
    ),
    "role_column_grants": ("c29fbab88d920e154f5185b653530ffe", "b77078e6664c36c0bfd304af8f8da89e"),
    "role_routine_grants": ("a82aad25383019502d9a70943988a00c", "597487f5b49171cbcab3492ed181b2d9"),
    "role_table_grants": ("ad3fc3286b3c629d0222f3e79d7ba25b", "ffeb916f9860affdbdcba4a7b0438080"),
    "role_udt_grants": ("0fa29059f523a56ad9561078942f80d3", "d7414257fa33f7ccfe20be36885dea15"),
    "role_usage_grants": ("138846a9a5825527b04a19d90027d4e1", "86d12fc2480c4e50f015ba87ae1d5e23"),
    "routine_column_usage": (
        "629eb42244294bca8c0725f8f36d0312",
        "00341f79ac460ad4c219a14facd2743e",
    ),
    "routine_privileges": ("5ee368aafdf89176954b87c3cea814f1", "8a14b15ea7421c496162bbb2dd8bebd8"),
    "routine_routine_usage": (
        "8e6dbaa5f060b3511216fa290bc63b3d",
        "a962a928d9e37f527055ed353b64f816",
    ),
    "routine_sequence_usage": (
        "51eb647078e221b1a338ded5c22517cd",
        "223a7d3d768eb9f12f0d7a12d993e0c1",
    ),
    "routine_table_usage": ("099e12531adb229ed6d697e1df01a68b", "6ced76f131f895b071e2a22642585f43"),
    "table_privileges": ("e179519f86779ed1d45f492d3c0c359e", "0b4ece261793c61344d6eeb823588e50"),
    "tables": ("844c95f09db89603b78a5cfdc895d49f", "acdc6eb1ff8215f525200f6742477ae0"),
    "transforms": ("0c6d73729f17d5a235d4091c2e73c40b", "0348811c002152461714eaf0586a3a97"),
    "triggers": ("5849ec685959aebf845b5a85a124c5aa", "ec7e95768097160e64cd4edb03d7f1c7"),
    "udt_privileges": ("bac682d5952abf036a787e84b8776ffb", "92ecf7b3132f3602424b245617cf449f"),
    "usage_privileges": ("adfdfbe67b522a00faa7d4a6af84ae8a", "cf35ea3a9ec6b7f9f29d0da25d36c417"),
    "view_column_usage": ("94497736c894537349d5f88b75b966a8", "5ce844882d47c30f8d24b996bcf03e34"),
    "view_routine_usage": ("3185c6efe32b5d8f47ae9f74f9964cb1", "31186fee452ad102d8fe3acb2569c481"),
    "view_table_usage": ("dda4436e9c406854bd32ee79f557bf1f", "493b3f348205698568dd0fc484c380e0"),
    "views": ("58cef64baa6a7ba40589cd9f220ca666", "5a8a15b275719003b6af8fd72c029228"),
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


# A user view over the shapes the system views needed prints as
# PostgreSQL's does (measured on a private PostgreSQL 15.20 cluster): a
# set operation's untyped literal takes the column's type, an `oid` meets
# a `regclass` constant at `oid`, mixed-width bitwise operators widen, an
# operator or IN over an `information_schema` domain column relabels it
# to its base type, and `(f(...)).*` expands. These printed as written or
# were refused (RP-798).
@pytest.mark.backlog_fixed("RP-798")
def test_pg_get_viewdef_of_user_views_over_the_system_view_shapes(dsn: str) -> None:
    with _connect(dsn) as c:
        c.execute("drop table if exists t798 cascade")
        c.execute(
            "create table t798 (k int, n name, s int2, flag information_schema.yes_or_no,"
            " cn information_schema.cardinal_number)"
        )
        views = {
            "a": "select n from t798 union all select 'x'",
            "b": "select c.relname from pg_class c where c.oid = 'pg_catalog.pg_class'::regclass",
            "c": "select k from t798 where (s & 3) <> 0 and (k | 1) = 1",
            "d": "select k from t798 where flag = 'YES' and cn > 2",
            "e": "select k from t798 where flag in (select flag from t798)",
            "f": "select (aclexplode(relacl)).* from pg_class",
            "i": "select n from t798 union select 'x' union all select 'y'",
        }
        got = {}
        for v, q in views.items():
            c.execute(f"create view v798{v} as {q}")
            got[v] = _answer(
                c, f"select pg_get_viewdef('v798{v}'::regclass), pg_get_viewdef('v798{v}', true)"
            )
        c.execute("drop table t798 cascade")
    acl = (
        " SELECT (aclexplode(pg_class.relacl)).grantor AS grantor,\n"
        "    (aclexplode(pg_class.relacl)).grantee AS grantee,\n"
        "    (aclexplode(pg_class.relacl)).privilege_type AS privilege_type,\n"
        "    (aclexplode(pg_class.relacl)).is_grantable AS is_grantable\n"
        "   FROM pg_class;"
    )
    union3 = (
        "(\n         SELECT t798.n\n           FROM t798\n        UNION\n"
        "         SELECT 'x'::name AS n\n) UNION ALL\n SELECT 'y'::name AS n;"
    )
    assert got == {
        "a": [(" SELECT t798.n\n   FROM t798\nUNION ALL\n SELECT 'x'::name AS n;",) * 2],
        "b": [
            (
                " SELECT c.relname\n   FROM pg_class c\n"
                "  WHERE (c.oid = ('pg_class'::regclass)::oid);",
                " SELECT c.relname\n   FROM pg_class c\n  WHERE c.oid = 'pg_class'::regclass::oid;",
            )
        ],
        "c": [
            (
                " SELECT t798.k\n   FROM t798\n"
                "  WHERE ((((t798.s)::integer & 3) <> 0) AND ((t798.k | 1) = 1));",
                " SELECT t798.k\n   FROM t798\n"
                "  WHERE (t798.s::integer & 3) <> 0 AND (t798.k | 1) = 1;",
            )
        ],
        "d": [
            (
                " SELECT t798.k\n   FROM t798\n"
                "  WHERE (((t798.flag)::text = 'YES'::text) AND ((t798.cn)::integer > 2));",
                " SELECT t798.k\n   FROM t798\n"
                "  WHERE t798.flag::text = 'YES'::text AND t798.cn::integer > 2;",
            )
        ],
        "e": [
            (
                " SELECT t798.k\n   FROM t798\n"
                "  WHERE ((t798.flag)::text IN ( SELECT t798_1.flag\n"
                "           FROM t798 t798_1));",
                " SELECT t798.k\n   FROM t798\n"
                "  WHERE (t798.flag::text IN ( SELECT t798_1.flag\n           FROM t798 t798_1));",
            )
        ],
        "f": [(acl, acl)],
        "i": [(union3, union3)],
    }


# The system views the printer does not reproduce are still NULL: an
# `information_schema` view over `_pg_expandarray`, and `pg_catalog`
# views such as `pg_stats_ext`.
@pytest.mark.backlog("RP-798")
def test_pg_get_viewdef_of_the_remaining_system_views(dsn: str) -> None:
    with _connect(dsn) as c:
        got = [
            _answer(c, sql)
            for sql in [
                "select pg_get_viewdef('information_schema.key_column_usage'::regclass)"
                " is not null",
                "select pg_get_viewdef('pg_stats_ext'::regclass) is not null",
            ]
        ]
    assert got == [[(True,)], [(True,)]]


# `information_schema` views calling the schema's SQL helpers print as
# PostgreSQL 15.20 prints them, in both modes (md5 of each text, measured
# on the shared 15 reference): a helper call qualified by its schema
# (`information_schema._pg_char_max_length(t.typbasetype, t.typtypmod)`),
# a whole-row argument as `a.*` (`_pg_truetypid(a.*, t.*)`), and a domain
# argument of a built-in relabelled to the base type the call resolves on
# (`pg_has_role((um.srvowner)::name, 'USAGE'::text)`). They were NULL: the
# printer refused a call qualified by any schema but `pg_catalog`, and
# resolved a call on the domain rather than its base (RP-798).
@pytest.mark.backlog_fixed("RP-798")
def test_pg_get_viewdef_of_views_calling_information_schema_helpers(dsn: str) -> None:
    views = {
        "domains": ("eedb92dc096ae8ea0e86ebf43df5c1f7", "c18879dd48dd60932c3e402697952534"),
        "sequences": ("0fcd87c11d82e3dbd4cc96b6e2642d11", "9dc8943d9e58e79638789eeab395998f"),
        "attributes": ("96f510bf100a3b070e25661e964fc98a", "5d7d3ad2209fd31066f6dcdb8169a049"),
        "columns": ("7842197321f6cba974acfc35b1c2c01d", "8efd84cfeebc0c9f6f413499222c33e8"),
        "user_mapping_options": (
            "1205e50dcb8538bc2fc4c4ae9ec28253",
            "2d490f03eac714685bc1a4619b003996",
        ),
    }
    with _connect(dsn) as c:
        got = {
            v: tuple(
                _answer(
                    c,
                    f"select md5(pg_get_viewdef('information_schema.{v}'::regclass, {p}))",
                )[0][0]
                for p in ("false", "true")
            )
            for v in views
        }
    assert got == views


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
@pytest.mark.backlog_fixed("RP-1045")
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


# `FigureColnameInternal` names a CASE by its ELSE result when that is a
# definite (strength 2) name. Here every unaliased CASE was `case`; fixed by
# RP-1046.
@pytest.mark.backlog_fixed("RP-1046")
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


# `transformAExprIn` ORed an `=` for each element holding a Var, which was
# printed inside the `= ANY` array (RP-1047).
@pytest.mark.backlog_fixed("RP-1047")
def test_pg_get_viewdef_of_an_in_list_holding_a_column(dsn: str) -> None:
    got = _viewdef_of(dsn, "v1047", "select c.relnatts in (1, c.relam::int) from pg_class c")
    assert got == [
        (
            ' SELECT ((c.relnatts = 1) OR (c.relnatts = (c.relam)::integer)) AS "?column?"\n'
            "   FROM pg_class c;",
        )
    ]


# `get_rule_sortgroupclause` parenthesises a FuncExpr item even when not
# pretty, and `name::text` is one (`text(name)`, `castmethod = 'f'`).
# Measured on a private PostgreSQL 15.20 cluster; the raw print gave
# `ORDER BY (c.relname)::text`, as the printer knew only the binary-coercible
# casts; fixed by RP-1094.
@pytest.mark.backlog_fixed("RP-1094")
def test_pg_get_viewdef_parenthesises_a_function_cast_sort_item(dsn: str) -> None:
    got = _viewdef_of(dsn, "v798o", "select c.oid from pg_class c order by c.relname::text")
    assert got == [(" SELECT c.oid\n   FROM pg_class c\n  ORDER BY ((c.relname)::text);",)]


# Which sort / group items `find_coercion_pathway` makes a FuncExpr, in both
# print modes, each measured on a private PostgreSQL 15.20 cluster: a
# `pg_cast` function cast, a length coercion, a literal under one, and a
# function call under a cast that builds no node are parenthesised raw; an
# I/O conversion, a binary-coercible cast and a domain are not; a same-type
# cast of a column, and a literal's cast, are bare even when pretty (RP-1094).
_SORT_ITEMS = [
    ("c.relname::varchar", "((c.relname)::character varying)", "(c.relname::character varying)"),
    ("c.relnatts::int8", "((c.relnatts)::bigint)", "(c.relnatts::bigint)"),
    ("c.reltuples::int4", "((c.reltuples)::integer)", "(c.reltuples::integer)"),
    ("c.relkind::text", "((c.relkind)::text)", "(c.relkind::text)"),
    ("c.relnatts::text", "(c.relnatts)::text", "(c.relnatts::text)"),
    ("c.oid::regclass", "(c.oid)::regclass", "(c.oid::regclass)"),
    ("c.relname::name", "c.relname", "c.relname"),
    ("'a'::text", "'a'::text", "'a'::text"),
    ("'a'::varchar(3)", "('a'::character varying(3))", "('a'::character varying(3))"),
    (
        "c.relname::varchar(10)",
        "((c.relname)::character varying(10))",
        "(c.relname::character varying(10))",
    ),
    ("c.relnatts::numeric(5,1)", "((c.relnatts)::numeric(5,1))", "(c.relnatts::numeric(5,1))"),
    ("c.relname::text::text", "((c.relname)::text)", "(c.relname::text)"),
    ("lower(c.relname)::text", "(lower((c.relname)::text))", "(lower(c.relname::text))"),
]


@pytest.mark.backlog_fixed("RP-1094")
@pytest.mark.parametrize(("item", "raw", "pretty"), _SORT_ITEMS)
def test_pg_get_viewdef_sort_item_parentheses(dsn: str, item: str, raw: str, pretty: str) -> None:
    head = " SELECT c.oid\n   FROM pg_class c\n  ORDER BY "
    with _connect(dsn) as c:
        c.execute(f"create or replace view v1094 as select c.oid from pg_class c order by {item}")
        got = _answer(
            c, "select pg_get_viewdef('v1094'::regclass), pg_get_viewdef('v1094'::regclass, true)"
        )
        c.execute("drop view v1094")
    assert got == [(f"{head}{raw};", f"{head}{pretty};")]


# A window's PARTITION BY, a GROUP BY and a DISTINCT ON print each item as
# an ORDER BY one (PostgreSQL 15.20).
@pytest.mark.backlog_fixed("RP-1094")
def test_pg_get_viewdef_function_cast_in_partition_group_distinct(dsn: str) -> None:
    got = [
        _viewdef_of(dsn, "v1094", q)
        for q in [
            "select c.oid, row_number() over (partition by c.relname::text) from pg_class c",
            "select c.relname::text, count(*) from pg_class c group by c.relname::text",
            "select distinct on (c.relname::text) c.oid from pg_class c",
        ]
    ]
    assert got == [
        [
            (
                " SELECT c.oid,\n"
                "    row_number() OVER (PARTITION BY ((c.relname)::text)) AS row_number\n"
                "   FROM pg_class c;",
            )
        ],
        [
            (
                " SELECT (c.relname)::text AS relname,\n"
                "    count(*) AS count\n"
                "   FROM pg_class c\n"
                "  GROUP BY ((c.relname)::text);",
            )
        ],
        [(" SELECT DISTINCT ON (((c.relname)::text)) c.oid\n   FROM pg_class c;",)],
    ]


# An operator over a column of a user domain resolves on the base type and
# relabels the operand, which `get_oper_expr` shows. Measured on a private
# PostgreSQL 15.20 cluster. The column was typed as its base type and
# printed bare, `(u798.dd > 2)`; fixed by RP-1095, whose wider matrix is
# tests/test_rust_pgserver_viewdef_domains.py.
WANT798 = [(" SELECT u798.k\n   FROM u798\n  WHERE ((u798.dd)::integer > 2);",)]


@pytest.mark.backlog_fixed("RP-1095")
def test_pg_get_viewdef_relabels_a_user_domain_operand(dsn: str) -> None:
    with _connect(dsn) as c:
        c.execute("drop table if exists u798 cascade")
        c.execute("drop domain if exists dint798")
        c.execute("create domain dint798 as int")
        c.execute("create table u798 (k int, dd dint798)")
        c.execute("create view w798 as select k from u798 where dd > 2")
        got = _answer(c, "select pg_get_viewdef('w798'::regclass)")
        # RP-1231: failed once on CI (#3109, 2026-11-28) printing `(u798.dd
        # > 2)`, and never in 120 runs here. When it misses, ask again on
        # this connection and on fresh ones, and whether the column reads
        # as the domain: the same answer everywhere means the view was
        # stored without the relabel; a right answer elsewhere, a stale
        # per-thread domain list where it was asked.
        context = ""
        if got != WANT798:
            again = [_answer(c, "select pg_get_viewdef('w798'::regclass)")]
            for _ in range(3):
                with _connect(dsn) as fresh:
                    again.append(_answer(fresh, "select pg_get_viewdef('w798'::regclass)"))
            col = _answer(
                c,
                "select format_type(atttypid, atttypmod) from pg_attribute"
                " where attrelid = 'u798'::regclass and attname = 'dd'",
            )
            context = f"; asked again (same, then fresh connections): {again}; dd is {col}"
        c.execute("drop table u798 cascade")
        c.execute("drop domain dint798")
    assert got == WANT798, f"{got}{context}"
