"""Batch 52 of the Rust PG server (2026-10-24): the regression tests of
RP-200 (a block reads its own index definitions and shared comments
through `pg_get_indexdef` and `shobj_description`, and another session
does not) and of RP-204 and RP-206 to RP-209, found while building
`pg_describe_object`; and the gap tests of RP-205 and RP-210.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch52.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario that changes
anything runs in transactions it rolls back.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "a block's own definitions, and no other session's": (
        "RP-200",
        False,
        [
            "@open b",
            "begin",
            "create table rp52_t (a int)",
            "create index rp52_i on rp52_t (a)",
            "comment on database postgres is 'in a block'",
            "select pg_get_indexdef('rp52_i'::regclass)",
            "select shobj_description(oid, 'pg_database') from pg_database "
            "where datname = 'postgres'",
            "@b select count(*) from pg_indexes where indexname = 'rp52_i'",
            "@b select shobj_description(oid, 'pg_database') from pg_database "
            "where datname = 'postgres'",
            "rollback",
            "select shobj_description(oid, 'pg_database') from pg_database "
            "where datname = 'postgres'",
            "select count(*) from pg_indexes where indexname = 'rp52_i'",
        ],
    ),
    "an executor function in a PL/pgSQL expression": (
        "RP-204",
        False,
        [
            "begin",
            "create sequence rp52_s",
            "~do $$ declare x bigint; begin x := nextval('rp52_s'); x := nextval('rp52_s'); "
            "raise notice 'got %', x; end $$",
            "savepoint e",
            "do $$ declare x bigint; begin x := nextval('rp52_nosuch'); end $$",
            "rollback to savepoint e",
            "savepoint e",
            "do $$ declare x text; begin x := pg_describe_object(99, 1, 0); end $$",
            "rollback to savepoint e",
            "~do $$ declare x text; begin "
            "x := pg_describe_object('pg_class'::regclass, 'pg_class'::regclass, 0); "
            "raise notice '%', x; end $$",
            "rollback",
        ],
    ),
    "a view over another schema's table depends on it": (
        "RP-206",
        False,
        [
            "begin",
            "create schema rp52_s",
            "create table rp52_s.t (a int, b int)",
            "create view rp52_s.v as select a from rp52_s.t",
            "create view rp52_pv as select t.b from rp52_s.t t",
            "select pg_describe_object(classid, objid, objsubid), refobjsubid, deptype "
            "from pg_depend where refobjid = 'rp52_s.t'::regclass "
            "and classid = 'pg_rewrite'::regclass order by 1, 2",
            "rollback",
        ],
    ),
    "pg_depend of a table, its view and its row type": (
        "RP-205",
        True,
        [
            "begin",
            "create table rp52_d (a int default 1, constraint rp52_dc check (a > 0))",
            "create view rp52_dv as select a from rp52_d",
            "create index rp52_di on rp52_d (a)",
            "select 'rp52_d'::regclass::oid <> 'rp52_d'::regtype::oid, "
            "(select reltype from pg_class where oid = 'rp52_d'::regclass) = 'rp52_d'::regtype",
            "select pg_describe_object(classid, objid, objsubid), refobjsubid, deptype "
            "from pg_depend where refobjid = 'rp52_d'::regclass order by 1, 2",
            "select pg_describe_object(classid, objid, objsubid), "
            "pg_describe_object(refclassid, refobjid, refobjsubid), deptype from pg_depend "
            "where objid = (select typarray from pg_type where oid = 'rp52_d'::regtype)",
            "rollback",
        ],
    ),
    "an index in another schema is listed in it": (
        "RP-207",
        False,
        [
            "begin",
            "create schema rp52_x",
            "create table rp52_x.t (a int)",
            "create index rp52_xi on rp52_x.t (a)",
            "select relname, relnamespace::regnamespace::text from pg_class "
            "where relname in ('t', 'rp52_xi') and relnamespace = 'rp52_x'::regnamespace "
            "order by 1",
            "rollback",
        ],
    ),
    "a built-in row type's name": (
        "RP-208",
        False,
        [
            "select 71::regtype::text, format_type(210, null), 1263::regtype::text, "
            "'information_schema.tables'::regtype::oid::regtype::text",
        ],
    ),
    "plpgsql's extension oid": (
        "RP-209",
        False,
        [
            "select oid::int, extname from pg_extension where extname = 'plpgsql'",
        ],
    ),
    "the visibility functions of an oid that names nothing": (
        "RP-210",
        False,
        [
            "select pg_table_is_visible(1), pg_type_is_visible(1), pg_function_is_visible(1), "
            "pg_operator_is_visible(1), pg_opclass_is_visible(1), pg_opfamily_is_visible(1), "
            "pg_collation_is_visible(1), pg_conversion_is_visible(1)",
            "select pg_statistics_obj_is_visible(1), pg_ts_parser_is_visible(1), "
            "pg_ts_dict_is_visible(1), pg_ts_template_is_visible(1), pg_ts_config_is_visible(1)",
            "select pg_type_is_visible('int4'::regtype), pg_function_is_visible('now'::regproc)",
        ],
    ),
    "pg_operator lists PostgreSQL's operators": (
        "RP-211",
        False,
        [
            "select count(*) from pg_operator",
            "select count(*) from pg_operator where pg_operator_is_visible(oid)",
            "select oprname, oprkind, oprleft::regtype::text, oprright::regtype::text, "
            "oprresult::regtype::text, oprcom::regoperator::text, oprnegate::regoperator::text, "
            "oprcode::text, oprrest::text, oprjoin::text from pg_operator "
            "where oid in (96, 3285, 3898) order by oid",
            "select pg_describe_object('pg_operator'::regclass, '+(int4,int4)'::regoperator, 0)",
        ],
    ),
    "an executor function in an aggregate": (
        "RP-212",
        True,
        [
            "select sum(pg_relation_size(oid)) >= 0 from pg_class",
            "select count(*) filter (where pg_type_is_visible(oid)) from pg_type",
            "select count(*) filter (where pg_relation_size(oid) >= 0) from pg_class",
        ],
    ),
    "a reg* column against a constant in WHERE": (
        "RP-213",
        True,
        [
            "select count(*) from pg_operator where oprcode = 'int4eq'::regproc",
            "select count(*) from pg_type where typinput = 'int4in'::regproc",
            "select aggtransfn::text from pg_aggregate where aggfnoid = 'sum(int4)'::regprocedure",
            "select count(*) from pg_aggregate where aggfnoid = 2108",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "a block's own definitions, and no other session's": [
        "BEGIN",
        "CREATE TABLE",
        "CREATE INDEX",
        "COMMENT",
        [("CREATE INDEX rp52_i ON public.rp52_t USING btree (a)",)],
        [("in a block",)],
        [("0",)],
        [("default administrative connection database",)],
        "ROLLBACK",
        [("default administrative connection database",)],
        [("0",)],
    ],
    "a built-in row type's name": [
        [("pg_type", "pg_type[]", "cstring[]", "information_schema.tables")]
    ],
    "a reg* column against a constant in WHERE": [[("1",)], [("1",)], [("int4_sum",)], [("1",)]],
    "a view over another schema's table depends on it": [
        "BEGIN",
        "CREATE SCHEMA",
        "CREATE TABLE",
        "CREATE VIEW",
        "CREATE VIEW",
        [("rule _RETURN on view rp52_pv", "2", "n"), ("rule _RETURN on view rp52_s.v", "1", "n")],
        "ROLLBACK",
    ],
    "an executor function in a PL/pgSQL expression": [
        "BEGIN",
        "CREATE SEQUENCE",
        ("DO", ["got 2"]),
        "SAVEPOINT",
        ("ERR", "42P01", 'relation "rp52_nosuch" does not exist', None),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "XX000", "unrecognized object class: 99", None),
        "ROLLBACK",
        ("DO", ["table pg_class"]),
        "ROLLBACK",
    ],
    "an executor function in an aggregate": [[("True",)], [("463",)], [("410",)]],
    "an index in another schema is listed in it": [
        "BEGIN",
        "CREATE SCHEMA",
        "CREATE TABLE",
        "CREATE INDEX",
        [("rp52_xi", "rp52_x"), ("t", "rp52_x")],
        "ROLLBACK",
    ],
    "pg_depend of a table, its view and its row type": [
        "BEGIN",
        "CREATE TABLE",
        "CREATE VIEW",
        "CREATE INDEX",
        [("True", "True")],
        [
            ("constraint rp52_dc on table rp52_d", "1", "a"),
            ("constraint rp52_dc on table rp52_d", "1", "n"),
            ("default value for column a of table rp52_d", "1", "a"),
            ("index rp52_di", "1", "a"),
            ("rule _RETURN on view rp52_dv", "1", "n"),
            ("type rp52_d", "0", "i"),
        ],
        [("type rp52_d[]", "type rp52_d", "i")],
        "ROLLBACK",
    ],
    "pg_operator lists PostgreSQL's operators": [
        [("799",)],
        [("799",)],
        [
            (
                "=",
                "b",
                "integer",
                "integer",
                "boolean",
                "=(integer,integer)",
                "<>(integer,integer)",
                "int4eq",
                "eqsel",
                "eqjoinsel",
            ),
            ("-", "b", "jsonb", "text", "jsonb", "0", "0", "pg_catalog.jsonb_delete", "-", "-"),
            (
                "+",
                "b",
                "anyrange",
                "anyrange",
                "anyrange",
                "+(anyrange,anyrange)",
                "0",
                "range_union",
                "-",
                "-",
            ),
        ],
        [("operator +(integer,integer)",)],
    ],
    "plpgsql's extension oid": [[("14031", "plpgsql")]],
    "the visibility functions of an oid that names nothing": [
        [("None", "None", "None", "None", "None", "None", "None", "None")],
        [("None", "None", "None", "None", "None")],
        [("True", "True")],
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
