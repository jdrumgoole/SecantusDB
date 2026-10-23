"""Batch 41 of the Rust PG server (2026-10-23): the regression tests of
RP-187 (an explicit operator class a method has for the key's type was
refused), RP-188 (`pg_opclass` listed only btree's default classes) and
RP-189 (`regtype` did not name the pseudo-types), all found while fixing
RP-183, whose test is in `test_rp_batch40.py`.

The scenarios run as `test_rp_batch34.py`'s do. The expected answers were
measured on a private, freshly initdb'd PostgreSQL 15.19 cluster by running
this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch41.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------------- RP-187 an explicit operator class
    "an explicit operator class for the key's type": (
        "RP-187",
        False,
        [
            "create table rp41_o (a int, b varchar, c text[], d int8, e int[])",
            "create index rp41_oa on rp41_o (a int4_ops)",
            "create index rp41_ob on rp41_o (b text_ops)",
            "create index rp41_oc on rp41_o (c array_ops)",
            "create index rp41_od on rp41_o (d int4_ops)",
            "create index rp41_oe on rp41_o using hash (a int4_ops)",
            "create index rp41_of on rp41_o (d int8_ops)",
            "create index rp41_og on rp41_o (b varchar_ops)",
            "create index rp41_oh on rp41_o (a rp41_nosuch_ops)",
            "select indexname, indexdef from pg_indexes where tablename = 'rp41_o' order by 1",
            "drop table rp41_o",
        ],
    ),
    # ------------------------------------------- RP-188 pg_opclass's rows
    "pg_opclass lists every built-in operator class": (
        "RP-188",
        False,
        [
            "select count(*), count(*) filter (where opcdefault),"
            " count(distinct opcmethod) from pg_opclass",
            "select a.amname, c.opcname, c.opcintype::regtype::text, c.opcdefault"
            " from pg_opclass c join pg_am a on a.oid = c.opcmethod"
            " where c.opcname in ('jsonb_ops', 'array_ops', 'range_ops', 'text_pattern_ops')"
            " order by 1, 2, 3",
        ],
    ),
    # ----------------------------------------- RP-189 pseudo-types' names
    "regtype names the pseudo-types": (
        "RP-189",
        False,
        [
            "select string_agg(o::regtype::text, ',') from unnest(array[32, 269, 325, 705,"
            " 2249, 2275, 2276, 2277, 2278, 2279, 2280, 2281, 2283, 2287, 2776, 3115, 3310,"
            " 3500, 3831, 3838, 4537, 4538, 5077, 5078, 5079, 5080]::oid[]) o",
            "select 'anyarray'::regtype::oid, to_regtype('anyelement')::oid,"
            " to_regtype('internal')::oid, to_regtype('record[]')::oid,"
            " to_regtype('table_am_handler')::oid, 'pg_catalog.void'::regtype::oid",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "an explicit operator class for the key's type": [
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        ("ERR", "42804", 'operator class "int4_ops" does not accept data type bigint', None),
        "CREATE INDEX",
        "CREATE INDEX",
        "CREATE INDEX",
        (
            "ERR",
            "42704",
            'operator class "rp41_nosuch_ops" does not exist for access method "btree"',
            None,
        ),
        [
            ("rp41_oa", "CREATE INDEX rp41_oa ON public.rp41_o USING btree (a)"),
            ("rp41_ob", "CREATE INDEX rp41_ob ON public.rp41_o USING btree (b)"),
            ("rp41_oc", "CREATE INDEX rp41_oc ON public.rp41_o USING btree (c)"),
            ("rp41_oe", "CREATE INDEX rp41_oe ON public.rp41_o USING hash (a)"),
            ("rp41_of", "CREATE INDEX rp41_of ON public.rp41_o USING btree (d)"),
            ("rp41_og", "CREATE INDEX rp41_og ON public.rp41_o USING btree (b varchar_ops)"),
        ],
        "DROP TABLE",
    ],
    "pg_opclass lists every built-in operator class": [
        [("177", "119", "6")],
        [
            ("btree", "array_ops", "anyarray", "True"),
            ("btree", "jsonb_ops", "jsonb", "True"),
            ("btree", "range_ops", "anyrange", "True"),
            ("btree", "text_pattern_ops", "text", "False"),
            ("gin", "array_ops", "anyarray", "True"),
            ("gin", "jsonb_ops", "jsonb", "True"),
            ("gist", "range_ops", "anyrange", "True"),
            ("hash", "array_ops", "anyarray", "True"),
            ("hash", "jsonb_ops", "jsonb", "True"),
            ("hash", "range_ops", "anyrange", "True"),
            ("hash", "text_pattern_ops", "text", "False"),
            ("spgist", "range_ops", "anyrange", "True"),
        ],
    ],
    "regtype names the pseudo-types": [
        [
            (
                'pg_ddl_command,table_am_handler,index_am_handler,unknown,record,cstring,"any",anyarray,void,trigger,language_handler,internal,anyelement,record[],anynonarray,fdw_handler,tsm_handler,anyenum,anyrange,event_trigger,anymultirange,anycompatiblemultirange,anycompatible,anycompatiblearray,anycompatiblenonarray,anycompatiblerange',
            )
        ],
        [("2277", "2283", "2281", "2287", "269", "2278")],
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
