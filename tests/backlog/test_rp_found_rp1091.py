"""Gap and regression tests for RP-1091 (a partition key's
COLLATE and operator class), each asserting PostgreSQL 15.19's answer
(the shared reference on port 5415)."""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _script(dsn: str, statements: list[str]) -> list[object]:
    """Each statement's status, rows or error."""
    out: list[object] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in statements:
            try:
                cur = c.execute(q)
            except psycopg.Error as e:
                out.append(("error", e.sqlstate, str(e).splitlines()[0]))
                continue
            out.append(cur.fetchall() if cur.description else cur.statusmessage)
    return out


@pytest.mark.backlog("RP-1228")
def test_a_range_key_routes_under_its_collation(dsn: str) -> None:
    """Under `partition by range (t collate "und-x-icu")`, `'A'` falls in
    `from ('a') to ('b')`, since ICU sorts `a < A < b`: PostgreSQL 15.19
    routes it there. Here the bound compares bytewise, and the row finds no
    partition (23514)."""
    assert _script(
        dsn,
        [
            "drop table if exists pr1091 cascade",
            'create table pr1091 (t text) partition by range (t collate "und-x-icu")',
            "create table pr1091a partition of pr1091 for values from ('a') to ('b')",
            "insert into pr1091 values ('A')",
            "select tableoid::regclass::text, t from pr1091",
            "drop table pr1091 cascade",
        ],
    ) == [
        "DROP TABLE",
        "CREATE TABLE",
        "CREATE TABLE",
        "INSERT 0 1",
        [("pr1091a", "A")],
        "DROP TABLE",
    ]


@pytest.mark.backlog("RP-1229")
def test_pg_get_partkeydef_quotes_a_column_key(dsn: str) -> None:
    """`pg_get_partkeydef` prints a column key through `quote_identifier`:
    `partition by list ("X")` is `LIST ("X")` on PostgreSQL 15.19. Here
    it is `LIST (X)`."""
    assert _script(
        dsn,
        [
            "drop table if exists pq1091",
            'create table pq1091 ("X" int) partition by list ("X")',
            "select pg_get_partkeydef('pq1091'::regclass)",
            "drop table pq1091",
        ],
    ) == ["DROP TABLE", "CREATE TABLE", [('LIST ("X")',)], "DROP TABLE"]


def _hinted(dsn: str, statements: list[str]) -> list[object]:
    """As `_script`, with each error's hint."""
    out: list[object] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in statements:
            try:
                cur = c.execute(q)
            except psycopg.Error as e:
                out.append(("error", e.sqlstate, str(e).splitlines()[0], e.diag.message_hint))
                continue
            out.append(cur.fetchall() if cur.description else cur.statusmessage)
    return out


_KEY = (
    "select pg_get_partkeydef('pk1091'::regclass), partclass::text, partcollation::text"
    " from pg_partitioned_table where partrelid = 'pk1091'::regclass"
)


@pytest.mark.backlog_fixed("RP-1091")
def test_a_partition_key_resolves_its_collation_and_opclass(dsn: str) -> None:
    """`ComputePartitionAttrs` resolves each key's COLLATE and operator
    class and keeps them in `pg_partitioned_table` (`partcollation`,
    `partclass`), and `pg_get_partkeydef` prints each where it is not the
    key's own; every answer measured on PostgreSQL 15.19. The server
    accepted any COLLATE and class, kept neither, and served both columns
    empty."""
    assert _hinted(
        dsn,
        [
            "drop table if exists pk1091",
            "create table pk1091 (x int, t text)"
            ' partition by range (t collate "C" text_pattern_ops, x)',
            _KEY,
            "drop table pk1091",
            'create table pk1091 (x int, t text) partition by range ((t collate "C"))',
            _KEY,
            "drop table pk1091",
            'create table pk1091 (x int, t text collate "C")'
            ' partition by list (t collate "default")',
            _KEY,
            "drop table pk1091",
            'create table pk1091 (x int, t text collate "C")'
            ' partition by range (upper(t) collate "C")',
            _KEY,
            "drop table pk1091",
            "create table pk1091 (x int, t text) partition by hash (t text_pattern_ops)",
            _KEY,
            "drop table pk1091",
            "create table pk1091 (x int, t varchar(5))"
            ' partition by list (t collate "POSIX" varchar_ops)',
            _KEY,
            "drop table pk1091",
            'create table pk1091 (x int, t text) partition by range (x collate "C")',
            'create table pk1091 (x int, t text) partition by range (t collate "nosuch")',
            "create table pk1091 (x int, t text) partition by range (x text_pattern_ops)",
            "create table pk1091 (x int, t text) partition by hash (t nosuch_ops)",
            "create table pk1091 (x int, j json) partition by range (j)",
            "create table pk1091 (x int, j json) partition by hash (j)",
            'create table pk1091 (x int, t text collate "C", u text collate "POSIX")'
            ' partition by range ((t || u) collate "C")',
        ],
    ) == [
        "DROP TABLE",
        "CREATE TABLE",
        [('RANGE (t COLLATE "C" text_pattern_ops, x)', "4217 1978", "950 0")],
        "DROP TABLE",
        "CREATE TABLE",
        [('RANGE (t COLLATE "C")', "3126", "950")],
        "DROP TABLE",
        "CREATE TABLE",
        [('LIST (t COLLATE "default")', "3126", "100")],
        "DROP TABLE",
        "CREATE TABLE",
        [("RANGE (upper(t))", "3126", "950")],
        "DROP TABLE",
        "CREATE TABLE",
        [("HASH (t text_pattern_ops)", "10056", "100")],
        "DROP TABLE",
        "CREATE TABLE",
        [('LIST (t COLLATE "POSIX" varchar_ops)', "10044", "951")],
        "DROP TABLE",
        ("error", "42804", "collations are not supported by type integer", None),
        ("error", "42704", 'collation "nosuch" for encoding "UTF8" does not exist', None),
        (
            "error",
            "42804",
            'operator class "text_pattern_ops" does not accept data type integer',
            None,
        ),
        (
            "error",
            "42704",
            'operator class "nosuch_ops" does not exist for access method "hash"',
            None,
        ),
        (
            "error",
            "42704",
            'data type json has no default operator class for access method "btree"',
            "You must specify a btree operator class or define a default btree operator"
            " class for the data type.",
        ),
        (
            "error",
            "42704",
            'data type json has no default operator class for access method "hash"',
            "You must specify a hash operator class or define a default hash operator"
            " class for the data type.",
        ),
        (
            "error",
            "42P16",
            "no collation was derived for partition key column 1 with collatable type text",
            "Use the COLLATE clause to set the collation explicitly.",
        ),
    ]
