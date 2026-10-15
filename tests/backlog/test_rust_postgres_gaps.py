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


@pytest.mark.backlog_fixed("RP-008")
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


@pytest.mark.backlog_fixed("RP-009")
def test_grouped_query_subquery_names_outer_table(grouped: Any) -> None:
    """PostgreSQL 15.19: the subquery may name the unaliased outer table."""
    sql = (
        "select g, (select max(v) from rp_gs i where i.g = rp_gs.g)"
        " from rp_gs group by g order by g"
    )
    assert pg_answer(grouped, sql) == ("ok", [(1, 3), (2, 5)])


@pytest.mark.backlog_fixed("RP-010")
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


@pytest.mark.backlog_fixed("RP-011")
@pytest.mark.parametrize(
    ("sql", "name"),
    [("select nextval('rp011_seq')", "rp011_seq"), ("select 'rp011_t'::regclass", "rp011_t")],
    ids=["nextval-string", "regclass-string"],
)
def test_public_off_path_not_found_from_a_string(public_hidden: Any, sql: str, name: str) -> None:
    """PostgreSQL 15.19: 42P01 `relation "..." does not exist`."""
    assert pg_answer(public_hidden, sql) == ("err", "42P01", f'relation "{name}" does not exist')


@pytest.mark.backlog_fixed("RP-011")
def test_public_off_path_drop_if_exists_skips(public_hidden: Any) -> None:
    """PostgreSQL 15.19: `drop table if exists` skips with a notice, and
    `public.rp011_t` is still there afterwards."""
    notices: list[str] = []
    public_hidden.add_notice_handler(lambda d: notices.append(d.message_primary))
    assert pg_answer(public_hidden, "drop table if exists rp011_t") == ("ok", "DROP TABLE")
    assert notices == ['table "rp011_t" does not exist, skipping']
    assert pg_answer(public_hidden, "select count(*) from public.rp011_t") == ("ok", [(0,)])


@pytest.mark.backlog_fixed("RP-012")
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


@pytest.mark.backlog_fixed("RP-013")
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
    "query-to-xmlschema": "select query_to_xmlschema('select 1 as a', true, false, '')",
    "ts-rewrite-no-rows": "select ts_rewrite('a'::tsquery, 'select 1, 2 where false')",
}

# What the 2026-10-15 batch fixed, kept as regression tests.
_RP015_FIXED = {
    "variadic-column": (
        "select concat(variadic a), format('%s-%s', variadic a)"
        " from (values (array['x', 'y'])) v(a)"
    ),
    "variadic-nested-array": "select concat(variadic array[[1, 2], [3, 4]])",
    "variadic-arity": "select concat('a', variadic array['b', 'c'])",
    "timezone-names": "select count(*) > 100 from pg_timezone_names",
    "timezone-abbrevs": (
        "select * from pg_timezone_abbrevs() where abbrev in ('EST', 'IST') order by 1"
    ),
    "ts-stat": "select * from ts_stat('select to_tsvector(''a b a'')') order by 1",
    "array-element-position": "select array_append('{1.5}', 2)",
    "record-input": "select '(1,x)'::record",
    "fmgr-validator": "select pg_typeof(fmgr_internal_validator('abs(int4)'::regprocedure))::text",
    "fdw-validator": "select postgresql_fdw_validator(array['nosuch=x'], 1418)",
    "record-srf-field": (
        "select (aclexplode('{postgres=ar/postgres}'::aclitem[])).privilege_type order by 1"
    ),
    "query-to-xml": (
        "select query_to_xml('select 1 as a, null::text as \"_xb\", array[1,null] as r',"
        " true, true, 'urn:x')"
    ),
    "tid-type": "select '(0,1)'::tid::text",
    "date-part-unit-text": "select date_part('foo', '2000-01-01'::timestamptz)",
    "jsonb-object-to-int": """select '{"a":1}'::jsonb::int""",
    "ts-parse": "select * from ts_parse('default', 'foo bar')",
    "latin2-non-ascii": "select convert_to('é', 'LATIN2')",
    "srf-in-expression": """select pg_typeof(json_each('{"a":1}'))::text""",
    "text-pattern-op": "select 'b'::text ~>~ 'a'::text",
    "date-plus-timetz": "select (date '2000-01-01' + time with time zone '01:00+00')::text",
    "untyped-literal-arg": "select abs('-1'), sqrt('4')",
    "float-input-range": "select '1e400'::float8",
    "srf-in-coalesce": "select coalesce(unnest(array[1, null]), 0)",
    "timetz-date-part": "select date_part('timezone_h', '01:00-03:30'::timetz)",
    "int2-to-char-cast": 'select 65::int2::"char"',
    "regconfig-by-oid": "select 3748::regconfig::text",
    "sjis-non-ascii": "select convert_to('é', 'SJIS')",
    "euc-jp-from-bytes": "select convert_from('\\xa4a2'::bytea, 'EUC_JP')",
    "pg-ts-config": "select count(*) from pg_ts_config",
    "regdictionary": "select 'simple'::regdictionary::oid",
    "ts-lexize": "select ts_lexize('english_stem', 'cats')",
    "ts-config-position": "select to_tsvector('nope', 'a b')",
    "ts-headline-untyped": "select ts_headline('english', 'a b c', 'b')",
    "ts-headline-options": (
        "select ts_headline('english', 'The quick brown fox jumps over the lazy dog. "
        "The fox is quick.', to_tsquery('english', 'fox'), "
        "'MaxFragments=2, MaxWords=4, MinWords=2, FragmentDelimiter=\" | \"')"
    ),
    "unnest-untyped": "select unnest('{1,2}')",
    "sum-untyped": "select sum('1')",
    "width-bucket-dates": (
        "select width_bucket('2020-01-02'::date, array['2020-01-01'::date, '2020-02-01'])"
    ),
}


def _same_as_reference(pg: Any, pg15: Any, sql: str) -> None:
    for conn in (pg, pg15):
        conn.execute("set timezone = 'UTC'")
    assert pg_answer(pg, sql) == pg_answer(pg15, sql)


@pytest.mark.backlog("RP-015")
@pytest.mark.parametrize("sql", list(_RP015.values()), ids=list(_RP015))
def test_builtin_coverage_residue(pg: Any, pg15: Any, sql: str) -> None:
    _same_as_reference(pg, pg15, sql)


@pytest.mark.backlog_fixed("RP-015")
@pytest.mark.parametrize("sql", list(_RP015_FIXED.values()), ids=list(_RP015_FIXED))
def test_builtin_coverage_fixed(pg: Any, pg15: Any, sql: str) -> None:
    _same_as_reference(pg, pg15, sql)


@pytest.mark.backlog_fixed("RP-015")
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


def _privilege_in_a_block(pg: Any, name: str) -> list:
    """GRANT, then REVOKE, each followed by a read as the role, inside one
    transaction block apiece."""
    out = []
    for stmt in ("DROP TABLE IF EXISTS {t}", "DROP ROLE IF EXISTS {r}"):
        pg_answer(pg, stmt.format(t=f"{name}_t", r=f"{name}_u"))
    pg_answer(pg, f"CREATE ROLE {name}_u")
    pg_answer(pg, f"CREATE TABLE {name}_t (a int)")
    pg_answer(pg, f"INSERT INTO {name}_t VALUES (1)")
    for change in (
        f"GRANT SELECT ON {name}_t TO {name}_u",
        f"REVOKE SELECT ON {name}_t FROM {name}_u",
    ):
        pg_answer(pg, "BEGIN")
        pg_answer(pg, change)
        pg_answer(pg, f"SET ROLE {name}_u")
        got = pg_answer(pg, f"SELECT count(*) FROM {name}_t")
        out.append(got[:2] if got[0] == "err" else got)
        pg_answer(pg, "ROLLBACK")
        pg_answer(pg, change)  # keep the effect for the next step, outside a block
    pg_answer(pg, f"DROP TABLE {name}_t")
    pg_answer(pg, f"DROP ROLE {name}_u")
    return out


@pytest.mark.backlog_fixed("RP-020")
def test_grant_and_revoke_take_effect_inside_a_transaction_block(pg: Any) -> None:
    """PostgreSQL 15.19: a GRANT inside a block lets the role read at once,
    and a REVOKE inside a block stops it at once. The Rust server ignored
    both until the block ended."""
    assert _privilege_in_a_block(pg, "rp020") == [("ok", [(1,)]), ("err", "42501")]


@pytest.mark.backlog_fixed("RP-021")
@pytest.mark.parametrize(
    ("sql", "position"),
    [
        ("insert into rp021_t values (1, 'a', 3)", "37"),
        ("insert into rp021_t (a) values (1, 2)", "36"),
        ("insert into rp021_t (a, b) values (1)", "25"),
        ("insert into rp021_t (a) select 1, 2", "35"),
    ],
    ids=["values-no-columns", "values-columns", "too-few-expressions", "select"],
)
def test_insert_arity_error_carries_position(pg: Any, sql: str, position: str) -> None:
    """PostgreSQL 15.19: 42601 with the position of the first expression
    past the target list (or of the first target column with nothing to
    fill it)."""
    pg.execute("create table if not exists rp021_t (a int, b text)")
    with pytest.raises(psycopg.errors.SyntaxError) as info:
        pg.execute(sql)
    assert info.value.diag.statement_position == position


@pytest.mark.backlog_fixed("RP-022")
def test_collated_literal_against_a_char_column(pg: Any) -> None:
    """PostgreSQL 15.19: an untyped literal under COLLATE takes the
    `char(n)` column's type, trailing blanks and all, so `'a  '` equals the
    stored `a` in the select list as it does in WHERE."""
    pg.execute("drop table if exists rp022_t")
    pg.execute("create table rp022_t (c char(3))")
    pg.execute("insert into rp022_t values ('a'), ('b')")
    sql = "select c = 'a  ' collate \"C\", c >= ('a  ' collate \"C\") from rp022_t order by c"
    assert pg_answer(pg, sql) == ("ok", [(True, True), (False, True)])


@pytest.mark.backlog_fixed("RP-023")
def test_case_over_a_scalar_subquery_keeps_its_type(pg: Any) -> None:
    """PostgreSQL 15.19: `case when true then (select 1) end` is an integer:
    an uncorrelated subquery's value keeps its column's type."""
    assert pg_answer(pg, "select case when true then (select 1) end") == ("ok", [(1,)])
    assert pg_answer(pg, "select pg_typeof(case when true then (select 1) end)::text") == (
        "ok",
        [("integer",)],
    )


@pytest.mark.backlog_fixed("RP-024")
def test_window_rows_come_out_in_window_sort_order(pg: Any) -> None:
    """PostgreSQL 15.19: with no ORDER BY of its own, a query with a window
    returns its rows in the window's sort order (the WindowAgg's input is
    sorted), which clients see and some rely on."""
    pg.execute("drop table if exists rp024_t")
    pg.execute("create table rp024_t (a int)")
    pg.execute("insert into rp024_t values (1), (2), (3)")
    sql = "select a, row_number() over (order by a desc) from rp024_t"
    assert pg_answer(pg, sql) == ("ok", [(3, 1), (2, 2), (1, 3)])


@pytest.mark.backlog("RP-028")
def test_grouped_output_ties_as_postgres_leaves_them(pg: Any) -> None:
    """PostgreSQL 15.19: groups tied under ORDER BY come out in the order its
    HashAggregate produced them -- `(0, 6), (1, 6)` here; this server's
    grouping hands them to the (stable) sort in another order."""
    pg.execute("drop table if exists rp028_g")
    pg.execute("create table rp028_g (k int, g int)")
    pg.execute("insert into rp028_g select i, i % 2 from generate_series(1, 12) i")
    got = pg_answer(pg, "select g, count(*) from rp028_g group by g order by count(*)")
    assert got == ("ok", [(0, 6), (1, 6)])


@pytest.mark.backlog_fixed("RP-028")
def test_order_by_ties_as_postgres_quicksort_leaves_them(pg: Any) -> None:
    """PostgreSQL 15.19: tied rows land where its quicksort (not stable from
    7 rows up) leaves them -- `7, 10, 3, 4, 5, 9` for the six rows at v = 1;
    a stable sort keeps them in insertion order."""
    pg.execute("drop table if exists rp028_t")
    pg.execute("create table rp028_t (id int, v int)")
    pg.execute(
        "insert into rp028_t values (1,2),(2,null),(3,1),(4,1),(5,1),(6,3),(7,1),(8,2),(9,1),(10,1)"
    )
    got = pg_answer(pg, "select id from rp028_t order by v")
    assert got == ("ok", [(i,) for i in [7, 10, 3, 4, 5, 9, 1, 8, 6, 2]])


@pytest.mark.backlog_fixed("RP-029")
@pytest.mark.parametrize(
    ("target", "source", "message"),
    [
        ("b", "i", 'column "b" is of type boolean but expression is of type integer'),
        ("n", "t", 'column "n" is of type integer but expression is of type text'),
    ],
    ids=["int-into-bool", "text-into-int"],
)
def test_insert_select_checks_assignment_types(
    pg: Any, target: str, source: str, message: str
) -> None:
    """PostgreSQL 15.19: an INSERT ... SELECT whose column has no assignment
    cast from the selected type is 42804 when the statement is analysed;
    here the int is stored into the bool column, and the text fails per row."""
    pg.execute("drop table if exists rp029_s, rp029_t")
    pg.execute("create table rp029_s (i int, t text)")
    pg.execute("insert into rp029_s values (1, 'x')")
    pg.execute("create table rp029_t (b bool, n int)")
    got = pg_answer(pg, f"insert into rp029_t ({target}) select {source} from rp029_s")
    assert got == ("err", "42804", message)


@pytest.mark.backlog_fixed("RP-030")
@pytest.mark.parametrize(
    ("sql", "expected"),
    [
        ("select '(0,1)'::tid::text", ("ok", [("(0,1)",)])),
        ("select row_to_json(x)::text from tid x", ("ok", [('{"d":"2020-01-01"}',)])),
    ],
    ids=["builtin-still-wins", "whole-row"],
)
def test_table_named_after_a_builtin_type(pg: Any, sql: str, expected: Any) -> None:
    """PostgreSQL 15.19: a table may be named `tid` (its row type is
    `public.tid`); a bare `tid` in a cast is still `pg_catalog.tid`, searched
    first, and the table's whole-row value is its own row type."""
    pg.execute("drop table if exists tid")
    pg.execute("create table tid (d date)")
    pg.execute("insert into tid values ('2020-01-01')")
    try:
        assert pg_answer(pg, sql) == expected
    finally:
        pg.execute("drop table if exists tid")


# RP-025: what the 2026-10-15 typed-value work left, each compared with the
# live PostgreSQL 15 reference.
_RP025 = {
    "array-of-row": "select array[row(1)]",
    "regprocedure-input": "select 'now()'::regprocedure::text",
    "function-style-refused-cast": "select int8(true)",
    "point-order-by": "select p from (values (point '(1,1)'), (point '(0,0)')) v(p) order by p",
}


@pytest.mark.backlog_fixed("RP-025")
@pytest.mark.parametrize("sql", list(_RP025.values()), ids=list(_RP025))
def test_typed_value_residue(pg: Any, pg15: Any, sql: str) -> None:
    _same_as_reference(pg, pg15, sql)


@pytest.mark.backlog_fixed("RP-031")
def test_update_multi_column_assignment_from_a_subquery(pg: Any) -> None:
    """PostgreSQL 15.19: `update t set (v, t) = (select 5, 'z')` assigns both
    columns from the subquery's one row; here it is 0A000 (MultiAssignRef)."""
    pg.execute("drop table if exists rp031_t")
    pg.execute("create table rp031_t (k int, v int, t text)")
    pg.execute("insert into rp031_t values (1, 0, 'a')")
    got = pg_answer(pg, "update rp031_t set (v, t) = (select 5, 'z') where k = 1")
    assert got[0] == "ok", got
    assert pg_answer(pg, "select v, t from rp031_t") == ("ok", [(5, "z")])


@pytest.mark.backlog_fixed("RP-032")
def test_record_text_keeps_char_padding(pg: Any) -> None:
    """PostgreSQL 15.19: a record's text form prints a `char(3)` field with its
    padding, quoted: `("ab ")`; here it is `(ab)`."""
    pg.execute("drop table if exists rp032_t")
    pg.execute("create table rp032_t (c char(3))")
    pg.execute("insert into rp032_t values ('ab')")
    assert pg_answer(pg, "select (t)::text from rp032_t t") == ("ok", [('("ab ")',)])


@pytest.mark.backlog_fixed("RP-032")
def test_record_literal_pads_char_field(pg: Any) -> None:
    """PostgreSQL 15.19: a text literal cast to a table's row type pads its
    `char(3)` field to the column's width, `("ab ",x)`; here it is `(ab,x)`,
    because the row type's catalog entry does not record the width."""
    pg.execute("drop table if exists rp032_l")
    pg.execute("create table rp032_l (c char(3), v text)")
    assert pg_answer(pg, "select '(ab,x)'::rp032_l::text") == ("ok", [('("ab ",x)',)])


@pytest.mark.backlog_fixed("RP-032")
def test_row_type_follows_alter_table(pg: Any) -> None:
    """PostgreSQL 15.19: after `alter table add column`, `'(1,x)'::t` reads
    both columns; here the row type kept the table's first shape and the
    literal was 22P02 `malformed record literal`."""
    pg.execute("drop table if exists rp032_a")
    pg.execute("create table rp032_a (a int)")
    pg.execute("alter table rp032_a add column b text")
    try:
        got = pg_answer(pg, "select '(1,x)'::rp032_a::text")
    finally:
        pg.execute("drop table rp032_a")
    assert got == ("ok", [("(1,x)",)])


@pytest.mark.backlog_fixed("RP-032")
def test_row_type_char_overflow_has_a_position(pg: Any) -> None:
    """PostgreSQL 15.19: `'(abcd,x)'::t` over `t (c char(3), v text)` is
    22001 `value too long for type character(3)` at the literal (position
    8); here it has no position."""
    pg.execute("drop table if exists rp032_p")
    pg.execute("create table rp032_p (c char(3), v text)")
    try:
        got = _error_and_position(pg, [], "select '(abcd,x)'::rp032_p")
    finally:
        pg.execute("drop table rp032_p")
    assert got == ("22001", "8")


@pytest.mark.backlog_fixed("RP-033")
def test_whole_row_of_a_union_with_unnamed_columns(pg: Any) -> None:
    """PostgreSQL 15.19: `select q from (select 1, 'a' union select 1, 'a') q`
    is `(1,a)`; here it is `(1,1)` -- both columns are named `?column?`, and
    the whole-row value reads its fields by name."""
    assert pg_answer(pg, "select q::text from (select 1, 'a' union select 1, 'a') q") == (
        "ok",
        [("(1,a)",)],
    )


@pytest.mark.backlog_fixed("RP-034")
def test_update_from_returning_target_star(pg: Any) -> None:
    """PostgreSQL 15.19: `UPDATE t ... FROM s ... RETURNING t.*` returns the
    updated row; here it is 0A000 `this target is not supported yet`."""
    pg.execute("drop table if exists rp034_t")
    pg.execute("drop table if exists rp034_s")
    pg.execute("create table rp034_t (k int, v int)")
    pg.execute("create table rp034_s (k int)")
    pg.execute("insert into rp034_t values (1, 0)")
    pg.execute("insert into rp034_s values (1)")
    got = pg_answer(
        pg, "update rp034_t set v = 5 from rp034_s s where s.k = rp034_t.k returning rp034_t.*"
    )
    assert got == ("ok", [(1, 5)]), got


# RP-035: field selection on a record. PostgreSQL 15.19 names an anonymous
# ROW()'s fields f1, f2, ... only where it can see the ROW() itself; a
# selected field keeps its type.
_RP035 = {
    "sublink-record": "select ((select row(1, 'a'))).f1",
    "subquery-column": "select (r).f1 from (select row(1, 'a') as r) s",
    "values-record": "select (x).f1 from (values (row(1, 'a'))) v(x)",
    "cte-column": "with w as (select row(1, 2) r) select (r).f2 from w",
    "nested-record": "select (row(row(1, 2), 3)).f1",
    "nested-field": "select ((row(row(1, 2), 3)).f1).f2",
}


@pytest.mark.backlog_fixed("RP-035")
@pytest.mark.parametrize("sql", list(_RP035.values()), ids=list(_RP035))
def test_record_field_selection(pg: Any, pg15: Any, sql: str) -> None:
    _same_as_reference(pg, pg15, sql)


# Fixed from RP-025: ORDER BY ... USING (2026-10-15).
_RP025_OPEN = {
    "order-by-using": (
        "select string_agg(x::text, ',' order by x using >) from (values (1), (3), (2)) v(x)"
    ),
    "order-by-using-plain": "select x from (values (1), (3), (2)) v(x) order by x using >",
}


@pytest.mark.backlog_fixed("RP-025")
@pytest.mark.parametrize("sql", list(_RP025_OPEN.values()), ids=list(_RP025_OPEN))
def test_typed_value_open(pg: Any, pg15: Any, sql: str) -> None:
    _same_as_reference(pg, pg15, sql)


@pytest.mark.backlog_fixed("RP-025")
def test_create_type_named_after_a_builtin(pg: Any) -> None:
    """PostgreSQL 15.19: `create type uuid as enum ('a')` succeeds (it is
    `public.uuid`); here it is 42710."""
    pg.execute("drop type if exists public.uuid")
    got = pg_answer(pg, "create type uuid as enum ('a')")
    pg.execute("drop type if exists public.uuid")
    assert got[0] == "ok", got


# Fixed from RP-025: an untyped side of `||` beside a type with a `||` of its
# own is read as that type (`byteacat`, `bitcat`, `tsvector_concat`,
# `tsquery_or`, `jsonb_concat`, array_cat); beside a TYPED string it is
# text concatenation.
_RP025_FIXED = {
    "bytea-concat-untyped": "select '\\x01'::bytea || 'x'",
    "untyped-concat-bytea": "select 'x' || '\\x01'::bytea",
    "bytea-concat-text": "select '\\x01'::bytea || 'x'::text",
    "bytea-concat-bad-hex": "select '\\x01'::bytea || '\\xzz'",
    "bit-concat-hex": "select B'10' || 'x1F'",
    "bit-concat-bad-digit": "select B'10' || '012'",
    "untyped-concat-tsvector": "select 'c' || 'a b'::tsvector",
    "untyped-concat-tsquery": "select 'b' || 'a'::tsquery",
    "tsvector-concat-text": "select 'a b'::tsvector || 'c'::text",
    "jsonb-concat-bad": """select '{"a":1}'::jsonb || 'x'""",
    "jsonb-concat-text": """select '{"a":1}'::jsonb || '{"b":2}'::text""",
    "null-array-concat": "select null::int[] || '{3}'",
}


@pytest.mark.backlog_fixed("RP-025")
@pytest.mark.parametrize("sql", list(_RP025_FIXED.values()), ids=list(_RP025_FIXED))
def test_typed_value_fixed(pg: Any, pg15: Any, sql: str) -> None:
    _same_as_reference(pg, pg15, sql)


def _error_and_position(conn: Any, setup: list[str], sql: str) -> tuple:
    for stmt in setup:
        conn.execute(stmt)
    try:
        conn.execute(sql)
    except psycopg.Error as e:
        if not conn.autocommit:
            conn.rollback()
        return (e.sqlstate, e.diag.statement_position)
    return ("ok", None)


@pytest.mark.backlog_fixed("RP-036")
def test_unknown_setting_is_refused(pg: Any, pg15: Any) -> None:
    """PostgreSQL 15.19: `SET nosuch_param = 1` is 42704 `unrecognized
    configuration parameter`; here it is accepted."""
    sql = "set nosuch_param = 1"
    assert pg_answer(pg, sql)[:2] == pg_answer(pg15, sql)[:2] == ("err", "42704")


@pytest.mark.backlog_fixed("RP-036")
@pytest.mark.parametrize(
    ("setup", "sql"),
    [
        (["set standard_conforming_strings = off"], "select 1, 'x'::int4"),
        ([], "select 1 fetch first $1 rows only"),
    ],
    ids=["nonconforming-strings", "missing-parameter"],
)
def test_error_positions(pg: Any, pg15: Any, setup: list[str], sql: str) -> None:
    """PostgreSQL 15.19 places these errors (22P02 at the literal under
    `standard_conforming_strings = off`, 42P02 at the `$1`); here they carry
    no position."""
    assert _error_and_position(pg, setup, sql) == _error_and_position(pg15, setup, sql)


@pytest.mark.backlog_fixed("RP-037")
@pytest.mark.parametrize(
    "sql",
    [
        "select r from (values ('[5,7)'::int4range), ('[1,3)'), ('empty'), ('[10,11)')) v(r)"
        " order by r",
        "select r from (values ('{[5,7)}'::int4multirange), ('{[1,3),[4,5)}'), ('{}'),"
        " ('{[1,3)}')) v(r) order by r",
    ],
    ids=["range", "multirange"],
)
def test_order_by_a_range(pg: Any, pg15: Any, sql: str) -> None:
    """PostgreSQL 15.19 sorts ranges by `range_cmp` (empty first, then by
    lower and upper bound) and multiranges by `multirange_cmp`; here both
    sort by their text, so `empty` is last and `[10,11)` before `[5,7)`."""
    _same_as_reference(pg, pg15, sql)


@pytest.mark.backlog_fixed("RP-025")
def test_untyped_record_array_compare(pg: Any, pg15: Any) -> None:
    """PostgreSQL 15.19: comparing arrays of records with an untyped field
    is 42883 `could not identify an equality operator for type unknown`;
    here it is 0A000."""
    sql = "select array[row(1, null)] = array[row(1, null)]"
    assert pg_answer(pg, sql)[:2] == pg_answer(pg15, sql)[:2] == ("err", "42883")


# RP-038: divergences the 2026-10-15 batch saw beside its items.
_RP038_FIXED = {
    "limit-text": "select 1 limit 'x'",
    "mode-unknown": "select mode() within group (order by NULL)",
    "xmlagg": "select xmlagg(null::xml)",
    "operator-function-typed": "select int4pl(a, b) from (values ('x'::text, 1)) v(a, b)",
}


@pytest.mark.backlog_fixed("RP-038")
@pytest.mark.parametrize("sql", list(_RP038_FIXED.values()), ids=list(_RP038_FIXED))
def test_batch_residue_fixed(pg: Any, pg15: Any, sql: str) -> None:
    _same_as_reference(pg, pg15, sql)


@pytest.mark.backlog_fixed("RP-038")
def test_pg_typeof_names_the_domain(pg: Any) -> None:
    """PostgreSQL 15.19: `pg_typeof` of a column declared `rp38_d` is
    `rp38_d`; here it is the domain's base type, `integer`."""
    pg.execute("drop table if exists rp38_t")
    pg.execute("drop domain if exists rp38_d")
    pg.execute("create domain rp38_d as int")
    pg.execute("create table rp38_t (c rp38_d)")
    try:
        pg.execute("insert into rp38_t values (1)")
        got = pg_answer(pg, "select pg_typeof(c)::text from rp38_t")
    finally:
        pg.execute("drop table rp38_t")
        pg.execute("drop domain rp38_d")
    assert got == ("ok", [("rp38_d",)])


@pytest.mark.backlog_fixed("RP-038")
def test_create_type_named_int(pg: Any) -> None:
    """PostgreSQL 15.19: `create type int as enum ('a')` creates
    `public."int"` (`int` is a grammar spelling, not a pg_type name), which
    `'a'::"int"` reaches and `pg_typeof` prints `"int"`, while a bare `int`
    is still `integer`; here the CREATE was 42710."""
    got = pg_answer(pg, "create type int as enum ('a')")
    try:
        typed = pg_answer(
            pg, """select 'a'::"int"::text, pg_typeof('a'::"int")::text, pg_typeof(1::int)::text"""
        )
    finally:
        pg.execute('drop type if exists public."int"')
    assert got == ("ok", "CREATE TYPE")
    assert typed == ("ok", [("a", '"int"', "integer")])


@pytest.mark.backlog_fixed("RP-038")
def test_copy_from_reports_the_bad_byte(pg: Any) -> None:
    """PostgreSQL 15.19: COPY FROM a UTF8 client with the byte 0x82 is 22021
    `invalid byte sequence for encoding "UTF8": 0x82`; here the message is
    `COPY data is not valid UTF-8`."""
    pg.execute("drop table if exists rp38_copy")
    pg.execute("create table rp38_copy (t text)")
    got: tuple = ("ok",)
    try:
        with pg.cursor().copy("copy rp38_copy from stdin") as cp:
            cp.write(b"\x82\n")
    except psycopg.Error as e:
        got = (e.sqlstate, e.diag.message_primary)
    finally:
        pg.execute("drop table rp38_copy")
    assert got == ("22021", 'invalid byte sequence for encoding "UTF8": 0x82')


@pytest.mark.backlog_fixed("RP-015")
def test_null_tsquery_sends_no_notice(pg: Any) -> None:
    """PostgreSQL 15.19: `select null::tsquery` and `numnode(null)` send no
    NOTICE (`''::tsquery` says it has no lexemes; a NULL is not parsed)."""
    notices: list[str] = []
    pg.add_notice_handler(lambda d: notices.append(d.message_primary))
    for sql in ("select null::tsquery", "select numnode(null)", "select querytree(null::tsquery)"):
        pg.execute(sql).fetchall()
    assert notices == []


@pytest.mark.backlog_fixed("RP-039")
@pytest.mark.parametrize(
    "sql",
    ["select 1 where 2 = any('{1,x}')", "select '{1,{2,x}}'::int[]"],
    ids=["any-untyped-array", "nested-array-dimensions"],
)
def test_untyped_array_literals(pg: Any, pg15: Any, sql: str) -> None:
    _same_as_reference(pg, pg15, sql)
