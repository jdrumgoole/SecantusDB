"""Gap tests for `tasks/backlog-rust-postgres.md`, sections 3, 3.1, 5 and the
2026-09-30 batch (RP-006 .. RP-019). RP-001 .. RP-005 are in
`test_rp_triage.py`."""

from __future__ import annotations

from collections.abc import Iterator
from typing import Any

import pytest

from tests.backlog.conftest import pg_answer, psycopg


@pytest.fixture
def grouped(pg: Any) -> Iterator[Any]:
    """`pg` with a small table `rp_gs (g, z, v)`."""
    pg.execute("drop table if exists rp_gs")
    pg.execute("create table rp_gs (g int, z int, v int)")
    pg.execute("insert into rp_gs values (1, 1, 1), (1, 2, 3), (2, 1, 5)")
    yield pg
    pg.execute("drop table if exists rp_gs")


@pytest.mark.backlog("RP-008")
def test_rollup_with_a_parenthesised_step(grouped: Any) -> None:
    """PostgreSQL 15.19 groups by the step `(g, z)` and then by `v`."""
    sql = "select g, z, v, count(*) from rp_gs group by rollup ((g, z), v) order by 1, 2, 3"
    assert pg_answer(grouped, sql) == (
        "ok",
        [
            (1, 1, 1, 1),
            (1, 1, None, 1),
            (1, 2, 3, 1),
            (1, 2, None, 1),
            (2, 1, 5, 1),
            (2, 1, None, 1),
            (None, None, None, 3),
        ],
    )


@pytest.mark.backlog("RP-009")
def test_grouped_query_subquery_names_outer_table(grouped: Any) -> None:
    """PostgreSQL 15.19: the subquery may name the unaliased outer table."""
    sql = (
        "select g, (select max(v) from rp_gs i where i.g = rp_gs.g)"
        " from rp_gs group by g order by g"
    )
    assert pg_answer(grouped, sql) == ("ok", [(1, 3), (2, 5)])


@pytest.mark.backlog("RP-010")
def test_having_42803_names_the_table(grouped: Any) -> None:
    """PostgreSQL 15.19 qualifies the column in the HAVING error."""
    assert pg_answer(grouped, "select g from rp_gs t group by g having z > 0") == (
        "err",
        "42803",
        'column "t.z" must appear in the GROUP BY clause or be used in an aggregate function',
    )


@pytest.fixture
def public_hidden(pg: Any) -> Iterator[Any]:
    """`pg` with a table and a sequence in `public`, and `public` off the path."""
    pg.execute("create schema if not exists rp011_s")
    pg.execute("create table if not exists public.rp011_t (a int)")
    pg.execute("create sequence if not exists public.rp011_seq")
    pg.execute("set search_path = rp011_s")
    yield pg
    pg.execute("reset search_path")
    pg.execute("drop table if exists public.rp011_t")
    pg.execute("drop sequence if exists public.rp011_seq")
    pg.execute("drop schema if exists rp011_s")


@pytest.mark.backlog("RP-011")
@pytest.mark.parametrize(
    ("sql", "name"),
    [("select nextval('rp011_seq')", "rp011_seq"), ("select 'rp011_t'::regclass", "rp011_t")],
    ids=["nextval-string", "regclass-string"],
)
def test_public_off_path_not_found_from_a_string(public_hidden: Any, sql: str, name: str) -> None:
    """PostgreSQL 15.19: 42P01 `relation "..." does not exist`."""
    assert pg_answer(public_hidden, sql) == ("err", "42P01", f'relation "{name}" does not exist')


@pytest.mark.backlog("RP-011")
def test_public_off_path_drop_if_exists_skips(public_hidden: Any) -> None:
    """PostgreSQL 15.19: `drop table if exists` skips with a notice, and
    `public.rp011_t` is still there afterwards."""
    notices: list[str] = []
    public_hidden.add_notice_handler(lambda d: notices.append(d.message_primary))
    assert pg_answer(public_hidden, "drop table if exists rp011_t") == ("ok", "DROP TABLE")
    assert notices == ['table "rp011_t" does not exist, skipping']
    assert pg_answer(public_hidden, "select count(*) from public.rp011_t") == ("ok", [(0,)])


@pytest.mark.backlog("RP-012")
def test_parse_syntax_error_carries_position(pg: Any) -> None:
    """PostgreSQL 15.19: `selct 1` through Parse is 42601 with position 1,
    the same as through a simple query."""
    with pytest.raises(psycopg.errors.SyntaxError) as info:
        pg.execute("selct 1", prepare=True)
    assert info.value.diag.statement_position == "1"


def _connect_error(port: int, conninfo: str) -> str:
    try:
        psycopg.connect(f"host=127.0.0.1 port={port} {conninfo}", connect_timeout=10).close()
    except psycopg.OperationalError as e:
        return str(e).splitlines()[0].split("FATAL:", 1)[-1].strip()
    return "connected"


@pytest.mark.backlog("RP-013")
def test_unknown_user_is_asked_for_a_password(rust_pg: Any) -> None:
    """PostgreSQL 15.19 (a scram-sha-256 cluster): a role nobody created is
    asked for a password and fails `password authentication failed for user
    "rp013_nobody"`. Here it is trusted by default (the item calls that a
    decision; `--refuse-unknown-users` is the PostgreSQL answer)."""
    assert _connect_error(rust_pg.port, "user=rp013_nobody password=x dbname=postgres") == (
        'password authentication failed for user "rp013_nobody"'
    )


@pytest.mark.backlog("RP-013")
def test_wrong_password_checked_before_the_database(pg: Any, rust_pg: Any) -> None:
    """PostgreSQL 15.19 (a scram-sha-256 cluster): a wrong password for a
    database that does not exist reads `password authentication failed`,
    not `database "..." does not exist` -- the login is proved first."""
    pg.execute("drop role if exists rp013_pw")
    pg.execute("create role rp013_pw login password 'right'")
    assert _connect_error(rust_pg.port, "user=rp013_pw password=wrong dbname=rp013_nodb") == (
        'password authentication failed for user "rp013_pw"'
    )


# RP-015: what `pg_builtin_coverage.py` still reports. Each is compared with
# the live PostgreSQL 15 reference, under the same session time zone.
_RP015 = {
    "tid-type": "select '(0,1)'::tid::text",
    "int2-to-char-cast": 'select 65::int2::"char"',
    "date-part-unit-text": "select date_part('foo', '2000-01-01'::timestamptz)",
    "jsonb-object-to-int": """select '{"a":1}'::jsonb::int""",
    "ts-parse": "select * from ts_parse('default', 'foo bar')",
    "latin2-non-ascii": "select convert_to('é', 'LATIN2')",
    "srf-in-expression": """select pg_typeof(json_each('{"a":1}'))::text""",
    "text-pattern-op": "select 'b'::text ~>~ 'a'::text",
    "date-plus-timetz": "select (date '2000-01-01' + time with time zone '01:00+00')::text",
}


@pytest.mark.backlog("RP-015")
@pytest.mark.parametrize("sql", list(_RP015.values()), ids=list(_RP015))
def test_builtin_coverage_residue(pg: Any, pg15: Any, sql: str) -> None:
    for conn in (pg, pg15):
        conn.execute("set timezone = 'UTC'")
    assert pg_answer(pg, sql) == pg_answer(pg15, sql)


@pytest.mark.backlog("RP-015")
def test_composite_column_star_expansion(pg: Any) -> None:
    """PostgreSQL 15.19: `(v).*` over a composite column expands its fields."""
    pg.execute("drop table if exists rp015_ct_t")
    pg.execute("drop type if exists rp015_ct")
    pg.execute("create type rp015_ct as (a int, b text)")
    pg.execute("create table rp015_ct_t (v rp015_ct)")
    pg.execute("insert into rp015_ct_t values (row(1, 'x'))")
    assert pg_answer(pg, "select (v).* from rp015_ct_t") == ("ok", [(1, "x")])


@pytest.mark.backlog_fixed("RP-017")
def test_wide_and_bc_timestamptz_take_the_session_offset(pg: Any) -> None:
    """PostgreSQL 15.19, stored under UTC and read under America/New_York, in
    the text format: `20000-01-01 07:00:00-05` and `0044-03-15 07:03:58-04:56:02
    BC` (LMT before 1883)."""
    from psycopg.types.string import TextLoader

    pg.adapters.register_loader("timestamptz", TextLoader)
    pg.execute("drop table if exists rp017_t")
    pg.execute("create table rp017_t (k int, v timestamptz)")
    pg.execute("set timezone = 'UTC'")
    pg.execute(
        "insert into rp017_t values (1, '20000-01-01 12:00:00+00'),"
        " (2, '0044-03-15 12:00:00+00 BC'), (3, 'infinity')"
    )
    pg.execute("set timezone = 'America/New_York'")
    assert pg_answer(pg, "select v, v::text from rp017_t order by k") == (
        "ok",
        [
            ("20000-01-01 07:00:00-05", "20000-01-01 07:00:00-05"),
            ("0044-03-15 07:03:58-04:56:02 BC", "0044-03-15 07:03:58-04:56:02 BC"),
            ("infinity", "infinity"),
        ],
    )
