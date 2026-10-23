"""Batch 33 of the Rust PG server (2026-10-23): the regression tests of
what it fixed -- RP-072's `ALTER SYSTEM`, and RP-159 (a setting's value
read and shown as its type does), RP-160 (a SET's value list), RP-161
(where a setting's value came from) and RP-163 (the system relations
PUBLIC may not read), found while building it -- and the gap tests of
what it filed open (RP-162, RP-164).

A scenario is a list of statements, each run in a named autocommit
connection: `a` unless the statement starts `@name `. `@open name k=v`
opens connection `name` with those connection options (a startup packet's
settings, another user); `@sleep` waits for a reload to reach every
PostgreSQL backend (a backend takes its SIGHUP when idle). An answer is a
statement's rows, its command tag, or `("ERR", sqlstate, message, hint)`.
The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster by running this file as a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch33.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made
and leaves the configuration file empty and reloaded. That cluster's
`postgresql.conf` sets nothing a scenario reads; this server has no
`postgresql.conf`, so the file a scenario reads is the auto file only.
"""

from __future__ import annotations

import sys
import time
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked


def _answer(conn: Any, sql: str) -> Any:
    try:
        cur = conn.execute(sql)
    except psycopg.Error as e:
        return ("ERR", e.sqlstate, e.diag.message_primary, e.diag.message_hint)
    if cur.description is None:
        return cur.statusmessage
    return [tuple(str(v) for v in row) for row in cur.fetchall()]


def run_scenario(dsn: str, statements: list[str]) -> list[Any]:
    conns: dict[str, Any] = {}

    def conn(name: str, options: str = "") -> Any:
        if name not in conns:
            conns[name] = psycopg.connect(f"{dsn} {options}", autocommit=True)
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
            out.append(_answer(conn(name), s))
    finally:
        for c in conns.values():
            if c.info.transaction_status != psycopg.pq.TransactionStatus.IDLE:
                c.execute("rollback")
            c.close()
    return out


# Leave the file empty, and every session with it.
_CLEAN = ["alter system reset all", "select pg_reload_conf()", "@sleep"]

# The auto file's rows of `pg_file_settings`, without the ones a
# `postgresql.conf` adds.
_FILE_ROWS = (
    "select name, setting, applied, error, sourceline from pg_file_settings "
    "where sourcefile like '%/postgresql.auto.conf' order by sourceline"
)


def _setting(names: str) -> str:
    return (
        "select name, setting, source, sourcefile like '%/postgresql.auto.conf', "
        f"sourceline, pending_restart, reset_val from pg_settings where name in ({names}) "
        "order by name"
    )


# (item, open?, statements). `open` scenarios are gap tests: they fail while
# their item is open.
SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ---------------------------------------------------- RP-072 ALTER SYSTEM
    "ALTER SYSTEM takes effect at a reload": (
        "RP-072",
        False,
        [
            *_CLEAN,
            "@open b application_name=rp33_b",
            "alter system set work_mem = '8MB'",
            "alter system set statement_timeout = 7000",
            "show work_mem",
            "@b show work_mem",
            _FILE_ROWS,
            _setting("'work_mem', 'statement_timeout'"),
            "select pg_reload_conf()",
            "@sleep",
            "show work_mem",
            "@b show work_mem",
            "@b show statement_timeout",
            _setting("'work_mem', 'statement_timeout'"),
            "@open c application_name=rp33_c",
            "@c show work_mem",
            "alter system set work_mem = '9MB'",
            "alter system set statement_timeout to default",
            _FILE_ROWS,
            "select pg_reload_conf()",
            "@sleep",
            "@b show work_mem",
            "@c show statement_timeout",
            _setting("'statement_timeout'"),
            *_CLEAN,
            "show work_mem",
            _setting("'work_mem'"),
        ],
    ),
    "a reload leaves what outranks the file": (
        "RP-072",
        False,
        [
            *_CLEAN,
            "@open b application_name=rp33_b",
            "@b set work_mem = '16MB'",
            "@b set temp_buffers = '8MB'",
            "@open t application_name=rp33_t",
            "@t begin",
            "@t set local work_mem = '1MB'",
            "@t set statement_timeout = '3s'",
            "alter system set work_mem = '8MB'",
            "alter system set temp_buffers = '8MB'",
            "alter system set statement_timeout = '7s'",
            "select pg_reload_conf()",
            "@sleep",
            "@b show work_mem",
            "@b " + _setting("'work_mem', 'temp_buffers'"),
            "@b reset work_mem",
            "@b show work_mem",
            "@b " + _setting("'work_mem'"),
            "@t show work_mem",
            "@t commit",
            "@t show work_mem",
            "@t show statement_timeout",
            "@t begin",
            "@t set work_mem = '3MB'",
            "@t rollback",
            "@t " + _setting("'work_mem'"),
            "@t begin",
            "@t set statement_timeout = '4s'",
            "@t savepoint s",
            "@t set statement_timeout = '5s'",
            "@t rollback to s",
            "@t show statement_timeout",
            "@t rollback",
            "@t " + _setting("'statement_timeout'"),
            *_CLEAN,
            "@b " + _setting("'work_mem', 'temp_buffers'"),
        ],
    ),
    "the file ALTER SYSTEM writes": (
        "RP-072",
        False,
        [
            *_CLEAN,
            "alter system set application_name = 'it''s a \\ b'",
            "alter system set work_mem = 8192",
            'alter system set search_path = a, "B c"',
            "alter system set DateStyle = 'iso', 'dmy'",
            "alter system set enable_seqscan = 'of'",
            "alter system set work_mem = '2MB'",
            _FILE_ROWS,
            "alter system reset work_mem",
            "alter system set enable_seqscan to default",
            _FILE_ROWS,
            "select pg_reload_conf()",
            "@sleep",
            "@open b",
            "@b show application_name",
            "@b show search_path",
            "@b show datestyle",
            *_CLEAN,
            _FILE_ROWS,
        ],
    ),
    "what ALTER SYSTEM refuses": (
        "RP-072",
        False,
        [
            "begin",
            "alter system set work_mem = '1MB'",
            "rollback",
            "alter system set nosuch = 1",
            "alter system reset nosuch",
            "alter system set rp33.custom = 1",
            "alter system set work_mem = 'abc'",
            "alter system set work_mem = '1kB'",
            "alter system set enable_seqscan = 'maybe'",
            "alter system set bytea_output = 'nope'",
            "alter system set work_mem = '1MB', '2MB'",
            "alter system set server_version = '1'",
            "alter system set transaction_isolation = 'serializable'",
            "alter system set data_directory = '/x'",
            "alter system set role = 'x'",
            "create role rp33_r login",
            "@open r user=rp33_r",
            "@r alter system set work_mem = '1MB'",
            "@r alter system reset work_mem",
            "@r alter system reset all",
            "@r select count(*) from pg_file_settings",
            "@r select sourcefile, sourceline from pg_settings where name = 'work_mem'",
            _FILE_ROWS,
            "drop role rp33_r",
        ],
    ),
    "a parameter only a restart changes is pending": (
        "RP-072",
        False,
        [
            *_CLEAN,
            "alter system set max_files_per_process = 500",
            "alter system set checkpoint_timeout = '10min'",
            "set checkpoint_timeout = '5min'",
            _setting("'max_files_per_process', 'checkpoint_timeout'"),
            _FILE_ROWS,
            _setting("'max_files_per_process'"),
            "select pg_reload_conf()",
            "@sleep",
            "show max_files_per_process",
            "show checkpoint_timeout",
            "@open b",
            "@b " + _setting("'max_files_per_process', 'checkpoint_timeout'"),
            *_CLEAN,
            _setting("'max_files_per_process', 'checkpoint_timeout'"),
            "@open c",
            "@c " + _setting("'max_files_per_process', 'checkpoint_timeout'"),
        ],
    ),
    # ---------------------------------------- RP-159 a setting's value, typed
    "a setting's value is read and shown as its type does": (
        "RP-159",
        False,
        [
            s
            for name, value in [
                ("work_mem", "8192"),
                ("work_mem", "8193"),
                ("work_mem", "1.5MB"),
                ("work_mem", "0x400"),
                ("work_mem", "010"),
                ("work_mem", "1e4"),
                ("work_mem", "  2MB  "),
                ("work_mem", "2048.4"),
                ("work_mem", "8mb"),
                ("work_mem", "4MB foo"),
                ("work_mem", "1kB"),
                ("work_mem", "1B"),
                ("work_mem", "abc"),
                ("work_mem", "2147483648"),
                ("work_mem", "5s"),
                ("temp_buffers", "800"),
                ("temp_buffers", "100kB"),
                ("statement_timeout", "1.5s"),
                ("statement_timeout", "1500us"),
                ("statement_timeout", "86400000"),
                ("statement_timeout", "-1"),
                ("statement_timeout", "1S"),
                ("log_min_duration_statement", "-1"),
                ("geqo_threshold", "2.5"),
                ("geqo_threshold", "5kB"),
                ("extra_float_digits", "4"),
                ("vacuum_cost_delay", "1.5"),
                ("vacuum_cost_delay", "1s"),
                ("random_page_cost", "4.0"),
                ("random_page_cost", "1.23456789"),
                ("random_page_cost", "123456789"),
                ("random_page_cost", "1e-10"),
                ("random_page_cost", "inf"),
                ("random_page_cost", "nan"),
                ("random_page_cost", "x"),
                ("cursor_tuple_fraction", "2"),
                ("enable_seqscan", "t"),
                ("enable_seqscan", "of"),
                ("enable_seqscan", "o"),
                ("enable_seqscan", "On "),
                ("bytea_output", "HEX"),
                ("bytea_output", "hex "),
                ("synchronous_commit", "Yes"),
                ("default_transaction_isolation", "Read Committed"),
                ("default_transaction_isolation", "read  committed"),
            ]
            for s in (f"set {name} = '{value}'", f"show {name}", f"reset {name}")
        ]
        + [
            "select set_config('work_mem', '8192', false)",
            "select set_config('enable_seqscan', 'maybe', false)",
            "reset all",
        ],
    ),
    # --------------------------------------------- RP-160 a SET's value list
    "a SET's value list is one text": (
        "RP-160",
        False,
        [
            "set search_path = 'a, b'",
            "show search_path",
            'set search_path = public, "$user", "Foo", \'x y\', bar',
            "show search_path",
            "set search_path = ''",
            "show search_path",
            "set search_path = 1.5",
            "show search_path",
            'create schema "Rp33 S"',
            'create table "Rp33 S".t (a int)',
            'set search_path = "Rp33 S"',
            "select count(*) from t",
            "reset search_path",
            'drop schema "Rp33 S" cascade',
            "set datestyle = 'iso', 'dmy'",
            "show datestyle",
            "reset datestyle",
            "set application_name = 'a', 'b'",
            "set work_mem = 1, 2",
            "set timezone = 'UTC', 'x'",
        ],
    ),
    # ------------------------------------ RP-161 where a value came from
    "where a setting's value came from": (
        "RP-161",
        False,
        [
            "@open b application_name=rp33_b",
            "@b " + _setting("'application_name', 'work_mem', 'transaction_isolation'"),
            "@b set work_mem = '4MB'",
            "@b set application_name = 'x'",
            "@b " + _setting("'application_name', 'work_mem'"),
            "@b reset all",
            "@b " + _setting("'application_name', 'work_mem'"),
            "@b set application_name to default",
            "@b set work_mem = '5MB'",
            "@b set work_mem to default",
            "@b " + _setting("'application_name', 'work_mem'"),
            "@b begin",
            "@b set work_mem = '6MB'",
            "@b reset application_name",
            "@b set application_name = 'y'",
            "@b rollback",
            "@b " + _setting("'application_name', 'work_mem'"),
            "@b begin",
            "@b reset work_mem",
            "@b commit",
            "@b " + _setting("'work_mem'"),
            "@b set default_transaction_read_only = on",
            "@b " + _setting("'transaction_read_only', 'transaction_deferrable'"),
            "@b set default_transaction_read_only = off",
            "@b begin",
            "@b set transaction isolation level serializable",
            "@b " + _setting("'transaction_isolation', 'transaction_deferrable'"),
            "@b commit",
        ],
    ),
    # ------------------------------ RP-162 parameters SHOW does not answer
    "every parameter answers SHOW": (
        "RP-162",
        True,
        [
            "select current_setting('is_superuser')",
            "select current_setting('default_with_oids')",
            "select current_setting('ssl_renegotiation_limit')",
            "select current_setting('data_directory') like '/%'",
            "select current_setting('config_file') like '%/postgresql.conf'",
            "select current_setting('hba_file') like '%/pg_hba.conf'",
            "select current_setting('ident_file') like '%/pg_ident.conf'",
            "select current_setting('port')::int > 0",
            "select current_setting('unix_socket_directories') is not null",
            "select current_setting('log_timezone') is not null",
        ],
    ),
    # ------------------------- RP-163 the system relations PUBLIC may not read
    "the system relations PUBLIC may not read": (
        "RP-163",
        False,
        [
            "create role rp33_q login",
            "@open r user=rp33_q",
            *(
                f"@r select count(*) > -1 from {rel}"
                for rel in (
                    "pg_authid",
                    "pg_catalog.pg_authid",
                    "pg_config",
                    "pg_file_settings",
                    "pg_hba_file_rules",
                    "pg_ident_file_mappings",
                    "pg_largeobject",
                    "pg_shadow",
                    "pg_statistic",
                    "pg_statistic_ext_data",
                    "pg_subscription",
                    "pg_user_mapping",
                    "pg_roles",
                    "pg_settings",
                    "pg_class",
                )
            ),
            "select count(*) > -1 from pg_authid",
            "drop role rp33_q",
        ],
    ),
    # --------------------------------------- RP-164 GRANT ... ON PARAMETER
    "GRANT SET and ALTER SYSTEM ON PARAMETER": (
        "RP-164",
        True,
        [
            "create role rp33_p login",
            "grant set on parameter log_min_duration_statement to rp33_p",
            "grant alter system on parameter work_mem to rp33_p",
            "select has_parameter_privilege('rp33_p', 'work_mem', 'ALTER SYSTEM')",
            "@open p user=rp33_p",
            "@p set log_min_duration_statement = 5",
            "@p alter system set work_mem = '2MB'",
            "@p alter system set temp_buffers = '2MB'",
            "revoke set on parameter log_min_duration_statement from rp33_p",
            "revoke alter system on parameter work_mem from rp33_p",
            *_CLEAN,
            "drop role rp33_p",
        ],
    ),
}

EXPECTED: dict[str, Any] = {
    "ALTER SYSTEM takes effect at a reload": [
        "ALTER SYSTEM",
        [("True",)],
        "ALTER SYSTEM",
        "ALTER SYSTEM",
        [("4MB",)],
        [("4MB",)],
        [
            ("work_mem", "8MB", "True", "None", "3"),
            ("statement_timeout", "7000", "True", "None", "4"),
        ],
        [
            ("statement_timeout", "0", "default", "None", "None", "False", "0"),
            ("work_mem", "4096", "default", "None", "None", "False", "4096"),
        ],
        [("True",)],
        [("8MB",)],
        [("8MB",)],
        [("7s",)],
        [
            ("statement_timeout", "7000", "configuration file", "True", "4", "False", "7000"),
            ("work_mem", "8192", "configuration file", "True", "3", "False", "8192"),
        ],
        [("8MB",)],
        "ALTER SYSTEM",
        "ALTER SYSTEM",
        [("work_mem", "9MB", "True", "None", "3")],
        [("True",)],
        [("9MB",)],
        [("0",)],
        [("statement_timeout", "0", "default", "None", "None", "False", "0")],
        "ALTER SYSTEM",
        [("True",)],
        [("4MB",)],
        [("work_mem", "4096", "default", "None", "None", "False", "4096")],
    ],
    "GRANT SET and ALTER SYSTEM ON PARAMETER": [
        "CREATE ROLE",
        "GRANT",
        "GRANT",
        [("True",)],
        "SET",
        "ALTER SYSTEM",
        ("ERR", "42501", 'permission denied to set parameter "temp_buffers"', None),
        "REVOKE",
        "REVOKE",
        "ALTER SYSTEM",
        [("True",)],
        "DROP ROLE",
    ],
    "a SET's value list is one text": [
        "SET",
        [('"a, b"',)],
        "SET",
        [('public, "$user", "Foo", "x y", bar',)],
        "SET",
        [('""',)],
        "SET",
        [("1.5",)],
        "CREATE SCHEMA",
        "CREATE TABLE",
        "SET",
        [("0",)],
        "RESET",
        "DROP SCHEMA",
        "SET",
        [("ISO, DMY",)],
        "RESET",
        ("ERR", "22023", "SET application_name takes only one argument", None),
        ("ERR", "22023", "SET work_mem takes only one argument", None),
        ("ERR", "22023", "SET timezone takes only one argument", None),
    ],
    "a parameter only a restart changes is pending": [
        "ALTER SYSTEM",
        [("True",)],
        "ALTER SYSTEM",
        "ALTER SYSTEM",
        ("ERR", "55P02", 'parameter "checkpoint_timeout" cannot be changed now', None),
        [
            ("checkpoint_timeout", "300", "default", "None", "None", "False", "300"),
            ("max_files_per_process", "1000", "default", "None", "None", "False", "1000"),
        ],
        [
            ("max_files_per_process", "500", "False", "setting could not be applied", "3"),
            ("checkpoint_timeout", "10min", "True", "None", "4"),
        ],
        [("max_files_per_process", "1000", "default", "None", "None", "True", "1000")],
        [("True",)],
        [("1000",)],
        [("10min",)],
        [
            ("checkpoint_timeout", "600", "configuration file", "True", "4", "False", "600"),
            ("max_files_per_process", "1000", "default", "None", "None", "True", "1000"),
        ],
        "ALTER SYSTEM",
        [("True",)],
        [
            ("checkpoint_timeout", "300", "default", "None", "None", "False", "300"),
            ("max_files_per_process", "1000", "default", "None", "None", "True", "1000"),
        ],
        [
            ("checkpoint_timeout", "300", "default", "None", "None", "False", "300"),
            ("max_files_per_process", "1000", "default", "None", "None", "True", "1000"),
        ],
    ],
    "a reload leaves what outranks the file": [
        "ALTER SYSTEM",
        [("True",)],
        "SET",
        "SET",
        "BEGIN",
        "SET",
        "SET",
        "ALTER SYSTEM",
        "ALTER SYSTEM",
        "ALTER SYSTEM",
        [("True",)],
        [("16MB",)],
        [
            ("temp_buffers", "1024", "session", "None", "None", "False", "1024"),
            ("work_mem", "16384", "session", "None", "None", "False", "8192"),
        ],
        "RESET",
        [("8MB",)],
        [("work_mem", "8192", "configuration file", "True", "3", "False", "8192")],
        [("1MB",)],
        "COMMIT",
        [("8MB",)],
        [("3s",)],
        "BEGIN",
        "SET",
        "ROLLBACK",
        [("work_mem", "8192", "configuration file", "True", "3", "False", "8192")],
        "BEGIN",
        "SET",
        "SAVEPOINT",
        "SET",
        "ROLLBACK",
        [("4s",)],
        "ROLLBACK",
        [("statement_timeout", "3000", "session", "None", "None", "False", "7000")],
        "ALTER SYSTEM",
        [("True",)],
        [
            ("temp_buffers", "1024", "session", "None", "None", "False", "1024"),
            ("work_mem", "4096", "default", "None", "None", "False", "4096"),
        ],
    ],
    "a setting's value is read and shown as its type does": [
        "SET",
        [("8MB",)],
        "RESET",
        "SET",
        [("8193kB",)],
        "RESET",
        "SET",
        [("1536kB",)],
        "RESET",
        "SET",
        [("1MB",)],
        "RESET",
        (
            "ERR",
            "22023",
            '8 kB is outside the valid range for parameter "work_mem" (64 .. 2147483647)',
            None,
        ),
        [("4MB",)],
        "RESET",
        "SET",
        [("10000kB",)],
        "RESET",
        "SET",
        [("2MB",)],
        "RESET",
        "SET",
        [("2MB",)],
        "RESET",
        (
            "ERR",
            "22023",
            'invalid value for parameter "work_mem": "8mb"',
            'Valid units for this parameter are "B", "kB", "MB", "GB", and "TB".',
        ),
        [("4MB",)],
        "RESET",
        (
            "ERR",
            "22023",
            'invalid value for parameter "work_mem": "4MB foo"',
            'Valid units for this parameter are "B", "kB", "MB", "GB", and "TB".',
        ),
        [("4MB",)],
        "RESET",
        (
            "ERR",
            "22023",
            '1 kB is outside the valid range for parameter "work_mem" (64 .. 2147483647)',
            None,
        ),
        [("4MB",)],
        "RESET",
        (
            "ERR",
            "22023",
            '0 kB is outside the valid range for parameter "work_mem" (64 .. 2147483647)',
            None,
        ),
        [("4MB",)],
        "RESET",
        ("ERR", "22023", 'invalid value for parameter "work_mem": "abc"', None),
        [("4MB",)],
        "RESET",
        (
            "ERR",
            "22023",
            'invalid value for parameter "work_mem": "2147483648"',
            "Value exceeds integer range.",
        ),
        [("4MB",)],
        "RESET",
        (
            "ERR",
            "22023",
            'invalid value for parameter "work_mem": "5s"',
            'Valid units for this parameter are "B", "kB", "MB", "GB", and "TB".',
        ),
        [("4MB",)],
        "RESET",
        "SET",
        [("6400kB",)],
        "RESET",
        (
            "ERR",
            "22023",
            '12 8kB is outside the valid range for parameter "temp_buffers" (100 .. 1073741823)',
            None,
        ),
        [("8MB",)],
        "RESET",
        "SET",
        [("1500ms",)],
        "RESET",
        "SET",
        [("2ms",)],
        "RESET",
        "SET",
        [("1d",)],
        "RESET",
        (
            "ERR",
            "22023",
            '-1 ms is outside the valid range for parameter "statement_timeout" (0 .. 2147483647)',
            None,
        ),
        [("0",)],
        "RESET",
        (
            "ERR",
            "22023",
            'invalid value for parameter "statement_timeout": "1S"',
            'Valid units for this parameter are "us", "ms", "s", "min", "h", and "d".',
        ),
        [("0",)],
        "RESET",
        "SET",
        [("-1",)],
        "RESET",
        "SET",
        [("2",)],
        "RESET",
        ("ERR", "22023", 'invalid value for parameter "geqo_threshold": "5kB"', None),
        [("12",)],
        "RESET",
        (
            "ERR",
            "22023",
            '4 is outside the valid range for parameter "extra_float_digits" (-15 .. 3)',
            None,
        ),
        [("1",)],
        "RESET",
        "SET",
        [("1500us",)],
        "RESET",
        (
            "ERR",
            "22023",
            '1000 ms is outside the valid range for parameter "vacuum_cost_delay" (0 .. 100)',
            None,
        ),
        [("0",)],
        "RESET",
        "SET",
        [("4",)],
        "RESET",
        "SET",
        [("1.23457",)],
        "RESET",
        "SET",
        [("1.23457e+08",)],
        "RESET",
        "SET",
        [("1e-10",)],
        "RESET",
        (
            "ERR",
            "22023",
            "Infinity is outside the valid range "
            'for parameter "random_page_cost" (0 .. '
            "1.79769e+308)",
            None,
        ),
        [("4",)],
        "RESET",
        ("ERR", "22023", 'invalid value for parameter "random_page_cost": "nan"', None),
        [("4",)],
        "RESET",
        ("ERR", "22023", 'invalid value for parameter "random_page_cost": "x"', None),
        [("4",)],
        "RESET",
        (
            "ERR",
            "22023",
            '2 is outside the valid range for parameter "cursor_tuple_fraction" (0 .. 1)',
            None,
        ),
        [("0.1",)],
        "RESET",
        "SET",
        [("on",)],
        "RESET",
        "SET",
        [("off",)],
        "RESET",
        ("ERR", "22023", 'parameter "enable_seqscan" requires a Boolean value', None),
        [("on",)],
        "RESET",
        ("ERR", "22023", 'parameter "enable_seqscan" requires a Boolean value', None),
        [("on",)],
        "RESET",
        "SET",
        [("hex",)],
        "RESET",
        (
            "ERR",
            "22023",
            'invalid value for parameter "bytea_output": "hex "',
            "Available values: escape, hex.",
        ),
        [("hex",)],
        "RESET",
        "SET",
        [("on",)],
        "RESET",
        "SET",
        [("read committed",)],
        "RESET",
        (
            "ERR",
            "22023",
            'invalid value for parameter "default_transaction_isolation": "read  committed"',
            "Available values: serializable, repeatable read, read committed, read uncommitted.",
        ),
        [("read committed",)],
        "RESET",
        [("8MB",)],
        ("ERR", "22023", 'parameter "enable_seqscan" requires a Boolean value', None),
        "RESET",
    ],
    "every parameter answers SHOW": [
        [("on",)],
        [("off",)],
        [("0",)],
        [("True",)],
        [("True",)],
        [("True",)],
        [("True",)],
        [("True",)],
        [("True",)],
        [("True",)],
    ],
    "the file ALTER SYSTEM writes": [
        "ALTER SYSTEM",
        [("True",)],
        "ALTER SYSTEM",
        "ALTER SYSTEM",
        "ALTER SYSTEM",
        "ALTER SYSTEM",
        "ALTER SYSTEM",
        "ALTER SYSTEM",
        [
            ("application_name", "it's a \\ b", "True", "None", "3"),
            ("search_path", 'a, "B c"', "True", "None", "4"),
            ("datestyle", "iso, dmy", "True", "None", "5"),
            ("enable_seqscan", "of", "True", "None", "6"),
            ("work_mem", "2MB", "True", "None", "7"),
        ],
        "ALTER SYSTEM",
        "ALTER SYSTEM",
        [
            ("application_name", "it's a \\ b", "True", "None", "3"),
            ("search_path", 'a, "B c"', "True", "None", "4"),
            ("datestyle", "iso, dmy", "True", "None", "5"),
        ],
        [("True",)],
        [("it's a \\ b",)],
        [('a, "B c"',)],
        [("ISO, DMY",)],
        "ALTER SYSTEM",
        [("True",)],
        [],
    ],
    "the system relations PUBLIC may not read": [
        "CREATE ROLE",
        ("ERR", "42501", "permission denied for table pg_authid", None),
        ("ERR", "42501", "permission denied for table pg_authid", None),
        ("ERR", "42501", "permission denied for view pg_config", None),
        ("ERR", "42501", "permission denied for view pg_file_settings", None),
        ("ERR", "42501", "permission denied for view pg_hba_file_rules", None),
        ("ERR", "42501", "permission denied for view pg_ident_file_mappings", None),
        ("ERR", "42501", "permission denied for table pg_largeobject", None),
        ("ERR", "42501", "permission denied for view pg_shadow", None),
        ("ERR", "42501", "permission denied for table pg_statistic", None),
        ("ERR", "42501", "permission denied for table pg_statistic_ext_data", None),
        [("True",)],
        ("ERR", "42501", "permission denied for table pg_user_mapping", None),
        [("True",)],
        [("True",)],
        [("True",)],
        [("True",)],
        "DROP ROLE",
    ],
    "what ALTER SYSTEM refuses": [
        "BEGIN",
        ("ERR", "25001", "ALTER SYSTEM cannot run inside a transaction block", None),
        "ROLLBACK",
        ("ERR", "42704", 'unrecognized configuration parameter "nosuch"', None),
        ("ERR", "42704", 'unrecognized configuration parameter "nosuch"', None),
        ("ERR", "42704", 'unrecognized configuration parameter "rp33.custom"', None),
        ("ERR", "22023", 'invalid value for parameter "work_mem": "abc"', None),
        (
            "ERR",
            "22023",
            '1 kB is outside the valid range for parameter "work_mem" (64 .. 2147483647)',
            None,
        ),
        ("ERR", "22023", 'parameter "enable_seqscan" requires a Boolean value', None),
        (
            "ERR",
            "22023",
            'invalid value for parameter "bytea_output": "nope"',
            "Available values: escape, hex.",
        ),
        ("ERR", "22023", "SET work_mem takes only one argument", None),
        ("ERR", "55P02", 'parameter "server_version" cannot be changed', None),
        ("ERR", "55P02", 'parameter "transaction_isolation" cannot be changed', None),
        ("ERR", "55P02", 'parameter "data_directory" cannot be changed', None),
        ("ERR", "55P02", 'parameter "role" cannot be changed', None),
        "CREATE ROLE",
        ("ERR", "42501", 'permission denied to set parameter "work_mem"', None),
        ("ERR", "42501", 'permission denied to set parameter "work_mem"', None),
        ("ERR", "42501", "permission denied to perform ALTER SYSTEM RESET ALL", None),
        ("ERR", "42501", "permission denied for view pg_file_settings", None),
        [("None", "None")],
        [],
        "DROP ROLE",
    ],
    "where a setting's value came from": [
        [
            ("application_name", "rp33_b", "client", "None", "None", "False", "rp33_b"),
            (
                "transaction_isolation",
                "read committed",
                "override",
                "None",
                "None",
                "False",
                "read committed",
            ),
            ("work_mem", "4096", "default", "None", "None", "False", "4096"),
        ],
        "SET",
        "SET",
        [
            ("application_name", "x", "session", "None", "None", "False", "rp33_b"),
            ("work_mem", "4096", "session", "None", "None", "False", "4096"),
        ],
        "RESET",
        [
            ("application_name", "rp33_b", "client", "None", "None", "False", "rp33_b"),
            ("work_mem", "4096", "default", "None", "None", "False", "4096"),
        ],
        "SET",
        "SET",
        "SET",
        [
            ("application_name", "rp33_b", "client", "None", "None", "False", "rp33_b"),
            ("work_mem", "4096", "default", "None", "None", "False", "4096"),
        ],
        "BEGIN",
        "SET",
        "RESET",
        "SET",
        "ROLLBACK",
        [
            ("application_name", "rp33_b", "client", "None", "None", "False", "rp33_b"),
            ("work_mem", "4096", "default", "None", "None", "False", "4096"),
        ],
        "BEGIN",
        "RESET",
        "COMMIT",
        [("work_mem", "4096", "default", "None", "None", "False", "4096")],
        "SET",
        [
            ("transaction_deferrable", "off", "override", "None", "None", "False", "off"),
            ("transaction_read_only", "on", "override", "None", "None", "False", "off"),
        ],
        "SET",
        "BEGIN",
        "SET",
        [
            ("transaction_deferrable", "off", "override", "None", "None", "False", "off"),
            (
                "transaction_isolation",
                "serializable",
                "session",
                "None",
                "None",
                "False",
                "read committed",
            ),
        ],
        "COMMIT",
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
