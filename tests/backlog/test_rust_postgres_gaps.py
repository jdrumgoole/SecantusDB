"""Gap tests for `tasks/backlog-rust-postgres.md`, sections 3, 3.1, 5 and the
2026-09-30 batch (RP-006 .. RP-019). RP-001 .. RP-005 are in
`test_rp_triage.py`."""

from __future__ import annotations

import contextlib
import threading
import time
from collections.abc import Callable, Iterator
from pathlib import Path
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


@pytest.mark.backlog_fixed("RP-013")
def test_unknown_user_is_asked_for_a_password(tmp_path: Path) -> None:
    """PostgreSQL 15.19 (a scram-sha-256 cluster): a role nobody created is
    asked for a password and fails `password authentication failed for user
    "rp013_nobody"`. The server's default since Joe's decision of 2026-10-16
    (`--trust-unknown-users` is the opt-out). Its own server: the module's
    `rust_pg` trusts unknown users, so the other tests can connect."""
    from tests.test_rust_pgserver_slice import _Server

    with _Server(tmp_path, args=("--refuse-unknown-users",)) as server:
        got = _connect_error(server.port, "user=rp013_nobody password=x dbname=postgres")
    assert got == 'password authentication failed for user "rp013_nobody"'


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

# What the 2026-10-15 batch fixed, kept as regression tests.
_RP015_FIXED = {
    "aclitem-unknown-role": (
        "select a.grantee from aclexplode(array[makeaclitem(12345, 10, 'SELECT', false)]) a"
    ),
    "operator-functions": (
        "select box_overlap('((0,0),(1,1))', '((0,0),(2,2))'), dist_lp('{1,-1,0}', '(1,2)'),"
        " lseg_intersect('[(0,0),(1,1)]', '[(0,1),(1,0)]'), pg_lsn_pli('0/16', 1.5),"
        " point_above('(1,3)', '(1,2)'), ts_match_vq('a b', 'a & b')"
    ),
    "line-box-intersect": "select '{1,-1,0}'::line ?# '((0,0),(1,1))'::box",
    "lseg-line-intersect": "select '[(0,0),(1,1)]'::lseg ?# '{1,-1,1}'::line",
    "line-distance": "select '{1,-1,0}'::line <-> '{1,-1,5}'::line",
    "jsonb-minus-text": "select '{\"a\":[1,2]}'::jsonb - 'a'::text",
    "close-ls-parallel": "select close_ls('{1,-1,0}'::line, '[(0,1),(1,2)]'::lseg)",
    "timestamp-of-date-and-time": (
        "select pg_catalog.timestamp('2020-02-03'::date, '04:05:06'::time),"
        " pg_catalog.timestamptz('2020-02-03'::date, '04:05:06+02'::timetz)"
    ),
    "interval-typmod-unrecognised": "select pg_catalog.interval('1 day'::interval, 2)",
    "type-output": (
        "select boolout(true), int4out(42), date_out('2020-01-02'::date),"
        " bpcharout('ab'::char(4)), bit_out(B'101'), pg_typeof(int4out(1))"
    ),
    "typmod-output": (
        "select varchartypmodout(7), numerictypmodout(131078),"
        " intervaltypmodout(2147418115), timestamptztypmodout(3)"
    ),
    "btree-compare": (
        "select pg_lsn_cmp('0/16', '0/17'), btcharcmp('c', 'a'),"
        " range_cmp('[1,5)'::int4range, '[1,7)'::int4range), xid8_larger('5', '9')"
    ),
    "aggregate-transition": (
        "select float8_accum('{1,2,0}'::float8[], 3), int8inc(4), int2_sum(null, 3::int2),"
        " float8_regr_combine('{1,2,0,3,0,0}'::float8[], '{2,6,2,1,0.5,0}'::float8[])"
    ),
    "aggregate-state-length": "select float8_avg('{1.5,2.5}'::float8[])",
    "interval-average": (
        'select interval_avg(\'{"3 days","00:00:00.000002"}\'::interval[]),'
        " int8_avg('{2,10}'::int8[])"
    ),
    "type-send": (
        "select int4send(1), boolsend(true), tsquerysend('!a | (b <2> c:*AB)'),"
        " tsvectorsend('a:1A,3 b:2B'), jsonpath_send('$.a')"
    ),
    "hash": (
        "select hashint4(1), hashtext('abcdefghijklm'), hashtextextended('a', 7),"
        " hash_numeric(12345678.9), hashtid('(70000,3)'), timetz_hash('04:05:06-05:30')"
    ),
    "hash-containers": (
        "select hash_array('{1,null}'::int4[]), hash_range('[1.5,2]'::numrange),"
        " hash_multirange('{[1,5),[7,9)}'::int4multirange),"
        ' jsonb_hash(\'{"a":1,"bb":[2,{"c":null}],"b":false}\')'
    ),
    "in-range": (
        "select in_range('01:00'::time, '22:00'::time, '1 day 2 hours'::interval, false, true),"
        " in_range('NaN'::float8, 1::float8, 1::float8, false, true),"
        " in_range(9223372036854775807::int8, 9223372036854775807::int8, 1::int8, false, false)"
    ),
    "in-range-negative": "select in_range(1, 2, -1, true, false)",
    "byte-compare": (
        "select byteacmp('\\x05'::bytea, '\\x01ff'::bytea), bitcmp(B'101', B'100'),"
        " btnamecmp('ab', 'a'), network_cmp('10.0.0.0/8', '10.0.0.0/16'),"
        " btbpchar_pattern_cmp('ab', 'a '), gin_compare_jsonb('abc', 'ab')"
    ),
    "infinite-timestamp-compare": (
        "select 'infinity'::timestamp > '2020-01-02'::timestamp,"
        " '-infinity'::timestamptz < '2020-01-01 00:00+00'::timestamptz"
    ),
    "ts-rewrite-no-rows": "select ts_rewrite('a'::tsquery, 'select 1, 2 where false')",
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
    "query-to-xmlschema": "select query_to_xmlschema('select 1 as a', true, false, '')",
    "query-to-xmlschema-types": (
        "select query_to_xmlschema('select 1::int2 as a, 2::int8 as b, 1.5::numeric as c,"
        " ''x''::varchar as d, now() as e, current_date as f, ''01:00''::time as g,"
        " ''01:00+01''::timetz as h, true as i, ''\\x01''::bytea as j, ''<a/>''::xml as k,"
        " array[1] as l, ''x''::name as \"xml:m\" where false', false, true, 'urn:t')"
    ),
    "query-to-xml-and-xmlschema": (
        "select query_to_xml_and_xmlschema('select 1 as a, null::text as b', true, false, 'urn:q')"
    ),
    "query-to-xmlschema-not-a-cursor": (
        "select query_to_xmlschema('delete from pg_class', true, false, '')"
    ),
    "cursor-to-xml-missing": "select cursor_to_xml('rp015_nope', 1, true, false, '')",
    "schema-to-xml-missing": "select schema_to_xml('rp015_nope', true, false, '')",
}


def _same_as_reference(pg: Any, pg15: Any, sql: str) -> None:
    for conn in (pg, pg15):
        conn.execute("set timezone = 'UTC'")
    assert pg_answer(pg, sql) == pg_answer(pg15, sql)


@pytest.mark.backlog_fixed("RP-015")
def test_strcoll_compares_answer_the_sign(pg: Any, pg15: Any) -> None:
    """PostgreSQL 15.19: `bttextcmp` / `bpcharcmp` / `jsonb_cmp` and
    `tsvector_cmp` / `tsquery_cmp` order as below. Only the SIGN is compared
    (Joe, 2026-10-16): the magnitude of a `strcoll` result is the platform's
    locale data, and PostgreSQL promises only the sign. The inputs order the
    same under `C` and `en_US.UTF-8`."""
    sql = (
        "select sign(bttextcmp('b', 'a')), sign(bttextcmp('a', 'b')), sign(bttextcmp('a', 'a')),"
        " sign(bpcharcmp('b', 'a')), sign(bpcharcmp('a ', 'a')),"
        " sign(jsonb_cmp('\"a\"', '\"c\"')), sign(jsonb_cmp('[1]', '{}')),"
        " sign(tsvector_cmp('a b', 'a')), sign(tsvector_cmp('a', 'b')),"
        " sign(tsquery_cmp('a', 'a & b')), sign(tsquery_cmp('b', 'a'))"
    )
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


@pytest.mark.backlog_fixed("RP-028")
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
def test_group_by_rows_come_out_in_hash_aggregate_order(pg: Any) -> None:
    """PostgreSQL 15.19: with no ORDER BY, a GROUP BY it plans as a
    HashAggregate returns the groups in its hash table's order --
    9 3 5 4 0 10 6 2 7 1 8 here, not the order the groups first appear."""
    pg.execute("drop table if exists rp028_h")
    pg.execute("create table rp028_h (k int, g int)")
    pg.execute("insert into rp028_h select i, (i * 7) % 11 from generate_series(1, 40) i")
    got = pg_answer(pg, "select g from rp028_h group by g")
    assert got == ("ok", [(g,) for g in [9, 3, 5, 4, 0, 10, 6, 2, 7, 1, 8]])


@pytest.mark.backlog_fixed("RP-028")
def test_distinct_rows_come_out_in_hash_aggregate_order(pg: Any) -> None:
    """PostgreSQL 15.19: `SELECT DISTINCT` is a HashAggregate too, and
    returns 9 3 5 4 0 10 6 2 7 1 8 for the table above (it kept the order
    the values first appear, until the hash table was modelled)."""
    pg.execute("drop table if exists rp028_d")
    pg.execute("create table rp028_d (k int, g int)")
    pg.execute("insert into rp028_d select i, (i * 7) % 11 from generate_series(1, 40) i")
    got = pg_answer(pg, "select distinct g from rp028_d")
    assert got == ("ok", [(g,) for g in [9, 3, 5, 4, 0, 10, 6, 2, 7, 1, 8]])


@pytest.mark.backlog_fixed("RP-028")
def test_few_estimated_rows_group_by_sorting(pg: Any) -> None:
    """PostgreSQL 15.19: `where k > 5 and k < 30` over a fresh (k int primary
    key, g int, t text) is estimated at 6 rows (1,200 assumed, at 0.005),
    and a Sort + Group over the bitmap heap scan (13.77..13.80) costs within
    the planner's 1% fuzz of a HashAggregate, so the sorted path's order wins
    in `add_path` and the groups come out sorted."""
    pg.execute("drop table if exists rp028_w")
    pg.execute("create table rp028_w (k int primary key, g int, t text)")
    pg.execute(
        "insert into rp028_w select i, (i * 7) % 11, 'x' || (i % 6) from generate_series(1, 40) i"
    )
    got = pg_answer(pg, "select g from rp028_w where k > 5 and k < 30 group by g")
    assert got == ("ok", [(g,) for g in range(11)])


@pytest.mark.backlog_fixed("RP-028")
def test_group_by_a_whole_primary_key_hashes_on_it(pg: Any) -> None:
    """PostgreSQL 15.19: a GROUP BY holding the whole primary key groups by
    the key alone (`remove_useless_groupby_columns`; `Group Key: k`), and
    over two columns estimated at 11 rows the HashAggregate wins, so the
    groups come out in the order of k's hash."""
    pg.execute("drop table if exists rp028_s")
    pg.execute("create table rp028_s (k int primary key, g int)")
    pg.execute("insert into rp028_s select i, (i * 7) % 11 from generate_series(1, 40) i")
    got = pg_answer(pg, "select k, g from rp028_s where k > 5 and k < 30 group by k, g")
    order = [18, 16, 15, 6, 26, 12, 27, 23, 24, 11, 8, 19]
    order += [25, 29, 21, 14, 17, 28, 22, 20, 13, 10, 9, 7]
    assert got == ("ok", [(k, (k * 7) % 11) for k in order])


@pytest.mark.backlog_fixed("RP-028")
def test_order_by_moves_its_group_keys_first(pg: Any) -> None:
    """PostgreSQL 15.19: `group by g, t order by t` is planned as `group by
    t, g` (`preprocess_groupclause`), so the HashAggregate hashes (t, g) and
    the Sort over it leaves the ties of each t in that table's order."""
    pg.execute("drop table if exists rp028_o")
    pg.execute("create table rp028_o (k int primary key, g int, t text)")
    pg.execute(
        "insert into rp028_o select i, (i * 7) % 11, 'x' || (i % 6) from generate_series(1, 40) i"
    )
    got = pg_answer(pg, "select g, t from rp028_o group by g, t order by t")
    want = {
        "x0": [3, 1, 5, 9, 7, 10],
        "x1": [8, 3, 1, 10, 5, 7, 6],
        "x2": [10, 6, 2, 1, 8, 3, 4],
        "x3": [2, 10, 9, 4, 6, 8, 0],
        "x4": [6, 7, 2, 9, 0, 4, 5],
        "x5": [9, 5, 7, 2, 0, 3],
    }
    assert got == ("ok", [(g, t) for t, gs in want.items() for g in gs])


@pytest.mark.backlog_fixed("RP-028")
def test_limit_reads_the_index_in_key_order(pg: Any) -> None:
    """PostgreSQL 15.19: under a LIMIT the planner takes the plan cheapest
    to its first rows -- a Group over the primary key's index scan for a
    whole table (Limit 0.15..0.35), so the first keys in order; but over
    the 11 rows a range keeps, the HashAggregate over a bitmap heap scan
    (Limit 14.99..15.04), in the hash's order. A DISTINCT of a column no
    index leads is hashed."""
    pg.execute("drop table if exists rp028_l")
    pg.execute("create table rp028_l (k int primary key, g int)")
    pg.execute("insert into rp028_l select i, (i * 7) % 11 from generate_series(1, 40) i")
    got = pg_answer(pg, "select k, g from rp028_l group by k, g limit 5")
    assert got == ("ok", [(k, (k * 7) % 11) for k in [1, 2, 3, 4, 5]])
    got = pg_answer(pg, "select k from rp028_l where k > 5 and k < 30 group by k limit 5")
    assert got == ("ok", [(k,) for k in [18, 16, 15, 6, 26]])
    got = pg_answer(pg, "select distinct g from rp028_l limit 4")
    assert got == ("ok", [(g,) for g in [9, 3, 5, 4]])


@pytest.mark.backlog_fixed("RP-028")
def test_create_index_measures_the_table(pg: Any) -> None:
    """PostgreSQL 15.19: CREATE INDEX over a table with rows records its
    size in `pg_class` (relpages 1, reltuples 40), so the planner estimates
    40 rows, not a never-measured table's 10 pages, and its HashAggregate
    is sized for 40 groups; a TRUNCATE forgets it again."""
    pg.execute("drop table if exists rp028_m")
    pg.execute("create table rp028_m (k int, g int)")
    pg.execute("insert into rp028_m select i, (i * 7) % 11 from generate_series(1, 40) i")
    unmeasured = [(g,) for g in [9, 3, 5, 4, 0, 10, 6, 2, 7, 1, 8]]
    assert pg_answer(pg, "select g from rp028_m group by g") == ("ok", unmeasured)
    pg.execute("create index on rp028_m (g)")
    got = pg_answer(pg, "select g from rp028_m group by g")
    assert got == ("ok", [(g,) for g in [4, 0, 9, 10, 7, 6, 3, 1, 5, 2, 8]])
    pg.execute("truncate rp028_m")
    pg.execute("insert into rp028_m select i, (i * 7) % 11 from generate_series(1, 40) i")
    assert pg_answer(pg, "select g from rp028_m group by g") == ("ok", unmeasured)


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


@pytest.mark.backlog_fixed("RP-041")
def test_enum_comparison_reads_its_literal_at_plan_time(pg: Any) -> None:
    """PostgreSQL 15.19: `c = 'zz'` over an enum column is 22P02 `invalid
    input value for enum`, even over an empty table; here it answers no rows."""
    pg.execute("drop table if exists rp041_t")
    pg.execute("drop type if exists rp041_e")
    pg.execute("create type rp041_e as enum ('a', 'b')")
    pg.execute("create table rp041_t (c rp041_e)")
    try:
        got = pg_answer(pg, "select c = 'zz' from rp041_t")
    finally:
        pg.execute("drop table rp041_t")
        pg.execute("drop type rp041_e")
    assert got[:2] == ("err", "22P02")


@pytest.mark.backlog_fixed("RP-041")
@pytest.mark.parametrize(
    ("sql", "expected"),
    [
        ("select 'a'::public.nosuch", ("42704", "13")),
        ("select 'nosuch'::regtype", ("42704", "8")),
    ],
    ids=["qualified-type", "regtype-literal"],
)
def test_unknown_type_error_position(pg: Any, sql: str, expected: tuple) -> None:
    """PostgreSQL 15.19 places these 42704s at the type name (13) and at the
    literal (8); here the first is at 20 and the second has none."""
    assert _error_and_position(pg, [], sql) == expected


@pytest.mark.backlog_fixed("RP-041")
def test_qualified_unknown_type_is_named_qualified(pg: Any) -> None:
    """PostgreSQL 15.19: `'a'::public.nosuch` is `type "public.nosuch" does
    not exist`; here the schema is dropped from the name."""
    got = pg_answer(pg, "select 'a'::public.nosuch")
    assert got[:2] == ("err", "42704") and 'type "public.nosuch" does not exist' in got[2]


@pytest.mark.backlog_fixed("RP-041")
def test_quoted_mixed_case_type_name(pg: Any) -> None:
    """PostgreSQL 15.19: `create type "Int" as enum ('x')` succeeds (`"Int"`
    is not `int`); here it is 42710, the name folded to `int4`."""
    got = pg_answer(pg, """create type "Int" as enum ('x')""")
    pg.execute('drop type if exists "Int"')
    assert got == ("ok", "CREATE TYPE")


@pytest.mark.backlog_fixed("RP-041")
def test_drop_type_cascade_names_the_column(pg: Any) -> None:
    """PostgreSQL 15.19: `drop type e cascade` with a column of the type sends
    NOTICE `drop cascades to column c of table t`; here it sends none."""
    pg.execute("drop table if exists rp041_c")
    pg.execute("drop type if exists rp041_ce")
    pg.execute("create type rp041_ce as enum ('a')")
    pg.execute("create table rp041_c (id int, c rp041_ce)")
    notices: list[str] = []
    pg.add_notice_handler(lambda d: notices.append(d.message_primary))
    try:
        pg.execute("drop type rp041_ce cascade")
    finally:
        pg.execute("drop table if exists rp041_c")
    assert notices == ["drop cascades to column c of table rp041_c"]


@pytest.mark.backlog_fixed("RP-041")
def test_domain_over_enum_is_described_as_the_enum(pg: Any) -> None:
    """PostgreSQL 15.19 describes a domain over an enum with the ENUM's oid
    (a domain is sent as its base type); here it is varchar (1043)."""
    pg.execute("drop domain if exists rp041_d")
    pg.execute("drop type if exists rp041_de")
    pg.execute("create type rp041_de as enum ('a')")
    pg.execute("create domain rp041_d as rp041_de")
    try:
        enum_oid = pg.execute("select 'rp041_de'::regtype::oid").fetchone()[0]
        cur = pg.execute("select 'a'::rp041_d")
        described = cur.description[0].type_code
    finally:
        pg.execute("drop domain rp041_d")
        pg.execute("drop type rp041_de")
    assert described == enum_oid


@pytest.mark.backlog_fixed("RP-041")
def test_drop_domain_cascade_drops_every_column(pg: Any) -> None:
    """PostgreSQL 15.19: `drop domain d cascade` over two columns of one table
    drops both (`select *` is `(1,)`), with `drop cascades to 2 other
    objects`; here the second column was left behind, reading NULL."""
    pg.execute("drop table if exists rp041_v")
    pg.execute("drop domain if exists rp041_d2")
    pg.execute("create domain rp041_d2 as int")
    pg.execute("create table rp041_v (id int, a rp041_d2, b rp041_d2)")
    pg.execute("insert into rp041_v values (1, 2, 3)")
    notices: list[str] = []
    pg.add_notice_handler(lambda d: notices.append(d.message_primary))
    try:
        pg.execute("drop domain rp041_d2 cascade")
        got = pg_answer(pg, "select * from rp041_v")
    finally:
        pg.execute("drop table if exists rp041_v")
    assert (got, notices) == (("ok", [(1,)]), ["drop cascades to 2 other objects"])


@pytest.mark.backlog_fixed("RP-013")
def test_restricted_builtin_needs_a_grant(pg: Any) -> None:
    """PostgreSQL 15.19 ships `pg_reload_conf()` (and 58 more) with EXECUTE
    revoked from PUBLIC: a role without a GRANT gets 42501 under SET ROLE,
    and one with a GRANT may call it. Here any role could reload."""
    pg.execute("drop role if exists rp013_rl")
    pg.execute("create role rp013_rl")
    try:
        pg.execute("set role rp013_rl")
        denied = pg_answer(pg, "select pg_reload_conf()")
        pg.execute("reset role")
        pg.execute("grant execute on function pg_reload_conf() to rp013_rl")
        pg.execute("set role rp013_rl")
        granted = pg_answer(pg, "select pg_reload_conf()")
    finally:
        pg.execute("reset role")
        pg.execute("revoke execute on function pg_reload_conf() from rp013_rl")
        pg.execute("drop role rp013_rl")
    assert denied[:2] == ("err", "42501") and granted == ("ok", [(True,)])


@pytest.mark.backlog_fixed("RP-042")
def test_schema_created_in_a_block_holds_a_table(pg: Any) -> None:
    """PostgreSQL 15.19: `create schema s; create table s.t (...)` in one
    transaction block works; here the table was 3F000 `schema "s" does not
    exist`, the schema not yet visible to planning."""
    pg.execute("begin")
    try:
        pg.execute("create schema rp042_s")
        got = pg_answer(pg, "create table rp042_s.t (a int)")
    finally:
        pg.execute("rollback")
    assert got == ("ok", "CREATE TABLE")


@pytest.mark.backlog_fixed("RP-042")
def test_has_schema_privilege_by_oid(pg: Any) -> None:
    """PostgreSQL 15.19: `has_schema_privilege(2200, 'USAGE')` (public's oid)
    is true, and NULL for an oid naming no schema; here the oid was read as
    a schema NAME (3F000 `schema "2200" does not exist`)."""
    got = pg_answer(
        pg,
        "select has_schema_privilege(2200, 'USAGE'), has_schema_privilege(12345678, 'USAGE')",
    )
    assert got == ("ok", [(True, None)])


@pytest.mark.backlog_fixed("RP-042")
def test_declare_with_hold_outside_a_block(pg: Any) -> None:
    """PostgreSQL 15.19 takes `DECLARE c CURSOR WITH HOLD` outside a
    transaction block (only a cursor without HOLD needs one), and `CLOSE ALL`
    is tagged `CLOSE CURSOR ALL`; here the DECLARE was 25P01."""
    declared = pg_answer(pg, "declare rp042_c cursor with hold for select 1")
    fetched = pg_answer(pg, "fetch all from rp042_c")
    closed = pg_answer(pg, "close all")
    assert (declared, fetched, closed) == (
        ("ok", "DECLARE CURSOR"),
        ("ok", [(1,)]),
        ("ok", "CLOSE CURSOR ALL"),
    )


@pytest.mark.backlog_fixed("RP-042")
def test_enum_outside_public_is_a_column_type(pg: Any) -> None:
    """PostgreSQL 15.19: a column of `s.e` (an enum in another schema) is in
    `pg_attribute`, is described with the enum's oid, and its
    `information_schema.columns` row names schema `s`; here the column was
    missing, described as varchar, and named `public`."""
    pg.execute("begin")
    try:
        pg.execute("create schema rp042_q")
        pg.execute("create type rp042_q.e as enum ('a')")
        pg.execute("create table rp042_q.z (id int, m rp042_q.e)")
        attnames = pg.execute(
            "select attname from pg_attribute where attrelid = 'rp042_q.z'::regclass"
            " and attnum > 0 order by attnum"
        ).fetchall()
        enum_oid = pg.execute("select 'rp042_q.e'::regtype::oid").fetchone()[0]
        described = pg.execute("select m from rp042_q.z").description[0].type_code
        udt = pg.execute(
            "select udt_schema, udt_name from information_schema.columns"
            " where table_schema = 'rp042_q' and column_name = 'm'"
        ).fetchall()
    finally:
        pg.execute("rollback")
    assert (attnames, described, udt) == ([("id",), ("m",)], enum_oid, [("rp042_q", "e")])


@pytest.mark.backlog_fixed("RP-028")
def test_join_cursor_ordered_by_its_key_cannot_scroll(pg: Any) -> None:
    """PostgreSQL 15.19: a cursor declared without SCROLL over
    `select * from t join u on t.a = u.a order by t.a` (a merge join in key
    order, the cheapest plan for a cursor's tenth of the rows) refuses
    FETCH BACKWARD with 55000."""
    pg.execute("begin")
    try:
        pg.execute("create table rp043_t (a int primary key)")
        pg.execute("create table rp043_u (a int primary key)")
        pg.execute("insert into rp043_t select generate_series(1, 20)")
        pg.execute("insert into rp043_u select generate_series(1, 20)")
        pg.execute(
            "declare c cursor for select * from rp043_t join rp043_u"
            " on rp043_t.a = rp043_u.a order by rp043_t.a"
        )
        pg.execute("fetch 2 from c")
        pg.execute("savepoint s")
        answer = pg_answer(pg, "fetch backward 1 from c")
    finally:
        pg.execute("rollback")
    assert answer[:2] == ("err", "55000")


@pytest.mark.backlog_fixed("RP-041")
def test_mixed_case_type_is_found_only_as_written(pg: Any) -> None:
    """PostgreSQL 15.19: a domain named `"Dom"` is reached by `5::"Dom"`
    (5), and an enum `"MyType"` is not reached by `"mytype"` (42704); here
    the first is 42704 and the second finds it, the names folded."""
    pg.execute("begin")
    try:
        pg.execute('create domain "Rp041Dom" as int')
        pg.execute("create type \"Rp041Type\" as enum ('y')")
        pg.execute("savepoint s")
        reached = pg_answer(pg, 'select 5::"Rp041Dom"')
        pg.execute("rollback to s")
        folded = pg_answer(pg, "select 'y'::\"rp041type\"")
    finally:
        pg.execute("rollback")
    assert reached == ("ok", [(5,)]) and folded[:2] == ("err", "42704")


@pytest.mark.backlog_fixed("RP-044")
def test_each_empty_tsquery_raises_its_own_notice(pg: Any) -> None:
    """PostgreSQL 15.19: every empty tsquery evaluated raises its own
    `text-search query doesn't contain lexemes: ""` NOTICE, so a statement
    casting two of them gets two. Here a statement's planner notices are
    deduplicated (it is planned for Describe and for Execute), and it gets
    one."""
    got: list[str] = []
    pg.add_notice_handler(lambda d: got.append(d.message_primary))
    pg.execute("select ''::tsquery, ''::tsquery").fetchall()
    assert got == ['text-search query doesn\'t contain lexemes: ""'] * 2


@pytest.mark.backlog_fixed("RP-045")
def test_rollback_to_savepoint_closes_its_cursors(pg: Any) -> None:
    """PostgreSQL 15.19: a cursor declared after a savepoint is closed by
    ROLLBACK TO it, so a FETCH from it is 34000; here it still reads."""
    pg.execute("begin")
    try:
        pg.execute("create table rp045 (a int)")
        pg.execute("insert into rp045 values (1)")
        pg.execute("savepoint s")
        pg.execute("declare k cursor for select a from rp045")
        pg.execute("rollback to s")
        answer = pg_answer(pg, "fetch 1 from k")
    finally:
        pg.execute("rollback")
    assert answer[:2] == ("err", "34000")


@pytest.mark.backlog_fixed("RP-046")
def test_prepared_fetch_follows_the_cursors_columns(pg: Any) -> None:
    """PostgreSQL 15.19: one prepared FETCH over a cursor re-declared with
    two columns, each after a savepoint, returns the new cursor's rows;
    here it is 0A000 `cached plan must not change result type`."""
    pg.execute("begin")
    try:
        pg.execute("create table rp046 (a int, b int)")
        pg.execute("insert into rp046 values (1, 2)")
        pg.execute("savepoint s1")
        pg.execute("declare k cursor for select a from rp046")
        pg.execute("fetch 1 from k", prepare=True)
        pg.execute("close k")
        pg.execute("savepoint s2")
        pg.execute("declare k cursor for select a, b from rp046")
        try:
            answer: tuple = ("ok", pg.execute("fetch 1 from k", prepare=True).fetchall())
        except psycopg.Error as e:
            answer = ("err", e.sqlstate)
    finally:
        pg.execute("rollback")
    assert answer == ("ok", [(1, 2)])


@pytest.mark.backlog_fixed("RP-028")
def test_group_by_over_an_analyzed_table_comes_out_in_hash_order(pg: Any) -> None:
    """PostgreSQL 15.19: after ANALYZE the planner estimates the 11 groups
    ANALYZE counted rather than its default 200, so the HashAggregate's
    table is 16 buckets and the 300 rows' groups come out
    8 9 10 7 1 5 4 2 0 6 3."""
    pg.execute("drop table if exists rp028_as")
    pg.execute("begin")
    try:
        pg.execute("create table rp028_as (k int primary key, g int)")
        pg.execute("insert into rp028_as select i, (i * 7) % 11 from generate_series(1, 300) i")
        pg.execute("analyze rp028_as")
        got = pg_answer(pg, "select g from rp028_as group by g")
    finally:
        pg.execute("rollback")
    assert got == ("ok", [(g,) for g in [8, 9, 10, 7, 1, 5, 4, 2, 0, 6, 3]])


@pytest.mark.backlog_fixed("RP-028")
def test_distinct_over_an_analyzed_column_with_nulls_comes_out_in_hash_order(
    pg: Any,
) -> None:
    """PostgreSQL 15.19: a text column a quarter NULL, 9 values, analysed
    over 300 rows: `select distinct t` estimates 9 groups and returns NULL
    n4 n5 n8 n6 n2 n3 n1 n0 n7."""
    pg.execute("drop table if exists rp028_an")
    pg.execute("begin")
    try:
        pg.execute("create table rp028_an (k int, t text)")
        pg.execute(
            "insert into rp028_an select i, case when i % 4 = 0 then null"
            " else 'n' || (i % 9) end from generate_series(1, 300) i"
        )
        pg.execute("analyze rp028_an")
        got = pg_answer(pg, "select distinct t from rp028_an")
    finally:
        pg.execute("rollback")
    want = [None, "n4", "n5", "n8", "n6", "n2", "n3", "n1", "n0", "n7"]
    assert got == ("ok", [(t,) for t in want])


@pytest.mark.backlog_fixed("RP-028")
def test_rows_inserted_after_analyze_keep_its_estimate(pg: Any) -> None:
    """PostgreSQL 15.19: ANALYZE's statistics are a snapshot. Over 20 rows
    it measured b's 3 values (-0.15 of the rows); 10 more rows on the same
    page leave the estimate at 20 rows and 3 groups, so the fourth group
    lands in a 4-bucket table and the groups come out 0 1 3 2."""
    pg.execute("drop table if exists rp028_ap")
    pg.execute("begin")
    try:
        pg.execute("create table rp028_ap (a int, b int)")
        pg.execute("insert into rp028_ap select i, i / 10 from generate_series(1, 20) i")
        pg.execute("analyze rp028_ap")
        pg.execute("insert into rp028_ap select i, i / 10 from generate_series(21, 30) i")
        got = pg_answer(pg, "select b, count(*) from rp028_ap group by b")
    finally:
        pg.execute("rollback")
    assert got == ("ok", [(0, 9), (1, 10), (3, 1), (2, 10)])


@pytest.mark.backlog_fixed("RP-028")
def test_rows_after_the_last_group_grow_the_hash_table(pg: Any) -> None:
    """PostgreSQL 15.19: 7 groups estimated make an 8-bucket table that
    grows at 7 members; the rows after the seventh group's first look it
    up and grow it (`SH_INSERT` checks the fill on every lookup), so 300
    analysed rows of `i % 7` come out 1 5 4 2 0 6 3."""
    pg.execute("drop table if exists rp028_aq")
    pg.execute("begin")
    try:
        pg.execute("create table rp028_aq (k int, g int)")
        pg.execute("insert into rp028_aq select i, i % 7 from generate_series(1, 300) i")
        pg.execute("analyze rp028_aq")
        got = pg_answer(pg, "select g from rp028_aq group by g")
    finally:
        pg.execute("rollback")
    assert got == ("ok", [(g,) for g in [1, 5, 4, 2, 0, 6, 3]])


def _analyzed(pg: Any, table: str, setup: list[str], sql: str) -> Any:
    pg.execute(f"drop table if exists {table}")
    pg.execute("begin")
    try:
        for statement in setup:
            pg.execute(statement)
        return pg_answer(pg, sql)
    finally:
        pg.execute("rollback")


@pytest.mark.backlog_fixed("RP-028")
def test_where_over_an_analyzed_table_estimates_from_its_histogram(pg: Any) -> None:
    """PostgreSQL 15.19: over 300 analysed rows `k > 5` keeps 295 by the
    histogram of k (`scalarineqsel`, its end read from the index), the 11
    groups of g fill an estimate of 11, and the HashAggregate returns
    8 10 7 9 1 5 2 4 6 0 3."""
    got = _analyzed(
        pg,
        "rp028_aw",
        [
            "create table rp028_aw (k int primary key, g int, t text)",
            "insert into rp028_aw select i, (i * 7) % 11, 'x' || (i % 6)"
            " from generate_series(1, 300) i",
            "analyze rp028_aw",
        ],
        "select g from rp028_aw where k > 5 group by g",
    )
    assert got == ("ok", [(g,) for g in [8, 10, 7, 9, 1, 5, 2, 4, 6, 0, 3]])


@pytest.mark.backlog_fixed("RP-028")
def test_where_over_an_analyzed_table_reads_a_correlated_index(pg: Any) -> None:
    """PostgreSQL 15.19: over 3,000 analysed rows `k < 10` is 9 rows by the
    histogram, read by an Index Scan its correlation of 1 makes cheapest;
    6 groups are estimated, and the 9 come out 8 9 10 7 1 5 2 6 3."""
    got = _analyzed(
        pg,
        "rp028_ax",
        [
            "create table rp028_ax (k int primary key, g int)",
            "insert into rp028_ax select i, (i * 7) % 11 from generate_series(1, 3000) i",
            "analyze rp028_ax",
        ],
        "select g from rp028_ax where k < 10 group by g",
    )
    assert got == ("ok", [(g,) for g in [8, 9, 10, 7, 1, 5, 2, 6, 3]])


@pytest.mark.backlog_fixed("RP-028")
def test_group_over_a_correlated_index_scan_comes_out_sorted(pg: Any) -> None:
    """PostgreSQL 15.19: over 3,000 analysed rows `where k < 50 group by k`
    is a Group over an Index Only Scan in k's order, cheaper than hashing
    once the index is correlated, so the groups come out 1 to 49."""
    got = _analyzed(
        pg,
        "rp028_ax",
        [
            "create table rp028_ax (k int primary key, g int)",
            "insert into rp028_ax select i, (i * 7) % 11 from generate_series(1, 3000) i",
            "analyze rp028_ax",
        ],
        "select k from rp028_ax where k < 50 group by k",
    )
    assert got == ("ok", [(k,) for k in range(1, 50)])


@pytest.mark.backlog_fixed("RP-028")
def test_where_on_a_composite_key_column_estimates_from_its_mcvs(pg: Any) -> None:
    """PostgreSQL 15.19: `a = 3` over 300 analysed rows of a table keyed
    (a, b) is 30 rows by a's most common values; 14 groups of v are
    estimated, and the HashAggregate returns 4 0 14 3 7 13 9 10 1 5 2 15
    16 6 12 11 8."""
    got = _analyzed(
        pg,
        "rp028_ac",
        [
            "create table rp028_ac (a int, b int, v int, primary key (a, b))",
            "insert into rp028_ac select i % 10, i / 10, (i * 13) % 17"
            " from generate_series(1, 300) i",
            "analyze rp028_ac",
        ],
        "select v from rp028_ac where a = 3 group by v",
    )
    want = [4, 0, 14, 3, 7, 13, 9, 10, 1, 5, 2, 15, 16, 6, 12, 11, 8]
    assert got == ("ok", [(v,) for v in want])


@pytest.mark.backlog_fixed("RP-028")
def test_in_list_over_an_analyzed_table_sums_its_elements(pg: Any) -> None:
    """PostgreSQL 15.19: `g in (1, 2, 3)` over 300 analysed rows sums the three
    values' MCV frequencies (`scalararraysel`), 82 rows, and the 6 groups of t
    come out x2 x0 x3 x5 x4 x1."""
    got = _analyzed(
        pg,
        "rp028_bi",
        [
            "create table rp028_bi (k int primary key, g int, t text)",
            "insert into rp028_bi select i, (i * 7) % 11, 'x' || (i % 6)"
            " from generate_series(1, 300) i",
            "analyze rp028_bi",
        ],
        "select t from rp028_bi where g in (1, 2, 3) group by t",
    )
    assert got == ("ok", [(t,) for t in ["x2", "x0", "x3", "x5", "x4", "x1"]])


@pytest.mark.backlog_fixed("RP-028")
def test_or_over_an_analyzed_table_combines_its_arms(pg: Any) -> None:
    """PostgreSQL 15.19: `g = 3 or h = 1` over 300 analysed rows is
    `s1 + s2 - s1 * s2` of the arms (101 rows); the 4 groups of h come out
    0 1 3 2."""
    got = _analyzed(
        pg,
        "rp028_bn",
        [
            "create table rp028_bn (k int primary key, g int, h int)",
            "insert into rp028_bn select i, case when i % 5 = 0 then null"
            " else (i * 3) % 7 end, i % 4 from generate_series(1, 300) i",
            "analyze rp028_bn",
        ],
        "select h from rp028_bn where g = 3 or h = 1 group by h",
    )
    assert got == ("ok", [(h,) for h in [0, 1, 3, 2]])


@pytest.mark.backlog_fixed("RP-028")
def test_is_null_over_an_analyzed_table_reads_its_null_fraction(pg: Any) -> None:
    """PostgreSQL 15.19: `g is null` over 300 analysed rows is g's null
    fraction, 60 rows (`nulltestsel`); the 4 groups of h come out 0 1 3 2."""
    got = _analyzed(
        pg,
        "rp028_bn",
        [
            "create table rp028_bn (k int primary key, g int, h int)",
            "insert into rp028_bn select i, case when i % 5 = 0 then null"
            " else (i * 3) % 7 end, i % 4 from generate_series(1, 300) i",
            "analyze rp028_bn",
        ],
        "select h from rp028_bn where g is null group by h",
    )
    assert got == ("ok", [(h,) for h in [0, 1, 3, 2]])


@pytest.mark.backlog_fixed("RP-028")
def test_where_under_a_limit_over_an_analyzed_table(pg: Any) -> None:
    """PostgreSQL 15.19: `where k > 5 group by g limit 3` over 300 analysed
    rows takes the first three groups of the HashAggregate it prices for
    the LIMIT: 8 10 7."""
    got = _analyzed(
        pg,
        "rp028_bi",
        [
            "create table rp028_bi (k int primary key, g int, t text)",
            "insert into rp028_bi select i, (i * 7) % 11, 'x' || (i % 6)"
            " from generate_series(1, 300) i",
            "analyze rp028_bi",
        ],
        "select g from rp028_bi where k > 5 group by g limit 3",
    )
    assert got == ("ok", [(g,) for g in [8, 10, 7]])


@pytest.mark.backlog_fixed("RP-028")
def test_bitmap_or_over_an_analyzed_table_is_priced(pg: Any) -> None:
    """PostgreSQL 15.19: `k < 5 or k > 2995` over 3,000 analysed rows reads a
    BitmapOr of two index scans on the key, and its 9 rows sort for a
    Group: 1 2 3 5 6 7 9 10."""
    got = _analyzed(
        pg,
        "rp028_bo",
        [
            "create table rp028_bo (k int primary key, g int)",
            "insert into rp028_bo select i, (i * 7) % 11 from generate_series(1, 3000) i",
            "analyze rp028_bo",
        ],
        "select g from rp028_bo where k < 5 or k > 2995 group by g",
    )
    assert got == ("ok", [(g,) for g in [1, 2, 3, 5, 6, 7, 9, 10]])


@pytest.mark.backlog_fixed("RP-028")
def test_not_in_over_an_analyzed_table_excludes_each_value(pg: Any) -> None:
    """PostgreSQL 15.19: `g not in (1, 2, 3)` over 300 analysed rows is
    `<> ALL`, each value's exclusion summed (`scalararraysel`), 218 rows;
    the 6 groups of t come out x2 x0 x3 x5 x4 x1."""
    got = _analyzed(
        pg,
        "rp028_ci",
        [
            "create table rp028_ci (k int primary key, g int, t text)",
            "insert into rp028_ci select i, (i * 7) % 11, 'x' || (i % 6)"
            " from generate_series(1, 300) i",
            "analyze rp028_ci",
        ],
        "select t from rp028_ci where g not in (1, 2, 3) group by t",
    )
    assert got == ("ok", [(t,) for t in ["x2", "x0", "x3", "x5", "x4", "x1"]])


@pytest.mark.backlog_fixed("RP-028")
def test_hashed_in_list_over_an_analyzed_table(pg: Any) -> None:
    """PostgreSQL 15.19: an `IN` list of nine values is a hashed
    ScalarArrayOpExpr, with a startup of one hash a value; 245 rows, and
    the groups of t come out x2 x0 x3 x5 x4 x1."""
    got = _analyzed(
        pg,
        "rp028_ci",
        [
            "create table rp028_ci (k int primary key, g int, t text)",
            "insert into rp028_ci select i, (i * 7) % 11, 'x' || (i % 6)"
            " from generate_series(1, 300) i",
            "analyze rp028_ci",
        ],
        "select t from rp028_ci where g in (0, 1, 2, 3, 4, 5, 6, 7, 8) group by t",
    )
    assert got == ("ok", [(t,) for t in ["x2", "x0", "x3", "x5", "x4", "x1"]])


@pytest.mark.backlog_fixed("RP-028")
def test_in_list_with_a_repeated_value_counts_it_twice(pg: Any) -> None:
    """PostgreSQL 15.19: `g in (1, 1, 2)` sums all three elements' selectivities,
    81 rows; the groups of t come out x2 x0 x3 x5 x1 x4."""
    got = _analyzed(
        pg,
        "rp028_ci",
        [
            "create table rp028_ci (k int primary key, g int, t text)",
            "insert into rp028_ci select i, (i * 7) % 11, 'x' || (i % 6)"
            " from generate_series(1, 300) i",
            "analyze rp028_ci",
        ],
        "select t from rp028_ci where g in (1, 1, 2) group by t",
    )
    assert got == ("ok", [(t,) for t in ["x2", "x0", "x3", "x5", "x1", "x4"]])


@pytest.mark.backlog_fixed("RP-028")
def test_or_with_a_repeated_arm_keeps_it(pg: Any) -> None:
    """PostgreSQL 15.19: `g = 1 or g = 1 or g = 2` keeps the repeated arm
    (`process_duplicate_ors` finds no clause every arm shares), 74 rows; the
    groups of t come out x2 x0 x3 x5 x1 x4."""
    got = _analyzed(
        pg,
        "rp028_ci",
        [
            "create table rp028_ci (k int primary key, g int, t text)",
            "insert into rp028_ci select i, (i * 7) % 11, 'x' || (i % 6)"
            " from generate_series(1, 300) i",
            "analyze rp028_ci",
        ],
        "select t from rp028_ci where g = 1 or g = 1 or g = 2 group by t",
    )
    assert got == ("ok", [(t,) for t in ["x2", "x0", "x3", "x5", "x1", "x4"]])


@pytest.mark.backlog_fixed("RP-028")
def test_null_test_on_an_indexed_column_reads_the_index(pg: Any) -> None:
    """PostgreSQL 15.19: `g is null` over 3,000 analysed rows with an index on
    g is a bitmap index scan on the null test, 600 rows; the 4 groups of h
    come out 0 1 3 2."""
    got = _analyzed(
        pg,
        "rp028_cn",
        [
            "create table rp028_cn (k int primary key, g int, h int)",
            "create index rp028_cn_g on rp028_cn (g)",
            "insert into rp028_cn select i, case when i % 5 = 0 then null"
            " else (i * 3) % 7 end, i % 4 from generate_series(1, 3000) i",
            "analyze rp028_cn",
        ],
        "select h from rp028_cn where g is null group by h",
    )
    assert got == ("ok", [(h,) for h in [0, 1, 3, 2]])


@pytest.mark.backlog_fixed("RP-028")
def test_bitmap_or_arm_on_two_columns_is_a_bitmap_and(pg: Any) -> None:
    """PostgreSQL 15.19: `(g = 1 and k < 100) or k > 9000` over 10,000 analysed
    rows reads a BitmapOr whose first arm is a BitmapAnd of both indexes
    (`choose_bitmap_and`), 1,008 rows; the groups of t come out x2 x0 x3
    x5 x4 x1."""
    got = _analyzed(
        pg,
        "rp028_cb",
        [
            "create table rp028_cb (k int primary key, g int, t text)",
            "create index rp028_cb_g on rp028_cb (g)",
            "insert into rp028_cb select i, (i * 7) % 11, 'x' || (i % 6)"
            " from generate_series(1, 10000) i",
            "analyze rp028_cb",
        ],
        "select t from rp028_cb where (g = 1 and k < 100) or k > 9000 group by t",
    )
    assert got == ("ok", [(t,) for t in ["x2", "x0", "x3", "x5", "x4", "x1"]])


@pytest.mark.backlog_fixed("RP-028")
def test_vacuum_makes_a_distinct_read_the_index_only(pg: Any) -> None:
    """PostgreSQL 15.19 (a private cluster: VACUUM cannot run in a
    transaction block): after VACUUM every page is all-visible
    (`relallvisible`), so `select distinct g ... limit 3` over 300 rows is
    a Unique over an Index Only Scan that fetches no heap page, and returns
    0 1 2."""
    pg.execute("drop table if exists rp028_cv")
    try:
        pg.execute("create table rp028_cv (k int primary key, g int, t text)")
        pg.execute("create index rp028_cv_g on rp028_cv (g)")
        pg.execute(
            "insert into rp028_cv select i, (i * 7) % 11, 'x' || (i % 6)"
            " from generate_series(1, 300) i"
        )
        pg.execute("vacuum rp028_cv")
        got = pg_answer(pg, "select distinct g from rp028_cv limit 3")
    finally:
        pg.execute("drop table if exists rp028_cv")
    assert got == ("ok", [(0,), (1,), (2,)])


@pytest.mark.backlog_fixed("RP-028")
def test_many_to_many_merge_join_leaves_ties_as_its_sorts_do(pg: Any) -> None:
    """PostgreSQL 15.19: `on l.b = r.c` over two fresh keyed tables is a
    Merge Join over two Sorts, so equal keys come out as PostgreSQL's
    quicksort leaves each side (7 then 14 on the left, 5 20 15 10 on the
    right)."""
    pg.execute("begin")
    try:
        pg.execute("create table rp028_jl (a int primary key, b int)")
        pg.execute("insert into rp028_jl select i, i % 7 from generate_series(1, 20) i")
        pg.execute("create table rp028_jr (a int primary key, c int)")
        pg.execute("insert into rp028_jr select i, i % 5 from generate_series(1, 20) i")
        got = pg_answer(
            pg,
            "select rp028_jl.a, rp028_jr.a from rp028_jl join rp028_jr on rp028_jl.b = rp028_jr.c",
        )
    finally:
        pg.execute("rollback")
    want = "7:5 7:20 7:15 7:10 14:5 14:20 14:15 14:10 15:11 15:6 15:1 15:16"
    assert got[0] == "ok"
    assert " ".join(f"{a}:{b}" for a, b in got[1][:12]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_merge_join_under_a_where_sorts_the_rows_it_keeps(pg: Any) -> None:
    """PostgreSQL 15.19: with `where l.a < 10` the merge join sorts only the
    left rows the WHERE keeps (a bitmap heap scan's), so their ties come out
    7 8 1 9 2 3 4."""
    pg.execute("begin")
    try:
        pg.execute("create table rp028_jl (a int primary key, b int)")
        pg.execute("insert into rp028_jl select i, i % 7 from generate_series(1, 20) i")
        pg.execute("create table rp028_jr (a int primary key, c int)")
        pg.execute("insert into rp028_jr select i, i % 5 from generate_series(1, 20) i")
        got = pg_answer(
            pg,
            "select rp028_jl.a, rp028_jr.a from rp028_jl join rp028_jr"
            " on rp028_jl.b = rp028_jr.c where rp028_jl.a < 10",
        )
    finally:
        pg.execute("rollback")
    want = "7:5 7:20 7:15 7:10 8:11 8:6 8:1 8:16 1:11 1:6 1:1 1:16"
    assert got[0] == "ok"
    assert " ".join(f"{a}:{b}" for a, b in got[1][:12]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_merge_join_on_an_expression_sorts_by_its_value(pg: Any) -> None:
    """PostgreSQL 15.19: `on l.b = r.c + 3` is a Merge Join over two Sorts,
    the right one by `c + 3`, so the rows come out in key order, each key's
    right rows as PostgreSQL's quicksort leaves them (5 20 15 10)."""
    pg.execute("begin")
    try:
        pg.execute("create table rp028_jl (a int primary key, b int)")
        pg.execute("insert into rp028_jl select i, i % 7 from generate_series(1, 20) i")
        pg.execute("create table rp028_jr (a int primary key, c int)")
        pg.execute("insert into rp028_jr select i, i % 5 from generate_series(1, 20) i")
        got = pg_answer(
            pg,
            "select rp028_jl.a, rp028_jr.a from rp028_jl join rp028_jr"
            " on rp028_jl.b = rp028_jr.c + 3",
        )
    finally:
        pg.execute("rollback")
    want = "3:5 3:20 3:15 3:10 10:5 10:20 10:15 10:10"
    assert got[0] == "ok"
    assert " ".join(f"{a}:{b}" for a, b in got[1][:8]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_join_with_a_second_equality_between_its_sides(pg: Any) -> None:
    """PostgreSQL 15.19: `on l.b = r.c where r.c = l.a` puts all three
    columns in one equivalence class, so `l.a = l.b` becomes a filter on
    the left side and the join a Hash Join with the left side hashed: the
    rows come out in the right side's scan order (1:1 2:2 3:3 4:4 1:6 ...)."""
    pg.execute("begin")
    try:
        pg.execute("create table rp028_jl (a int primary key, b int)")
        pg.execute("insert into rp028_jl select i, i % 7 from generate_series(1, 20) i")
        pg.execute("create table rp028_jr (a int primary key, c int)")
        pg.execute("insert into rp028_jr select i, i % 5 from generate_series(1, 20) i")
        got = pg_answer(
            pg,
            "select rp028_jl.a, rp028_jr.a from rp028_jl join rp028_jr"
            " on rp028_jl.b = rp028_jr.c where rp028_jr.c = rp028_jl.a",
        )
    finally:
        pg.execute("rollback")
    want = "1:1 2:2 3:3 4:4 1:6 2:7 3:8 4:9"
    assert got[0] == "ok"
    assert " ".join(f"{a}:{b}" for a, b in got[1][:8]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_three_table_left_join_chain_merges_in_key_order(pg: Any) -> None:
    """PostgreSQL 15.19: `l LEFT JOIN r ON l.b = r.c LEFT JOIN m ON r.a =
    m.a` joins r and m first (a Hash Right Join), sorts that by `r.c` and
    merges it with l sorted by `b`, so the rows come out in key order (7
    then 14 on the left, 5 20 15 10 on the right)."""
    pg.execute("begin")
    try:
        pg.execute("create table rp028_jl (a int primary key, b int)")
        pg.execute("insert into rp028_jl select i, i % 7 from generate_series(1, 20) i")
        pg.execute("create table rp028_jr (a int primary key, c int)")
        pg.execute("insert into rp028_jr select i, i % 5 from generate_series(1, 20) i")
        pg.execute("create table rp028_jm (a int, d int)")
        pg.execute("insert into rp028_jm select i, i % 3 from generate_series(1, 20) i")
        got = pg_answer(
            pg,
            "select rp028_jl.a, rp028_jr.a, rp028_jm.a from rp028_jl"
            " left join rp028_jr on rp028_jl.b = rp028_jr.c"
            " left join rp028_jm on rp028_jr.a = rp028_jm.a",
        )
    finally:
        pg.execute("rollback")
    want = "7:5:5 7:20:20 7:15:15 7:10:10 14:5:5 14:20:20 14:15:15 14:10:10"
    assert got[0] == "ok"
    assert " ".join(f"{a}:{b}:{c}" for a, b, c in got[1][:8]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_three_tables_on_one_equivalence_class(pg: Any) -> None:
    """PostgreSQL 15.19: `l JOIN r ON l.b = r.c JOIN m ON l.b = m.d` puts the
    three keys in one equivalence class, so any two tables can join first;
    the plan merges l and r, materialises that, and merges it with m sorted
    by `d` as the outer side (7:5:15 7:20:15 ...)."""
    pg.execute("begin")
    try:
        pg.execute("create table rp028_jl (a int primary key, b int)")
        pg.execute("insert into rp028_jl select i, i % 7 from generate_series(1, 20) i")
        pg.execute("create table rp028_jr (a int primary key, c int)")
        pg.execute("insert into rp028_jr select i, i % 5 from generate_series(1, 20) i")
        pg.execute("create table rp028_jm (a int, d int)")
        pg.execute("insert into rp028_jm select i, i % 3 from generate_series(1, 20) i")
        got = pg_answer(
            pg,
            "select rp028_jl.a, rp028_jr.a, rp028_jm.a from rp028_jl"
            " join rp028_jr on rp028_jl.b = rp028_jr.c"
            " join rp028_jm on rp028_jl.b = rp028_jm.d",
        )
    finally:
        pg.execute("rollback")
    want = "7:5:15 7:20:15 7:15:15 7:10:15 14:5:15 14:20:15 14:15:15"
    assert got[0] == "ok"
    assert " ".join(f"{a}:{b}:{c}" for a, b, c in got[1][:7]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_four_table_chain_joins_in_the_searched_order(pg: Any) -> None:
    """PostgreSQL 15.19: a chain of four tables, `l JOIN r ON l.b = r.c JOIN
    m ON r.a = m.a JOIN k ON m.d = k.e`, is planned by the join order search
    over four relations: m and k merge first, l and r merge, and the two
    hash join with l-r hashed (14:15:15:12 7:15:15:12 ...)."""
    pg.execute("begin")
    try:
        pg.execute("create table rp028_jl (a int primary key, b int)")
        pg.execute("insert into rp028_jl select i, i % 7 from generate_series(1, 20) i")
        pg.execute("create table rp028_jr (a int primary key, c int)")
        pg.execute("insert into rp028_jr select i, i % 5 from generate_series(1, 20) i")
        pg.execute("create table rp028_jm (a int, d int)")
        pg.execute("insert into rp028_jm select i, i % 3 from generate_series(1, 20) i")
        pg.execute("create table rp028_jk (a int, e int)")
        pg.execute("insert into rp028_jk select i, i % 4 from generate_series(1, 20) i")
        got = pg_answer(
            pg,
            "select rp028_jl.a, rp028_jr.a, rp028_jm.a, rp028_jk.a from rp028_jl"
            " join rp028_jr on rp028_jl.b = rp028_jr.c"
            " join rp028_jm on rp028_jr.a = rp028_jm.a"
            " join rp028_jk on rp028_jm.d = rp028_jk.e",
        )
    finally:
        pg.execute("rollback")
    want = "14:15:15:12 7:15:15:12 14:15:15:8 7:15:15:8 14:15:15:16 7:15:15:16"
    assert got[0] == "ok"
    assert " ".join(f"{a}:{b}:{c}:{d}" for a, b, c, d in got[1][:6]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_four_table_left_join_chain_joins_in_the_searched_order(pg: Any) -> None:
    """PostgreSQL 15.19: the same four tables as LEFT joins, `l LEFT JOIN r
    ... LEFT JOIN m ... LEFT JOIN k`, plan as two LEFT merge joins, m-k and
    l-r, joined by a Hash Right Join with l-r hashed (14:15:15:12
    7:15:15:12 ...)."""
    pg.execute("begin")
    try:
        pg.execute("create table rp028_jl (a int primary key, b int)")
        pg.execute("insert into rp028_jl select i, i % 7 from generate_series(1, 20) i")
        pg.execute("create table rp028_jr (a int primary key, c int)")
        pg.execute("insert into rp028_jr select i, i % 5 from generate_series(1, 20) i")
        pg.execute("create table rp028_jm (a int, d int)")
        pg.execute("insert into rp028_jm select i, i % 3 from generate_series(1, 20) i")
        pg.execute("create table rp028_jk (a int, e int)")
        pg.execute("insert into rp028_jk select i, i % 4 from generate_series(1, 20) i")
        got = pg_answer(
            pg,
            "select rp028_jl.a, rp028_jr.a, rp028_jm.a, rp028_jk.a from rp028_jl"
            " left join rp028_jr on rp028_jl.b = rp028_jr.c"
            " left join rp028_jm on rp028_jr.a = rp028_jm.a"
            " left join rp028_jk on rp028_jm.d = rp028_jk.e",
        )
    finally:
        pg.execute("rollback")
    want = "14:15:15:12 7:15:15:12 14:15:15:8 7:15:15:8 14:15:15:16 7:15:15:16"
    assert got[0] == "ok"
    assert " ".join(f"{a}:{b}:{c}:{d}" for a, b, c, d in got[1][:6]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_four_tables_with_a_shared_key_join_on_its_class(pg: Any) -> None:
    """PostgreSQL 15.19: `l JOIN r ON l.b = r.c JOIN m ON r.a = m.a JOIN k
    ON m.a = k.a` puts r.a, m.a and k.a in one equivalence class, so r, m
    and k hash join first (k probing r-m) and that, sorted by r.c, merges
    with l (7:5:5:5 14:5:5:5 7:20:20:20 ...). Fixed 2026-10-19: the
    four-table search plans the class's derived clauses."""
    pg.execute("begin")
    try:
        pg.execute("create table rp028_jl (a int primary key, b int)")
        pg.execute("insert into rp028_jl select i, i % 7 from generate_series(1, 20) i")
        pg.execute("create table rp028_jr (a int primary key, c int)")
        pg.execute("insert into rp028_jr select i, i % 5 from generate_series(1, 20) i")
        pg.execute("create table rp028_jm (a int, d int)")
        pg.execute("insert into rp028_jm select i, i % 3 from generate_series(1, 20) i")
        pg.execute("create table rp028_jk (a int, e int)")
        pg.execute("insert into rp028_jk select i, i % 4 from generate_series(1, 20) i")
        got = pg_answer(
            pg,
            "select rp028_jl.a, rp028_jr.a, rp028_jm.a, rp028_jk.a from rp028_jl"
            " join rp028_jr on rp028_jl.b = rp028_jr.c"
            " join rp028_jm on rp028_jr.a = rp028_jm.a"
            " join rp028_jk on rp028_jm.a = rp028_jk.a",
        )
    finally:
        pg.execute("rollback")
    want = "7:5:5:5 14:5:5:5 7:20:20:20 14:20:20:20 7:15:15:15 14:15:15:15"
    assert got[0] == "ok"
    assert " ".join(f"{a}:{b}:{c}:{d}" for a, b, c, d in got[1][:6]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_five_tables_read_a_classes_sorted_join_under_a_sorted_outer(pg: Any) -> None:
    """PostgreSQL 15.19: five tables, `k1.a`, `k2.a` and `k3.a` one
    equivalence class, no primary keys, 20 rows each. The top join merges
    k0-k1, explicitly sorted by k1.a, with k2-k3-k4 READ PRESORTED in the
    class's order (`Materialize` over its merge on k2.a = k3.a), although
    that path ties another of k2-k3-k4's as its cheapest. Here the search
    takes the mirror image, k3-k4 sorted under k0-k1-k2, at the same cost,
    and the rows tied on the class come out the other way round."""
    pg.execute("begin")
    try:
        mods = [(7, 5), (5, 3), (3, 4), (4, 7), (6, 5)]
        for t, (mb, mc) in enumerate(mods):
            pg.execute(f"create table rp028_k{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_k{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_k0.a, rp028_k1.a, rp028_k2.a, rp028_k3.a, rp028_k4.a"
            " from rp028_k0 join rp028_k1 on rp028_k0.b = rp028_k1.c"
            " join rp028_k2 on rp028_k1.a = rp028_k2.a"
            " join rp028_k3 on rp028_k2.a = rp028_k3.a"
            " join rp028_k4 on rp028_k3.b = rp028_k4.c",
        )
    finally:
        pg.execute("rollback")
    want = "15:1:1:1:11 15:1:1:1:16 15:1:1:1:1 15:1:1:1:6 8:1:1:1:11 8:1:1:1:16"
    assert got[0] == "ok"
    assert " ".join(":".join(str(v) for v in r) for r in got[1][:6]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_a_hash_join_too_big_for_work_mem_returns_rows_batch_by_batch(pg: Any) -> None:
    """PostgreSQL 15.19: the same five-table chain with `rp028_h1.a` a
    primary key and the class's first ON written `h2.a = h1.a`. The top join
    hashes the 212,242 rows estimated for h0-h1-h3-h4 (width 16), more than
    `work_mem` x `hash_mem_multiplier` holds, so the hash join runs in
    batches (`ExecChooseHashTableSize`): an outer h2 row whose hash falls in
    a later batch is joined after every row of batch 0. So after the
    `h2.a = 1` rows come `h2.a = 3`'s, where a single batch gives `2`."""
    pg.execute("begin")
    try:
        mods = [(7, 5), (5, 3), (3, 4), (4, 7), (6, 5)]
        for t, (mb, mc) in enumerate(mods):
            key = " primary key" if t == 1 else ""
            pg.execute(f"create table rp028_h{t} (a int{key}, b int, c int)")
            pg.execute(
                f"insert into rp028_h{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_h0.a, rp028_h1.a, rp028_h2.a, rp028_h3.a, rp028_h4.a"
            " from rp028_h0 join rp028_h1 on rp028_h0.b = rp028_h1.c"
            " join rp028_h2 on rp028_h2.a = rp028_h1.a"
            " join rp028_h3 on rp028_h2.a = rp028_h3.a"
            " join rp028_h4 on rp028_h3.b = rp028_h4.c",
        )
    finally:
        pg.execute("rollback")
    want = (
        "15:1:1:1:16 8:1:1:1:16 1:1:1:1:16 15:1:1:1:1 8:1:1:1:1 1:1:1:1:1"
        " 15:1:1:1:6 8:1:1:1:6 1:1:1:1:6 15:1:1:1:11 8:1:1:1:11 1:1:1:1:11"
        " 7:3:3:3:18 14:3:3:3:18 7:3:3:3:3 14:3:3:3:3 7:3:3:3:13 14:3:3:3:13"
    )
    assert got[0] == "ok"
    assert len(got[1]) == 216
    assert " ".join(":".join(str(v) for v in r) for r in got[1][:18]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_a_right_join_of_two_tables_is_planned_as_postgresql_plans_it(pg: Any) -> None:
    """PostgreSQL 15.19, two 20-row tables never analysed: `r0 RIGHT JOIN
    r1 on r0.b = r1.c` is a Merge Left Join with r1 outer, each side sorted
    on its key, and the rows come out in that merge's order. The general
    join path does not plan a two-table RIGHT join, and returns the same
    54 rows in another order."""
    pg.execute("begin")
    try:
        for t, (mb, mc) in enumerate([(7, 5), (5, 3)]):
            pg.execute(f"create table rp028_r{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_r{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_r0.a, rp028_r1.a from rp028_r0 right join rp028_r1"
            " on rp028_r0.b = rp028_r1.c",
        )
    finally:
        pg.execute("rollback")
    want = "7:15 14:15 7:9 14:9 7:18 14:18 7:3 14:3 7:12 14:12 7:6 14:6 15:10 8:10 1:10 15:13"
    assert got[0] == "ok"
    assert len(got[1]) == 54
    assert " ".join(":".join(str(v) for v in r) for r in got[1][:16]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_a_right_join_over_a_join_is_searched_as_its_left_join(pg: Any) -> None:
    """PostgreSQL 15.19, three 20-row tables never analysed: `s0 JOIN s1
    RIGHT JOIN s2` is flipped to `s2 LEFT JOIN (s0 JOIN s1)`
    (`reduce_outer_joins`) and searched like any other three tables -- a
    Merge Left Join of s2 sorted on b with s0-s1's merge sorted on s1.a. The
    LEFT join's nullable side is then a join, not a table, which the
    three-table search does not take, so the rows come out in the general
    path's order."""
    pg.execute("begin")
    try:
        for t, (mb, mc) in enumerate([(7, 5), (5, 3), (3, 4)]):
            pg.execute(f"create table rp028_s{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_s{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_s0.a, rp028_s1.a, rp028_s2.a from rp028_s0 join rp028_s1"
            " on rp028_s0.b = rp028_s1.c right join rp028_s2 on rp028_s1.a = rp028_s2.b",
        )
    finally:
        pg.execute("rollback")
    want = (
        "None:None:15 None:None:9 None:None:18 None:None:3 None:None:12 None:None:6"
        " 15:1:10 8:1:10 1:1:10 15:1:13 8:1:13 1:1:13"
    )
    assert got[0] == "ok"
    assert len(got[1]) == 48
    assert " ".join(":".join(str(v) for v in r) for r in got[1][:12]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_a_left_join_whose_nullable_side_holds_a_left_join_is_searched(pg: Any) -> None:
    """PostgreSQL 15.19, four 20-row tables never analysed: `l0 LEFT JOIN
    (l1 LEFT JOIN l2) JOIN l3` is searched like any four tables, under
    `make_outerjoininfo`'s rules for an outer join inside another's
    nullable side -- here a Merge Left Join of l0-l3's merge, sorted on
    l0.b, with l1-l2."""
    pg.execute("begin")
    try:
        for t, (mb, mc) in enumerate([(7, 5), (5, 3), (3, 4), (4, 7)]):
            pg.execute(f"create table rp028_l{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_l{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_l0.a, rp028_l1.a, rp028_l2.a, rp028_l3.a from rp028_l0 left join"
            " (rp028_l1 left join rp028_l2 on rp028_l1.a = rp028_l2.b)"
            " on rp028_l0.b = rp028_l1.c join rp028_l3 on rp028_l0.a = rp028_l3.b",
        )
    finally:
        pg.execute("rollback")
    want = (
        "1:1:4:1 1:19:None:1 1:1:10:1 1:1:13:1 1:4:None:1 1:1:16:1"
        " 1:1:7:1 1:7:None:1 1:10:None:1 1:1:19:1 1:13:None:1 1:1:1:1"
    )
    assert got[0] == "ok"
    assert len(got[1]) == 135
    assert " ".join(":".join(str(v) for v in r) for r in got[1][:12]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_a_full_join_is_planned(pg: Any) -> None:
    """PostgreSQL 15.19, two 20-row tables never analysed: `f0 FULL JOIN f1
    ON f0.b = f1.a` is a Merge Full Join of both sides sorted on the key,
    ties as quicksort leaves them, then the rows of f1 nothing matched."""
    pg.execute("begin")
    try:
        for t, m in enumerate([7, 5]):
            pg.execute(f"create table rp028_f{t} (a int, b int)")
            pg.execute(f"insert into rp028_f{t} select i, i % {m} from generate_series(1, 20) i")
        got = pg_answer(
            pg,
            "select rp028_f0.a, rp028_f1.a from rp028_f0 full join rp028_f1"
            " on rp028_f0.b = rp028_f1.a",
        )
    finally:
        pg.execute("rollback")
    want = "7:None 14:None 15:1 8:1 1:1 16:2 9:2 2:2 3:3 10:3 17:3 11:4"
    assert got[0] == "ok"
    assert len(got[1]) == 34
    assert " ".join(":".join(str(v) for v in r) for r in got[1][:12]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_a_full_join_under_an_inner_join_is_searched(pg: Any) -> None:
    """PostgreSQL 15.19, three 20-row tables never analysed: `f0 FULL JOIN
    f1 ON f0.b = f1.a JOIN f2 ON f1.b = f2.a`. The inner join's clause makes
    f1 non-null, so `reduce_outer_joins` leaves `f1 LEFT JOIN f0`, and the
    join search makes it a Merge Join of f2 with that Merge Left Join sorted
    on f1.b."""
    pg.execute("begin")
    try:
        for t, m in enumerate([7, 5, 3]):
            pg.execute(f"create table rp028_f{t} (a int, b int)")
            pg.execute(f"insert into rp028_f{t} select i, i % {m} from generate_series(1, 20) i")
        got = pg_answer(
            pg,
            "select rp028_f0.a, rp028_f1.a, rp028_f2.a from rp028_f0 full join rp028_f1"
            " on rp028_f0.b = rp028_f1.a join rp028_f2 on rp028_f1.b = rp028_f2.a",
        )
    finally:
        pg.execute("rollback")
    want = "20:6:1 8:1:1 1:1:1 6:6:1 15:1:1 13:6:1 None:11:1 None:16:1 9:2:2 2:2:2 None:12:2 16:2:2"
    assert got[0] == "ok"
    assert len(got[1]) == 26
    assert " ".join(":".join(str(v) for v in r) for r in got[1][:12]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_a_full_join_below_a_left_join_is_searched(pg: Any) -> None:
    """PostgreSQL 15.19, three 20-row tables never analysed: `f0 FULL JOIN
    f1 ON f0.b = f1.a LEFT JOIN f2 ON f1.b = f2.a`. Nothing above the FULL
    join proves either side non-NULL, so it stays FULL: a Merge Right Join
    of f2 with the Merge Full Join sorted on f1.b."""
    pg.execute("begin")
    try:
        for t, m in enumerate([7, 5, 3]):
            pg.execute(f"create table rp028_f{t} (a int, b int)")
            pg.execute(f"insert into rp028_f{t} select i, i % {m} from generate_series(1, 20) i")
        got = pg_answer(
            pg,
            "select rp028_f0.a, rp028_f1.a, rp028_f2.a from rp028_f0 full join rp028_f1"
            " on rp028_f0.b = rp028_f1.a left join rp028_f2 on rp028_f1.b = rp028_f2.a",
        )
    finally:
        pg.execute("rollback")
    want = (
        "None:10:None 19:5:None 5:5:None 12:5:None None:20:None None:15:None"
        " 6:6:1 15:1:1 8:1:1 1:1:1 20:6:1 13:6:1"
    )
    assert got[0] == "ok"
    assert len(got[1]) == 34
    assert " ".join(":".join(str(v) for v in r) for r in got[1][:12]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_a_full_join_in_four_tables_is_searched(pg: Any) -> None:
    """PostgreSQL 15.19, four 20-row tables never analysed: `f0 FULL JOIN f1
    ON f0.b = f1.a LEFT JOIN f2 ON f1.b = f2.a LEFT JOIN f3 ON f2.c = f3.a`.
    The LEFT joins' ONs read only their nullable sides, so the FULL join
    stays FULL, and the join search keeps it whole."""
    pg.execute("begin")
    try:
        for t, (mb, mc) in enumerate([(7, 5), (5, 3), (3, 4), (4, 7)]):
            pg.execute(f"create table rp028_f{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_f{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_f0.a, rp028_f1.a, rp028_f2.a, rp028_f3.a from rp028_f0 full join"
            " rp028_f1 on rp028_f0.b = rp028_f1.a left join rp028_f2 on rp028_f1.b = rp028_f2.a"
            " left join rp028_f3 on rp028_f2.c = rp028_f3.a",
        )
    finally:
        pg.execute("rollback")
    want = (
        "18:4:4:None None:9:4:None None:19:4:None None:14:4:None 11:4:4:None 4:4:4:None"
        " 6:6:1:1 15:1:1:1 8:1:1:1 1:1:1:1 20:6:1:1 13:6:1:1"
    )
    assert got[0] == "ok"
    assert len(got[1]) == 34
    assert " ".join(":".join(str(v) for v in r) for r in got[1][:12]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_a_left_join_over_an_inner_class_is_searched(pg: Any) -> None:
    """PostgreSQL 15.19, four 20-row tables never analysed: `l0 LEFT JOIN
    (l1 LEFT JOIN l2 ON l1.a = l2.b JOIN l3 ON l1.c = l3.a) ON l0.b = l1.c`.
    l1.c is in two clauses, but only the inner join's makes an equivalence
    class ({l1.c, l3.a}); the LEFT join's ON is an outer-join clause. So
    PostgreSQL merges l3 with l1-l2 on the class and Merge Left Joins l0 to
    it."""
    pg.execute("begin")
    try:
        for t, (mb, mc) in enumerate([(7, 5), (5, 3), (3, 4), (4, 7)]):
            pg.execute(f"create table rp028_l{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_l{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_l0.a, rp028_l1.a, rp028_l2.a, rp028_l3.a from rp028_l0 left join"
            " (rp028_l1 left join rp028_l2 on rp028_l1.a = rp028_l2.b join rp028_l3"
            " on rp028_l1.c = rp028_l3.a) on rp028_l0.b = rp028_l1.c",
        )
    finally:
        pg.execute("rollback")
    want = (
        "7:None:None:None 14:None:None:None 15:1:4:1 15:19:None:1 15:1:10:1 15:1:13:1"
        " 15:4:None:1 15:1:16:1 15:1:7:1 15:7:None:1 15:10:None:1 15:1:19:1"
    )
    assert got[0] == "ok"
    assert len(got[1]) == 92
    assert " ".join(":".join(str(v) for v in r) for r in got[1][:12]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_a_left_join_on_a_class_member_is_in_the_class_order(pg: Any) -> None:
    """PostgreSQL 15.19, four 20-row tables never analysed: `c0 JOIN c1 ON
    c0.c = c1.c JOIN c2 ON c1.c = c2.a LEFT JOIN c3 ON c0.c = c3.a`. The
    LEFT join's outer key c0.c is in the inner joins' class, so a Merge
    Left Join of c0 with c3 comes out in the class's order, and the Merge
    Join with c1-c2 above reads it unsorted."""
    pg.execute("begin")
    try:
        for t, (mb, mc) in enumerate([(7, 5), (5, 3), (3, 4), (4, 7)]):
            pg.execute(f"create table rp028_c{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_c{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_c0.a, rp028_c1.a, rp028_c2.a, rp028_c3.a from rp028_c0 join rp028_c1"
            " on rp028_c0.c = rp028_c1.c join rp028_c2 on rp028_c1.c = rp028_c2.a"
            " left join rp028_c3 on rp028_c0.c = rp028_c3.a",
        )
    finally:
        pg.execute("rollback")
    want = (
        "11:10:1:1 11:13:1:1 11:16:1:1 11:7:1:1 11:19:1:1 11:1:1:1 11:4:1:1"
        " 6:10:1:1 6:13:1:1 6:16:1:1 6:7:1:1 6:19:1:1"
    )
    assert got[0] == "ok"
    assert len(got[1]) == 56
    assert " ".join(":".join(str(v) for v in r) for r in got[1][:12]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_a_join_on_two_equalities_is_planned(pg: Any) -> None:
    """PostgreSQL 15.19, two 20-row tables never analysed: `k0 JOIN k1 ON
    k0.b = k1.b AND k0.c = k1.c` is a Merge Join on both keys, each side
    sorted on (b, c)."""
    pg.execute("begin")
    try:
        for t, (mb, mc) in enumerate([(7, 5), (5, 3)]):
            pg.execute(f"create table rp028_k{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_k{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_k0.a, rp028_k1.a from rp028_k0 join rp028_k1"
            " on rp028_k0.b = rp028_k1.b and rp028_k0.c = rp028_k1.c",
        )
    finally:
        pg.execute("rollback")
    want = "7:20 7:5 15:6 1:1 1:16 16:7 2:2 2:17 10:3 10:18 17:8 11:4"
    assert got[0] == "ok"
    assert len(got[1]) == 13
    assert " ".join(":".join(str(v) for v in r) for r in got[1][:12]) == want


@pytest.mark.backlog_fixed("RP-028")
def test_a_where_over_two_of_three_tables_is_planned(pg: Any) -> None:
    """PostgreSQL 15.19, three 20-row tables never analysed: `w0 JOIN w1 ON
    w0.b = w1.a JOIN w2 ON w1.b = w2.a WHERE w0.c < w2.c`. The WHERE is a
    join filter of the top join, the first that holds w0 and w2."""
    pg.execute("begin")
    try:
        for t, (mb, mc) in enumerate([(7, 5), (5, 3), (3, 4)]):
            pg.execute(f"create table rp028_w{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_w{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_w0.a, rp028_w1.a, rp028_w2.a from rp028_w0 join rp028_w1"
            " on rp028_w0.b = rp028_w1.a join rp028_w2 on rp028_w1.b = rp028_w2.a"
            " where rp028_w0.c < rp028_w2.c",
        )
    finally:
        pg.execute("rollback")
    assert got == ("ok", [(15, 1, 1), (20, 6, 1), (16, 2, 2), (17, 3, 3), (10, 3, 3)])


@pytest.mark.backlog_fixed("RP-028")
def test_a_where_over_a_pair_joined_first_is_planned(pg: Any) -> None:
    """PostgreSQL 15.19, three 20-row tables never analysed: `w0 JOIN w1 ON
    w0.b = w1.a JOIN w2 ON w1.b = w2.a WHERE w0.c > w1.c AND w1.c < w2.c`.
    `w0.c > w1.c` is a join filter of the w0-w1 join, below the top: it
    filters that join's rows (DEFAULT_INEQ_SEL in its estimate) before
    they are sorted for the join above."""
    pg.execute("begin")
    try:
        for t, (mb, mc) in enumerate([(7, 5), (5, 3), (3, 4)]):
            pg.execute(f"create table rp028_w{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_w{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_w0.a, rp028_w1.a, rp028_w2.a from rp028_w0 join rp028_w1"
            " on rp028_w0.b = rp028_w1.a join rp028_w2 on rp028_w1.b = rp028_w2.a"
            " where rp028_w0.c > rp028_w1.c and rp028_w1.c < rp028_w2.c",
        )
    finally:
        pg.execute("rollback")
    assert got == ("ok", [(6, 6, 1), (13, 6, 1), (3, 3, 3), (17, 3, 3)])


@pytest.mark.backlog_fixed("RP-028")
def test_a_where_equality_over_two_of_three_tables_is_planned(pg: Any) -> None:
    """PostgreSQL 15.19, three 20-row tables never analysed: `w0 JOIN w1 ON
    w0.b = w1.a JOIN w2 ON w1.b = w2.a WHERE w0.c = w2.c`. The WHERE's
    equality is an equivalence class of its own, so w0 and w2 have a join
    clause and the search weighs joining them first: a Merge Join of w0-w1
    with w2 on both clauses."""
    pg.execute("begin")
    try:
        for t, (mb, mc) in enumerate([(7, 5), (5, 3), (3, 4)]):
            pg.execute(f"create table rp028_w{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_w{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_w0.a, rp028_w1.a, rp028_w2.a from rp028_w0 join rp028_w1"
            " on rp028_w0.b = rp028_w1.a join rp028_w2 on rp028_w1.b = rp028_w2.a"
            " where rp028_w0.c = rp028_w2.c",
        )
    finally:
        pg.execute("rollback")
    assert got == ("ok", [(6, 6, 1), (1, 1, 1), (2, 2, 2), (3, 3, 3)])


@pytest.mark.backlog_fixed("RP-028")
def test_a_limit_over_three_tables_is_planned(pg: Any) -> None:
    """PostgreSQL 15.19, three 20-row tables never analysed: `w0 JOIN w1 ON
    w0.b = w1.a JOIN w2 ON w1.b = w2.a LIMIT 5` is planned for its first five
    rows (`get_cheapest_fractional_path`), a nested loop that returns them
    in w0's scan order. The three-table search plans for every row, so the
    LIMIT takes the first five of a different order."""
    pg.execute("begin")
    try:
        for t, (mb, mc) in enumerate([(7, 5), (5, 3), (3, 4)]):
            pg.execute(f"create table rp028_w{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_w{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_w0.a, rp028_w1.a, rp028_w2.a from rp028_w0 join rp028_w1"
            " on rp028_w0.b = rp028_w1.a join rp028_w2 on rp028_w1.b = rp028_w2.a limit 5",
        )
    finally:
        pg.execute("rollback")
    assert got == ("ok", [(1, 1, 1), (2, 2, 2), (3, 3, 3), (4, 4, 4), (6, 6, 1)])


@pytest.mark.backlog_fixed("RP-298")
def test_create_index_packs_posting_lists_as_postgres_builds_them(pg: Any) -> None:
    """PostgreSQL 15.19: CREATE INDEX over many repeated keys limits a
    posting list to a tenth of a page (`_bt_load`'s maxpostingsize, 812
    bytes, not the 1,352 an insert's deduplication allows), and finishes a
    leaf counting the posting list its high key sheds (`btps_lastextra`)
    and the heap TID it may gain. 10,000 rows of five values make an index
    of 11 pages, 30,000 of 500 values 27; the model built 12 and 29, which
    the planner prices index scans by. (Committed: an index made in the
    open transaction is RP-299.)"""
    got = []
    for rows, values in [(10000, 5), (30000, 500), (3000, 7)]:
        name = f"rp290_{values}"
        pg.execute(f"drop table if exists {name}")
        pg.execute(f"create table {name} (a int, b int)")
        try:
            pg.execute(
                f"insert into {name} select i, i % {values} from generate_series(1, {rows}) i"
            )
            pg.execute(f"create index {name}_b on {name} (b)")
            got.append(pg_answer(pg, f"select pg_relation_size('{name}_b') / 8192"))
        finally:
            pg.execute(f"drop table {name}")
    assert got == [("ok", [(11,)]), ("ok", [(27,)]), ("ok", [(5,)])]


@pytest.mark.backlog("RP-299")
def test_an_index_made_in_the_open_transaction_is_a_relation(pg: Any) -> None:
    """PostgreSQL 15.19: inside the transaction that created it, an index
    is a relation `regclass` finds, so `pg_relation_size('t_b')` answers
    (8192 for an empty index: its metapage alone). Here the name is
    not found until the transaction commits: 42P01."""
    pg.execute("begin")
    try:
        pg.execute("create table rp291_t (a int, b int)")
        pg.execute("create index rp291_t_b on rp291_t (b)")
        got = pg_answer(pg, "select pg_relation_size('rp291_t_b')")
    finally:
        pg.execute("rollback")
    assert got == ("ok", [(8192,)])


@pytest.mark.backlog_fixed("RP-028")
def test_a_limit_over_left_joins_reads_a_parameterized_join(pg: Any) -> None:
    """PostgreSQL 15.19, four 300-row tables never analysed, an index on
    v1.a: `v0 LEFT JOIN v1 ON v0.b = v1.a LEFT JOIN v2 ON v1.b = v2.a LEFT
    JOIN v3 ON v2.b = v3.a LIMIT 5` is a Nested Loop Left Join whose inner
    is the join of v1 and v2 parameterized by v0.b (a Hash Right Join over
    v1's Index Scan with `a = v0.b`), so the rows come in v0's scan order.
    A join relation parameterized by an outer table (`param_source_rels`,
    which an outer join's SpecialJoinInfo allows) is not modelled: the
    LIMIT's search declines, and the join runs as written."""
    pg.execute("begin")
    try:
        mods = [7, 5, 3, 4, 6]
        for t in range(4):
            pg.execute(f"create table rp028_v{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_v{t} select i, i % {mods[t]}, i % {mods[t + 1]}"
                " from generate_series(1, 300) i"
            )
        pg.execute("create index rp028_v1_a on rp028_v1 (a)")
        got = pg_answer(
            pg,
            "select rp028_v0.a, rp028_v1.a, rp028_v2.a, rp028_v3.a from rp028_v0"
            " left join rp028_v1 on rp028_v0.b = rp028_v1.a"
            " left join rp028_v2 on rp028_v1.b = rp028_v2.a"
            " left join rp028_v3 on rp028_v2.b = rp028_v3.a limit 5",
        )
    finally:
        pg.execute("rollback")
    assert got == (
        "ok",
        [(1, 1, 1, 1), (2, 2, 2, 2), (3, 3, 3, None), (4, 4, 4, 1), (5, 5, None, None)],
    )


@pytest.mark.backlog("RP-028")
def test_a_join_on_two_equivalence_classes_is_searched(pg: Any) -> None:
    """PostgreSQL 15.19, five 20-row tables never analysed, s0's b joined
    to s2 and s4 and its c to s1 and s3: two equivalence classes, {s0.b,
    s2.a, s4.a} and {s0.c, s1.a, s3.a}. PostgreSQL's search merges s1 with
    s3 on one class and s0, s2, s4 on the other, then the two on s0.c, and
    returns s0's rows 11, 1, 16, 6, ... The join order search models one
    class only and declines, so the join runs as written."""
    pg.execute("begin")
    try:
        for t, (mb, mc) in enumerate([(7, 5), (5, 3), (3, 4), (4, 6), (6, 5)]):
            pg.execute(f"create table rp028_s{t} (a int, b int, c int)")
            pg.execute(
                f"insert into rp028_s{t} select i, i % {mb}, i % {mc} from generate_series(1, 20) i"
            )
        got = pg_answer(
            pg,
            "select rp028_s0.a, rp028_s1.a, rp028_s2.a, rp028_s3.a, rp028_s4.a from rp028_s0"
            " join rp028_s1 on rp028_s0.c = rp028_s1.a join rp028_s2 on rp028_s0.b = rp028_s2.a"
            " join rp028_s3 on rp028_s0.c = rp028_s3.a join rp028_s4 on rp028_s0.b = rp028_s4.a",
        )
    finally:
        pg.execute("rollback")
    assert got[0] == "ok"
    assert [r[0] for r in got[1]] == [11, 1, 16, 6, 2, 12, 17, 3, 18, 13, 8, 4, 9, 19]


# RP-047: the lock manager. Each test needs two sessions on one server, so
# it opens its own connections to the module's `rust_pg`; the expected
# values were measured on PostgreSQL 15.19 with two sessions on a private
# cluster.


def _rp047_table(rust_pg: Any, name: str) -> None:
    with rust_pg.connect() as c:
        c.execute(f"drop table if exists {name}")
        c.execute(f"create table {name} (id int primary key, v int)")
        c.execute(f"insert into {name} values (1, 0), (2, 0)")


def _relation_modes(rust_pg: Any, table: str, statement: str) -> list[str]:
    with rust_pg.connect(autocommit=False) as a, rust_pg.connect() as c:
        a.execute(statement)
        pid = a.execute("select pg_backend_pid()").fetchone()[0]
        rows = c.execute(
            "select mode from pg_locks where locktype = 'relation'"
            f" and relation = '{table}'::regclass and pid = %s order by mode",
            (pid,),
        ).fetchall()
        a.rollback()
    return [r[0] for r in rows]


@pytest.mark.backlog_fixed("RP-047")
def test_row_locking_statements_take_postgres_relation_lock_modes(rust_pg: Any) -> None:
    """PostgreSQL 15.19: `FOR UPDATE` and `FOR SHARE` take RowShareLock on
    the table, `DELETE` RowExclusiveLock alone, `TRUNCATE`
    AccessExclusiveLock and ShareLock; here FOR UPDATE / FOR SHARE show
    AccessShareLock, DELETE an extra AccessShareLock, TRUNCATE no
    ShareLock."""
    _rp047_table(rust_pg, "rp047_m")
    got = {
        "for update": _relation_modes(rust_pg, "rp047_m", "select * from rp047_m for update"),
        "for share": _relation_modes(rust_pg, "rp047_m", "select * from rp047_m for share"),
        "delete": _relation_modes(rust_pg, "rp047_m", "delete from rp047_m where id = 2"),
        "truncate": _relation_modes(rust_pg, "rp047_m", "truncate rp047_m"),
    }
    assert got == {
        "for update": ["RowShareLock"],
        "for share": ["RowShareLock"],
        "delete": ["RowExclusiveLock"],
        "truncate": ["AccessExclusiveLock", "ShareLock"],
    }


@pytest.mark.backlog_fixed("RP-047")
def test_pg_locks_shows_transaction_id_locks(rust_pg: Any) -> None:
    """PostgreSQL 15.19: a transaction that has written holds an
    ExclusiveLock on its own transaction id and on its virtual one, both
    shown in `pg_locks`; here neither is."""
    _rp047_table(rust_pg, "rp047_x")
    with rust_pg.connect(autocommit=False) as a, rust_pg.connect() as c:
        a.execute("update rp047_x set v = 5 where id = 1")
        pid = a.execute("select pg_backend_pid()").fetchone()[0]
        got = c.execute(
            "select locktype, mode, granted from pg_locks"
            " where locktype in ('transactionid', 'virtualxid') and pid = %s order by locktype",
            (pid,),
        ).fetchall()
        a.rollback()
    assert got == [("transactionid", "ExclusiveLock", True), ("virtualxid", "ExclusiveLock", True)]


def _with_a_waiter(rust_pg: Any, hold: str, wait: str, look: Callable[[Any, int], Any]) -> Any:
    """Session A runs `hold` in a transaction; session B runs `wait`, which
    blocks on it, in a thread; `look(observer, a_pid)` reads the catalogs
    while B waits. A then rolls back, releasing B."""
    a = rust_pg.connect(autocommit=False)
    b = rust_pg.connect(autocommit=False)
    c = rust_pg.connect()
    try:
        b.execute("set lock_timeout = '10s'")
        a.execute(hold)
        a_pid = a.execute("select pg_backend_pid()").fetchone()[0]

        def blocked() -> None:
            with contextlib.suppress(psycopg.Error):
                b.execute(wait)

        t = threading.Thread(target=blocked, daemon=True)
        t.start()
        time.sleep(0.5)
        got = look(c, a_pid)
        a.rollback()
        t.join(10)
        return got
    finally:
        for conn in (a, b, c):
            with contextlib.suppress(psycopg.Error):
                conn.rollback()
            conn.close()


@pytest.mark.backlog_fixed("RP-047")
def test_a_row_lock_waiter_waits_on_the_holders_transaction_id(rust_pg: Any) -> None:
    """PostgreSQL 15.19: a session waiting for a row another transaction
    updated shows in `pg_locks` as an ungranted ShareLock on that
    transaction's id (`XactLockTableWait`); here no lock is ungranted."""
    _rp047_table(rust_pg, "rp047_r")
    got = _with_a_waiter(
        rust_pg,
        "update rp047_r set v = 5 where id = 1",
        "update rp047_r set v = v + 10 where id = 1",
        lambda c, _pid: c.execute(
            "select locktype, mode, granted from pg_locks where not granted"
        ).fetchall(),
    )
    assert got == [("transactionid", "ShareLock", False)]


@pytest.mark.backlog_fixed("RP-047")
def test_a_lock_waiter_is_visible_with_its_blocker(rust_pg: Any) -> None:
    """PostgreSQL 15.19: a session blocked on a table lock shows in
    `pg_stat_activity` with `wait_event_type = 'Lock'`, and
    `pg_blocking_pids()` gives the holder; here neither."""
    _rp047_table(rust_pg, "rp047_w")

    def look(c: Any, a_pid: int) -> Any:
        rows = c.execute(
            "select pg_blocking_pids(pid) from pg_stat_activity where wait_event_type = 'Lock'"
        ).fetchall()
        return [list(r[0]) == [a_pid] for r in rows]

    got = _with_a_waiter(
        rust_pg,
        "lock table rp047_w in access exclusive mode",
        "select * from rp047_w",
        look,
    )
    assert got == [True]
