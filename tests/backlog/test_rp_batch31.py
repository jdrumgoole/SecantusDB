"""Batch 31 of the Rust PG server (2026-10-23): the regression tests of
what it fixed -- RP-072's `WHERE CURRENT OF`, RP-143, two schemas'
routines of one name, and RP-145 .. RP-148, found while fixing it -- and
the gap tests of what it filed open (RP-149, RP-150, RP-154, and RP-137's
DROP SCHEMA order). RP-151 .. RP-153 and RP-155 were found and fixed here
too.

As in `test_rp_batch30.py`: each scenario is a list of statements run in
one autocommit connection, and a statement's answer is its rows, its
command tag, or `("ERR", sqlstate)`. A statement prefixed `!` reports its
error's DETAIL too; one prefixed `~` reports its answer with the notices
it raised (message, then DETAIL). The expected answers were measured on a
private, freshly initdb'd PostgreSQL 15.19 cluster by running this file as
a script:

    PYTHONPATH=. python tests/backlog/test_rp_batch31.py "host=127.0.0.1 port=5499 user=postgres"

which prints `EXPECTED` for pasting back. Every scenario drops what it made.
"""

from __future__ import annotations

import sys
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _answer, _dsn, _marked
from tests.backlog.test_rp_batch30 import _answer_detail


def run_scenario(dsn: str, statements: list[str]) -> list[Any]:
    """Each statement's answer, with an error's DETAIL (`!`) or the
    statement's notices (`~`) where it asks."""
    notices: list[str] = []
    with psycopg.connect(dsn, autocommit=True) as conn:
        conn.add_notice_handler(
            lambda d: notices.append(
                d.message_primary
                + (f"\n{d.message_detail}" if d.message_detail is not None else "")
            )
        )
        out: list[Any] = []
        for s in statements:
            if s.startswith("!"):
                out.append(_answer_detail(conn, s[1:]))
            elif s.startswith("~"):
                notices.clear()
                answer = _answer(conn, s[1:])
                out.append((answer, list(notices)))
            else:
                out.append(_answer(conn, s))
        if conn.info.transaction_status != psycopg.pq.TransactionStatus.IDLE:
            conn.execute("rollback")
        return out


# (item, open?, statements). `open` scenarios are gap tests: they fail while
# their item is open.
SCENARIOS: dict[str, tuple[str, bool, list[str]]] = {
    # ------------------------------------- RP-143 routines and their schemas
    "a call finds its routine by schema and search_path": (
        "RP-143",
        False,
        [
            "create schema rp31_a",
            "create schema rp31_b",
            "create function rp31_a.h(int) returns int language sql as 'select 1'",
            "create function rp31_b.h(int) returns int language sql as 'select 2'",
            "create or replace function rp31_b.h(int) returns int language sql as 'select 22'",
            "select rp31_a.h(1), rp31_b.h(1)",
            "set search_path = rp31_b, public",
            "select h(1)",
            "set search_path = rp31_a, rp31_b, public",
            "select h(1)",
            "reset search_path",
            "select h(1)",
            "select n.nspname, p.proname from pg_proc p join pg_namespace n "
            "on n.oid = p.pronamespace where p.proname = 'h' order by 1",
            "drop schema rp31_a, rp31_b cascade",
        ],
    ),
    "DDL on a routine names it by schema": (
        "RP-143",
        False,
        [
            "create schema rp31_c",
            "create schema rp31_d",
            "create role rp31_g",
            "create function rp31_c.h() returns int language sql as 'select 1'",
            "create function rp31_d.h() returns int language sql as 'select 2'",
            "grant execute on function rp31_d.h() to rp31_g",
            "select n.nspname, p.proacl::text from pg_proc p join pg_namespace n "
            "on n.oid = p.pronamespace where p.proname = 'h' order by 1",
            "revoke execute on function rp31_d.h() from rp31_g",
            "drop function rp31_c.h()",
            "select rp31_d.h()",
            "alter function rp31_d.h() set schema rp31_c",
            "select rp31_c.h()",
            "create function rp31_d.h() returns int language sql as 'select 3'",
            "alter function rp31_c.h() set schema rp31_d",
            "alter function rp31_c.h() rename to h2",
            "select rp31_c.h2(), rp31_d.h()",
            "create procedure rp31_c.pr() language sql as 'select 1'",
            "call rp31_c.pr()",
            "select rp31_c.pr()",
            "call pr()",
            "drop schema rp31_c, rp31_d cascade",
            "drop role rp31_g",
        ],
    ),
    "an unqualified CREATE FUNCTION lands on the search_path": (
        "RP-143",
        False,
        [
            "create schema rp31_e",
            "set search_path = rp31_e, public",
            "create function rp31_k() returns int language sql as 'select 5'",
            "select n.nspname from pg_proc p join pg_namespace n "
            "on n.oid = p.pronamespace where p.proname = 'rp31_k'",
            "set search_path = rp31_nosuch",
            "create function rp31_k2() returns int language sql as 'select 5'",
            "reset search_path",
            "drop schema rp31_e cascade",
        ],
    ),
    "a routine named in a schema that does not exist": (
        "RP-143",
        False,
        [
            "create function rp31_m(int) returns int language sql as 'select 1'",
            "drop function rp31_nosuch.rp31_m(int)",
            "~drop function if exists rp31_nosuch.rp31_m(int)",
            "select rp31_m(1)",
            "grant execute on function rp31_nosuch.rp31_m(int) to public",
            "alter function rp31_nosuch.rp31_m(int) rename to j",
            "call rp31_nosuch.p()",
            "create function rp31_nosuch.f() returns int language sql as 'select 1'",
            "drop function rp31_m(int)",
        ],
    ),
    # ------------------------------------------ RP-145 DROP SCHEMA a, b
    "DROP SCHEMA of several schemas is one deletion": (
        "RP-145",
        False,
        [
            "create schema rp31_f",
            "create schema rp31_h",
            "create table rp31_f.t (a int)",
            "create table rp31_h.t (a int)",
            "!drop schema rp31_f, rp31_h",
            "drop schema rp31_f, rp31_nosuch cascade",
            "~drop schema if exists rp31_f, rp31_nosuch, rp31_h cascade",
        ],
    ),
    "DROP SCHEMA names what its members' drops reach": (
        "RP-145",
        False,
        [
            "create schema rp31_i",
            "create table rp31_i.t (a int primary key)",
            "create index on rp31_i.t (a)",
            "create view rp31_iv as select * from rp31_i.t",
            "~drop schema rp31_i cascade",
            "select count(*) from pg_class where relname = 'rp31_iv'",
        ],
    ),
    # --------------------------------- RP-146 a schema made in the open block
    "DROP SCHEMA of a schema made in the open block": (
        "RP-146",
        False,
        [
            "begin",
            "create schema rp31_j",
            "drop schema rp31_j",
            "select count(*) from pg_namespace where nspname = 'rp31_j'",
            "rollback",
            "create schema rp31_j",
            "begin",
            "drop schema rp31_j",
            "rollback",
            "select count(*) from pg_namespace where nspname = 'rp31_j'",
            "drop schema rp31_j",
        ],
    ),
    # ---------------------------------- RP-147 the IF EXISTS notice's types
    "DROP FUNCTION IF EXISTS names the types as written": (
        "RP-147",
        False,
        [
            "~drop function if exists rp31_nosuchfn(int, varchar(3)[], text)",
            "~drop procedure if exists public.rp31_nosuchfn(int)",
            "drop function rp31_nosuchfn(int)",
        ],
    ),
    # ------------------------------------- RP-148 a procedure is a function
    "a procedure in DROP SCHEMA's notice": (
        "RP-148",
        False,
        [
            "create schema rp31_l",
            "create procedure rp31_l.pr() language sql as 'select 1'",
            "!drop schema rp31_l",
            "~drop schema rp31_l cascade",
        ],
    ),
    # ------------------------------------------ RP-072 WHERE CURRENT OF
    "UPDATE and DELETE WHERE CURRENT OF": (
        "RP-072",
        False,
        [
            "create table rp31_w (a int, b text)",
            "create table rp31_w2 (a int)",
            "insert into rp31_w values (1, 'x'), (2, 'y'), (3, 'z')",
            "begin",
            "declare rp31_c cursor for select * from rp31_w where a > 1",
            "fetch rp31_c",
            "update rp31_w set b = 'u' where current of rp31_c",
            "fetch rp31_c",
            "delete from rp31_w where current of rp31_c returning *",
            "delete from rp31_w where current of rp31_c",
            "select * from rp31_w order by a",
            "fetch rp31_c",
            "update rp31_w set b = 'q' where current of rp31_c",
            "rollback",
            "begin",
            "declare rp31_c cursor for select * from rp31_w order by a desc for update",
            "fetch rp31_c",
            "update rp31_w set b = 'f' where current of rp31_c returning *",
            "rollback",
            "drop table rp31_w, rp31_w2",
        ],
    ),
    "WHERE CURRENT OF a cursor it cannot use": (
        "RP-072",
        False,
        [
            "create table rp31_w (a int, b text)",
            "create table rp31_w2 (a int)",
            "insert into rp31_w values (1, 'x'), (2, 'y')",
            "begin",
            "update rp31_w set a = 9 where current of rp31_nosuch",
            "rollback",
            "begin",
            "declare rp31_c cursor for select * from rp31_w",
            "fetch rp31_c",
            "update rp31_w2 set a = 9 where current of rp31_c",
            "rollback",
            "begin",
            "declare rp31_c cursor for select distinct a from rp31_w",
            "fetch rp31_c",
            "update rp31_w set a = 9 where current of rp31_c",
            "rollback",
            "begin",
            "declare rp31_c cursor for select * from rp31_w order by a desc limit 1",
            "fetch rp31_c",
            "update rp31_w set a = 9 where current of rp31_c",
            "rollback",
            "begin",
            "declare rp31_c cursor for select count(*) from rp31_w",
            "fetch rp31_c",
            "update rp31_w set a = 9 where current of rp31_c",
            "rollback",
            "begin",
            "declare rp31_c cursor with hold for select * from rp31_w",
            "commit",
            "fetch rp31_c",
            "update rp31_w set a = 9 where current of rp31_c",
            "close rp31_c",
            "drop table rp31_w, rp31_w2",
        ],
    ),
    "WHERE CURRENT OF a cursor ordered by an index": (
        "RP-072",
        False,
        [
            "create table rp31_y (a int primary key, b text, c int)",
            "create index rp31_yc on rp31_y (c desc)",
            "insert into rp31_y values (1, 'x', 5), (2, 'y', 6)",
            "begin",
            "declare rp31_c1 cursor for select * from rp31_y order by a",
            "fetch rp31_c1",
            "update rp31_y set b = 'a1' where current of rp31_c1",
            "declare rp31_c2 cursor for select * from rp31_y order by a desc",
            "fetch rp31_c2",
            "update rp31_y set b = 'a2' where current of rp31_c2",
            "declare rp31_c3 cursor for select * from rp31_y order by c",
            "fetch rp31_c3",
            "update rp31_y set b = 'c3' where current of rp31_c3",
            "select * from rp31_y order by a",
            "declare rp31_c4 cursor for select * from rp31_y order by b",
            "fetch rp31_c4",
            "update rp31_y set b = 'c4' where current of rp31_c4",
            "rollback",
            "begin",
            "declare rp31_c5 cursor for select * from rp31_y order by a nulls first",
            "fetch rp31_c5",
            "update rp31_y set b = 'c5' where current of rp31_c5",
            "rollback",
            "drop table rp31_y",
        ],
    ),
    "FETCH and MOVE in PL/pgSQL take SQL's steps": (
        "RP-151",
        False,
        [
            "create table rp31_x (a int, b text)",
            "insert into rp31_x values (1, 'x'), (2, 'y'), (3, 'z')",
            "~do $$ declare c scroll cursor for select a from rp31_x order by a; v int; n int; "
            "begin open c; fetch last from c into v; raise notice 'last %', v; "
            "fetch prior from c into v; raise notice 'prior %', v; "
            "fetch absolute 1 from c into v; raise notice 'abs %', v; "
            "fetch relative 5 from c into v; raise notice 'rel % found %', v, found; "
            "move first from c; get diagnostics n = row_count; "
            "raise notice 'moved % found %', n, found; "
            "move forward all from c; get diagnostics n = row_count; "
            "raise notice 'all % found %', n, found; close c; end $$",
            "do $$ declare c refcursor; v int; begin fetch c into v; end $$",
            "~do $$ declare c refcursor; v int; begin open c for select a from rp31_x order by a; "
            "fetch c into v; raise notice 'v % c %', v, c; close c; fetch c into v; end $$",
            "~do $$ declare c cursor for select a from rp31_x order by a; v int; begin open c; "
            "loop fetch c into v; exit when not found; raise notice 'loop %', v; end loop; "
            "close c; end $$",
            "do $$ declare c no scroll cursor for select a from rp31_x order by a; v int; "
            "begin open c; fetch c into v; fetch prior from c into v; end $$",
            "do $$ declare c refcursor := 'rp31_mine'; v int; begin open c for select a from "
            "rp31_x where a = 2; fetch c into v; update rp31_x set b = 'rc' where current of c; "
            "close c; end $$",
            "select * from rp31_x order by a",
            "drop table rp31_x",
        ],
    ),
    # ---------------------------------------- RP-152 RAISE's boolean text
    "RAISE writes a boolean as its output function does": (
        "RP-152",
        False,
        [
            "~do $$ declare b boolean := true; begin raise notice '% % %', b, false, found; end $$",
        ],
    ),
    # --------------------------- RP-153 / RP-155 a record with no row yet
    "an INTO with no row leaves a record of NULLs": (
        "RP-153",
        False,
        [
            "create table rp31_r (a int, b text)",
            "~do $$ declare r record; begin select * into r from rp31_r; "
            "raise notice 'r % %', r.a, r.b; end $$",
            "drop table rp31_r",
        ],
    ),
    "a field of a record nothing has assigned": (
        "RP-155",
        False,
        [
            "!do $$ declare r record; begin raise notice 'r %', r.a; end $$",
            "!do $$ declare r record; x int; begin x := r.a; end $$",
        ],
    ),
    # ---------------------------------------------------- open: filed here
    "a whole record in a PL/pgSQL expression": (
        "RP-154",
        True,
        [
            "create table rp31_r (a int, b text)",
            "insert into rp31_r values (1, 'q')",
            "~do $$ declare r record; begin select * into r from rp31_r; "
            "raise notice 'r %', r; end $$",
            "~do $$ declare r record; begin select * into r from rp31_r where false; "
            "raise notice 'r %', r; end $$",
            "drop table rp31_r",
        ],
    ),
    "FETCH and CLOSE a cursor in PL/pgSQL": (
        "RP-151",
        False,
        [
            "create table rp31_x (a int, b text)",
            "insert into rp31_x values (1, 'x'), (2, 'y')",
            "do $$ declare c cursor for select * from rp31_x order by a for update; r record; "
            "begin open c; fetch c into r; update rp31_x set b = 'pl' where current of c; "
            "move next from c; fetch c into r; raise notice 'r %', r.a; close c; end $$",
            "select * from rp31_x order by a",
            "drop table rp31_x",
        ],
    ),
    "a built-in reached through a schema": (
        "RP-149",
        True,
        [
            "select public.lower('A')",
            "create schema rp31_n",
            "select rp31_n.lower('A')",
            "select pg_catalog.lower('A')",
            "drop schema rp31_n",
        ],
    ),
    "DROP SCHEMA's refusal names what depends on its members": (
        "RP-150",
        True,
        [
            "create schema rp31_o",
            "create table rp31_o.t (a int)",
            "create view rp31_ov as select * from rp31_o.t",
            "!drop schema rp31_o",
            "drop schema rp31_o cascade",
        ],
    ),
    "DROP SCHEMA lists its members in creation order": (
        "RP-137",
        True,
        [
            "create schema rp31_p",
            "create table rp31_p.t (a int)",
            "create function rp31_p.f() returns int language sql as 'select 1'",
            "create view rp31_p.v as select * from rp31_p.t",
            "~drop schema rp31_p cascade",
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
    "DDL on a routine names it by schema": [
        "CREATE SCHEMA",
        "CREATE SCHEMA",
        "CREATE ROLE",
        "CREATE FUNCTION",
        "CREATE FUNCTION",
        "GRANT",
        [("rp31_c", "None"), ("rp31_d", "{=X/postgres,postgres=X/postgres,rp31_g=X/postgres}")],
        "REVOKE",
        "DROP FUNCTION",
        [("2",)],
        "ALTER FUNCTION",
        [("2",)],
        "CREATE FUNCTION",
        ("ERR", "42723"),
        "ALTER FUNCTION",
        [("2", "3")],
        "CREATE PROCEDURE",
        "CALL",
        ("ERR", "42809"),
        ("ERR", "42883"),
        "DROP SCHEMA",
        "DROP ROLE",
    ],
    "DROP FUNCTION IF EXISTS names the types as written": [
        (
            "DROP FUNCTION",
            [
                "function "
                "rp31_nosuchfn(pg_catalog.int4,pg_catalog.varchar[],text) "
                "does not exist, skipping"
            ],
        ),
        (
            "DROP PROCEDURE",
            ["procedure public.rp31_nosuchfn(pg_catalog.int4) does not exist, skipping"],
        ),
        ("ERR", "42883"),
    ],
    "DROP SCHEMA lists its members in creation order": [
        "CREATE SCHEMA",
        "CREATE TABLE",
        "CREATE FUNCTION",
        "CREATE VIEW",
        (
            "DROP SCHEMA",
            [
                "drop cascades to 3 other objects\n"
                "drop cascades to table rp31_p.t\n"
                "drop cascades to function rp31_p.f()\n"
                "drop cascades to view rp31_p.v"
            ],
        ),
    ],
    "DROP SCHEMA names what its members' drops reach": [
        "CREATE SCHEMA",
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE VIEW",
        (
            "DROP SCHEMA",
            [
                "drop cascades to 2 other objects\n"
                "drop cascades to table rp31_i.t\n"
                "drop cascades to view rp31_iv"
            ],
        ),
        [("0",)],
    ],
    "DROP SCHEMA of a schema made in the open block": [
        "BEGIN",
        "CREATE SCHEMA",
        "DROP SCHEMA",
        [("0",)],
        "ROLLBACK",
        "CREATE SCHEMA",
        "BEGIN",
        "DROP SCHEMA",
        "ROLLBACK",
        [("1",)],
        "DROP SCHEMA",
    ],
    "DROP SCHEMA of several schemas is one deletion": [
        "CREATE SCHEMA",
        "CREATE SCHEMA",
        "CREATE TABLE",
        "CREATE TABLE",
        (
            "ERR",
            "2BP01",
            "table rp31_h.t depends on schema rp31_h\ntable rp31_f.t depends on schema rp31_f",
        ),
        ("ERR", "3F000"),
        (
            "DROP SCHEMA",
            [
                'schema "rp31_nosuch" does not exist, skipping',
                "drop cascades to 2 other objects\n"
                "drop cascades to table rp31_h.t\n"
                "drop cascades to table rp31_f.t",
            ],
        ),
    ],
    "DROP SCHEMA's refusal names what depends on its members": [
        "CREATE SCHEMA",
        "CREATE TABLE",
        "CREATE VIEW",
        (
            "ERR",
            "2BP01",
            "table rp31_o.t depends on schema rp31_o\nview rp31_ov depends on table rp31_o.t",
        ),
        "DROP SCHEMA",
    ],
    "FETCH and CLOSE a cursor in PL/pgSQL": [
        "CREATE TABLE",
        "INSERT 0 2",
        "DO",
        [("1", "pl"), ("2", "y")],
        "DROP TABLE",
    ],
    "FETCH and MOVE in PL/pgSQL take SQL's steps": [
        "CREATE TABLE",
        "INSERT 0 3",
        (
            "DO",
            [
                "last 3",
                "prior 2",
                "abs 1",
                "rel <NULL> found f",
                "moved 1 found t",
                "all 2 found t",
            ],
        ),
        ("ERR", "22004"),
        (("ERR", "34000"), ["v 1 c <unnamed portal 1>"]),
        ("DO", ["loop 1", "loop 2", "loop 3"]),
        ("ERR", "55000"),
        "DO",
        [("1", "x"), ("2", "rc"), ("3", "z")],
        "DROP TABLE",
    ],
    "RAISE writes a boolean as its output function does": [("DO", ["t f f"])],
    "UPDATE and DELETE WHERE CURRENT OF": [
        "CREATE TABLE",
        "CREATE TABLE",
        "INSERT 0 3",
        "BEGIN",
        "DECLARE CURSOR",
        [("2", "y")],
        "UPDATE 1",
        [("3", "z")],
        [("3", "z")],
        "DELETE 0",
        [("1", "x"), ("2", "u")],
        [],
        ("ERR", "24000"),
        "ROLLBACK",
        "BEGIN",
        "DECLARE CURSOR",
        [("3", "z")],
        [("3", "f")],
        "ROLLBACK",
        "DROP TABLE",
    ],
    "WHERE CURRENT OF a cursor it cannot use": [
        "CREATE TABLE",
        "CREATE TABLE",
        "INSERT 0 2",
        "BEGIN",
        ("ERR", "34000"),
        "ROLLBACK",
        "BEGIN",
        "DECLARE CURSOR",
        [("1", "x")],
        ("ERR", "24000"),
        "ROLLBACK",
        "BEGIN",
        "DECLARE CURSOR",
        [("2",)],
        ("ERR", "24000"),
        "ROLLBACK",
        "BEGIN",
        "DECLARE CURSOR",
        [("2", "y")],
        ("ERR", "24000"),
        "ROLLBACK",
        "BEGIN",
        "DECLARE CURSOR",
        [("2",)],
        ("ERR", "24000"),
        "ROLLBACK",
        "BEGIN",
        "DECLARE CURSOR",
        "COMMIT",
        [("1", "x")],
        ("ERR", "24000"),
        "CLOSE CURSOR",
        "DROP TABLE",
    ],
    "WHERE CURRENT OF a cursor ordered by an index": [
        "CREATE TABLE",
        "CREATE INDEX",
        "INSERT 0 2",
        "BEGIN",
        "DECLARE CURSOR",
        [("1", "x", "5")],
        "UPDATE 1",
        "DECLARE CURSOR",
        [("2", "y", "6")],
        "UPDATE 1",
        "DECLARE CURSOR",
        [("1", "a1", "5")],
        "UPDATE 1",
        [("1", "c3", "5"), ("2", "a2", "6")],
        "DECLARE CURSOR",
        [("2", "a2", "6")],
        ("ERR", "24000"),
        "ROLLBACK",
        "BEGIN",
        "DECLARE CURSOR",
        [("1", "x", "5")],
        ("ERR", "24000"),
        "ROLLBACK",
        "DROP TABLE",
    ],
    "a built-in reached through a schema": [
        ("ERR", "42883"),
        "CREATE SCHEMA",
        ("ERR", "42883"),
        [("a",)],
        "DROP SCHEMA",
    ],
    "a call finds its routine by schema and search_path": [
        "CREATE SCHEMA",
        "CREATE SCHEMA",
        "CREATE FUNCTION",
        "CREATE FUNCTION",
        "CREATE FUNCTION",
        [("1", "22")],
        "SET",
        [("22",)],
        "SET",
        [("1",)],
        "RESET",
        ("ERR", "42883"),
        [("rp31_a", "h"), ("rp31_b", "h")],
        "DROP SCHEMA",
    ],
    "a field of a record nothing has assigned": [
        ("ERR", "55000", "The tuple structure of a not-yet-assigned record is indeterminate."),
        ("ERR", "55000", "The tuple structure of a not-yet-assigned record is indeterminate."),
    ],
    "a procedure in DROP SCHEMA's notice": [
        "CREATE SCHEMA",
        "CREATE PROCEDURE",
        ("ERR", "2BP01", "function rp31_l.pr() depends on schema rp31_l"),
        ("DROP SCHEMA", ["drop cascades to function rp31_l.pr()"]),
    ],
    "a routine named in a schema that does not exist": [
        "CREATE FUNCTION",
        ("ERR", "3F000"),
        ("DROP FUNCTION", ['schema "rp31_nosuch" does not exist, skipping']),
        [("1",)],
        ("ERR", "3F000"),
        ("ERR", "3F000"),
        ("ERR", "3F000"),
        ("ERR", "3F000"),
        "DROP FUNCTION",
    ],
    "a whole record in a PL/pgSQL expression": [
        "CREATE TABLE",
        "INSERT 0 1",
        ("DO", ["r (1,q)"]),
        ("DO", ["r (,)"]),
        "DROP TABLE",
    ],
    "an INTO with no row leaves a record of NULLs": [
        "CREATE TABLE",
        ("DO", ["r <NULL> <NULL>"]),
        "DROP TABLE",
    ],
    "an unqualified CREATE FUNCTION lands on the search_path": [
        "CREATE SCHEMA",
        "SET",
        "CREATE FUNCTION",
        [("rp31_e",)],
        "SET",
        ("ERR", "3F000"),
        "RESET",
        "DROP SCHEMA",
    ],
}


if __name__ == "__main__":
    import pprint

    dsn = sys.argv[1]
    measured = {
        name: run_scenario(dsn + " dbname=postgres", stmts)
        for name, (_, _, stmts) in SCENARIOS.items()
    }
    print("EXPECTED: dict[str, Any] = " + pprint.pformat(measured, width=100, sort_dicts=True))
