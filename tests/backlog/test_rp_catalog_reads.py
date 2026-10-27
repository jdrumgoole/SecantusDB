"""RP-225: the catalog reads PostgreSQL 15.19 counts in
`pg_stat_sys_tables` / `pg_stat_sys_indexes`, scenario by scenario.

PostgreSQL reads its catalogs through each backend's caches (the catalog
cache behind the syscaches, the relation cache, the type cache), so what a
statement scans depends on what that backend already holds: a new session
reads its database, role and settings; its first query on a table builds
the table's and its indexes' descriptors and looks up its name, operators
and statistics; the same query again reads nothing; another session's DDL
makes the reader read the changed entries again. Each scenario runs its
steps over one or more sessions and records, between marks, every system
catalog's and catalog index's `(numscans, tuples_returned,
tuples_fetched)` delta, read by an observer whose own queries are all
cache hits. A mark makes every open session flush its counts
(`pg_stat_force_next_flush()`).

The expected answers were measured on a private PostgreSQL 15.19 cluster
(`initdb -U postgres --locale=C -E UTF8`, `autovacuum = off`, so no worker
reads the catalogs between marks), with the relation cache init files in
place, by running this file as a script:

    DSN="host=127.0.0.1 port=5495 user=postgres"
    PYTHONPATH=. python tests/backlog/test_rp_catalog_reads.py "$DSN"

twice, the two recordings identical. A scenario's objects are created, and
read once by another session (so PostgreSQL's index scans skip the dead
catalog entries earlier runs left), before its first mark.
"""

from __future__ import annotations

import pprint
import sys
import time
from typing import Any

import psycopg
import pytest

from tests.backlog.test_rp_batch26 import _dsn

SETUP = [
    "drop view if exists rpc_v",
    "drop table if exists rpc_t, rpc_p cascade",
    "drop function if exists rpc_f(int)",
    "drop role if exists rpc_u",
    "create table rpc_t (id int primary key, v text)",
    "insert into rpc_t values (1, 'a'), (2, 'b')",
    "create table rpc_p (x int, y text)",
    "insert into rpc_p values (1, 'a')",
    "create function rpc_f(int) returns int language sql as 'select $1 + 1'",
    "create view rpc_v as select x, y from rpc_p where x > 0",
    "create role rpc_u login",
    "grant select, insert on rpc_p to rpc_u",
]

WARM = [
    "select * from rpc_t where id = 1",
    "select * from rpc_p",
    "select rpc_f(1)",
    "select * from rpc_v",
    "select relname from pg_class where relname = 'rpc_t'",
    "select * from pg_attribute where attrelid = 'rpc_t'::regclass",
]

TEARDOWN = [
    "drop view if exists rpc_v",
    "drop table if exists rpc_t, rpc_p cascade",
    "drop function if exists rpc_f(int)",
    "drop role if exists rpc_u",
]

# A step is "session|sql" ("session|" only opens it; a session named
# "a::role" connects as that role), or "mark". The sessions close at the
# end, and a last segment holds what their ends read.
SCENARIOS: dict[str, list[str]] = {
    "a new session": ["a|", "mark"],
    "a new session's first statement": ["a|select 1", "mark"],
    "a statement again reads nothing": ["a|", "mark", "a|select 1", "mark", "a|select 1", "mark"],
    "a table by its primary key": [
        "a|",
        "mark",
        "a|select v from rpc_t where id = 1",
        "mark",
        "a|select v from rpc_t where id = 1",
        "mark",
    ],
    "a table without an index": [
        "a|",
        "mark",
        "a|select * from rpc_p",
        "mark",
        "a|select x from rpc_p where x = 1",
        "mark",
        "a|select * from rpc_p",
        "mark",
    ],
    "two tables": ["a|", "mark", "a|select * from rpc_p", "mark", "a|select * from rpc_t", "mark"],
    "a SQL function": ["a|", "mark", "a|select rpc_f(1)", "mark", "a|select rpc_f(2)", "mark"],
    "built-in functions": [
        "a|",
        "mark",
        "a|select now()",
        "mark",
        "a|select length('abc')",
        "mark",
        "a|select upper('a')",
        "mark",
    ],
    "casts": [
        "a|",
        "mark",
        "a|select 2::int8",
        "mark",
        "a|select 'x'",
        "mark",
        "a|select 'x'::text",
        "mark",
    ],
    "a catalog by name": [
        "a|",
        "mark",
        "a|select relname from pg_class where relname = 'rpc_t'",
        "mark",
        "a|select relname from pg_class where relname = 'rpc_t'",
        "mark",
    ],
    "INSERT, and DELETE with IN": [
        "a|",
        "mark",
        "a|insert into rpc_p values (2, 'b')",
        "mark",
        "a|insert into rpc_p values (3, 'c')",
        "mark",
        "a|delete from rpc_p where x in (2, 3)",
        "mark",
    ],
    "INSERT into a table with a key": [
        "a|",
        "mark",
        "a|insert into rpc_t values (10, 'x')",
        "mark",
        "a|insert into rpc_t values (11, 'y')",
        "mark",
        "a|delete from rpc_t where id > 9",
        "mark",
    ],
    "UPDATE": [
        "a|",
        "mark",
        "a|update rpc_p set y = 'z' where x = 1",
        "mark",
        "a|update rpc_p set y = 'a' where x = 1",
        "mark",
    ],
    "count(*)": [
        "a|",
        "mark",
        "a|select count(*) from rpc_p",
        "mark",
        "a|select count(*) from rpc_t",
        "mark",
    ],
    "ORDER BY": [
        "a|",
        "mark",
        "a|select x from rpc_p order by x",
        "mark",
        "a|select y from rpc_p order by y",
        "mark",
        "a|select x from rpc_p order by x desc",
        "mark",
    ],
    "GROUP BY": ["a|", "mark", "a|select x from rpc_p group by x", "mark"],
    "LIMIT": ["a|", "mark", "a|select * from rpc_p limit 1", "mark"],
    "a join": ["a|", "mark", "a|select * from rpc_p join rpc_t on rpc_t.id = rpc_p.x", "mark"],
    "a view": ["a|", "mark", "a|select * from rpc_v", "mark", "a|select * from rpc_v", "mark"],
    "a role that is not a superuser": [
        "a::rpc_u|",
        "mark",
        "a::rpc_u|select 1",
        "mark",
        "a::rpc_u|select x from rpc_p",
        "mark",
    ],
    # Another session's ALTER TABLE invalidates what the reader cached: its
    # next query reads the table's descriptor (and, as ADD COLUMN changed
    # the table's pg_class row, its name and row) again. The ALTERs' own
    # reads are RP-290's; their segments are left out of the comparison.
    "DDL in another session": [
        "a|",
        "b|",
        "mark",
        "a|select v from rpc_t where id = 1",
        "mark",
        "b|alter table rpc_t add column w int",
        "mark",
        "a|select v from rpc_t where id = 1",
        "mark",
        "b|alter table rpc_t drop column w",
        "mark",
        "a|select v from rpc_t where id = 1",
        "mark",
    ],
}

# The open items' gap tests: what PostgreSQL 15.19 reads for statements
# this model does not walk.
GAPS: dict[str, tuple[str, list[str]]] = {
    # ALTER TABLE's own reads: the target's descriptor and indexes, the
    # event trigger cache, pg_depend and friends for DROP COLUMN.
    "an ALTER TABLE's own catalog reads": (
        "RP-290",
        [
            "b|",
            "mark",
            "b|alter table rpc_t add column w int",
            "mark",
            "b|alter table rpc_t drop column w",
            "mark",
        ],
    ),
    # A system view's rule: pg_stat_user_tables over pg_class, pg_index and
    # pg_namespace, grouped.
    "a statistics view": (
        "RP-290",
        [
            "a|",
            "mark",
            "a|select seq_scan from pg_stat_user_tables where relname = 'rpc_p'",
            "mark",
        ],
    ),
    # psql's \d pattern query: a regular expression's fixed prefix as an
    # index condition, pg_table_is_visible per row, the join's sort.
    "psql's describe query": (
        "RP-290",
        [
            "a|",
            "mark",
            "a|SELECT c.oid, n.nspname, c.relname FROM pg_catalog.pg_class c "
            "LEFT JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace "
            "WHERE c.relname OPERATOR(pg_catalog.~) '^(rpc_t)$' COLLATE pg_catalog.default "
            "AND pg_catalog.pg_table_is_visible(c.oid) ORDER BY 2, 3",
            "mark",
        ],
    ),
}

# Segments whose reads a statement this model does not walk makes: the
# ALTER TABLEs' own catalog reads (RP-290, open). Left out of the
# comparison, by index.
UNMODELLED = {"DDL in another session": {2, 4}}


def _connect(dsn: str, session: str) -> Any:
    parts = session.split(":")
    db = parts[1] if len(parts) > 1 and parts[1] else "postgres"
    user = parts[2] if len(parts) > 2 else "postgres"
    return psycopg.connect(
        dsn.replace("user=postgres", f"user={user}") + f" dbname={db}", autocommit=True
    )


class Observer:
    """Reads every system catalog's and catalog index's counters through
    `pg_stat_get_*` by oid: once warm, its own query reads no catalog."""

    def __init__(self, dsn: str) -> None:
        self.conn = _connect(dsn, "obs")
        rels = self.conn.execute(
            "select relid::int, relname::text from pg_stat_sys_tables "
            "where schemaname = 'pg_catalog' order by 1"
        ).fetchall()
        idxs = self.conn.execute(
            "select indexrelid::int, indexrelname::text from pg_stat_sys_indexes "
            "where schemaname = 'pg_catalog' order by 1"
        ).fetchall()
        self.names = dict(rels) | dict(idxs)
        self.query = (
            "select o, pg_stat_get_numscans(o), pg_stat_get_tuples_returned(o), "
            "pg_stat_get_tuples_fetched(o) from (values {}) v(o)"
        ).format(",".join(f"({o})" for o in self.names))
        for _ in range(3):
            self.snap()
        # Its warming reads reach the shared counts now, not mid-scenario.
        self.conn.execute("select pg_stat_force_next_flush()")
        time.sleep(1.2)
        self.snap()

    def snap(self) -> dict[str, tuple[int, int, int]]:
        self.conn.execute("select pg_stat_clear_snapshot()")
        return {self.names[o]: (a, b, c) for o, a, b, c in self.conn.execute(self.query).fetchall()}


def _delta(a: dict[str, Any], b: dict[str, Any]) -> dict[str, list[int]]:
    out = {}
    for k in sorted(b):
        d = [y - x for x, y in zip(a.get(k, (0, 0, 0)), b[k], strict=True)]
        if any(d):
            out[k] = d
    return out


def run_scenario(dsn: str, obs: Observer, steps: list[str]) -> list[dict[str, list[int]]]:
    conns: dict[str, Any] = {}
    out = []
    last = obs.snap()
    try:
        for st in steps:
            if st == "mark":
                for c in conns.values():
                    c.execute("select pg_stat_force_next_flush()")
                time.sleep(0.2)
                now = obs.snap()
                out.append(_delta(last, now))
                last = now
                continue
            name, sql = st.split("|", 1)
            if name not in conns:
                conns[name] = _connect(dsn, name)
            if sql:
                conns[name].execute(sql)
    finally:
        for c in conns.values():
            c.close()
    # A session's end reads nothing; its last counts flush as it goes.
    time.sleep(0.5)
    out.append(_delta(last, obs.snap()))
    return out


def catalog_reads(
    dsn: str, scenarios: dict[str, list[str]] | None = None
) -> dict[str, list[dict[str, list[int]]]]:
    scenarios = SCENARIOS if scenarios is None else scenarios
    with _connect(dsn, "setup") as c:
        for s in SETUP:
            c.execute(s)
    with _connect(dsn, "warm") as c:
        for s in WARM:
            c.execute(s)
    obs = Observer(dsn)
    try:
        return {name: run_scenario(dsn, obs, steps) for name, steps in scenarios.items()}
    finally:
        obs.conn.close()
        with _connect(dsn, "teardown") as c:
            for s in TEARDOWN:
                c.execute(s)


def _comparable(name: str, segments: list[Any]) -> list[Any]:
    skip = UNMODELLED.get(name, set())
    return [s for i, s in enumerate(segments) if i not in skip]


EXPECTED: dict[str, Any] = {
    "a new session": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {},
    ],
    "a new session's first statement": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [2, 2, 2],
        },
        {},
    ],
    "a statement again reads nothing": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {"pg_type_oid_index": [1, 1, 1]},
        {},
        {},
    ],
    "a table by its primary key": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_amop_opr_fam_index": [1, 5, 5],
            "pg_amproc_fam_proc_index": [1, 4, 4],
            "pg_attribute_relid_attnum_index": [7, 8, 8],
            "pg_class_oid_index": [5, 5, 5],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [3, 3, 3],
            "pg_index_indrelid_index": [1, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_operator_oid_index": [1, 1, 1],
            "pg_operator_oprname_l_r_n_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [3, 3, 3],
        },
        {},
        {},
    ],
    "a table without an index": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_attribute_relid_attnum_index": [3, 4, 4],
            "pg_class_oid_index": [3, 3, 3],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [2, 2, 2],
        },
        {
            "pg_amop_opr_fam_index": [1, 5, 5],
            "pg_operator_oid_index": [1, 1, 1],
            "pg_operator_oprname_l_r_n_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {},
        {},
    ],
    "two tables": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_attribute_relid_attnum_index": [3, 4, 4],
            "pg_class_oid_index": [3, 3, 3],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [2, 2, 2],
        },
        {
            "pg_amproc_fam_proc_index": [1, 4, 4],
            "pg_attribute_relid_attnum_index": [5, 6, 6],
            "pg_class_oid_index": [4, 4, 4],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [2, 2, 2],
            "pg_index_indrelid_index": [1, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
        },
        {},
    ],
    "a SQL function": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_operator_oid_index": [1, 1, 1],
            "pg_operator_oprname_l_r_n_index": [1, 1, 1],
            "pg_proc_oid_index": [2, 2, 2],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {},
        {},
    ],
    "built-in functions": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 8, 8],
            "pg_type_oid_index": [9, 9, 9],
            "pg_type_typname_nsp_index": [2, 0, 0],
        },
        {
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 3, 3],
            "pg_type_oid_index": [2, 2, 2],
            "pg_type_typname_nsp_index": [2, 0, 0],
        },
        {},
    ],
    "casts": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_cast_source_target_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_type_oid_index": [2, 2, 2],
            "pg_type_typname_nsp_index": [1, 1, 1],
        },
        {"pg_type_oid_index": [1, 1, 1]},
        {"pg_type_typname_nsp_index": [1, 1, 1]},
        {},
    ],
    "a catalog by name": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_amop_fam_strat_index": [1, 1, 1],
            "pg_amop_opr_fam_index": [1, 4, 4],
            "pg_attribute_relid_attnum_index": [7, 8, 8],
            "pg_class_oid_index": [4, 4, 4],
            "pg_class_relname_nsp_index": [2, 2, 2],
            "pg_index_indexrelid_index": [3, 3, 3],
            "pg_index_indrelid_index": [1, 3, 3],
            "pg_operator_oid_index": [1, 1, 1],
            "pg_operator_oprname_l_r_n_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [1, 1, 1],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [3, 3, 3],
        },
        {"pg_class_relname_nsp_index": [1, 1, 1]},
        {},
    ],
    "INSERT, and DELETE with IN": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_attribute_relid_attnum_index": [1, 2, 2],
            "pg_class_oid_index": [2, 2, 2],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_type_oid_index": [2, 2, 2],
        },
        {},
        {
            "pg_amop_fam_strat_index": [1, 1, 1],
            "pg_amop_opr_fam_index": [1, 5, 5],
            "pg_amproc_fam_proc_index": [1, 1, 1],
            "pg_attribute_relid_attnum_index": [4, 4, 4],
            "pg_cast_source_target_index": [12, 6, 6],
            "pg_class_oid_index": [2, 2, 2],
            "pg_index_indexrelid_index": [2, 2, 2],
            "pg_index_indrelid_index": [1, 0, 0],
            "pg_opclass_am_name_nsp_index": [1, 44, 44],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_operator_oid_index": [1, 1, 1],
            "pg_operator_oprname_l_r_n_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_publication": [1, 0, 0],
            "pg_publication_namespace_pnnspid_pnpubid_index": [1, 0, 0],
            "pg_publication_rel_prrelid_prpubid_index": [1, 0, 0],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [3, 3, 3],
        },
        {},
    ],
    "INSERT into a table with a key": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 4, 4],
            "pg_attribute_relid_attnum_index": [5, 6, 6],
            "pg_class_oid_index": [4, 4, 4],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [2, 2, 2],
            "pg_index_indrelid_index": [1, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_type_oid_index": [2, 2, 2],
        },
        {},
        {
            "pg_amop_opr_fam_index": [1, 1, 1],
            "pg_attribute_relid_attnum_index": [2, 2, 2],
            "pg_class_oid_index": [1, 1, 1],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_operator_oid_index": [1, 1, 1],
            "pg_operator_oprname_l_r_n_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_publication": [1, 0, 0],
            "pg_publication_namespace_pnnspid_pnpubid_index": [1, 0, 0],
            "pg_publication_rel_prrelid_prpubid_index": [1, 0, 0],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [2, 2, 2],
        },
        {},
    ],
    "UPDATE": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_amop_opr_fam_index": [1, 5, 5],
            "pg_attribute_relid_attnum_index": [5, 6, 6],
            "pg_class_oid_index": [4, 4, 4],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [2, 2, 2],
            "pg_index_indrelid_index": [1, 0, 0],
            "pg_operator_oid_index": [1, 1, 1],
            "pg_operator_oprname_l_r_n_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_publication": [1, 0, 0],
            "pg_publication_namespace_pnnspid_pnpubid_index": [1, 0, 0],
            "pg_publication_rel_prrelid_prpubid_index": [1, 0, 0],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [4, 4, 4],
        },
        {},
        {},
    ],
    "count(*)": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_aggregate_fnoid_index": [1, 1, 1],
            "pg_am": [1, 1, 0],
            "pg_attribute_relid_attnum_index": [3, 4, 4],
            "pg_class_oid_index": [3, 3, 3],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_proc_oid_index": [2, 2, 2],
            "pg_proc_proname_args_nsp_index": [1, 2, 2],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [3, 3, 3],
        },
        {
            "pg_amproc_fam_proc_index": [1, 4, 4],
            "pg_attribute_relid_attnum_index": [5, 6, 6],
            "pg_class_oid_index": [4, 4, 4],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [2, 2, 2],
            "pg_index_indrelid_index": [1, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
        },
        {},
    ],
    "ORDER BY": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_amop_fam_strat_index": [4, 4, 4],
            "pg_amop_opr_fam_index": [2, 8, 8],
            "pg_amproc_fam_proc_index": [2, 2, 2],
            "pg_attribute_relid_attnum_index": [3, 4, 4],
            "pg_cast_source_target_index": [14, 6, 6],
            "pg_class_oid_index": [3, 3, 3],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_opclass_am_name_nsp_index": [2, 85, 85],
            "pg_opclass_oid_index": [2, 2, 2],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [2, 2, 2],
        },
        {
            "pg_amop_fam_strat_index": [4, 4, 4],
            "pg_amop_opr_fam_index": [2, 10, 10],
            "pg_amproc_fam_proc_index": [2, 2, 2],
            "pg_cast_source_target_index": [29, 3, 3],
            "pg_opclass_am_name_nsp_index": [2, 85, 85],
            "pg_opclass_oid_index": [2, 2, 2],
            "pg_type_oid_index": [1, 1, 1],
        },
        {"pg_amop_opr_fam_index": [1, 3, 3]},
        {},
    ],
    "GROUP BY": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_amop_fam_strat_index": [4, 4, 4],
            "pg_amop_opr_fam_index": [2, 8, 8],
            "pg_amproc_fam_proc_index": [1, 1, 1],
            "pg_attribute_relid_attnum_index": [3, 4, 4],
            "pg_cast_source_target_index": [14, 6, 6],
            "pg_class_oid_index": [3, 3, 3],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_opclass_am_name_nsp_index": [2, 85, 85],
            "pg_opclass_oid_index": [2, 2, 2],
            "pg_operator_oid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [2, 2, 2],
        },
        {},
    ],
    "LIMIT": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_attribute_relid_attnum_index": [3, 4, 4],
            "pg_cast_source_target_index": [1, 1, 1],
            "pg_class_oid_index": [3, 3, 3],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [3, 3, 3],
        },
        {},
    ],
    "a join": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_amop_fam_strat_index": [4, 4, 4],
            "pg_amop_opr_fam_index": [1, 5, 5],
            "pg_amproc_fam_proc_index": [2, 5, 5],
            "pg_attribute_relid_attnum_index": [8, 10, 10],
            "pg_cast_source_target_index": [14, 6, 6],
            "pg_class_oid_index": [7, 7, 7],
            "pg_class_relname_nsp_index": [4, 2, 2],
            "pg_index_indexrelid_index": [3, 3, 3],
            "pg_index_indrelid_index": [1, 1, 1],
            "pg_opclass_am_name_nsp_index": [2, 85, 85],
            "pg_opclass_oid_index": [3, 3, 3],
            "pg_operator_oid_index": [1, 1, 1],
            "pg_operator_oprname_l_r_n_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [2, 0, 0],
            "pg_statistic_relid_att_inh_index": [4, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [3, 3, 3],
        },
        {},
    ],
    "a view": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_attribute_relid_attnum_index": [4, 6, 6],
            "pg_class_oid_index": [5, 5, 5],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_operator_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_rewrite_rel_rulename_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [2, 2, 2],
        },
        {},
        {},
    ],
    "a role that is not a superuser": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_namespace_oid_index": [1, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {"pg_type_oid_index": [1, 1, 1]},
        {
            "pg_am": [1, 1, 0],
            "pg_attribute_relid_attnum_index": [3, 4, 4],
            "pg_class_oid_index": [3, 3, 3],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {},
    ],
    "DDL in another session": [
        {
            "pg_am": [2, 2, 0],
            "pg_amproc_fam_proc_index": [2, 6, 6],
            "pg_attribute_relid_attnum_index": [8, 14, 14],
            "pg_authid_oid_index": [2, 2, 2],
            "pg_authid_rolname_index": [2, 2, 2],
            "pg_class_oid_index": [4, 4, 4],
            "pg_database_datname_index": [2, 2, 2],
            "pg_database_oid_index": [4, 4, 4],
            "pg_db_role_setting_databaseid_rol_index": [8, 0, 0],
            "pg_index_indexrelid_index": [2, 2, 2],
            "pg_namespace_nspname_index": [4, 2, 2],
            "pg_opclass_oid_index": [2, 2, 2],
            "pg_proc_oid_index": [2, 2, 2],
            "pg_proc_proname_args_nsp_index": [2, 2, 2],
            "pg_type_oid_index": [2, 2, 2],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_amop_opr_fam_index": [1, 5, 5],
            "pg_amproc_fam_proc_index": [1, 4, 4],
            "pg_attribute_relid_attnum_index": [7, 8, 8],
            "pg_class_oid_index": [5, 5, 5],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_index_indexrelid_index": [3, 3, 3],
            "pg_index_indrelid_index": [1, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_operator_oid_index": [1, 1, 1],
            "pg_operator_oprname_l_r_n_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 0, 0],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [3, 3, 3],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_attribute_relid_attnam_index": [1, 0, 0],
            "pg_attribute_relid_attnum_index": [8, 18, 18],
            "pg_class_oid_index": [7, 7, 7],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_event_trigger_evtname_index": [1, 0, 0],
            "pg_index_indexrelid_index": [2, 2, 2],
            "pg_index_indrelid_index": [2, 5, 5],
            "pg_namespace_nspname_index": [1, 1, 1],
            "pg_namespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
            "pg_type_typname_nsp_index": [1, 1, 1],
        },
        {
            "pg_attribute_relid_attnum_index": [1, 3, 3],
            "pg_class_oid_index": [2, 2, 2],
            "pg_class_relname_nsp_index": [1, 1, 1],
            "pg_index_indrelid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [1, 0, 0],
        },
        {
            "pg_amproc_fam_proc_index": [2, 7, 7],
            "pg_attribute_relid_attnam_index": [1, 1, 1],
            "pg_attribute_relid_attnum_index": [33, 69, 68],
            "pg_class_oid_index": [13, 13, 13],
            "pg_class_relname_nsp_index": [1, 1, 1],
            "pg_depend_depender_index": [2, 0, 0],
            "pg_depend_reference_index": [1, 0, 0],
            "pg_description_o_c_o_index": [1, 0, 0],
            "pg_index_indexrelid_index": [6, 6, 6],
            "pg_init_privs_o_c_o_index": [1, 0, 0],
            "pg_opclass_oid_index": [2, 2, 2],
            "pg_seclabel_object_index": [1, 0, 0],
            "pg_shdepend_depender_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [1, 0, 0],
        },
        {
            "pg_attribute_relid_attnum_index": [1, 4, 3],
            "pg_class_oid_index": [1, 1, 1],
            "pg_index_indrelid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
        },
        {},
    ],
}


EXPECTED_GAPS: dict[str, Any] = {
    "an ALTER TABLE's own catalog reads": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_am": [1, 1, 0],
            "pg_attribute_relid_attnam_index": [1, 0, 0],
            "pg_attribute_relid_attnum_index": [8, 18, 18],
            "pg_class_oid_index": [7, 7, 7],
            "pg_class_relname_nsp_index": [2, 1, 1],
            "pg_event_trigger_evtname_index": [1, 0, 0],
            "pg_index_indexrelid_index": [2, 2, 2],
            "pg_index_indrelid_index": [2, 5, 5],
            "pg_namespace_nspname_index": [1, 1, 1],
            "pg_namespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
            "pg_type_typname_nsp_index": [1, 1, 1],
        },
        {
            "pg_amproc_fam_proc_index": [2, 7, 7],
            "pg_attribute_relid_attnam_index": [1, 1, 1],
            "pg_attribute_relid_attnum_index": [33, 69, 68],
            "pg_class_oid_index": [13, 13, 13],
            "pg_class_relname_nsp_index": [1, 1, 1],
            "pg_depend_depender_index": [2, 0, 0],
            "pg_depend_reference_index": [1, 0, 0],
            "pg_description_o_c_o_index": [1, 0, 0],
            "pg_index_indexrelid_index": [6, 6, 6],
            "pg_init_privs_o_c_o_index": [1, 0, 0],
            "pg_opclass_oid_index": [2, 2, 2],
            "pg_seclabel_object_index": [1, 0, 0],
            "pg_shdepend_depender_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [1, 0, 0],
        },
        {},
    ],
    "a statistics view": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_amop_fam_strat_index": [7, 7, 7],
            "pg_amop_opr_fam_index": [9, 21, 21],
            "pg_amproc_fam_proc_index": [7, 12, 12],
            "pg_attribute_relid_attnum_index": [75, 125, 125],
            "pg_cast_source_target_index": [49, 4, 4],
            "pg_class_oid_index": [13, 13, 13],
            "pg_class_relname_nsp_index": [2, 2, 2],
            "pg_constraint_conrelid_contypid_conname_index": [1, 1, 1],
            "pg_index_indexrelid_index": [5, 5, 5],
            "pg_index_indrelid_index": [7, 10, 10],
            "pg_namespace": [1, 3, 0],
            "pg_namespace_nspname_index": [1, 1, 0],
            "pg_namespace_oid_index": [2, 2, 0],
            "pg_opclass_am_name_nsp_index": [4, 173, 173],
            "pg_opclass_oid_index": [6, 6, 6],
            "pg_operator_oid_index": [10, 10, 10],
            "pg_operator_oprname_l_r_n_index": [1, 1, 1],
            "pg_proc_oid_index": [27, 27, 27],
            "pg_rewrite_rel_rulename_index": [2, 2, 2],
            "pg_statistic_ext_relid_index": [3, 0, 0],
            "pg_statistic_relid_att_inh_index": [8, 7, 7],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [15, 15, 15],
        },
        {},
    ],
    "psql's describe query": [
        {
            "pg_am": [1, 1, 0],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [4, 7, 7],
            "pg_authid_oid_index": [1, 1, 1],
            "pg_authid_rolname_index": [1, 1, 1],
            "pg_class_oid_index": [2, 2, 2],
            "pg_database_datname_index": [1, 1, 1],
            "pg_database_oid_index": [2, 2, 2],
            "pg_db_role_setting_databaseid_rol_index": [4, 0, 0],
            "pg_index_indexrelid_index": [1, 1, 1],
            "pg_namespace_nspname_index": [2, 1, 1],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_amop_fam_strat_index": [8, 8, 8],
            "pg_amop_opr_fam_index": [6, 16, 16],
            "pg_amproc_fam_proc_index": [4, 4, 4],
            "pg_attribute_relid_attnum_index": [7, 8, 8],
            "pg_cast_source_target_index": [47, 4, 4],
            "pg_class_oid_index": [6, 6, 6],
            "pg_class_relname_nsp_index": [4, 3, 3],
            "pg_collation_name_enc_nsp_index": [2, 1, 1],
            "pg_collation_oid_index": [1, 1, 1],
            "pg_index_indexrelid_index": [3, 3, 3],
            "pg_index_indrelid_index": [2, 5, 5],
            "pg_namespace": [1, 3, 0],
            "pg_namespace_nspname_index": [1, 1, 1],
            "pg_namespace_oid_index": [2, 2, 0],
            "pg_opclass_am_name_nsp_index": [4, 170, 170],
            "pg_opclass_oid_index": [4, 4, 4],
            "pg_operator_oid_index": [5, 5, 5],
            "pg_operator_oprname_l_r_n_index": [3, 11, 11],
            "pg_proc_oid_index": [3, 3, 3],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [2, 0, 0],
            "pg_statistic_relid_att_inh_index": [5, 5, 5],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [14, 14, 14],
        },
        {},
    ],
}


@pytest.mark.backlog_fixed("RP-225")
def test_catalog_reads(rust_pg: Any) -> None:
    got = catalog_reads(_dsn(rust_pg))
    for name in SCENARIOS:
        assert (name, _comparable(name, got[name])) == (name, _comparable(name, EXPECTED[name]))


@pytest.mark.parametrize(
    "name",
    [pytest.param(n, marks=pytest.mark.backlog(item), id=n) for n, (item, _) in GAPS.items()],
)
def test_gap(name: str, rust_pg: Any) -> None:
    got = catalog_reads(_dsn(rust_pg), {name: GAPS[name][1]})
    assert got[name] == EXPECTED_GAPS[name]


def catalog_blocks(dsn: str) -> list[bool]:
    """A new session's catalog reads touch catalog pages: `pg_class`'s
    heap and its name index each count blocks (read or hit) in
    `pg_statio_sys_tables` / `pg_statio_sys_indexes`."""
    with _connect(dsn, "setup") as c:
        for s in SETUP:
            c.execute(s)
    obs = Observer(dsn)
    query = (
        "select pg_stat_get_blocks_fetched(1259), pg_stat_get_blocks_fetched(2663), "
        "pg_stat_get_blocks_hit(1259) + pg_stat_get_blocks_hit(2663)"
    )
    try:
        obs.conn.execute(query)
        obs.conn.execute("select pg_stat_clear_snapshot()")
        before = obs.conn.execute(query).fetchone()
        with _connect(dsn, "a") as a:
            a.execute("select relname from pg_class where relname = 'rpc_t'")
            a.execute("select pg_stat_force_next_flush()")
        time.sleep(0.3)
        obs.conn.execute("select pg_stat_clear_snapshot()")
        after = obs.conn.execute(query).fetchone()
        return [after[i] > before[i] for i in range(3)]
    finally:
        obs.conn.close()
        with _connect(dsn, "teardown") as c:
            for s in TEARDOWN:
                c.execute(s)


@pytest.mark.backlog("RP-291")
def test_catalog_blocks(rust_pg: Any) -> None:
    """PostgreSQL 15.19: all three rise."""
    assert catalog_blocks(_dsn(rust_pg)) == [True, True, True]


# RP-292: the first backend after the server starts finds no relation cache
# init file and builds every critical relation's descriptor, by heap scans
# until the critical indexes are built, and writes the files. PostgreSQL
# 15.19's reads, a session that ran `select 1` then an observer's start,
# measured across a restart of the private cluster (the counts survive a
# clean shutdown, so the delta is the two sessions').
FIRST_BACKEND: dict[str, list[int]] = {
    "pg_am": [2, 2, 0],
    "pg_amproc": [2, 7, 0],
    "pg_amproc_fam_proc_index": [7, 19, 19],
    "pg_attribute": [11, 53, 0],
    "pg_attribute_relid_attnum_index": [249, 566, 566],
    "pg_authid": [2, 2, 0],
    "pg_authid_oid_index": [1, 1, 1],
    "pg_authid_rolname_index": [1, 1, 1],
    "pg_class": [11, 11, 0],
    "pg_class_oid_index": [129, 129, 129],
    "pg_database": [2, 2, 0],
    "pg_database_datname_index": [1, 1, 1],
    "pg_database_oid_index": [3, 3, 3],
    "pg_db_role_setting_databaseid_rol_index": [8, 0, 0],
    "pg_index": [7, 7, 0],
    "pg_index_indexrelid_index": [80, 80, 80],
    "pg_opclass": [2, 2, 0],
    "pg_opclass_oid_index": [7, 7, 7],
    "pg_subscription": [7, 0, 0],
    "pg_type_oid_index": [1, 1, 1],
}


@pytest.mark.backlog("RP-292")
def test_the_first_backend_after_a_start(rust_pg: Any, tmp_path: Any) -> None:
    from tests.test_rust_pgserver_slice import _Server

    # The catalogs' oids, from another server: asking the fresh one would
    # make its observer the first backend.
    names = Observer(_dsn(rust_pg)).names
    query = (
        "select o, pg_stat_get_numscans(o), pg_stat_get_tuples_returned(o), "
        "pg_stat_get_tuples_fetched(o) from (values {}) v(o)"
    ).format(",".join(f"({o})" for o in names))
    with _Server(tmp_path) as server:
        dsn = _dsn(server)
        first = _connect(dsn, "first")
        first.execute("select 1")
        first.close()
        time.sleep(0.5)
        with _connect(dsn, "obs") as obs:
            rows = obs.execute(query).fetchall()
    got = {names[o]: [a, b, c] for o, a, b, c in rows if a or b or c}
    assert got == FIRST_BACKEND


# Found while measuring: RP-293, `pg_class.relnatts` left out a dropped
# column's attribute; RP-294, an oidvector would not cast to regtype[].
FOUND = {
    "relnatts counts dropped columns": (
        "RP-293",
        [
            "create table rpc_n (a int)",
            "alter table rpc_n add column b int",
            "alter table rpc_n drop column b",
            "select relnatts from pg_class where relname = 'rpc_n'",
            "drop table rpc_n",
        ],
    ),
    "an oidvector casts to regtype[]": (
        "RP-294",
        ["select proargtypes::regtype[]::text from pg_proc where oid = 1317"],
    ),
}

EXPECTED_FOUND: dict[str, Any] = {
    "relnatts counts dropped columns": [None, None, None, [(2,)], None],
    "an oidvector casts to regtype[]": [[("[0:0]={text}",)]],
}


def found(dsn: str, steps: list[str]) -> list[Any]:
    out: list[Any] = []
    with _connect(dsn, "found") as c:
        for sql in steps:
            cur = c.execute(sql)
            out.append(cur.fetchall() if cur.description else None)
    return out


@pytest.mark.parametrize(
    "name",
    [pytest.param(n, marks=pytest.mark.backlog(item), id=n) for n, (item, _) in FOUND.items()],
)
def test_found(name: str, rust_pg: Any) -> None:
    assert found(_dsn(rust_pg), FOUND[name][1]) == EXPECTED_FOUND[name]


if __name__ == "__main__":
    measured = catalog_reads(sys.argv[1])
    print("EXPECTED: dict[str, Any] = " + pprint.pformat(measured, width=100, sort_dicts=False))
    gaps = {n: catalog_reads(sys.argv[1], {n: steps})[n] for n, (_, steps) in GAPS.items()}
    print("EXPECTED_GAPS: dict[str, Any] = " + pprint.pformat(gaps, width=100, sort_dicts=False))
    print(
        "EXPECTED_FOUND: dict[str, Any] = "
        + pprint.pformat({n: found(sys.argv[1], st) for n, (_, st) in FOUND.items()}, width=100)
    )
