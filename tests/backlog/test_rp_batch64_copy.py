"""Batch 64 of the Rust PG server (2026-10-24): COPY FROM checks each row
as an INSERT does (RP-241), in PostgreSQL's order, and says where a row
failed (RP-242).

Each scenario runs the same steps against a private, freshly initdb'd
PostgreSQL 15.19 cluster and this server; the literals are 15.19's answers,
measured by running this file as a script:

    PYTHONPATH=. python -m tests.backlog.test_rp_batch64_copy \
        "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. A step is SQL, or `(copy
statement, data)`: the data is sent one line per message, as `pgbench` and
`psql` send it. An error is `(ERR, sqlstate, message, detail)`, with the
context too where a scenario asks (`context=True`). Every scenario drops
what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn, _marked

Step = str | tuple[str, str]

_SETUP = [
    "drop table if exists rp64c_a, rp64c_p, rp64c_l cascade",
    "create table rp64c_p (id int primary key)",
    "insert into rp64c_p values (1)",
    "create table rp64c_a (a int not null check (a < 5), b int references rp64c_p, c int, d int)",
    "create unique index rp64c_u on rp64c_a ((c % 10))",
]
_COPY = "copy rp64c_a from stdin"
_DROP = "drop table rp64c_a, rp64c_p"

SCENARIOS: dict[str, tuple[str, bool, bool, list[Step]]] = {
    "COPY FROM checks NOT NULL CHECK and foreign keys": (
        "RP-241",
        False,
        False,
        [
            *_SETUP,
            (_COPY, "\\N\t1\t1\t1\n"),
            (_COPY, "7\t1\t1\t1\n"),
            (_COPY, "1\t9\t1\t1\n"),
            (_COPY, "1\t1\t3\t1\n1\t1\t13\t1\n"),
            (_COPY, "1\t1\t1\t1\n2\t1\t2\t2\n"),
            "select * from rp64c_a order by c",
            _DROP,
        ],
    ),
    "COPY FROM reports the error PostgreSQL meets first": (
        "RP-241",
        False,
        False,
        [
            *_SETUP,
            # A CHECK is met as its row is read, before a later bad row.
            (_COPY, "1\t1\t1\t1\n7\t1\t2\t1\nx\t1\t3\t1\n"),
            # A foreign key only at the end, after every row is read.
            (_COPY, "1\t9\t1\t1\n2\t1\t2\t1\nx\t1\t3\t1\n"),
            (_COPY, "1\t9\t1\t1\n8\t1\t2\t1\n"),
            # A unique index when the buffered rows are inserted: after a bad
            # row read later.
            (_COPY, "1\t1\t1\t1\n1\t1\t11\t1\nx\t1\t3\t1\n"),
            "select count(*) from rp64c_a",
            _DROP,
        ],
    ),
    "COPY FROM fires row and statement triggers": (
        "RP-241",
        False,
        False,
        [
            *_SETUP,
            "create table rp64c_l (n text)",
            "create function rp64c_t() returns trigger language plpgsql as $$ begin"
            " insert into rp64c_l values (tg_when || ' ' || tg_level || ' '"
            " || coalesce(new.d::text, '-'));"
            " if tg_level = 'ROW' and tg_when = 'BEFORE' then new.d := new.d * 10; end if;"
            " return new; end $$",
            "create trigger rp64c_b before insert on rp64c_a for each row"
            " execute function rp64c_t()",
            "create trigger rp64c_ar after insert on rp64c_a for each row"
            " execute function rp64c_t()",
            "create trigger rp64c_bs before insert on rp64c_a for each statement"
            " execute function rp64c_t()",
            "create trigger rp64c_as after insert on rp64c_a for each statement"
            " execute function rp64c_t()",
            (_COPY, "1\t1\t1\t1\n2\t1\t2\t2\n"),
            "select a, d from rp64c_a order by a",
            "select n from rp64c_l",
            _DROP,
            "drop table rp64c_l",
            "drop function rp64c_t",
        ],
    ),
    "a COPY error names its line": (
        "RP-242",
        True,
        True,
        [
            *_SETUP,
            (_COPY, "1\t1\t1\t1\nx\t1\t2\t1\n"),
            (_COPY, "1\t1\t1\t1\n7\t1\t2\t1\n"),
            (_COPY, "1\t1\t3\t1\n1\t1\t13\t1\n"),
            _DROP,
        ],
    ),
}


def _step(conn: Any, step: Step, context: bool) -> Any:
    try:
        if isinstance(step, str):
            cur = conn.execute(step)
            if cur.description is None:
                return cur.statusmessage
            return [tuple(str(v) for v in row) for row in cur.fetchall()]
        sql, data = step
        cur = conn.cursor()
        with cur.copy(sql) as copy:
            for line in data.splitlines(keepends=True):
                copy.write(line)
        return cur.statusmessage
    except psycopg.Error as e:
        answer = ("ERR", e.sqlstate, e.diag.message_primary, e.diag.message_detail)
        return (*answer, e.diag.context) if context else answer


def run_copy_scenario(dsn: str, steps: list[Step], context: bool) -> list[Any]:
    with psycopg.connect(dsn, autocommit=True) as conn:
        return [_step(conn, s, context) for s in steps]


def _scenario(name: str, server: Any) -> list[Any]:
    _, _, context, steps = SCENARIOS[name]
    return run_copy_scenario(_dsn(server) + " dbname=postgres", steps, context)


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(name, marks=_marked(item, is_open), id=name)
        for name, (item, is_open, _, _) in sorted(SCENARIOS.items())
    ],
)
def test_scenario(name: str, rust_pg: Any) -> None:
    assert _scenario(name, rust_pg) == EXPECTED[name]


EXPECTED: dict[str, Any] = {
    "COPY FROM checks NOT NULL CHECK and foreign keys": [
        "DROP TABLE",
        "CREATE TABLE",
        "INSERT 0 1",
        "CREATE TABLE",
        "CREATE INDEX",
        (
            "ERR",
            "23502",
            'null value in column "a" of relation "rp64c_a" violates not-null constraint',
            "Failing row contains (null, 1, 1, 1).",
        ),
        (
            "ERR",
            "23514",
            'new row for relation "rp64c_a" violates check constraint "rp64c_a_a_check"',
            "Failing row contains (7, 1, 1, 1).",
        ),
        (
            "ERR",
            "23503",
            'insert or update on table "rp64c_a" violates foreign key constraint "rp64c_a_b_fkey"',
            'Key (b)=(9) is not present in table "rp64c_p".',
        ),
        (
            "ERR",
            "23505",
            'duplicate key value violates unique constraint "rp64c_u"',
            "Key ((c % 10))=(3) already exists.",
        ),
        "COPY 2",
        [("1", "1", "1", "1"), ("2", "1", "2", "2")],
        "DROP TABLE",
    ],
    "COPY FROM reports the error PostgreSQL meets first": [
        "DROP TABLE",
        "CREATE TABLE",
        "INSERT 0 1",
        "CREATE TABLE",
        "CREATE INDEX",
        (
            "ERR",
            "23514",
            'new row for relation "rp64c_a" violates check constraint "rp64c_a_a_check"',
            "Failing row contains (7, 1, 2, 1).",
        ),
        ("ERR", "22P02", 'invalid input syntax for type integer: "x"', None),
        (
            "ERR",
            "23514",
            'new row for relation "rp64c_a" violates check constraint "rp64c_a_a_check"',
            "Failing row contains (8, 1, 2, 1).",
        ),
        ("ERR", "22P02", 'invalid input syntax for type integer: "x"', None),
        [("0",)],
        "DROP TABLE",
    ],
    "COPY FROM fires row and statement triggers": [
        "DROP TABLE",
        "CREATE TABLE",
        "INSERT 0 1",
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE TABLE",
        "CREATE FUNCTION",
        "CREATE TRIGGER",
        "CREATE TRIGGER",
        "CREATE TRIGGER",
        "CREATE TRIGGER",
        "COPY 2",
        [("1", "10"), ("2", "20")],
        [
            ("BEFORE STATEMENT -",),
            ("BEFORE ROW 1",),
            ("BEFORE ROW 2",),
            ("AFTER ROW 10",),
            ("AFTER ROW 20",),
            ("AFTER STATEMENT -",),
        ],
        "DROP TABLE",
        "DROP TABLE",
        "DROP FUNCTION",
    ],
    "a COPY error names its line": [
        "DROP TABLE",
        "CREATE TABLE",
        "INSERT 0 1",
        "CREATE TABLE",
        "CREATE INDEX",
        (
            "ERR",
            "22P02",
            'invalid input syntax for type integer: "x"',
            None,
            'COPY rp64c_a, line 2, column a: "x"',
        ),
        (
            "ERR",
            "23514",
            'new row for relation "rp64c_a" violates check constraint "rp64c_a_a_check"',
            "Failing row contains (7, 1, 2, 1).",
            'COPY rp64c_a, line 2: "7\t1\t2\t1"',
        ),
        (
            "ERR",
            "23505",
            'duplicate key value violates unique constraint "rp64c_u"',
            "Key ((c % 10))=(3) already exists.",
            "COPY rp64c_a, line 2",
        ),
        "DROP TABLE",
    ],
}


if __name__ == "__main__":
    dsn = sys.argv[1] + " dbname=postgres"
    print({name: run_copy_scenario(dsn, s, c) for name, (_, _, c, s) in SCENARIOS.items()})
