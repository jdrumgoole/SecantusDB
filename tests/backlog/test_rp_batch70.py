"""Batch 70 of the Rust PG server (2026-10-25): a database's encoding is
its sessions' server encoding (RP-064), in three encodings: LATIN1, WIN1252
and the multi-byte EUC_JP.

The answers are literals measured on a PRIVATE PostgreSQL 15.19 cluster
(`initdb --locale=C.UTF-8 -E UTF8`), because the scenario creates databases,
which the shared reference must not be given:

    PYTHONPATH=. python -m tests.backlog.test_rp_batch70 5499
"""

from __future__ import annotations

import sys
from typing import Any

import psycopg
import pytest

_DATABASES = [("rp70_l1", "LATIN1"), ("rp70_w", "WIN1252"), ("rp70_e", "EUC_JP")]
_QUERIES = [
    "select '€'",
    "select 'ж'",
    "select 'é' as x, octet_length('é'), bit_length('é'), md5('é'), ascii('é'), 'é'::bytea",
    "select chr(233), chr(128), chr(255), chr(127)",
    "select chr(300)",
    "select chr(0)",
    "select convert_from('\\xe282ac'::bytea, 'UTF8')",
    "select convert_to('é', 'UTF8')",
    "select length('日本'), octet_length('日本')",
    "select ascii('日')",
    "show client_encoding",
    "show server_encoding",
    "select pg_column_size('ééééé'::text)",
    "select pg_encoding_to_char(encoding) from pg_database where datname = current_database()",
]


def _answer(c: Any, sql: str, params: Any = None) -> Any:
    try:
        cur = c.execute(sql, params)
        return [tuple(r) for r in cur.fetchall()] if cur.description else cur.statusmessage
    except psycopg.Error as e:
        return ("ERR", e.sqlstate, e.diag.message_primary)


def encodings(port: int) -> list[Any]:
    dsn = f"host=127.0.0.1 port={port} user=postgres dbname="
    out: list[Any] = []
    with psycopg.connect(dsn + "postgres", autocommit=True) as c:
        for name, enc in _DATABASES:
            _answer(c, f"drop database if exists {name}")
            _answer(c, f"create database {name} template template0 encoding '{enc}' locale 'C'")
    for name, _ in _DATABASES:
        with psycopg.connect(dsn + name + " client_encoding=utf8", autocommit=True) as d:
            out.extend(_answer(d, q) for q in _QUERIES)
            # A bound parameter and COPY's data are converted as query text is.
            out.append(_answer(d, "select %s::text", ("€",)))
            out.append(_answer(d, "create table t (x text) with (autovacuum_enabled = false)"))
            try:
                with d.cursor().copy("copy t from stdin") as copy:
                    copy.write("a\n€\n")
                out.append("COPY")
            except psycopg.Error as e:
                out.append(("ERR", e.sqlstate, e.diag.message_primary))
            # A tuple holds the text's bytes in the server encoding.
            out.append(
                _answer(d, "insert into t select repeat('é', 100) from generate_series(1, 300)")
            )
            out.append(_answer(d, "select pg_relation_size('t') / 8192"))
    with psycopg.connect(dsn + "postgres", autocommit=True) as c:
        for name, _ in _DATABASES:
            _answer(c, f"drop database if exists {name}")
    return out


#: Measured on PostgreSQL 15.19, a private cluster, 2026-10-25.
EXPECTED: list[Any] = [
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xe2 0x82 0xac in encoding "UTF8" has no equivalent in '
        'encoding "LATIN1"',
    ),
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xd0 0xb6 in encoding "UTF8" has no equivalent in '
        'encoding "LATIN1"',
    ),
    [("é", 1, 8, "3406877694691ddd1dfb0aca54681407", 233, b"\xe9")],
    [("é", "\x80", "ÿ", "\x7f")],
    ("ERR", "54000", "requested character too large for encoding: 300"),
    ("ERR", "54000", "null character not permitted"),
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xe2 0x82 0xac in encoding "UTF8" has no equivalent in '
        'encoding "LATIN1"',
    ),
    [(b"\xc3\xa9",)],
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xe6 0x97 0xa5 in encoding "UTF8" has no equivalent in '
        'encoding "LATIN1"',
    ),
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xe6 0x97 0xa5 in encoding "UTF8" has no equivalent in '
        'encoding "LATIN1"',
    ),
    [("UTF8",)],
    [("LATIN1",)],
    [(9,)],
    [("LATIN1",)],
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xe2 0x82 0xac in encoding "UTF8" has no equivalent in '
        'encoding "LATIN1"',
    ),
    "CREATE TABLE",
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xe2 0x82 0xac in encoding "UTF8" has no equivalent in '
        'encoding "LATIN1"',
    ),
    "INSERT 0 300",
    [(5,)],
    [("€",)],
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xd0 0xb6 in encoding "UTF8" has no equivalent in '
        'encoding "WIN1252"',
    ),
    [("é", 1, 8, "3406877694691ddd1dfb0aca54681407", 233, b"\xe9")],
    [("é", "€", "ÿ", "\x7f")],
    ("ERR", "54000", "requested character too large for encoding: 300"),
    ("ERR", "54000", "null character not permitted"),
    [("€",)],
    [(b"\xc3\xa9",)],
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xe6 0x97 0xa5 in encoding "UTF8" has no equivalent in '
        'encoding "WIN1252"',
    ),
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xe6 0x97 0xa5 in encoding "UTF8" has no equivalent in '
        'encoding "WIN1252"',
    ),
    [("UTF8",)],
    [("WIN1252",)],
    [(9,)],
    [("WIN1252",)],
    [("€",)],
    "CREATE TABLE",
    "COPY",
    "INSERT 0 300",
    [(5,)],
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xe2 0x82 0xac in encoding "UTF8" has no equivalent in '
        'encoding "EUC_JP"',
    ),
    [("ж",)],
    ("ERR", "54000", "requested character too large"),
    ("ERR", "54000", "requested character too large for encoding: 233"),
    ("ERR", "54000", "requested character too large for encoding: 300"),
    ("ERR", "54000", "null character not permitted"),
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xe2 0x82 0xac in encoding "UTF8" has no equivalent in '
        'encoding "EUC_JP"',
    ),
    [(b"\xc3\xa9",)],
    [(2, 4)],
    ("ERR", "54000", "requested character too large"),
    [("UTF8",)],
    [("EUC_JP",)],
    [(19,)],
    [("EUC_JP",)],
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xe2 0x82 0xac in encoding "UTF8" has no equivalent in '
        'encoding "EUC_JP"',
    ),
    "CREATE TABLE",
    (
        "ERR",
        "22P05",
        'character with byte sequence 0xe2 0x82 0xac in encoding "UTF8" has no equivalent in '
        'encoding "EUC_JP"',
    ),
    "INSERT 0 300",
    [(13,)],
]


@pytest.mark.backlog_fixed("RP-064")
def test_each_encoding_is_its_databases_server_encoding(rust_pg: Any) -> None:
    assert encodings(rust_pg.port) == EXPECTED


if __name__ == "__main__":
    print(repr(encodings(int(sys.argv[1]))))
