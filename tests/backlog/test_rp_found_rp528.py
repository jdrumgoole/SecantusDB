"""Gap test of the divergence found while working RP-528 (a DROP of the
wrong kind of relation): a DROP naming a schema that does not exist.

PostgreSQL's `DropErrorMsgNonExistent` asks for the schema first: a
qualified name in a schema that does not exist is 3F000 `schema "x" does not
exist`, or under IF EXISTS the NOTICE `schema "x" does not exist, skipping`.
A quoted name with a dot in it is a relation in the search path, and is
reported as one. The expected answers were measured on a private, freshly
initdb'd PostgreSQL 15.19 cluster.
"""

from __future__ import annotations

from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn


def drop_answer(dsn: str, drop: str) -> Any:
    notes: list[str] = []
    with psycopg.connect(dsn, autocommit=True) as c:
        c.add_notice_handler(lambda d: notes.append(d.message_primary))
        try:
            c.execute(drop)
            result: Any = "ok"
        except psycopg.Error as e:
            result = (e.sqlstate, e.diag.message_primary)
    return (result, notes)


_NO_SCHEMA = (("3F000", 'schema "rq529x" does not exist'), [])
_SKIPPED = ("ok", ['schema "rq529x" does not exist, skipping'])
MISSING_SCHEMA = {
    "drop table rq529x.t": _NO_SCHEMA,
    "drop view rq529x.t": _NO_SCHEMA,
    "drop materialized view rq529x.m": _NO_SCHEMA,
    "drop index rq529x.i": _NO_SCHEMA,
    "drop sequence rq529x.s": _NO_SCHEMA,
    "drop table if exists rq529x.t": _SKIPPED,
    "drop index if exists rq529x.i": _SKIPPED,
    # A quoted name is one identifier: no schema is asked for.
    'drop table "rq529x.t"': (("42P01", 'table "rq529x.t" does not exist'), []),
    'drop table if exists "rq529x.t"': (
        "ok",
        ['table "rq529x.t" does not exist, skipping'],
    ),
}


@pytest.mark.backlog("RP-533")
@pytest.mark.parametrize("drop", list(MISSING_SCHEMA))
def test_drop_in_a_missing_schema(drop: str, rust_pg: object) -> None:
    dsn = _dsn(rust_pg) + " dbname=postgres"
    assert drop_answer(dsn, drop) == MISSING_SCHEMA[drop]
