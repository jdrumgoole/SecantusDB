"""Gap tests of divergences found while working RP-694 (`SELECT ... INTO` a
scalar variable stored the column's value uncoerced):

- an assignment whose types have only an EXPLICIT cast between them (`int`
  from `bool`, `bool` from `int`) used that cast, where PostgreSQL's
  PL/pgSQL coercion (`COERCION_PLPGSQL`) converts through the types' text
  I/O and refuses `t` as an integer (RP-712, fixed: such an assignment now
  goes through the source's output function and the target's input);
- a `FOR` over a query, or an `EXECUTE ... INTO`, whose target is a scalar
  variable or a list of them was refused 0A000 (RP-713, fixed: the
  anonymous row the grammar makes of the list is filled column by column,
  as a plain `INTO`'s is, and a `FOR` over no rows sets its target NULL);
- a variable of a NOT NULL domain with no default was created NULL, where
  PostgreSQL refuses the NULL at the block's entry (RP-714, fixed: the
  block's entry now assigns the NULL through the domain coercion).

The expected answers were measured on a private, freshly initdb'd
PostgreSQL 15.19 cluster with `_SETUP`.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

_DOMAINS = ["dnn714o", "dck714", "dpos714", "dnn694"]
_SETUP = [
    "drop domain if exists " + ", ".join(_DOMAINS) + " cascade",
    "create domain dnn694 as int not null",
    "create domain dnn714o as dnn694",
    "create domain dck714 as int check (value is not null)",
    "create domain dpos714 as int check (value > 0)",
]


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute(_SETUP[0])


def _answer(dsn: str, query: str) -> object:
    notes: list[str] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        c.add_notice_handler(lambda d: notes.append(d.message_primary))
        try:
            c.execute(query)
            return ("ok", notes)
        except psycopg.Error as e:
            return ("error", e.sqlstate, str(e).splitlines()[0])


_NOT_INT = ("error", "22P02", 'invalid input syntax for type integer: "t"')
_NOT_BOOL = ("error", "22P02", 'invalid input syntax for type boolean: "5"')

# `exec_cast_value` -> `get_cast_hashentry` coerces with COERCION_PLPGSQL:
# with no implicit or assignment cast it converts through the source type's
# output and the target's input, so `true` reaches `int4in` as `t`.
EXPLICIT_ONLY = {
    "do $$ declare x int; begin x := true; end $$": _NOT_INT,
    "do $$ declare x int := true; begin end $$": _NOT_INT,
    "do $$ declare x bool; begin x := 5; end $$": _NOT_BOOL,
    "do $$ declare x int; begin select true into x; end $$": _NOT_INT,
    "do $$ declare x bool; begin select 5 into x; end $$": _NOT_BOOL,
    # bool -> text is an assignment cast (`booltext`): it is used.
    "do $$ declare x text; begin x := true; raise notice '%', x; end $$": ("ok", ["true"]),
}


@pytest.mark.backlog_fixed("RP-712")
@pytest.mark.parametrize("query", list(EXPLICIT_ONLY))
def test_assignment_with_only_an_explicit_cast_converts_through_text(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == EXPLICIT_ONLY[query]


def _framed(dsn: str, query: str) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        try:
            c.execute(query)
            return ("ok",)
        except psycopg.Error as e:
            return (e.sqlstate, e.diag.message_primary, e.diag.context)


_AT_ASSIGNMENT = "PL/pgSQL function inline_code_block line 1 at assignment"
_AT_SQL = "PL/pgSQL function inline_code_block line 1 at SQL statement"


def _assignment(text: str) -> str:
    return f'PL/pgSQL assignment "{text}"\n{_AT_ASSIGNMENT}'


# The conversion through text I/O, and where its error is framed. A `:=`
# coerces inside its own query (`transformPLAssignStmt`): over a constant,
# through immutable I/O functions (`boolout` / `int4in`, `inet_in`,
# `bit_in`), the conversion folds while the query is planned and its error
# carries the `PL/pgSQL assignment` frame; through a STABLE one (`date_in`,
# `range_in`, `cash_out`), or over a variable, it runs in the executor's
# simple-expression path, which frames nothing. An `INTO` casts outside
# the query (`exec_move_row`): no `SQL statement` frame.
VIA_IO = {
    "do $$ declare x date; begin x := 5; end $$": (
        "22007",
        'invalid input syntax for type date: "5"',
        _AT_ASSIGNMENT,
    ),
    "do $$ declare x int[]; begin x := array[true]; end $$": (
        "22P02",
        'invalid input syntax for type integer: "t"',
        _assignment("x := array[true]"),
    ),
    "do $$ declare x bit(3); begin x := 5; end $$": (
        "22P02",
        '"5" is not a valid binary digit',
        _assignment("x := 5"),
    ),
    "do $$ declare x inet; begin x := 5; end $$": (
        "22P02",
        'invalid input syntax for type inet: "5"',
        _assignment("x := 5"),
    ),
    "do $$ declare x int; begin x := true; end $$": (
        "22P02",
        'invalid input syntax for type integer: "t"',
        _assignment("x := true"),
    ),
    "do $$ declare x float8; begin x := 5::money; end $$": (
        "22P02",
        'invalid input syntax for type double precision: "$5.00"',
        _AT_ASSIGNMENT,
    ),
    "do $$ declare x int4range; begin x := 5; end $$": (
        "22P02",
        'malformed range literal: "5"',
        _AT_ASSIGNMENT,
    ),
    "do $$ declare x int; b bool := false; begin x := b; end $$": (
        "22P02",
        'invalid input syntax for type integer: "f"',
        _AT_ASSIGNMENT,
    ),
    "do $$ declare x int; begin select false, 1 into x; end $$": (
        "22P02",
        'invalid input syntax for type integer: "f"',
        _AT_SQL,
    ),
}


@pytest.mark.backlog_fixed("RP-712")
@pytest.mark.parametrize("query", list(VIA_IO))
def test_conversion_through_text_is_framed_as_postgres_frames_it(query: str, dsn: str) -> None:
    assert _framed(dsn, query) == VIA_IO[query]


# An implicit or assignment cast is used, through text I/O never.
ASSIGNED = {
    "do $$ declare x int; begin x := 1.5; raise notice '%', x; end $$": ["2"],
    "do $$ declare x int; begin x := 5::bigint; raise notice '%', x; end $$": ["5"],
    "do $$ declare x bool; begin x := 'yes'; raise notice '%', x; end $$": ["t"],
    "do $$ declare x int; begin x := '12'::text; raise notice '%', x; end $$": ["12"],
    "do $$ declare x json; begin x := 5; raise notice '%', x; end $$": ["5"],
    "do $$ declare x numeric; begin x := '5'::jsonb; raise notice '%', x; end $$": ["5"],
    "do $$ declare x text; b bool := true; begin x := b; raise notice '%', x; end $$": ["true"],
    "do $$ declare x int; begin x := null::bool; raise notice '%', x is null; end $$": ["t"],
}


@pytest.mark.backlog_fixed("RP-712")
@pytest.mark.parametrize("query", list(ASSIGNED))
def test_assignment_cast_is_used_where_one_exists(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == ("ok", ASSIGNED[query])


_VARCHAR3 = ("error", "22001", "value too long for type character varying(3)")

# The grammar makes a list of scalar variables a row target
# (`read_into_scalar_list`), which `exec_move_row` fills column by column.
SCALAR_TARGETS = {
    "do $$ declare x int; begin for x in select '7'::text loop"
    " raise notice '%', x + 1; end loop; end $$": ("ok", ["8"]),
    "do $$ declare x int; y text; begin for x, y in select 1, 'a' loop"
    " raise notice '% %', x, y; end loop; end $$": ("ok", ["1 a"]),
    "do $$ declare x varchar(3); begin for x in select 'abcdef' loop end loop; end $$": (_VARCHAR3),
    "do $$ declare x int; begin execute 'select 7' into x; raise notice '%', x; end $$": (
        "ok",
        ["7"],
    ),
    "do $$ declare x int; y text; begin execute 'select 1, ''a''' into x, y;"
    " raise notice '% %', x, y; end $$": ("ok", ["1 a"]),
    "do $$ declare x varchar(3); begin execute 'select ''abcdef''' into x; end $$": _VARCHAR3,
}


@pytest.mark.backlog_fixed("RP-713")
@pytest.mark.parametrize("query", list(SCALAR_TARGETS))
def test_for_and_execute_into_scalar_variables(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == SCALAR_TARGETS[query]


# `exec_for_query` moves a NULL tuple into the target when the query returns
# no rows; `exec_stmt_dynexecute` and `exec_stmt_fetch` do the same for an
# INTO with no row. A scalar list takes one column per variable, the extra
# columns ignored, and is filled afresh on every row and every execution.
SCALAR_TARGETS_MOVED = {
    "do $$ declare x int := 5; y text := 'k'; begin for x, y in select 1 where false loop"
    " end loop; raise notice '% % %', x is null, y is null, found; end $$": ["t t f"],
    "do $$ declare x int := 5; begin for x in execute 'select 1 where false' loop end loop;"
    " raise notice '%', x is null; end $$": ["t"],
    "do $$ declare r record; begin select 1 a into r; for r in select 2 a where false loop"
    " end loop; raise notice '%', r; end $$": ["()"],
    "do $$ declare x int; y text; begin for x, y in select g, g::text || 'z'"
    " from generate_series(1, 3) g loop raise notice '% %', x, y; end loop; end $$": [
        "1 1z",
        "2 2z",
        "3 3z",
    ],
    "do $$ declare x int; y text; begin for x, y in select 1 loop"
    " raise notice '% %', x, y is null; end loop; end $$": ["1 t"],
    "do $$ declare x int; begin for x in select 1, 2 loop raise notice '%', x; end loop; end $$": [
        "1"
    ],
    "do $$ declare s int := 0; x int; begin for i in 1..3 loop for x in select i * 10 loop"
    " s := s + x; end loop; end loop; raise notice '%', s; end $$": ["60"],
    "do $$ declare x int := 5; begin execute 'select 1 where false' into x;"
    " raise notice '% %', x is null, found; end $$": ["t f"],
    "do $$ declare x int; y text; c cursor for select 4, 'q'; begin open c;"
    " fetch c into x, y; raise notice '% %', x, y; fetch c into x, y;"
    " raise notice '% % %', x is null, y is null, found; end $$": ["4 q", "t t f"],
}


@pytest.mark.backlog_fixed("RP-713")
@pytest.mark.parametrize("query", list(SCALAR_TARGETS_MOVED))
def test_scalar_targets_are_moved_as_postgresql_moves_them(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == ("ok", SCALAR_TARGETS_MOVED[query])


_NOT_NULL = ("error", "23502", "domain dnn694 does not allow null values")

# `exec_stmt_block` initialises a variable with no DEFAULT by assigning it
# NULL through `exec_assign_value`, which checks the domain's NOT NULL and
# evaluates its CHECKs on the NULL; a domain over a NOT NULL domain is named
# itself, and an array of the domain is no domain.
NOT_NULL_DOMAIN = {
    "do $$ declare x dnn694; begin end $$": _NOT_NULL,
    "do $$ declare x dnn694; begin select 5 into x; end $$": _NOT_NULL,
    "do $$ declare x dnn694 := 5; begin raise notice '%', x; end $$": ("ok", ["5"]),
    "do $$ declare x dnn694 := null; begin end $$": _NOT_NULL,
    "do $$ declare x dnn714o; begin end $$": (
        "error",
        "23502",
        "domain dnn714o does not allow null values",
    ),
    "do $$ declare x dck714; begin end $$": (
        "error",
        "23514",
        'value for domain dck714 violates check constraint "dck714_check"',
    ),
    "do $$ declare x dpos714; begin raise notice '%', x is null; end $$": ("ok", ["t"]),
    "do $$ declare x dnn694[]; begin raise notice '%', x is null; end $$": ("ok", ["t"]),
    "do $$ begin for i in 1..2 loop declare x dnn694; begin end; end loop;"
    " exception when not_null_violation then raise notice 'caught'; end $$": ("ok", ["caught"]),
}


@pytest.mark.backlog_fixed("RP-714")
@pytest.mark.parametrize("query", list(NOT_NULL_DOMAIN))
def test_a_not_null_domain_variable_without_a_default_is_refused(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == NOT_NULL_DOMAIN[query]


@pytest.mark.backlog_fixed("RP-714")
def test_a_not_null_domain_variables_refusal_names_the_block_initialization(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        for q in _SETUP:
            c.execute(q)
        with pytest.raises(psycopg.errors.NotNullViolation) as e:
            c.execute("do $$ declare x dnn694; begin end $$")
    assert e.value.diag.context == (
        "PL/pgSQL function inline_code_block line 1 during statement block local variable"
        " initialization"
    )
