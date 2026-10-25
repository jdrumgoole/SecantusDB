"""Batch 68 of the Rust PG server (2026-10-25): the heap buffers each kind
of write reads (RP-239), and `SELECT ... INTO` (RP-246).

Each scenario runs the same steps against a private, freshly initdb'd
PostgreSQL 15.19 cluster and this server; the literals are 15.19's answers,
measured by running this file as a script:

    PYTHONPATH=. python -m tests.backlog.test_rp_batch68 \\
        "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. A step is SQL, or `(copy
statement, data)` as `test_rp_batch64_copy` sends it. Every scenario drops
what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch64_copy import Step, run_copy_scenario

_FLUSH = "select pg_stat_force_next_flush()"
# The heap buffers `rp68_t`'s writes read since `rp68_base` noted them.
_NOTE = [
    _FLUSH,
    "select 1",
    "drop table if exists rp68_base",
    "create table rp68_base as select heap_blks_hit + heap_blks_read n"
    " from pg_statio_user_tables where relname = 'rp68_t'",
]
_READ = [
    _FLUSH,
    "select 1",
    "select s.heap_blks_hit + s.heap_blks_read - b.n"
    " from pg_statio_user_tables s, rp68_base b where s.relname = 'rp68_t'",
]
_TABLE = "create table rp68_t (g int, p text) with (autovacuum_enabled = false)"
_FULL = (
    "create table rp68_t with (autovacuum_enabled = false) as"
    " select g, repeat('x', 100) p from generate_series(1, 100) g"
)
_ROWS = "".join(f"{i}\t{'x' * 100}\n" for i in range(2000))


def _writes(setup: str, *steps: Step) -> list[Step]:
    return [setup, *_NOTE, *steps, *_READ, "drop table rp68_t, rp68_base"]


SCENARIOS: dict[str, tuple[str, bool, list[Step]]] = {
    "INSERT reads its target page and the free space map": (
        "RP-239",
        False,
        _writes(
            _TABLE,
            "insert into rp68_t values (1, 'a'), (2, 'b'), (3, 'c')",
            "insert into rp68_t select g, repeat('x', 100) from generate_series(1, 2000) g",
            "insert into rp68_t select g, repeat('x', 100) from generate_series(1, 2000) g",
        ),
    ),
    "UPDATE reads the old version twice": (
        "RP-239",
        False,
        _writes(_FULL, "update rp68_t set g = 7 where g = 1"),
    ),
    "UPDATE of every row reads the old versions and new pages": (
        "RP-239",
        False,
        _writes(_FULL, "update rp68_t set g = g + 1"),
    ),
    # ---------------------------------- RP-247 an UPDATE's rows' new places
    "an UPDATE after a row moved places rows as PostgreSQL does": (
        "RP-247",
        False,
        [
            _FULL,
            "update rp68_t set g = -g where g = 1",
            "update rp68_t set g = g + 1000",
            "select string_agg(ctid::text || ':' || g, ' ' order by ctid) from rp68_t",
            "drop table rp68_t",
        ],
    ),
    "DELETE reads each row's page": (
        "RP-239",
        False,
        _writes(_FULL, "delete from rp68_t where g = 5", "delete from rp68_t"),
    ),
    "COPY writes through a bulk insert": (
        "RP-239",
        False,
        _writes(_TABLE, ("copy rp68_t from stdin", _ROWS), ("copy rp68_t from stdin", _ROWS)),
    ),
    "CREATE TABLE AS and SELECT INTO skip the free space map": (
        "RP-239",
        False,
        [
            "select g, repeat('x', 100) p into rp68_s from generate_series(1, 2000) g",
            "create table rp68_c as select g, repeat('x', 100) p from generate_series(1, 2000) g",
            _FLUSH,
            "select 1",
            "select relname, heap_blks_hit + heap_blks_read from pg_statio_user_tables"
            " where relname in ('rp68_s', 'rp68_c') order by 1",
            "drop table rp68_s, rp68_c",
        ],
    ),
    # ------------------------------------------ RP-246 SELECT ... INTO
    "SELECT INTO creates its table": (
        "RP-246",
        False,
        [
            "select g into rp68_i from generate_series(1, 3) g",
            "select * from rp68_i",
            "select g, g * 2 as h into temp rp68_it from generate_series(1, 2) g",
            "select * from rp68_it",
            "select 1 into rp68_i",
            "select x.* into unlogged rp68_iu from (values (1, 'a')) x(a, b)",
            "select attname, atttypid::regtype::text from pg_attribute"
            " where attrelid = 'rp68_iu'::regclass and attnum > 0 order by attnum",
            "create unlogged table rp68_cu as select 1",
            "select relname, relpersistence from pg_class"
            " where relname in ('rp68_i', 'rp68_iu', 'rp68_cu') order by 1",
            "select g into rp68_ie from generate_series(1, 2) g where false",
            "select count(*) from rp68_ie",
            "drop table rp68_i, rp68_it, rp68_iu, rp68_cu, rp68_ie",
        ],
    ),
}


def _scenario(name: str, server: Any) -> list[Any]:
    return run_copy_scenario(_dsn(server) + " dbname=postgres", SCENARIOS[name][2], False)


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
    "INSERT reads its target page and the free space map": [
        "CREATE TABLE",
        [("",)],
        [("1",)],
        "DROP TABLE",
        "SELECT 1",
        "INSERT 0 3",
        "INSERT 0 2000",
        "INSERT 0 2000",
        [("",)],
        [("1",)],
        [("4207",)],
        "DROP TABLE",
    ],
    "UPDATE reads the old version twice": [
        "SELECT 100",
        [("",)],
        [("1",)],
        "DROP TABLE",
        "SELECT 1",
        "UPDATE 1",
        [("",)],
        [("1",)],
        [("5",)],
        "DROP TABLE",
    ],
    "UPDATE of every row reads the old versions and new pages": [
        "SELECT 100",
        [("",)],
        [("1",)],
        "DROP TABLE",
        "SELECT 1",
        "UPDATE 100",
        [("",)],
        [("1",)],
        [("308",)],
        "DROP TABLE",
    ],
    "an UPDATE after a row moved places rows as PostgreSQL does": [
        "SELECT 100",
        "UPDATE 1",
        "UPDATE 100",
        [
            (
                "(0,59):1002 "
                "(1,44):1003 "
                "(1,45):1004 "
                "(1,46):1005 "
                "(1,47):1006 "
                "(1,48):1007 "
                "(1,49):1008 "
                "(1,50):1009 "
                "(1,51):1010 "
                "(1,52):1011 "
                "(1,53):1012 "
                "(1,54):1013 "
                "(1,55):1014 "
                "(1,56):1015 "
                "(1,57):1016 "
                "(1,58):1017 "
                "(2,1):1018 "
                "(2,2):1019 "
                "(2,3):1020 "
                "(2,4):1021 "
                "(2,5):1022 "
                "(2,6):1023 "
                "(2,7):1024 "
                "(2,8):1025 "
                "(2,9):1026 "
                "(2,10):1027 "
                "(2,11):1028 "
                "(2,12):1029 "
                "(2,13):1030 "
                "(2,14):1031 "
                "(2,15):1032 "
                "(2,16):1033 "
                "(2,17):1034 "
                "(2,18):1035 "
                "(2,19):1036 "
                "(2,20):1037 "
                "(2,21):1038 "
                "(2,22):1039 "
                "(2,23):1040 "
                "(2,24):1041 "
                "(2,25):1042 "
                "(2,26):1043 "
                "(2,27):1044 "
                "(2,28):1045 "
                "(2,29):1046 "
                "(2,30):1047 "
                "(2,31):1048 "
                "(2,32):1049 "
                "(2,33):1050 "
                "(2,34):1051 "
                "(2,35):1052 "
                "(2,36):1053 "
                "(2,37):1054 "
                "(2,38):1055 "
                "(2,39):1056 "
                "(2,40):1057 "
                "(2,41):1058 "
                "(2,42):1059 "
                "(2,43):1060 "
                "(2,44):1061 "
                "(2,45):1062 "
                "(2,46):1063 "
                "(2,47):1064 "
                "(2,48):1065 "
                "(2,49):1066 "
                "(2,50):1067 "
                "(2,51):1068 "
                "(2,52):1069 "
                "(2,53):1070 "
                "(2,54):1071 "
                "(2,55):1072 "
                "(2,56):1073 "
                "(2,57):1074 "
                "(2,58):1075 "
                "(3,1):1076 "
                "(3,2):1077 "
                "(3,3):1078 "
                "(3,4):1079 "
                "(3,5):1080 "
                "(3,6):1081 "
                "(3,7):1082 "
                "(3,8):1083 "
                "(3,9):1084 "
                "(3,10):1085 "
                "(3,11):1086 "
                "(3,12):1087 "
                "(3,13):1088 "
                "(3,14):1089 "
                "(3,15):1090 "
                "(3,16):1091 "
                "(3,17):1092 "
                "(3,18):1093 "
                "(3,19):1094 "
                "(3,20):1095 "
                "(3,21):1096 "
                "(3,22):1097 "
                "(3,23):1098 "
                "(3,24):1099 "
                "(3,25):1100 "
                "(3,26):999",
            )
        ],
        "DROP TABLE",
    ],
    "DELETE reads each row's page": [
        "SELECT 100",
        [("",)],
        [("1",)],
        "DROP TABLE",
        "SELECT 1",
        "DELETE 1",
        "DELETE 99",
        [("",)],
        [("1",)],
        [("104",)],
        "DROP TABLE",
    ],
    "COPY writes through a bulk insert": [
        "CREATE TABLE",
        [("",)],
        [("1",)],
        "DROP TABLE",
        "SELECT 1",
        "COPY 2000",
        "COPY 2000",
        [("",)],
        [("1",)],
        [("206",)],
        "DROP TABLE",
    ],
    "CREATE TABLE AS and SELECT INTO skip the free space map": [
        "SELECT 2000",
        "SELECT 2000",
        [("",)],
        [("1",)],
        [("rp68_c", "35"), ("rp68_s", "35")],
        "DROP TABLE",
    ],
    "SELECT INTO creates its table": [
        "SELECT 3",
        [("1",), ("2",), ("3",)],
        "SELECT 2",
        [("1", "2"), ("2", "4")],
        ("ERR", "42P07", 'relation "rp68_i" already exists', None),
        "SELECT 1",
        [("a", "integer"), ("b", "text")],
        "SELECT 1",
        [("rp68_cu", "u"), ("rp68_i", "p"), ("rp68_iu", "u")],
        "SELECT 0",
        [("0",)],
        "DROP TABLE",
    ],
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_copy_scenario(dsn, s, False) for name, (_, _, s) in SCENARIOS.items()})
