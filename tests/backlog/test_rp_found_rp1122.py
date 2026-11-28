"""What working RP-1122 (a regex's director, embedded options and newline
default) found:

- a `$` in a table's WHERE also matched before a final newline, as PCRE's
  does (RP-1235, 2026-11-28).

It also holds RP-1122's regressions beyond its gap test: a regex's flavour
and newline default through a table's WHERE, and a regex function's
options read as `parse_re_flags` reads them.

Measured on PostgreSQL 15.20 (the shared reference on port 5415).
"""

from __future__ import annotations

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> str:
    return _dsn(rust_pg) + " dbname=postgres"


@pytest.mark.backlog("RP-1235")
def test_a_dollar_in_a_where_is_the_end_of_the_text(dsn: str) -> None:
    """`x ~ 'a$'` over a table matched `E'a\\n'`; outside a WHERE it did not."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists r1122")
        c.execute("create table r1122 (id int primary key, x text)")
        c.execute(r"insert into r1122 values (1, 'a'), (2, E'a\n'), (3, E'a\nb')")
        rows = {
            q: c.execute(f"select array_agg(id order by id) from r1122 where {q}").fetchone()
            for q in [
                "x ~ 'a$'",
                "x ~ '(?n)a$'",
                "x ~* 'A$'",
                "x !~ 'a$'",
            ]
        }
    assert rows == {
        "x ~ 'a$'": ([1],),
        "x ~ '(?n)a$'": ([1, 2, 3],),
        "x ~* 'A$'": ([1],),
        "x !~ 'a$'": ([2, 3],),
    }


@pytest.mark.backlog_fixed("RP-1122")
def test_regex_flavours_in_a_where(dsn: str) -> None:
    """A table's WHERE lowers the regex to the scan's filter."""
    with psycopg.connect(dsn, autocommit=True) as c:
        c.execute("drop table if exists f1122")
        c.execute("create table f1122 (id int primary key, x text)")
        c.execute(
            r"insert into f1122 values (1, E'a\nb'), (2, 'a.b'), (3, 'axb'),"
            r" (4, E'x\nab'), (5, 'W'), (6, 'a<b')"
        )
        rows = {
            q: c.execute(f"select array_agg(id order by id) from f1122 where {q}").fetchone()
            for q in [
                "x ~ 'a.b'",
                "x ~ '(?n)a.b'",
                "x ~ '(?q)a.b'",
                "x ~ '(?n)^ab'",
                "x ~* '(?c)w'",
                "x !~ 'a.b'",
                "x ~ '(?n)a[^y]b'",
                r"x ~ 'a\<b'",
                "x similar to 'a_b'",
            ]
        }
    assert rows == {
        "x ~ 'a.b'": ([1, 2, 3, 6],),
        "x ~ '(?n)a.b'": ([2, 3, 6],),
        "x ~ '(?q)a.b'": ([2],),
        "x ~ '(?n)^ab'": ([4],),
        "x ~* '(?c)w'": (None,),
        "x !~ 'a.b'": ([4, 5],),
        "x ~ '(?n)a[^y]b'": ([2, 3, 6],),
        r"x ~ 'a\<b'": ([6],),
        "x similar to 'a_b'": ([1, 2, 3, 6],),
    }


# A regex function's options (`parse_re_flags`): `e` gives a BRE, as
# `REG_EXTENDED` is cleared with `REG_ADVANCED`; a later `c` or `i` wins;
# `q` makes the pattern a literal and an embedded `q` clears `x`.
OPTIONS = {
    r"select regexp_replace(E'a\nb', 'a.b', 'X', 'n') as r": ([("a\nb",)], ["r"]),
    r"select regexp_replace(E'x\nab', '^ab', 'X', 'w') as r": ([("x\nX",)], ["r"]),
    "select regexp_replace('ab', 'a b', 'X', 'x') as r": ([("X",)], ["r"]),
    "select regexp_match('ab', '(?q)a b', 'x') as r": ([(None,)], ["r"]),
    "select regexp_match('W', '(?c)w', 'i') as r": ([(None,)], ["r"]),
    "select regexp_match('W', 'w', 'ic') as r, regexp_match('W', 'w', 'ci') as s": (
        [(None, ["W"])],
        ["r", "s"],
    ),
    "select regexp_match('w00', 'w0+', 'e') as r, regexp_match('w(0)', 'w(0)', 'e') as s": (
        [(None, ["w(0)"])],
        ["r", "s"],
    ),
    "select regexp_match('abc', 'a.c', 'q') as r, regexp_match('x', '***:x', 'b') as s": (
        [(None, ["x"])],
        ["r", "s"],
    ),
    r"select regexp_count(E'a\nb a\nb', 'a.b') as c, regexp_like(E'a\nb', 'a.b', 'n') as l": (
        [(2, False)],
        ["c", "l"],
    ),
    r"select regexp_split_to_array(E'a\nb', '.') as r": ([(["", "", "", ""],)], ["r"]),
    r"select substring(E'a\nb' from 'a.b') as r": ([("a\nb",)], ["r"]),
}


@pytest.mark.backlog_fixed("RP-1122")
@pytest.mark.parametrize("query", list(OPTIONS))
def test_regex_function_options(query: str, dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as c:
        cur = c.execute(query)
        got = (cur.fetchall(), [d.name for d in cur.description or []])
    assert got == OPTIONS[query]
