"""Batch 72 of the Rust PG server (2026-10-25): operator classes of the
GiST, GIN, SP-GiST and BRIN methods (RP-169), and every user class's and
member's `pg_depend` rows.

Each scenario runs the same statements against a private, freshly
initdb'd PostgreSQL 15.19 cluster and this server; the literals are 15.19's
answers, measured by running this file as a script:

    PYTHONPATH=. python -m tests.backlog.test_rp_batch72 "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario rolls back what it
made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_GIST_BOX = (
    "create operator class rp72_gb for type box using gist as operator 3 &&,"
    " function 1 gist_box_consistent(internal, box, smallint, oid, internal),"
    " function 2 gist_box_union(internal, internal),"
    " function 3 gist_box_union(internal, internal),"
    " function 5 gist_box_penalty(internal, internal, internal),"
    " function 6 gist_box_picksplit(internal, internal),"
    " function 7 gist_box_same(box, box, internal)"
)
_DEPENDS = (
    "select classid::regclass::text, pg_describe_object(classid, objid, objsubid),"
    " refclassid::regclass::text, pg_describe_object(refclassid, refobjid, refobjsubid),"
    " deptype from pg_depend where classid in ('pg_amop'::regclass, 'pg_amproc'::regclass,"
    " 'pg_opclass'::regclass, 'pg_opfamily'::regclass) and objid >= 16384"
    " and pg_describe_object(classid, objid, objsubid) like '%rp72%' order by 1, 2, 4"
)

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    "operator classes of the GiST GIN SP-GiST and BRIN methods": (
        "RP-169",
        False,
        [
            "begin",
            _GIST_BOX,
            "select opcname, opcmethod::int, opcintype::regtype::text, opckeytype::int"
            " from pg_opclass where opcname = 'rp72_gb'",
            "select amprocnum, amproclefttype::regtype::text, amprocrighttype::regtype::text,"
            " amproc::text from pg_amproc p join pg_opfamily f on f.oid = p.amprocfamily"
            " where f.opfname = 'rp72_gb' order by 1",
            "savepoint s",
            "create operator class rp72_g2 for type box using gist as"
            " function 12 gist_box_union(internal, internal)",
            "rollback to s",
            "create operator class rp72_gin for type int4 using gin as operator 1 <,"
            " function 1 btint4cmp(int4, int4), storage int8",
            "select opckeytype::regtype::text from pg_opclass where opcname = 'rp72_gin'",
            "savepoint s",
            "create operator class rp72_bt for type int4 using btree as operator 1 <,"
            " function 1 btint4cmp(int4, int4), storage int8",
            "rollback to s",
            "savepoint s",
            "create operator class rp72_go for type int4 using gin as"
            " operator 1 < (int4, int4) for order by integer_ops, function 1 btint4cmp(int4, int4)",
            "rollback to s",
            "create operator class rp72_br for type int4 using brin as operator 1 <,"
            " function 1 brin_minmax_opcinfo(internal)",
            "create operator class rp72_sp for type box using spgist as operator 3 &&,"
            " function 1 spg_box_quad_config(internal, internal)",
            "select opcname, opcmethod::int from pg_opclass where opcname like 'rp72%' order by 1",
            "rollback",
        ],
    ),
    "a class's members depend on it or on its family": (
        "RP-169",
        False,
        [
            "begin",
            "create operator class rp72_b for type int4 using btree as operator 1 <,"
            " function 1 btint4cmp(int4, int4)",
            _GIST_BOX,
            "create operator class rp72_br for type int4 using brin as operator 1 <,"
            " function 1 brin_minmax_opcinfo(internal)",
            _DEPENDS,
            "rollback",
        ],
    ),
}


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


EXPECTED: dict[str, Any] = {
    "operator classes of the GiST GIN SP-GiST and BRIN methods": [
        "BEGIN",
        "CREATE OPERATOR CLASS",
        [("rp72_gb", "783", "box", "0")],
        [
            ("1", "box", "box", "gist_box_consistent"),
            ("2", "box", "box", "gist_box_union"),
            ("3", "box", "box", "gist_box_union"),
            ("5", "box", "box", "gist_box_penalty"),
            ("6", "box", "box", "gist_box_picksplit"),
            ("7", "box", "box", "gist_box_same"),
        ],
        "SAVEPOINT",
        ("ERR", "42P17", "invalid function number 12, must be between 1 and 11", None),
        "ROLLBACK",
        "CREATE OPERATOR CLASS",
        [("bigint",)],
        "SAVEPOINT",
        (
            "ERR",
            "42P17",
            'storage type cannot be different from data type for access method "btree"',
            None,
        ),
        "ROLLBACK",
        "SAVEPOINT",
        ("ERR", "42P17", 'access method "gin" does not support ordering operators', None),
        "ROLLBACK",
        "CREATE OPERATOR CLASS",
        "CREATE OPERATOR CLASS",
        [("rp72_br", "3580"), ("rp72_gb", "783"), ("rp72_gin", "2742"), ("rp72_sp", "4000")],
        "ROLLBACK",
    ],
    "a class's members depend on it or on its family": [
        "BEGIN",
        "CREATE OPERATOR CLASS",
        "CREATE OPERATOR CLASS",
        "CREATE OPERATOR CLASS",
        [
            (
                "pg_amop",
                "operator 1 (integer, integer) "
                "of operator family rp72_b for "
                "access method btree: "
                "<(integer,integer)",
                "pg_opclass",
                "operator class rp72_b for access method btree",
                "i",
            ),
            (
                "pg_amop",
                "operator 1 (integer, integer) "
                "of operator family rp72_br for "
                "access method brin: "
                "<(integer,integer)",
                "pg_opclass",
                "operator class rp72_br for access method brin",
                "i",
            ),
            (
                "pg_amop",
                "operator 3 (box, box) of "
                "operator family rp72_gb for "
                "access method gist: "
                "&&(box,box)",
                "pg_opfamily",
                "operator family rp72_gb for access method gist",
                "a",
            ),
            (
                "pg_amproc",
                "function 1 (box, box) of "
                "operator family rp72_gb for "
                "access method gist: "
                "gist_box_consistent(internal,box,smallint,oid,internal)",
                "pg_opclass",
                "operator class rp72_gb for access method gist",
                "i",
            ),
            (
                "pg_amproc",
                "function 1 (integer, integer) "
                "of operator family rp72_b for "
                "access method btree: "
                "btint4cmp(integer,integer)",
                "pg_opclass",
                "operator class rp72_b for access method btree",
                "i",
            ),
            (
                "pg_amproc",
                "function 1 (integer, integer) "
                "of operator family rp72_br for "
                "access method brin: "
                "brin_minmax_opcinfo(internal)",
                "pg_opclass",
                "operator class rp72_br for access method brin",
                "i",
            ),
            (
                "pg_amproc",
                "function 2 (box, box) of "
                "operator family rp72_gb for "
                "access method gist: "
                "gist_box_union(internal,internal)",
                "pg_opclass",
                "operator class rp72_gb for access method gist",
                "i",
            ),
            (
                "pg_amproc",
                "function 3 (box, box) of "
                "operator family rp72_gb for "
                "access method gist: "
                "gist_box_union(internal,internal)",
                "pg_opfamily",
                "operator family rp72_gb for access method gist",
                "a",
            ),
            (
                "pg_amproc",
                "function 5 (box, box) of "
                "operator family rp72_gb for "
                "access method gist: "
                "gist_box_penalty(internal,internal,internal)",
                "pg_opclass",
                "operator class rp72_gb for access method gist",
                "i",
            ),
            (
                "pg_amproc",
                "function 6 (box, box) of "
                "operator family rp72_gb for "
                "access method gist: "
                "gist_box_picksplit(internal,internal)",
                "pg_opclass",
                "operator class rp72_gb for access method gist",
                "i",
            ),
            (
                "pg_amproc",
                "function 7 (box, box) of "
                "operator family rp72_gb for "
                "access method gist: "
                "gist_box_same(box,box,internal)",
                "pg_opclass",
                "operator class rp72_gb for access method gist",
                "i",
            ),
            (
                "pg_opclass",
                "operator class rp72_b for access method btree",
                "pg_opfamily",
                "operator family rp72_b for access method btree",
                "a",
            ),
            (
                "pg_opclass",
                "operator class rp72_b for access method btree",
                "pg_namespace",
                "schema public",
                "n",
            ),
            (
                "pg_opclass",
                "operator class rp72_br for access method brin",
                "pg_opfamily",
                "operator family rp72_br for access method brin",
                "a",
            ),
            (
                "pg_opclass",
                "operator class rp72_br for access method brin",
                "pg_namespace",
                "schema public",
                "n",
            ),
            (
                "pg_opclass",
                "operator class rp72_gb for access method gist",
                "pg_opfamily",
                "operator family rp72_gb for access method gist",
                "a",
            ),
            (
                "pg_opclass",
                "operator class rp72_gb for access method gist",
                "pg_namespace",
                "schema public",
                "n",
            ),
            (
                "pg_opfamily",
                "operator family rp72_b for access method btree",
                "pg_namespace",
                "schema public",
                "n",
            ),
            (
                "pg_opfamily",
                "operator family rp72_br for access method brin",
                "pg_namespace",
                "schema public",
                "n",
            ),
            (
                "pg_opfamily",
                "operator family rp72_gb for access method gist",
                "pg_namespace",
                "schema public",
                "n",
            ),
        ],
        "ROLLBACK",
    ],
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_scenario(dsn, stmts) for name, (_, _, stmts) in SCENARIOS.items()})
