"""Gap test for RP-064's remainder: a database created with a non-UTF8
ENCODING behaves as that encoding (`tasks/backlog-rust-postgres.md`).

PostgreSQL reports `server_encoding` as the database's encoding, defaults
`client_encoding` to it, refuses a character the encoding lacks (22P05) on the
way from the client, and its byte functions (`octet_length`, `convert_to`,
`chr`, `::bytea`) work in it. The answers are literals measured on a PRIVATE
PostgreSQL 15.19 cluster (`initdb --locale=C.UTF-8 -E UTF8`), because the
scenario creates a database, which the shared reference must not be given.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest


def _answer(c: Any, sql: str) -> Any:
    try:
        cur = c.execute(sql)
        return [tuple(r) for r in cur.fetchall()] if cur.description else cur.statusmessage
    except psycopg.Error as e:
        return ("ERR", e.sqlstate)


def latin1_database(port: int) -> list[Any]:
    dsn = f"host=127.0.0.1 port={port} user=postgres dbname="
    out: list[Any] = []
    with psycopg.connect(dsn + "postgres", autocommit=True) as c:
        _answer(c, "drop database if exists rp064_l1")
        out.append(
            _answer(c, "create database rp064_l1 template template0 encoding 'LATIN1' locale 'C'")
        )
    # The client says UTF8, so the conversion to LATIN1 is the server's.
    with psycopg.connect(dsn + "rp064_l1 client_encoding=utf8", autocommit=True) as d:
        out.append(d.info.parameter_status("server_encoding"))
        for sql in (
            "show server_encoding",
            "select '€'",
            "create table t (x text)",
            "insert into t values ('é')",
            "select x, length(x), octet_length(x), convert_to(x, 'LATIN1') from t",
            "select chr(8364)",
            "select 'é'::bytea",
        ):
            out.append(_answer(d, sql))
    with psycopg.connect(dsn + "rp064_l1", autocommit=True) as d:
        out.append(d.info.parameter_status("client_encoding"))
    with psycopg.connect(dsn + "postgres", autocommit=True) as c:
        _answer(c, "drop database if exists rp064_l1")
    return out


#: Measured on PostgreSQL 15.19, a private cluster, 2026-10-22.
EXPECTED: list[Any] = [
    "CREATE DATABASE",
    "LATIN1",
    [("LATIN1",)],
    ("ERR", "22P05"),
    "CREATE TABLE",
    "INSERT 0 1",
    [("é", 1, 1, b"\xe9")],
    ("ERR", "54000"),
    [(b"\xe9",)],
    "LATIN1",
]


@pytest.mark.backlog("RP-064")
def test_latin1_database_behaves_as_latin1(rust_pg: Any) -> None:
    assert latin1_database(rust_pg.port) == EXPECTED


if __name__ == "__main__":
    print(repr(latin1_database(5499)))
