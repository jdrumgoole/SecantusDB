"""Batch 57 of the Rust PG server (2026-10-24): the regression tests of
RP-105 (an extension's members, in the catalogs and `pg_depend`) and of
RP-228 to RP-232, found while building it.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch57.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario that changes
anything runs in transactions it rolls back.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_EXTENSIONS = ["citext", "hstore", "pg_trgm", "pgcrypto", "btree_gin", "btree_gist"]

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # --------------------------------------------- RP-105 an extension's members
    "every extension's members, described": (
        "RP-105",
        False,
        [
            "begin",
            *[f"create extension {e}" for e in _EXTENSIONS],
            "select e.extname::text, d.classid::regclass::text, count(*), "
            "md5(string_agg(pg_describe_object(d.classid, d.objid, d.objsubid), ',' "
            "order by pg_describe_object(d.classid, d.objid, d.objsubid))) "
            "from pg_depend d join pg_extension e on e.oid = d.refobjid "
            "where d.refclassid = 'pg_extension'::regclass and d.deptype = 'e' "
            "group by 1, 2 order by 1, 2",
            "select count(*), md5(string_agg(sig, ',' order by sig)) "
            "from (select proname || '(' || "
            "pg_get_function_identity_arguments(oid) || ') ' || "
            "coalesce(pg_get_function_result(oid), '-') as sig "
            "from pg_proc where pronamespace = 'public'::regnamespace) s",
            "select 'citext_eq(citext,citext)'::regprocedure::text, 'gtrgm'::regtype::text, "
            "(select typarray::regtype::text from pg_type where typname = 'ghstore')",
            "select count(*) from pg_amop a join pg_opfamily f on f.oid = a.amopfamily "
            "where f.opfnamespace = 'public'::regnamespace",
            "rollback",
        ],
    ),
    "an extension's members go with it": (
        "RP-105",
        False,
        [
            "begin",
            "create extension citext",
            "select count(*) from pg_proc where pronamespace = 'public'::regnamespace",
            "drop extension citext",
            "select count(*) from pg_proc where pronamespace = 'public'::regnamespace",
            "select count(*) from pg_opclass where opcnamespace = 'public'::regnamespace",
            "rollback",
        ],
    ),
    # ------------------------------------- RP-228 a built-in function's signature
    "every built-in function's signature": (
        "RP-228",
        False,
        [
            "select count(*), md5(string_agg(oid::text || ':' || "
            "coalesce(pg_get_function_arguments(oid), '~') || '|' || "
            "coalesce(pg_get_function_identity_arguments(oid), '~') || '|' || "
            "coalesce(pg_get_function_result(oid), '~'), ',' order by oid)) "
            "from pg_proc where oid < 16384",
            "select pg_get_function_identity_arguments('int4pl'::regproc), "
            "pg_get_function_result('generate_series(int4,int4)'::regprocedure), "
            "pg_get_function_arguments('pg_get_keywords'::regproc)",
        ],
    ),
    # --------------------------------- RP-229 a built-in function's definition
    "a built-in function's definition": (
        "RP-229",
        False,
        [
            "select pg_get_functiondef('int4pl'::regproc)",
            "select pg_get_functiondef('generate_series(int4,int4)'::regprocedure)",
            "select pg_get_functiondef('count(\"any\")'::regprocedure)",
            "select count(*), md5(string_agg(pg_get_functiondef(oid), '' order by oid)) "
            "from pg_proc where oid < 16384 and prokind <> 'a'",
            "begin",
            "create extension citext",
            "select pg_get_functiondef('citext_eq(citext,citext)'::regprocedure)",
            "rollback",
        ],
    ),
    # ------------------------- RP-232 a reserved word as a regprocedure's type
    "a regprocedure's argument that is a reserved word": (
        "RP-232",
        False,
        [
            "select 'count(any)'::regprocedure",
            "select 'f(int, table)'::regprocedure",
            "select to_regprocedure('count(any)')",
            "select 'count(\"any\")'::regprocedure::oid, 'now()'::regprocedure::oid",
        ],
    ),
    # ------------------------- RP-230 an operator class is visible per method
    "an operator class hides another only within its access method": (
        "RP-230",
        False,
        [
            "begin",
            "create extension btree_gin",
            "select pg_describe_object('pg_opclass'::regclass, c.oid, 0), "
            "pg_describe_object('pg_opfamily'::regclass, c.opcfamily, 0) "
            "from pg_opclass c where c.opcname = 'int4_ops' order by c.opcmethod",
            "rollback",
        ],
    ),
    # ---------------------------- RP-231 a function a built-in hides, qualified
    "a public function a built-in of its name hides prints qualified": (
        "RP-231",
        False,
        [
            "begin",
            "create function public.now() returns timestamptz language sql "
            "as 'select pg_catalog.now()'",
            "create function public.rp57_f() returns int language sql as 'select 1'",
            "select oid::regprocedure::text from pg_proc "
            "where pronamespace = 'public'::regnamespace order by 1",
            "rollback",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a built-in function's definition": [
        [
            (
                "CREATE OR REPLACE FUNCTION pg_catalog.int4pl(integer, "
                "integer)\n"
                " RETURNS integer\n"
                " LANGUAGE internal\n"
                " IMMUTABLE PARALLEL SAFE STRICT\n"
                "AS $function$int4pl$function$\n",
            )
        ],
        [
            (
                "CREATE OR REPLACE FUNCTION "
                "pg_catalog.generate_series(integer, integer)\n"
                " RETURNS SETOF integer\n"
                " LANGUAGE internal\n"
                " IMMUTABLE PARALLEL SAFE STRICT SUPPORT "
                "generate_series_int4_support\n"
                "AS $function$generate_series_int4$function$\n",
            )
        ],
        ("ERR", "42809", '"count" is an aggregate function', None),
        [("3096", "7573a1f744f260ef033f330a8042a337")],
        "BEGIN",
        "CREATE EXTENSION",
        [
            (
                "CREATE OR REPLACE FUNCTION public.citext_eq(citext, "
                "citext)\n"
                " RETURNS boolean\n"
                " LANGUAGE c\n"
                " IMMUTABLE PARALLEL SAFE STRICT\n"
                "AS '$libdir/citext', $function$citext_eq$function$\n",
            )
        ],
        "ROLLBACK",
    ],
    "a public function a built-in of its name hides prints qualified": [
        "BEGIN",
        "CREATE FUNCTION",
        "CREATE FUNCTION",
        [("public.now()",), ("rp57_f()",)],
        "ROLLBACK",
    ],
    "a regprocedure's argument that is a reserved word": [
        ("ERR", "42601", 'syntax error at or near "any"', None),
        ("ERR", "42601", 'syntax error at or near "table"', None),
        ("ERR", "42601", 'syntax error at or near "any"', None),
        [("2147", "1299")],
    ],
    "an extension's members go with it": [
        "BEGIN",
        "CREATE EXTENSION",
        [("47",)],
        "DROP EXTENSION",
        [("0",)],
        [("0",)],
        "ROLLBACK",
    ],
    "an operator class hides another only within its access method": [
        "BEGIN",
        "CREATE EXTENSION",
        [
            (
                "operator class int4_ops for access method btree",
                "operator family integer_ops for access method btree",
            ),
            (
                "operator class int4_ops for access method hash",
                "operator family integer_ops for access method hash",
            ),
            (
                "operator class int4_ops for access method gin",
                "operator family int4_ops for access method gin",
            ),
        ],
        "ROLLBACK",
    ],
    "every built-in function's signature": [
        [("3244", "438fa1f2cca719ea898d8c5040c25a11")],
        [
            (
                "integer, integer",
                "SETOF integer",
                'OUT word text, OUT catcode "char", OUT barelabel '
                "boolean, OUT catdesc text, OUT baredesc text",
            )
        ],
    ],
    "every extension's members, described": [
        "BEGIN",
        "CREATE EXTENSION",
        "CREATE EXTENSION",
        "CREATE EXTENSION",
        "CREATE EXTENSION",
        "CREATE EXTENSION",
        "CREATE EXTENSION",
        [
            ("btree_gin", "pg_opclass", "29", "e57fd41872190caeacfbc1a82f8290d2"),
            ("btree_gin", "pg_opfamily", "29", "a77c7d80939de4e763e49e35cea24a82"),
            ("btree_gin", "pg_proc", "87", "3bf3409a3d6d15aad7c752d4616f7125"),
            ("btree_gist", "pg_opclass", "26", "bcfdc6780fa9778317c1f2c97a423d24"),
            ("btree_gist", "pg_operator", "12", "27eb97bc795385c4de65c0bedd2f65c0"),
            ("btree_gist", "pg_opfamily", "26", "9b493bca1fea117b0c3b21f6762a24f7"),
            ("btree_gist", "pg_proc", "188", "a2c006c988d86300b2e8334882de3925"),
            ("btree_gist", "pg_type", "6", "820cb9d5b8d486af9b31bf94bc7fe87c"),
            ("citext", "pg_cast", "8", "51617f06c12ba03ecb42b1cef1e54355"),
            ("citext", "pg_opclass", "3", "587d2ed851b0efd81204d3d926c48121"),
            ("citext", "pg_operator", "26", "817e38e55682f9165bfe27f85ea8139b"),
            ("citext", "pg_opfamily", "3", "0e40f619b8d4dad665d8bbc1b2b6d158"),
            ("citext", "pg_proc", "47", "e2b8168119b4c35eab81b355534542ce"),
            ("citext", "pg_type", "1", "03ea94f8be5558bb6631c6ba6adc84ac"),
            ("hstore", "pg_cast", "3", "2f1c9f08e5b7ad4df196e80fc8c5fe31"),
            ("hstore", "pg_opclass", "4", "cacf58d7f11bd380a2a4c402121783db"),
            ("hstore", "pg_operator", "20", "2f6c558a2b3d5cbb030ecaecf2edbf05"),
            ("hstore", "pg_opfamily", "4", "735d60eb2eb8b3917e2362071f353659"),
            ("hstore", "pg_proc", "60", "d5b61b8e3f32fb79df2a8cf429b16fd5"),
            ("hstore", "pg_type", "2", "5a2508ca23091b322af9086101744510"),
            ("pg_trgm", "pg_opclass", "2", "76433cbe8a66c9a101bb43d4739b57b8"),
            ("pg_trgm", "pg_operator", "10", "89be9c743afff4fac8c23e0af775dacb"),
            ("pg_trgm", "pg_opfamily", "2", "6f5629165486c80d2b4c476c7054c186"),
            ("pg_trgm", "pg_proc", "31", "f5c20a333b702b771e5416ccf796b296"),
            ("pg_trgm", "pg_type", "1", "6e1590cc29c96253a73c165a3d2b1504"),
            ("pgcrypto", "pg_proc", "36", "5e3b82cb24d79e0e9d5750314f71a2a6"),
            ("plpgsql", "pg_language", "1", "d0796b8e138b3c586549829d43e76279"),
            ("plpgsql", "pg_proc", "3", "9cd1300771cb42d8426493cbdebfa3f6"),
        ],
        [("449", "a3471b81e6de2eabe5f1ed0dd18cb20b")],
        [("citext_eq(citext,citext)", "gtrgm", "ghstore[]")],
        [("357",)],
        "ROLLBACK",
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
