"""Gap tests of divergences found while working RP-823 (a cursor FOR loop's
portal was not pinned):

- a FOR loop over a query or an EXECUTE runs no portal: `pg_cursors` lists
  none while it runs, and a CLOSE ALL in its body is not refused;
- a BEGIN inside a multi-statement simple query warns 25001.

The expected answers were measured on PostgreSQL 15.19; none of the
statements creates an object.
"""

from __future__ import annotations

from collections.abc import Iterator

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


@pytest.fixture(scope="module")
def dsn(rust_pg: object) -> Iterator[str]:
    yield _dsn(rust_pg) + " dbname=postgres"


def _answer(dsn: str, query: str) -> object:
    with psycopg.connect(dsn, autocommit=True) as c:
        notices: list[str] = []
        c.add_notice_handler(lambda d: notices.append(d.message_primary or ""))
        try:
            c.execute(query)
        except psycopg.Error as e:
            return ("error", e.sqlstate, e.diag.message_primary)
        return ("notice", notices)


QUERY_LOOP_PORTAL = {
    # `exec_stmt_fors` and `exec_stmt_dynfors` open the query as an unnamed
    # portal and walk it through `exec_for_query`, which pins it.
    "do $$ declare x record; h text; begin for x in select 1 loop "
    "select string_agg(name, ',') into h from pg_cursors; end loop; "
    "raise notice '[%]', h; end $$": ("notice", ["[<unnamed portal 1>]"]),
    "do $$ declare x record; h text; begin for x in execute 'select 1' loop "
    "select string_agg(name, ',') into h from pg_cursors; end loop; "
    "raise notice '[%]', h; end $$": ("notice", ["[<unnamed portal 1>]"]),
    "do $$ declare x record; begin for x in select 1 loop execute 'close all'; end loop; end $$": (
        "error",
        "24000",
        'cannot drop pinned portal "<unnamed portal 1>"',
    ),
    "do $$ declare x record; begin for x in execute 'select 1' loop "
    "execute 'close all'; end loop; end $$": (
        "error",
        "24000",
        'cannot drop pinned portal "<unnamed portal 1>"',
    ),
}


@pytest.mark.backlog("RP-828")
@pytest.mark.parametrize("query", list(QUERY_LOOP_PORTAL))
def test_a_query_loop_walks_a_pinned_portal(query: str, dsn: str) -> None:
    assert _answer(dsn, query) == QUERY_LOOP_PORTAL[query]


@pytest.mark.backlog("RP-829")
@pytest.mark.parametrize("query", ["begin; select 1; commit", "select 1; begin; select 2; commit"])
def test_a_begin_in_a_multi_statement_query_does_not_warn(query: str, dsn: str) -> None:
    # `BeginTransactionBlock` turns an implicit block (TBLOCK_IMPLICIT_
    # INPROGRESS) into a real one with no WARNING; only a BEGIN inside an
    # explicit block warns 25001.
    assert _answer(dsn, query) == ("notice", [])
