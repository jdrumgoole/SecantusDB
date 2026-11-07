"""PL/pgSQL labels and the namespace they name.

RP-463 (fixed): a block's label qualifies the variables its DECLARE section
declares, and a FOR loop's label its loop variable, as `plpgsql_ns_lookup`
searches the namespace from the innermost level out. Found while working
it: RP-486 (fixed: a DO block with a label was a syntax error), RP-487
(fixed: a bare name read after an inner block or a FOR loop ends was the
inner level's variable) and RP-488 (an expression's error in a PL/pgSQL
statement carries no QUERY and no internal position). Found while working
RP-487: RP-491 (fixed: a block's variables were initialised once, when the
function started, not each time the block is entered).

An answer is the rows, or `(sqlstate, message)` for an error. The expected
answers were measured on a private, freshly initdb'd PostgreSQL 15.19
cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_BODIES = {
    # A block's label, and its declared variable.
    "lb1()": "<<blk>> declare x int := 3; begin return blk.x; end",
    # The outer block's x by its label; the inner one shadows the bare name.
    "lb2()": "<<o>> declare x int := 1; begin declare x int := 2;"
    " begin return o.x || ',' || x; end; end",
    "lb3()": "<<o>> declare x int := 1; begin <<i>> declare x int := 2;"
    " begin return o.x || ',' || i.x; end; end",
    # A record a block declares: a field of it, and the whole.
    "lb4()": "<<o>> declare r record; begin select 1 as f into r; return o.r.f || ',' || o.r; end",
    # A FOR loop's variable is in the loop's level, not the block's.
    "lb5()": "<<l>> begin for i in 1..2 loop return l.i; end loop; end",
    "lb6()": "begin <<l>> for i in 2..3 loop return l.i; end loop; end",
    # An ALIAS a block declares.
    "lb7(int)": "<<o>> declare y alias for $1; begin return o.y; end",
    # A default read in its block's namespace.
    "lb8()": "<<o>> declare x int := 5; y int := o.x + 1; begin return y; end",
    # An assignment through the label.
    "lb9()": "<<o>> declare x int := 1; begin o.x := 9; return o.x; end",
    # SQLSTATE is a level of its own, beyond the block's label.
    "lb10()": "<<o>> declare x int := 1; begin begin raise exception 'boom';"
    " exception when others then return o.sqlstate; end; end",
    # Two levels of one label: a name the inner one lacks is the outer's.
    "lb11()": "<<o>> declare x int := 1; begin <<o>> declare y int := 2;"
    " begin return o.x || ',' || o.y; end; end",
    # A handler reads its own block's variable by the label.
    "lb12()": "<<o>> declare x int := 4; begin perform 1/0; return '0';"
    " exception when others then return o.x; end",
    # A FOR loop's variable shadows the block's of its name.
    "lb13()": "<<o>> declare i int := 7; begin for i in 1..1 loop"
    " return o.i || ',' || i; end loop; end",
    # RP-487: after an inner block, the bare name is the outer variable.
    "lb20()": "<<o>> declare x int := 1; begin declare x int := 2; begin null; end; return x; end",
    "lb21()": "declare x int := 1; begin begin declare x int := 2;"
    " begin null; end; end; return x; end",
    # A handler outside a FOR loop reads the block's i, not the loop's.
    "lb22()": "declare i int := 7; begin begin for i in 1..1 loop"
    " perform 1/0; end loop; exception when others then return i; end; end",
    # A default reads its own level's names declared before it only.
    "lb23()": "declare x int := 1; begin declare y int := x; x int := 3;"
    " begin return y || ',' || x; end; end",
    # SQLSTATE after a nested handler ends is the outer handler's.
    "lb24()": "begin begin perform 1/0; exception when others then begin raise"
    " exception 'inner'; exception when others then null; end; return sqlstate; end; end",
    # A name only an ended block declared is no variable at all.
    "lb25()": "begin declare z int := 1; begin null; end; return z; end",
    # RP-491: a block's variables are initialised each time it is entered.
    "lb30()": "declare x int := 1; begin x := 5; declare y int := x; begin return y; end; end",
    "lb31()": "declare s text := ''; begin for i in 1..3 loop declare c int := 0;"
    " begin c := c + 1; s := s || c; end; end loop; return s; end",
    "lb32()": "declare s text := ''; begin for i in 1..2 loop declare c int;"
    " begin s := s || coalesce(c, -1); c := 9; end; end loop; return s; end",
    "lb33()": "declare s text := ''; begin for i in 1..2 loop declare c int := i * 10;"
    " begin s := s || c || ';'; end; end loop; return s; end",
    # A record a block declares is unassigned again on each entry.
    "lb34()": "declare s text := ''; begin for i in 1..2 loop declare r record;"
    " begin s := s || coalesce(r::text, 'N'); select i as f into r; end; end loop; return s; end",
    # A default reads its own level's earlier names and the outer level's.
    "lb35()": "declare a int := 2; b int := a + 1; begin declare a int := b * 10;"
    " c int := a + 1; begin return a || ',' || c; end; end",
    # A block with handlers, entered on each pass.
    "lb36()": "declare s text := ''; begin for i in 1..2 loop declare c int := i;"
    " begin s := s || c; exception when others then null; end; end loop; return s; end",
    # A default after an earlier block assigned what it reads.
    "lb37()": "declare x int := 1; begin begin x := 2; end; declare y int := x * 100;"
    " begin return y; end; end",
    # A default's error: its frame is the block's initialisation.
    "lb38()": "declare x int := 1/0; begin return x; end",
    "lb39()": "declare s text := ''; begin for i in 1..3 loop declare c int := 10 / (2 - i);"
    " begin s := s || c || ';'; end; end loop; return s; end",
}

_SETUP = [
    f"create or replace function {sig} returns text language plpgsql as $$ {body} $$"
    for sig, body in _BODIES.items()
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        for sig in _BODIES:
            c.execute(f"drop function if exists {sig}")


def answer(dsn: str, query: str) -> Any:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            return c.execute(query).fetchall()
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary)


BLOCK_LABEL = {
    "select lb1()": [("3",)],
    "select lb2()": [("1,2",)],
    "select lb3()": [("1,2",)],
    "select lb4()": [("1,(1)",)],
    "select lb5()": ("42P01", 'missing FROM-clause entry for table "l"'),
    "select lb6()": [("2",)],
    "select lb7(5)": [("5",)],
    "select lb8()": [("6",)],
    "select lb9()": [("9",)],
    "select lb10()": ("42P01", 'missing FROM-clause entry for table "o"'),
    "select lb11()": [("1,2",)],
    "select lb12()": [("4",)],
    "select lb13()": [("7,1",)],
}


@pytest.mark.backlog_fixed("RP-463")
@pytest.mark.parametrize("query", list(BLOCK_LABEL))
def test_label_qualifies_a_variable(query: str, dsn: str) -> None:
    """Was 42P01 `missing FROM-clause entry for table "blk"` for every
    label but the function's own name: the interpreter kept no namespace
    levels, so `blk.x` reached SQL as a column of a table `blk`."""
    assert answer(dsn, query) == BLOCK_LABEL[query]


INNER_BLOCK_ENDED = {
    "select lb20()": [("1",)],
    "select lb21()": [("1",)],
    "select lb22()": [("7",)],
    "select lb23()": [("1,3",)],
    "select lb24()": [("22012",)],
    "select lb25()": ("42703", 'column "z" does not exist'),
}


@pytest.mark.backlog_fixed("RP-487")
@pytest.mark.parametrize("query", list(INNER_BLOCK_ENDED))
def test_bare_name_after_inner_block(query: str, dsn: str) -> None:
    """Was the inner level's variable (2, NULL, 1, P0001, 1 in turn): the
    interpreter resolved a bare name against one flat list of datums, the
    last of that name winning, so a block's variable outlived the block.
    A bare name now goes through the levels being run, innermost first,
    then the function's own, as `plpgsql_ns_lookup` does."""
    assert answer(dsn, query) == INNER_BLOCK_ENDED[query]


BLOCK_ENTRY = {
    "select lb30()": [("5",)],
    "select lb31()": [("111",)],
    "select lb32()": [("-1-1",)],
    "select lb33()": [("10;20;",)],
    "select lb34()": [("NN",)],
    "select lb35()": [("30,31",)],
    "select lb36()": [("12",)],
    "select lb37()": [("200",)],
}


@pytest.mark.backlog_fixed("RP-491")
@pytest.mark.parametrize("query", list(BLOCK_ENTRY))
def test_block_initialises_its_variables_on_entry(query: str, dsn: str) -> None:
    """`exec_stmt_block` sets each variable the block declares to its
    default (or NULL) every time the block is entered. Every default was
    evaluated once, before the body ran: 1, 123, -19, and 42703 `column
    "i" does not exist` for a default reading a FOR loop's variable, and a
    block's record kept the last pass's row (`N(1)`)."""
    assert answer(dsn, query) == BLOCK_ENTRY[query]


def _context(dsn: str, query: str) -> Any:
    """An error's sqlstate, message and CONTEXT."""
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        try:
            return c.execute(query).fetchall()
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.context)


DEFAULT_ERROR = {
    "select lb38()": (
        "22012",
        "division by zero",
        'SQL expression "1/0"\nPL/pgSQL function lb38() line 1 during statement'
        " block local variable initialization",
    ),
    "select lb39()": (
        "22012",
        "division by zero",
        "PL/pgSQL function lb39() line 1 during statement block local variable initialization",
    ),
}


@pytest.mark.backlog_fixed("RP-491")
@pytest.mark.parametrize("query", list(DEFAULT_ERROR))
def test_default_error_names_the_block_initialisation(query: str, dsn: str) -> None:
    """A default's error carries `exec_stmt_block`'s `during statement block
    local variable initialization` frame. It carried no function frame,
    and a default reading a loop's variable was 42703, as every default
    ran before the body."""
    assert _context(dsn, query) == DEFAULT_ERROR[query]


def _do(dsn: str, body: str) -> Any:
    """A DO block's notices and outcome: `(notices, "DO")`, or `(notices,
    (sqlstate, message, statement position, internal query, internal
    position))` for an error."""
    with psycopg.connect(dsn, autocommit=True) as c:
        notices: list[str] = []
        c.add_notice_handler(lambda d: notices.append(d.message_primary or ""))
        try:
            c.execute(f"do $$ {body} $$")
        except psycopg.Error as e:
            d = e.diag
            return (
                notices,
                (
                    e.sqlstate,
                    d.message_primary,
                    d.statement_position,
                    d.internal_query,
                    d.internal_position,
                ),
            )
        return (notices, "DO")


DO_LABEL = {
    "<<blk>> declare x int := 3; begin raise notice 'a %', blk.x; end": (["a 3"], "DO"),
    "<<blk>> begin raise notice 'z'; end": (["z"], "DO"),
    "begin <<l>> for i in 1..2 loop raise notice 'f %', l.i; end loop; end": (
        ["f 1", "f 2"],
        "DO",
    ),
}


@pytest.mark.backlog_fixed("RP-486")
@pytest.mark.parametrize("body", list(DO_LABEL))
def test_do_block_with_a_label(body: str, dsn: str) -> None:
    """Was 42601 `syntax error at or near "<"`, or `at end of input` for a
    labelled loop: the DO block's own path read no `<<`, and answered a
    syntax error instead of handing the block to the function interpreter."""
    assert _do(dsn, body) == DO_LABEL[body]


EXPRESSION_ERROR = {
    "declare x int; begin raise notice '%', nosuch.x; end": (
        [],
        (
            "42P01",
            'missing FROM-clause entry for table "nosuch"',
            None,
            "nosuch.x",
            "1",
        ),
    ),
}


@pytest.mark.backlog("RP-488")
@pytest.mark.parametrize("body", list(EXPRESSION_ERROR))
def test_expression_error_carries_its_query(body: str, dsn: str) -> None:
    assert _do(dsn, body) == EXPRESSION_ERROR[body]
