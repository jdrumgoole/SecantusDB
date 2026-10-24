"""Batch 64 of the Rust PG server (2026-10-24): `pg_subscription` is one
catalog for the cluster (RP-169), an MCV list's timestamps (RP-099), a
`timestamptz` assigned or bounded under a session's zone (RP-240), and a
statement trigger's NEW (RP-243).

Each scenario runs the same statements against a private, freshly
initdb'd PostgreSQL 15.19 cluster and this server; the literals are 15.19's
answers, measured by running this file as a script:

    PYTHONPATH=. python -m tests.backlog.test_rp_batch64 "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it
made.
"""

from __future__ import annotations

import sys
from typing import Any

import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked
from tests.backlog.test_rp_batch34 import run_scenario

_SUB = "connection 'dbname=x' publication p with (connect = false, slot_name = none)"

SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # --------------------------------------- RP-099 a timestamp's MCV datums
    "an MCV list over timestamps keeps their microseconds": (
        "RP-099",
        False,
        [
            "begin",
            "set local timezone = 'America/New_York'",
            "create table rp64_m (t timestamp, z timestamptz, a int)",
            "insert into rp64_m select"
            " timestamp '2020-01-01 10:00' + (g % 3) * interval '1.0003 sec',"
            " timestamptz '2021-06-01 00:00+02' + (g % 2) * interval '5 microseconds', g % 3"
            " from generate_series(1, 30) g",
            "create statistics rp64_ms (mcv) on t, z, a from rp64_m",
            "analyze rp64_m",
            "select d.stxdmcv::text from pg_statistic_ext s join pg_statistic_ext_data d"
            " on d.stxoid = s.oid where s.stxname = 'rp64_ms'",
            "select m.index, m.values, m.frequency from pg_statistic_ext s"
            " join pg_statistic_ext_data d on d.stxoid = s.oid, pg_mcv_list_items(d.stxdmcv) m"
            " where s.stxname = 'rp64_ms' order by 1",
            "rollback",
        ],
    ),
    # ------------------------- RP-240 a timestamptz under a session's zone
    "INSERT ... SELECT keeps a timestamptz's instant": (
        "RP-240",
        False,
        [
            "begin",
            "set local timezone = 'America/New_York'",
            "create table rp64_z (k text, z timestamptz, t timestamp, d date, r tstzrange,"
            " a timestamptz[])",
            "insert into rp64_z select 'tz', timestamptz '2021-06-01 00:00+02',"
            " timestamptz '2021-06-01 00:00+02', timestamptz '2021-06-01 00:00+02',"
            " tstzrange(timestamptz '2021-06-01 00:00+02', null),"
            " array[timestamptz '2021-06-01 00:00+02']",
            "insert into rp64_z select 'ts', timestamp '2021-06-01 00:00',"
            " timestamp '2021-06-01 00:00',"
            " timestamp '2021-06-01 00:00', tstzrange(timestamp '2021-06-01 00:00', null),"
            " array[timestamp '2021-06-01 00:00']",
            "insert into rp64_z select 'plus', z + interval '5 microseconds', t, d, r, a"
            " from rp64_z where k = 'tz'",
            "insert into rp64_z select 'copy', z, t, d, r, a from rp64_z where k = 'plus'",
            "select k, z, t, d, r, a, extract(epoch from z) from rp64_z order by k",
            "rollback",
        ],
    ),
    "a tstzrange of timestamptz bounds keeps their instants": (
        "RP-240",
        False,
        [
            "begin",
            "set local timezone = 'America/New_York'",
            "select tstzrange(timestamptz '2021-06-01 00:00+02', null),"
            " tstzrange(timestamp '2021-06-01 00:00', timestamp '2021-06-02 00:00'),"
            " tstzrange('2021-06-01 00:00', '2021-06-02 00:00+02', '[]'),"
            " tstzrange(timestamptz '2021-06-01 00:00:00.000005+02', null)",
            "create table rp64_r (z timestamptz, t timestamp)",
            "insert into rp64_r values ('2021-06-01 00:00+02', '2021-06-01 00:00'),"
            " ('2021-06-01 00:00:00.5+02', '2021-06-01 00:00:00.25')",
            "select tstzrange(z, z + interval '1 hour'), tstzrange(t, t + interval '1 hour')"
            " from rp64_r order by z",
            "rollback",
        ],
    ),
    # --------------------------- RP-243 NEW in a statement-level trigger
    "a statement trigger's NEW is a NULL row": (
        "RP-243",
        False,
        [
            "begin",
            "create table rp64_t (d int, e text)",
            "create table rp64_tl (n text)",
            "create function rp64_tf() returns trigger language plpgsql as $$ begin"
            " insert into rp64_tl values (tg_level || ' ' || coalesce(new.d::text, '-')"
            " || ' ' || coalesce(old.e, '-') || ' ' || pg_typeof(new.d)::text);"
            " return new; end $$",
            "create trigger rp64_tb before insert on rp64_t for each row"
            " execute function rp64_tf()",
            "create trigger rp64_ts before insert on rp64_t for each statement"
            " execute function rp64_tf()",
            "insert into rp64_t values (1, 'x')",
            "select n from rp64_tl",
            "rollback",
        ],
    ),
    "each database names its own subscriptions": (
        "RP-169",
        False,
        # Committed, then dropped: CREATE DATABASE cannot run in a block.
        [
            "drop database if exists rp64_d1",
            "drop database if exists rp64_d2",
            "create database rp64_d1",
            "create database rp64_d2",
            "@open b dbname=rp64_d1",
            "@open c dbname=rp64_d2",
            f"@b create subscription rp64_s {_SUB}",
            f"@b create subscription rp64_t {_SUB}",
            # One name in another database; a name resolves in its own.
            f"@c create subscription rp64_s {_SUB}",
            "@c drop subscription rp64_t",
            "@c alter subscription rp64_t rename to rp64_u",
            "select subname, d.datname, o.roname is not null from pg_subscription s"
            " join pg_database d on d.oid = s.subdbid"
            " left join pg_replication_origin o on o.roname = 'pg_' || s.oid order by 1, 2",
            "select subname from pg_stat_subscription order by 1",
            "select subname from pg_stat_subscription_stats order by 1",
            "select count(*) from pg_replication_origin",
            # A subscription keeps its database.
            "!drop database rp64_d1",
            "@b drop subscription rp64_s",
            "!drop database rp64_d1",
            "@b drop subscription rp64_t",
            "@c drop subscription rp64_s",
            "@b select 1",
            "select count(*) from pg_subscription",
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
    "an MCV list over timestamps keeps their microseconds": [
        "BEGIN",
        "SET",
        "CREATE TABLE",
        "INSERT 0 30",
        "CREATE STATISTICS",
        "ANALYZE",
        [
            (
                "\\xc251a6e1010000000600000003005a040000a00400001700000003000000180000000000000008000000010000000200000010000000000000000800000001000000030000000c00000000000000040000000100000000c886e80f3e02006c0b96e80f3e0200d84ea5e80f3e020000f88f1da666020005f88f1da6660200000000000100000002000000000000555555555555c53f1cc7711cc771ac3f000000000000000000555555555555c53f1cc7711cc771ac3f000001000000000000555555555555c53f1cc7711cc771ac3f010000000100000000555555555555c53f1cc7711cc771ac3f010001000100000000555555555555c53f1cc7711cc771ac3f020000000200000000555555555555c53f1cc7711cc771ac3f020001000200",
            )
        ],
        [
            ("0", "['2020-01-01 10:00:00', '2021-05-31 18:00:00-04', '0']", "0.16666666666666666"),
            (
                "1",
                "['2020-01-01 10:00:00', '2021-05-31 18:00:00.000005-04', '0']",
                "0.16666666666666666",
            ),
            (
                "2",
                "['2020-01-01 10:00:01.0003', '2021-05-31 18:00:00-04', '1']",
                "0.16666666666666666",
            ),
            (
                "3",
                "['2020-01-01 10:00:01.0003', '2021-05-31 18:00:00.000005-04', '1']",
                "0.16666666666666666",
            ),
            (
                "4",
                "['2020-01-01 10:00:02.0006', '2021-05-31 18:00:00-04', '2']",
                "0.16666666666666666",
            ),
            (
                "5",
                "['2020-01-01 10:00:02.0006', '2021-05-31 18:00:00.000005-04', '2']",
                "0.16666666666666666",
            ),
        ],
        "ROLLBACK",
    ],
    "INSERT ... SELECT keeps a timestamptz's instant": [
        "BEGIN",
        "SET",
        "CREATE TABLE",
        "INSERT 0 1",
        "INSERT 0 1",
        "INSERT 0 1",
        "INSERT 0 1",
        [
            (
                "copy",
                "2021-05-31 18:00:00.000005-04:00",
                "2021-05-31 18:00:00",
                "2021-05-31",
                "[2021-05-31 18:00:00-04:00, None)",
                "[datetime.datetime(2021, 5, "
                "31, 18, 0, "
                "tzinfo=zoneinfo.ZoneInfo(key='America/New_York'))]",
                "1622498400.000005",
            ),
            (
                "plus",
                "2021-05-31 18:00:00.000005-04:00",
                "2021-05-31 18:00:00",
                "2021-05-31",
                "[2021-05-31 18:00:00-04:00, None)",
                "[datetime.datetime(2021, 5, "
                "31, 18, 0, "
                "tzinfo=zoneinfo.ZoneInfo(key='America/New_York'))]",
                "1622498400.000005",
            ),
            (
                "ts",
                "2021-06-01 00:00:00-04:00",
                "2021-06-01 00:00:00",
                "2021-06-01",
                "[2021-06-01 00:00:00-04:00, None)",
                "[datetime.datetime(2021, 6, 1, "
                "0, 0, "
                "tzinfo=zoneinfo.ZoneInfo(key='America/New_York'))]",
                "1622520000.000000",
            ),
            (
                "tz",
                "2021-05-31 18:00:00-04:00",
                "2021-05-31 18:00:00",
                "2021-05-31",
                "[2021-05-31 18:00:00-04:00, None)",
                "[datetime.datetime(2021, 5, "
                "31, 18, 0, "
                "tzinfo=zoneinfo.ZoneInfo(key='America/New_York'))]",
                "1622498400.000000",
            ),
        ],
        "ROLLBACK",
    ],
    "a tstzrange of timestamptz bounds keeps their instants": [
        "BEGIN",
        "SET",
        [
            (
                "[2021-05-31 18:00:00-04:00, None)",
                "[2021-06-01 00:00:00-04:00, 2021-06-02 00:00:00-04:00)",
                "[2021-06-01 00:00:00-04:00, 2021-06-01 18:00:00-04:00]",
                "[2021-05-31 18:00:00.000005-04:00, None)",
            )
        ],
        "CREATE TABLE",
        "INSERT 0 2",
        [
            (
                "[2021-05-31 18:00:00-04:00, 2021-05-31 19:00:00-04:00)",
                "[2021-06-01 00:00:00-04:00, 2021-06-01 01:00:00-04:00)",
            ),
            (
                "[2021-05-31 18:00:00.500000-04:00, 2021-05-31 19:00:00.500000-04:00)",
                "[2021-06-01 00:00:00.250000-04:00, 2021-06-01 01:00:00.250000-04:00)",
            ),
        ],
        "ROLLBACK",
    ],
    "a statement trigger's NEW is a NULL row": [
        "BEGIN",
        "CREATE TABLE",
        "CREATE TABLE",
        "CREATE FUNCTION",
        "CREATE TRIGGER",
        "CREATE TRIGGER",
        "INSERT 0 1",
        [("STATEMENT - - integer",), ("ROW 1 - integer",)],
        "ROLLBACK",
    ],
    "each database names its own subscriptions": [
        "DROP DATABASE",
        "DROP DATABASE",
        "CREATE DATABASE",
        "CREATE DATABASE",
        "CREATE SUBSCRIPTION",
        "CREATE SUBSCRIPTION",
        "CREATE SUBSCRIPTION",
        ("ERR", "42704", 'subscription "rp64_t" does not exist', None),
        ("ERR", "42704", 'subscription "rp64_t" does not exist', None),
        [
            ("rp64_s", "rp64_d1", "True"),
            ("rp64_s", "rp64_d2", "True"),
            ("rp64_t", "rp64_d1", "True"),
        ],
        [("rp64_s",), ("rp64_s",), ("rp64_t",)],
        [("rp64_s",), ("rp64_s",), ("rp64_t",)],
        [("3",)],
        (
            "ERR",
            "55006",
            'database "rp64_d1" is being used by logical replication subscription',
            None,
            "There are 2 subscriptions.",
        ),
        "DROP SUBSCRIPTION",
        (
            "ERR",
            "55006",
            'database "rp64_d1" is being used by logical replication subscription',
            None,
            "There is 1 subscription.",
        ),
        "DROP SUBSCRIPTION",
        "DROP SUBSCRIPTION",
        [("1",)],
        [("0",)],
    ],
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_scenario(dsn, stmts) for name, (_, _, stmts) in SCENARIOS.items()})
