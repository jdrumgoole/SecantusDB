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

# A built-in catalog's first scan in a session builds its descriptors and
# reads its planner statistics; the reg* input functions look a name up,
# and an operator's oid scan meets its casts and types (RP-316).
FIRST_READS = "a built-in catalog's first reads"
FIRST_READS_STEPS: list[str] = [
    "a|",
    "mark",
    "a|select count(*) from pg_am",
    "mark",
    "a|select 'int4'::regtype::oid",
    "mark",
    "a|select oprname from pg_operator where oid = 96",
    "mark",
    "a|select 'pg_class'::regclass::oid",
    "mark",
    "a|select 'now'::text::regproc::oid",
    "mark",
    "a|select count(*) from pg_namespace",
    "mark",
    "a|select 'pg_catalog'::regnamespace::oid",
    "mark",
]

# The items' gap tests: what PostgreSQL 15.19 reads for statements this
# model did not walk, each `(item, open, steps)`.
GAPS: dict[str, tuple[str, bool, list[str]]] = {
    # ALTER TABLE's own reads: the event trigger cache, the target's lookup
    # and descriptor, ATParseTransformCmd's schema name, the column-name
    # check, pg_class's and pg_attribute's index lists, the descriptor
    # rebuilt at each CommandCounterIncrement, has_subclass; for DROP
    # COLUMN, pg_depend and the other catalogs deleteOneObject scans.
    "an ALTER TABLE's own catalog reads": (
        "RP-290",
        False,
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
        True,
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
        True,
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
    "a built-in catalog's first reads": [
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
            "pg_am": [1, 7, 0],
            "pg_amop_fam_strat_index": [2, 2, 2],
            "pg_amproc_fam_proc_index": [1, 3, 3],
            "pg_attribute_relid_attnum_index": [8, 8, 8],
            "pg_class_oid_index": [5, 5, 5],
            "pg_class_relname_nsp_index": [1, 1, 1],
            "pg_index_indexrelid_index": [4, 4, 4],
            "pg_index_indrelid_index": [1, 2, 2],
            "pg_opclass_oid_index": [1, 1, 1],
            "pg_proc_oid_index": [2, 2, 2],
            "pg_proc_proname_args_nsp_index": [1, 2, 2],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 2, 2],
            "pg_tablespace_oid_index": [1, 1, 1],
            "pg_type_oid_index": [5, 5, 5],
        },
        {
            "pg_cast_source_target_index": [1, 1, 1],
            "pg_type_oid_index": [2, 2, 2],
            "pg_type_typname_nsp_index": [3, 3, 3],
        },
        {
            "pg_amop_opr_fam_index": [1, 5, 5],
            "pg_cast_source_target_index": [42, 3, 3],
            "pg_class_oid_index": [1, 1, 1],
            "pg_class_relname_nsp_index": [1, 1, 1],
            "pg_index_indrelid_index": [1, 2, 2],
            "pg_operator_oid_index": [2, 2, 2],
            "pg_operator_oprname_l_r_n_index": [2, 63, 63],
            "pg_proc_oid_index": [1, 1, 1],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 2, 2],
            "pg_type_oid_index": [37, 37, 37],
        },
        {
            "pg_cast_source_target_index": [1, 1, 1],
            "pg_class_relname_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
            "pg_type_typname_nsp_index": [1, 1, 1],
        },
        {
            "pg_cast_source_target_index": [2, 1, 1],
            "pg_proc_oid_index": [3, 3, 3],
            "pg_proc_proname_args_nsp_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
            "pg_type_typname_nsp_index": [2, 2, 2],
        },
        {
            "pg_class_oid_index": [1, 1, 1],
            "pg_class_relname_nsp_index": [1, 1, 1],
            "pg_index_indrelid_index": [1, 2, 2],
            "pg_namespace": [1, 4, 0],
            "pg_statistic_ext_relid_index": [1, 0, 0],
            "pg_statistic_relid_att_inh_index": [2, 2, 2],
            "pg_type_oid_index": [1, 1, 1],
        },
        {
            "pg_cast_source_target_index": [1, 1, 1],
            "pg_namespace_nspname_index": [1, 1, 1],
            "pg_type_oid_index": [1, 1, 1],
            "pg_type_typname_nsp_index": [1, 1, 1],
        },
        {},
    ],
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
        assert (name, got[name]) == (name, EXPECTED[name])


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(
            n,
            marks=pytest.mark.backlog(item) if is_open else pytest.mark.backlog_fixed(item),
            id=n,
        )
        for n, (item, is_open, _) in GAPS.items()
    ],
)
def test_gap(name: str, rust_pg: Any) -> None:
    got = catalog_reads(_dsn(rust_pg), {name: GAPS[name][2]})
    assert got[name] == EXPECTED_GAPS[name]


# ALTER TABLE on a table with no index, twice each way in one session: the
# first ADD builds what any first DDL of a backend builds (pg_depend's
# descriptor, opened even for a pinned type; pg_class's and pg_attribute's
# index lists); the second reads the table's name and row again (the first
# changed its pg_class row) and rebuilds its descriptor twice; a DROP meets
# the earlier DROP's dead pg_attribute entry once, killing it, and its own
# twice. Measured on PostgreSQL 15.19 (RP-290).
ALTER_TWICE_STEPS = [
    "b|",
    "mark",
    "b|alter table rpc_p add column w int",
    "mark",
    "b|alter table rpc_p add column w2 text",
    "mark",
    "b|alter table rpc_p drop column w2",
    "mark",
    "b|alter table rpc_p drop column w",
    "mark",
]

EXPECTED_ALTER_TWICE: list[dict[str, list[int]]] = [
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
        "pg_attribute_relid_attnam_index": [1, 0, 0],
        "pg_attribute_relid_attnum_index": [2, 7, 7],
        "pg_class_oid_index": [4, 4, 4],
        "pg_class_relname_nsp_index": [1, 1, 1],
        "pg_type_oid_index": [1, 1, 1],
        "pg_type_typname_nsp_index": [1, 1, 1],
    },
    {
        "pg_amproc_fam_proc_index": [2, 7, 7],
        "pg_attribute_relid_attnam_index": [1, 1, 1],
        "pg_attribute_relid_attnum_index": [33, 71, 70],
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
        "pg_attribute_relid_attnam_index": [1, 1, 1],
        "pg_attribute_relid_attnum_index": [3, 11, 9],
        "pg_class_oid_index": [2, 2, 2],
        "pg_depend_depender_index": [2, 0, 0],
        "pg_depend_reference_index": [1, 0, 0],
        "pg_description_o_c_o_index": [1, 0, 0],
        "pg_init_privs_o_c_o_index": [1, 0, 0],
        "pg_seclabel_object_index": [1, 0, 0],
        "pg_shdepend_depender_index": [1, 0, 0],
        "pg_statistic_relid_att_inh_index": [1, 0, 0],
    },
    {},
]


@pytest.mark.backlog_fixed("RP-290")
def test_an_alter_table_twice_each_way(rust_pg: Any) -> None:
    got = catalog_reads(_dsn(rust_pg), {"alter": ALTER_TWICE_STEPS})["alter"]
    assert got == EXPECTED_ALTER_TWICE


# The reg* input functions' name lookups (`regproc.c`), each statement in
# one new session: `regtypein` parses its string as a type name,
# `regclassin` looks the relation up along the path (an explicit schema by
# name first), `regprocin` lists the functions of the name, and
# `regnamespacein` looks the schema up (`pg_catalog`, already looked up by
# the qualified `regclass` before it, is a cache hit); an oid string looks
# nothing up. Measured on PostgreSQL 15.19; the session's start (segment 0)
# is "a new session"'s and is left out.
REG_INPUT_STEPS = [
    "a|",
    "mark",
    "a|select 'int4'::regtype::oid",
    "mark",
    "a|select 'pg_class'::regclass::oid",
    "mark",
    "a|select 'pg_catalog.pg_type'::regclass::oid",
    "mark",
    "a|select 'now'::regproc::oid",
    "mark",
    "a|select 'pg_catalog'::regnamespace::oid",
    "mark",
    "a|select '1259'::regclass::oid",
    "mark",
]

EXPECTED_REG_INPUT: list[dict[str, list[int]]] = [
    {
        "pg_cast_source_target_index": [1, 1, 1],
        "pg_type_oid_index": [3, 3, 3],
        "pg_type_typname_nsp_index": [3, 3, 3],
    },
    {
        "pg_cast_source_target_index": [1, 1, 1],
        "pg_class_relname_nsp_index": [1, 1, 1],
        "pg_type_oid_index": [1, 1, 1],
        "pg_type_typname_nsp_index": [1, 1, 1],
    },
    {
        "pg_class_relname_nsp_index": [1, 1, 1],
        "pg_namespace_nspname_index": [1, 1, 1],
    },
    {
        "pg_cast_source_target_index": [1, 1, 1],
        "pg_proc_proname_args_nsp_index": [1, 1, 1],
        "pg_type_oid_index": [1, 1, 1],
        "pg_type_typname_nsp_index": [1, 1, 1],
    },
    {
        "pg_cast_source_target_index": [1, 1, 1],
        "pg_type_oid_index": [1, 1, 1],
        "pg_type_typname_nsp_index": [1, 1, 1],
    },
    {},
    {},
]


@pytest.mark.backlog_fixed("RP-316")
def test_the_reg_input_functions_look_names_up(rust_pg: Any) -> None:
    """RP-316 (slice): the reg* input functions made no name lookup."""
    got = catalog_reads(_dsn(rust_pg), {"reg": REG_INPUT_STEPS})["reg"]
    assert got[1:] == EXPECTED_REG_INPUT


# The type and function lookups of "a built-in catalog's first reads", as
# PostgreSQL 15.19 makes them (traced in the backend): `count(*)` over a
# lone catalog scans it with a physical target list, so the executor's
# tuple descriptor looks up every column's type (`pg_am`'s oid, name,
# regproc and char); and `'now'::text::regproc` is a `CoerceViaIO` the
# planner folds as far as it can -- `textout` and `regprocin` fetched by
# `simplify_function`, `cstring`'s type by `evaluate_expr`, and
# `cstring_out` by `cost_qual_eval` pricing the rebuilt coercion over the
# folded `cstring`, since the stable `regprocin` does not fold.
TYPE_AND_PROC_LOOKUPS = ("pg_type_oid_index", "pg_proc_oid_index")


@pytest.mark.backlog_fixed("RP-316")
def test_a_catalog_scans_types_and_an_io_coercions_functions(rust_pg: Any) -> None:
    """RP-316 (slice): a scan under an aggregate looked up none of its row's
    column types, and an I/O coercion's folding fetched no functions."""
    name = FIRST_READS
    got = catalog_reads(_dsn(rust_pg), {name: FIRST_READS_STEPS})[name]

    def only(segments: list[dict[str, list[int]]]) -> list[dict[str, list[int]]]:
        return [{k: v for k, v in seg.items() if k in TYPE_AND_PROC_LOOKUPS} for seg in segments]

    assert only(got) == only(EXPECTED_GAPS[name])


@pytest.mark.backlog_fixed("RP-316")
def test_a_built_in_catalogs_first_reads(rust_pg: Any) -> None:
    """RP-316: a new session's first reads of a built-in catalog were not
    PostgreSQL's -- `pg_am`'s two indexes were taken as held by the init
    file (`CatalogCacheInitializeCache` never opens them, as `touch_index`
    is false for `AMOID` / `AMNAME`); a `count(*)`, which an index-only
    scan of any B-tree can answer, read no index's leading-column
    statistics for `btcostestimate`; and an implicit coercion took the
    I/O path to a string type, which `find_coercion_pathway` allows only
    from `COERCION_ASSIGNMENT`, so `oid = 96` checked three operator
    candidates' right side PostgreSQL rejects on the left."""
    got = catalog_reads(_dsn(rust_pg), {FIRST_READS: FIRST_READS_STEPS})[FIRST_READS]
    assert got == EXPECTED_GAPS[FIRST_READS]


def catalog_blocks(dsn: str) -> list[dict[str, int]]:
    """The heap blocks each catalog counts in `pg_statio_sys_tables` over
    `INDEX_BLOCK_STEPS`, which read this module's objects' catalog rows:
    rows added after `initdb`, which the model has not placed yet."""
    return _block_deltas(dsn, INDEX_BLOCK_STEPS, "pg_stat_sys_tables", "relid", True)


EXPECTED_CATALOG_BLOCKS: list[dict[str, int]] = [
    {
        "pg_am": 1,
        "pg_amproc": 1,
        "pg_attribute": 4,
        "pg_authid": 2,
        "pg_class": 2,
        "pg_database": 3,
        "pg_index": 1,
        "pg_namespace": 1,
        "pg_opclass": 1,
        "pg_proc": 2,
        "pg_type": 1,
    },
    {
        "pg_amop": 4,
        "pg_attribute": 7,
        "pg_class": 7,
        "pg_index": 5,
        "pg_operator": 2,
        "pg_proc": 1,
        "pg_statistic": 1,
        "pg_tablespace": 1,
        "pg_type": 3,
    },
    {
        "pg_am": 1,
        "pg_amop": 3,
        "pg_amproc": 1,
        "pg_attribute": 3,
        "pg_class": 4,
        "pg_index": 2,
        "pg_opclass": 1,
        "pg_operator": 2,
        "pg_proc": 1,
        "pg_type": 2,
    },
    {},
]


@pytest.mark.backlog("RP-291")
def test_catalog_blocks(rust_pg: Any) -> None:
    """PostgreSQL 15.19: the heap blocks behind every catalog row a scan
    returns, `initdb`'s or a later one's."""
    assert catalog_blocks(_dsn(rust_pg)) == EXPECTED_CATALOG_BLOCKS


# RP-291, its first slice: the pages a catalog index scan reads, as
# PostgreSQL 15.19 counts them in `pg_statio_sys_indexes` -- the metapage on
# the backend's first scan of the index, the way down from the root, and
# each leaf the scan walks across -- over `initdb`'s own index pages
# (`pg15_catalog_indexes.txt`). Measured on the private cluster the module
# docstring describes; `blocks_fetched` per catalog index, a new session's
# start and then its statement.
INDEX_BLOCK_STEPS = [
    "a|",
    "mark",
    "a|select relname from pg_class where relname = 'rpc_t'",
    "mark",
    "a|select * from rpc_t where id = 1",
    "mark",
]


def index_blocks(dsn: str) -> list[dict[str, int]]:
    """Each catalog index's `blocks_fetched` delta between `INDEX_BLOCK_STEPS`'
    marks, as `run_scenario` takes them."""
    return _block_deltas(dsn, INDEX_BLOCK_STEPS, "pg_stat_sys_indexes", "indexrelid", True)


def _block_deltas(
    dsn: str, steps: list[str], view: str, column: str, setup: bool
) -> list[dict[str, int]]:
    """Each catalog relation of `view`'s `blocks_fetched` delta between
    `steps`' marks ("drop" takes one and drops it), and over the sessions'
    ends; with `setup`, over the
    module's objects, created and warmed first."""
    if setup:
        with _connect(dsn, "setup") as c:
            for s in SETUP:
                c.execute(s)
        with _connect(dsn, "warm") as c:
            for s in WARM:
                c.execute(s)
    obs = Observer(dsn)
    rels = [
        o
        for (o,) in obs.conn.execute(
            f"select {column}::int from {view} where schemaname = 'pg_catalog'"
        ).fetchall()
    ]
    query = "select o, pg_stat_get_blocks_fetched(o) from (values {}) v(o)".format(
        ",".join(f"({o})" for o in rels)
    )

    def snap() -> dict[str, int]:
        obs.conn.execute("select pg_stat_clear_snapshot()")
        return {obs.names[o]: n for o, n in obs.conn.execute(query).fetchall()}

    for _ in range(3):
        snap()
    obs.conn.execute("select pg_stat_force_next_flush()")
    time.sleep(1.2)
    conns: dict[str, Any] = {}
    out = []
    last = snap()

    def delta() -> dict[str, int]:
        nonlocal last
        now = snap()
        d = {k: v - last.get(k, 0) for k, v in sorted(now.items()) if v != last.get(k, 0)}
        last = now
        return d

    try:
        for st in steps:
            if st in ("mark", "drop"):
                for c in conns.values():
                    c.execute("select pg_stat_force_next_flush()")
                time.sleep(0.2)
                d = delta()
                if st == "mark":
                    out.append(d)
                continue
            name, sql = st.split("|", 1)
            if name not in conns:
                conns[name] = _connect(dsn, name)
            if sql:
                conns[name].execute(sql)
        for c in conns.values():
            c.close()
        time.sleep(0.5)
        out.append(delta())
        return out
    finally:
        obs.conn.close()
        if setup:
            with _connect(dsn, "teardown") as c:
                for s in TEARDOWN:
                    c.execute(s)


EXPECTED_INDEX_BLOCKS: list[dict[str, int]] = [
    {
        "pg_amproc_fam_proc_index": 3,
        "pg_attribute_relid_attnum_index": 9,
        "pg_authid_oid_index": 2,
        "pg_authid_rolname_index": 2,
        "pg_class_oid_index": 5,
        "pg_database_datname_index": 2,
        "pg_database_oid_index": 3,
        "pg_db_role_setting_databaseid_rol_index": 8,
        "pg_index_indexrelid_index": 2,
        "pg_namespace_nspname_index": 3,
        "pg_opclass_oid_index": 2,
        "pg_proc_oid_index": 3,
        "pg_proc_proname_args_nsp_index": 3,
        "pg_type_oid_index": 3,
    },
    {
        "pg_amop_fam_strat_index": 3,
        "pg_amop_opr_fam_index": 3,
        "pg_attribute_relid_attnum_index": 14,
        "pg_class_oid_index": 8,
        "pg_class_relname_nsp_index": 5,
        "pg_class_tblspc_relfilenode_index": 1,
        "pg_index_indexrelid_index": 3,
        "pg_index_indrelid_index": 2,
        "pg_operator_oid_index": 3,
        "pg_operator_oprname_l_r_n_index": 3,
        "pg_proc_oid_index": 2,
        "pg_statistic_ext_relid_index": 2,
        "pg_statistic_relid_att_inh_index": 2,
        "pg_tablespace_oid_index": 2,
        "pg_type_oid_index": 6,
    },
    {
        "pg_amop_opr_fam_index": 2,
        "pg_amproc_fam_proc_index": 2,
        "pg_attribute_relid_attnum_index": 6,
        "pg_class_oid_index": 6,
        "pg_class_relname_nsp_index": 4,
        "pg_index_indexrelid_index": 1,
        "pg_index_indrelid_index": 1,
        "pg_opclass_oid_index": 1,
        "pg_operator_oid_index": 2,
        "pg_operator_oprname_l_r_n_index": 2,
        "pg_proc_oid_index": 2,
        "pg_statistic_ext_relid_index": 2,
        "pg_statistic_relid_att_inh_index": 2,
        "pg_type_oid_index": 4,
    },
    {},
]


@pytest.mark.backlog_fixed("RP-291")
def test_catalog_index_blocks(rust_pg: Any) -> None:
    """The index part of RP-291, its first slice (2026-11-01)."""
    assert index_blocks(_dsn(rust_pg)) == EXPECTED_INDEX_BLOCKS


# RP-291, its heap slice: the heap blocks a catalog scan reads, as
# PostgreSQL 15.19 counts them in `pg_statio_sys_tables` -- an index scan's
# fetch of each tuple it returns (a block again only after another), a
# sequential scan's every block -- over reads that meet only `initdb`'s own
# rows (no object of this module's), whose places `pg15_catalog_heaps.txt`
# records. Measured as `EXPECTED_INDEX_BLOCKS` was.
HEAP_BLOCK_STEPS = [
    "a|",
    "mark",
    "a|select typname from pg_type where oid = 23",
    "mark",
    "a|select 'postgres'::regrole::oid",
    "mark",
    # The first scan of each builds the table's descriptors (not yet the
    # same reads as PostgreSQL's, RP-316); the second scans its heap alone.
    "a|select count(*) from pg_namespace",
    "a|select count(*) from pg_am",
    "drop",
    "a|select count(*) from pg_namespace",
    "mark",
    "a|select count(*) from pg_am",
    "mark",
]


def heap_blocks(dsn: str) -> list[dict[str, int]]:
    """Each catalog's `blocks_fetched` delta between `HEAP_BLOCK_STEPS`' marks."""
    return _block_deltas(dsn, HEAP_BLOCK_STEPS, "pg_stat_sys_tables", "relid", False)


EXPECTED_HEAP_BLOCKS: list[dict[str, int]] = [
    {
        "pg_am": 1,
        "pg_amproc": 1,
        "pg_attribute": 4,
        "pg_authid": 2,
        "pg_class": 2,
        "pg_database": 3,
        "pg_index": 1,
        "pg_namespace": 1,
        "pg_opclass": 1,
        "pg_proc": 2,
        "pg_type": 1,
    },
    {
        "pg_amop": 4,
        "pg_attribute": 4,
        "pg_cast": 3,
        "pg_class": 4,
        "pg_index": 3,
        "pg_operator": 43,
        "pg_proc": 1,
        "pg_statistic": 2,
        "pg_tablespace": 1,
        "pg_type": 43,
    },
    {"pg_cast": 1, "pg_type": 3},
    {"pg_namespace": 1},
    {"pg_am": 1},
    {},
]


@pytest.mark.backlog_fixed("RP-291")
def test_catalog_heap_blocks(rust_pg: Any) -> None:
    """The heap part of RP-291 over `initdb`'s rows (2026-11-01)."""
    assert heap_blocks(_dsn(rust_pg)) == EXPECTED_HEAP_BLOCKS


@pytest.mark.backlog_fixed("RP-311")
def test_builtin_statistics(rust_pg: Any) -> None:
    """PostgreSQL 15.19: `initdb`'s ANALYZE leaves 404 `pg_statistic` rows
    over 38 catalogs, in every database. They were missing; they are its
    rows now (`pg15_catalog_statistics.txt`)."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        got = c.execute(
            "select count(*), count(distinct starelid) from pg_statistic where starelid < 16384"
        ).fetchone()
        assert tuple(got) == (404, 38)
        # Read as text: the anyarray's values print as PostgreSQL's do.
        row = c.execute(
            "select null_frac::text, avg_width, n_distinct::text, most_common_vals::text,"
            " most_common_freqs::text from pg_stats"
            " where tablename = 'pg_class' and attname = 'relkind'"
        ).fetchone()
        assert tuple(row) == (
            "0",
            1,
            "4",
            "{i,v,r,t}",
            "{0.39512196,0.34146342,0.16585366,0.09756097}",
        )
        # An array column's values are each its own text, quoted.
        mcv = c.execute(
            "select most_common_vals::text from pg_stats"
            " where tablename = 'pg_class' and attname = 'reloptions'"
        ).fetchone()[0]
        assert mcv == '{"{security_barrier=true}"}'
        c.execute("drop database if exists rp311_d")
        c.execute("create database rp311_d")
    with psycopg.connect(_dsn(rust_pg) + " dbname=rp311_d", autocommit=True) as c:
        got = c.execute(
            "select count(*), count(distinct starelid) from pg_statistic where starelid < 16384"
        ).fetchone()
        assert tuple(got) == (404, 38)


@pytest.mark.backlog_fixed("RP-313")
def test_a_database_wide_analyze_measures_the_catalogs(rust_pg: Any) -> None:
    """PostgreSQL 15.19: a plain ANALYZE analysed the catalogs too, so a
    new table's oid became the top of `pg_class.oid`'s histogram (fixed
    for the catalog columns of a modelled type, `oid` among them)."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp313_d")
        c.execute("create database rp313_d")
    with psycopg.connect(_dsn(rust_pg) + " dbname=rp313_d", autocommit=True) as c:
        c.execute("create table rp313_t (a int)")
        c.execute("analyze")
        top = c.execute(
            "select (histogram_bounds::text::oid[])[array_upper(histogram_bounds::text::oid[], 1)]"
            " from pg_stats where tablename = 'pg_class' and attname = 'oid'"
        ).fetchone()[0]
        assert top >= 16384


@pytest.mark.backlog_fixed("RP-313")
def test_an_oid_column_is_analysed(rust_pg: Any) -> None:
    """PostgreSQL 15.19: ANALYZE measured an `oid` column (`oideq` /
    `oidlt`), an oid above 2^31 sorting as the unsigned number it is."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("create table rp313_o (o oid)")
        c.execute("insert into rp313_o select (g % 7)::oid from generate_series(1, 200) g")
        c.execute("insert into rp313_o select 4000000000::oid from generate_series(1, 3)")
        c.execute("analyze rp313_o")
        row = c.execute(
            "select null_frac, avg_width, n_distinct, most_common_vals::text,"
            " most_common_freqs::text, histogram_bounds::text, correlation"
            " from pg_stats where tablename = 'rp313_o'"
        ).fetchone()
        c.execute("drop table rp313_o")
    assert row == (
        0.0,
        4,
        8.0,
        "{1,2,3,4,0,5,6,4000000000}",
        "{0.14285715,0.14285715,0.14285715,0.14285715,0.13793103,0.13793103,0.13793103,0.014778325}",
        None,
        0.1805073,
    )


@pytest.mark.backlog_fixed("RP-313")
def test_a_database_wide_analyze_measures_a_name_column(rust_pg: Any) -> None:
    """PostgreSQL 15.19 (measured on a fresh database): a plain ANALYZE
    measured the catalogs' `name` columns too, so a new table whose name
    sorts last became the top of `pg_class.relname`'s histogram, where
    `initdb`'s was `views`; this server kept `initdb`'s row."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp313_n")
        c.execute("create database rp313_n")
    with psycopg.connect(_dsn(rust_pg) + " dbname=rp313_n", autocommit=True) as c:
        c.execute("create table zzzz_rp313 (a int)")
        c.execute("analyze")
        top = c.execute(
            "select (histogram_bounds::text::name[])"
            "[array_upper(histogram_bounds::text::name[], 1)]"
            " from pg_stats where tablename = 'pg_class' and attname = 'relname'"
        ).fetchone()[0]
        assert top == "zzzz_rp313"


@pytest.mark.backlog_fixed("RP-313")
def test_a_database_wide_analyze_measures_a_quoted_char_column(rust_pg: Any) -> None:
    """PostgreSQL 15.19 (a fresh database on a private cluster): after a new
    table and a plain ANALYZE, `pg_class.relkind`'s (`"char"`) most common
    values are re-measured over the catalog's rows; this server kept
    `initdb`'s row."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp313_c")
        c.execute("create database rp313_c")
    with psycopg.connect(_dsn(rust_pg) + " dbname=rp313_c", autocommit=True) as c:
        c.execute("create table zzzz_rp313 (a int)")
        c.execute("analyze")
        row = c.execute(
            "select most_common_vals::text, most_common_freqs::text, n_distinct"
            " from pg_stats where tablename = 'pg_class' and attname = 'relkind'"
        ).fetchone()
    # Its correlation (0.7882681) follows PostgreSQL's heap order: RP-358.
    assert row == ("{i,v,r,t}", "{0.3941606,0.34063262,0.16788322,0.097323604}", 4.0)


@pytest.mark.backlog("RP-358")
def test_a_catalog_columns_correlation_follows_the_heap(rust_pg: Any) -> None:
    """PostgreSQL 15.19 (a fresh database on a private cluster): ANALYZE
    correlates each catalog column with PostgreSQL's heap order, so after a
    new table `pg_class`'s correlations are these; this server walks its own
    row order."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp358")
        c.execute("create database rp358")
    with psycopg.connect(_dsn(rust_pg) + " dbname=rp358", autocommit=True) as c:
        c.execute("create table zzzz_rp313 (a int)")
        c.execute("analyze")
        rows = c.execute(
            "select attname::text, correlation from pg_stats where tablename = 'pg_class'"
            " and attname in ('oid', 'relkind', 'relname', 'relnatts') order by 1"
        ).fetchall()
    assert rows == [
        ("oid", 0.6547713),
        ("relkind", 0.7882681),
        ("relname", -0.12838678),
        ("relnatts", 0.67018104),
    ]


@pytest.mark.backlog_fixed("RP-313")
def test_a_database_wide_analyze_measures_an_array_column(rust_pg: Any) -> None:
    """PostgreSQL 15.19 (a fresh database on a private cluster): a plain
    ANALYZE measured the catalogs' array columns too, so after a table
    created `with (fillfactor=50)` `pg_class.reloptions` (`text[]`) had a
    second distinct value and a smaller null fraction than `initdb`'s
    0.99268293 / 1, where this server kept `initdb`'s row."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp313_a")
        c.execute("create database rp313_a")
    with psycopg.connect(_dsn(rust_pg) + " dbname=rp313_a", autocommit=True) as c:
        c.execute("create table zz_opt (a int) with (fillfactor=50)")
        c.execute("analyze")
        row = c.execute(
            "select null_frac, n_distinct, most_common_vals::text from pg_stats"
            " where tablename = 'pg_class' and attname = 'reloptions'"
        ).fetchone()
    assert row == (0.99026763, 2.0, '{"{security_barrier=true}"}')


@pytest.mark.backlog_fixed("RP-313")
def test_a_database_wide_analyze_measures_array_elements(rust_pg: Any) -> None:
    """PostgreSQL 15.19 (a fresh database on a private cluster):
    `compute_array_stats` gives `pg_class.reloptions` its element slots
    over the catalog's rows as they stand -- the three system views
    `system_views.sql` creates `WITH (security_barrier)` and the new table
    -- and its width is the arrays' own (`VARSIZE`, 4-byte element
    headers); here the column kept `initdb`'s row."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp313_e")
        c.execute("create database rp313_e")
    with psycopg.connect(_dsn(rust_pg) + " dbname=rp313_e", autocommit=True) as c:
        views = c.execute(
            "select relname, reloptions from pg_class where reloptions is not null order by 1"
        ).fetchall()
        c.execute("create table zz_opt (a int) with (fillfactor=50)")
        c.execute("analyze")
        row = c.execute(
            "select avg_width, correlation, most_common_elems::text,"
            " most_common_elem_freqs::text, array_length(elem_count_histogram, 1),"
            " elem_count_histogram[101] from pg_stats"
            " where tablename = 'pg_class' and attname = 'reloptions'"
        ).fetchone()
        slots = c.execute(
            "select stakind1, stakind2, stakind3, stakind4, staop1, staop2, staop3,"
            " staop4, stacoll1, stacoll2, stacoll3, stacoll4 from pg_statistic"
            " where starelid = 'pg_class'::regclass and staattnum = 32"
        ).fetchone()
    assert views == [
        ("pg_stats", ["security_barrier=true"]),
        ("pg_stats_ext", ["security_barrier=true"]),
        ("pg_stats_ext_exprs", ["security_barrier=true"]),
    ]
    assert row == (
        47,
        1.0,
        "{fillfactor=50,security_barrier=true}",
        "{0.25,0.75,0.25,0.75,0}",
        101,
        1.0,
    )
    assert slots == (1, 3, 4, 5, 1070, 1072, 98, 98, 950, 950, 950, 950)


@pytest.mark.backlog_fixed("RP-313")
def test_an_analyzed_array_column_is_estimated(rust_pg: Any) -> None:
    """PostgreSQL 15.19 (a private cluster): ANALYZE of a table's `int4[]`
    and `text[]` columns records `array_typanalyze`'s statistics, and the
    planner estimates `=`, `<` and `<>` on them from the most common
    values (`array_eq` / `array_lt`); here the columns had no statistics,
    and any filter on them printed zeros."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp313_u")
        c.execute("create database rp313_u")
    with psycopg.connect(_dsn(rust_pg) + " dbname=rp313_u", autocommit=True) as c:
        c.execute("create table ue (b int4[], t text[])")
        c.execute(
            "insert into ue select array[i % 3, 1], array['x' || (i % 4)]"
            " from generate_series(1, 50) i"
        )
        c.execute("analyze ue")
        stats = c.execute(
            "select attname, null_frac, avg_width, n_distinct, most_common_vals::text,"
            " most_common_freqs::text, correlation, most_common_elems::text,"
            " most_common_elem_freqs::text, elem_count_histogram[101]"
            " from pg_stats where tablename = 'ue' order by attname"
        ).fetchall()
        plans = [
            [r[0] for r in c.execute("explain select * from ue where " + w)]
            for w in ("b = '{1,1}'", "b < '{1,1}'", "b <> '{1,1}'", "t = '{x1}'")
        ]
    assert stats == [
        (
            "b",
            0.0,
            29,
            3.0,
            '{"{1,1}","{2,1}","{0,1}"}',
            "{0.34,0.34,0.32}",
            0.3469388,
            "{0,1,2}",
            "{0.32,1,0.34,0.32,1,0}",
            1.66,
        ),
        (
            "t",
            0.0,
            29,
            4.0,
            '{"{x1}","{x2}","{x0}","{x3}"}',
            "{0.26,0.26,0.24,0.24}",
            0.23591837,
            "{x0,x1,x2,x3}",
            "{0.24,0.26,0.26,0.24,0.24,0.26,0}",
            1.0,
        ),
    ]
    assert plans == [
        [
            "Seq Scan on ue  (cost=0.00..1.62 rows=17 width=58)",
            "  Filter: (b = '{1,1}'::integer[])",
        ],
        [
            "Seq Scan on ue  (cost=0.00..1.62 rows=16 width=58)",
            "  Filter: (b < '{1,1}'::integer[])",
        ],
        [
            "Seq Scan on ue  (cost=0.00..1.62 rows=33 width=58)",
            "  Filter: (b <> '{1,1}'::integer[])",
        ],
        ["Seq Scan on ue  (cost=0.00..1.62 rows=13 width=58)", "  Filter: (t = '{x1}'::text[])"],
    ]


@pytest.mark.backlog_fixed("RP-313")
def test_a_database_wide_analyze_measures_an_aclitem_array(rust_pg: Any) -> None:
    """PostgreSQL 15.19 (a fresh database on a private cluster): a plain
    ANALYZE re-measures `pg_class.relacl` (`aclitem[]`, which has `=` but
    no `<`: `compute_distinct_stats`), so after a GRANT on a new table its
    null fraction and frequencies follow the catalog's rows. The column
    kept `initdb`'s 0.5097561 and
    {0.45365855,0.029268293,0.004878049,0.0024390244}; and seven
    `information_schema` relations carried a PUBLIC grant initdb does not
    give them."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp313_b")
        c.execute("create database rp313_b")
    with psycopg.connect(_dsn(rust_pg) + " dbname=rp313_b", autocommit=True) as c:
        c.execute("create table zz_acl (a int)")
        c.execute("grant select on zz_acl to public")
        c.execute("analyze")
        row = c.execute(
            "select null_frac, n_distinct, most_common_freqs::text from pg_stats"
            " where tablename = 'pg_class' and attname = 'relacl'"
        ).fetchone()
    assert row == (0.50851583, 4.0, "{0.45498782,0.02919708,0.00486618,0.00243309}")


@pytest.mark.backlog_fixed("RP-313")
def test_a_database_wide_analyze_measures_a_regproc_column(rust_pg: Any) -> None:
    """PostgreSQL 15.19 (a fresh database on a private cluster, after one
    CREATE TABLE): a plain ANALYZE re-measures `pg_type.typinput`
    (`regproc`, which sorts under `oid`'s B-tree opclass), so its distinct
    count and frequencies follow the two new `pg_type` rows; the column
    kept `initdb`'s row. (Its correlation follows the heap's order,
    RP-358.)"""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp313_c")
        c.execute("create database rp313_c")
    with psycopg.connect(_dsn(rust_pg) + " dbname=rp313_c", autocommit=True) as c:
        c.execute("create table zz_reg (a int)")
        c.execute("analyze")
        row = c.execute(
            "select null_frac, n_distinct, most_common_freqs::text from pg_stats"
            " where tablename = 'pg_type' and attname = 'typinput'"
        ).fetchone()
    assert row == (
        0.0,
        -0.15986949,
        "{0.47634584,0.3425775,0.009787928,0.009787928,0.008156607,0.0032626428}",
    )


@pytest.mark.backlog_fixed("RP-313")
def test_a_database_wide_analyze_measures_an_int2vector_column(rust_pg: Any) -> None:
    """PostgreSQL 15.19 (a fresh database on a private cluster, measured on
    two new clusters): after a CREATE TABLE with a primary key, a plain
    ANALYZE re-measures `pg_index.indkey` (`int2vector`, an array to
    `type_is_array`, so `array_ops`' `array_eq` / `array_lt` and
    `compute_scalar_stats`), so its frequencies follow the new index's
    row; the column kept `initdb`'s row."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp313_d")
        c.execute("create database rp313_d")
    with psycopg.connect(_dsn(rust_pg) + " dbname=rp313_d", autocommit=True) as c:
        c.execute("create table zz_vec (a int primary key)")
        c.execute("analyze")
        row = c.execute(
            "select null_frac, n_distinct, avg_width, most_common_vals::text,"
            " most_common_freqs::text from pg_stats"
            " where tablename = 'pg_index' and attname = 'indkey'"
        ).fetchone()
    assert row == (
        0.0,
        -0.1595092,
        27,
        '{1,"1 2",2,"2 3","1 2 3","3 2","2 3 4","2 4","1 2 3 4","2 1","2 3 4 5",11}',
        "{0.3006135,0.2822086,0.10429448,0.0797546,0.036809817,0.024539877,"
        "0.018404908,0.018404908,0.012269938,0.012269938,0.012269938,0.012269938}",
    )


@pytest.mark.backlog("RP-313")
def test_a_database_wide_analyze_measures_an_oidvector_column(rust_pg: Any) -> None:
    """PostgreSQL 15.19 (a fresh database on a private cluster, measured on
    two new clusters): after a CREATE FUNCTION, a plain ANALYZE re-measures
    `pg_proc.proargtypes` (`oidvector`, under its own B-tree opclass
    `oidvector_ops`), so its distinct count and frequencies follow the new
    row (`initdb`'s are -0.18156597 and 0.051787917, 0.045930948,
    0.04377312); here the column keeps `initdb`'s row."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp313_o")
        c.execute("create database rp313_o")
    with psycopg.connect(_dsn(rust_pg) + " dbname=rp313_o", autocommit=True) as c:
        c.execute(
            "create function zz_f(a int, b text, c oid) returns int language sql as 'select 1'"
        )
        c.execute("analyze")
        row = c.execute(
            "select n_distinct, avg_width, (most_common_freqs::text::float4[])[1:3]::text"
            " from pg_stats where tablename = 'pg_proc' and attname = 'proargtypes'"
        ).fetchone()
    assert row == (-0.18181819, 31, "{0.051771957,0.045916796,0.04375963}")


@pytest.mark.backlog_fixed("RP-314")
def test_a_catalog_scan_is_estimated(rust_pg: Any) -> None:
    """PostgreSQL 15.19, a fresh database: the planner estimated a scan of
    a catalog from `initdb`'s statistics and `relpages` / `reltuples`, where
    this server printed zeros."""
    with psycopg.connect(_dsn(rust_pg) + " dbname=postgres", autocommit=True) as c:
        c.execute("drop database if exists rp314_d")
        c.execute("create database rp314_d")
    with psycopg.connect(_dsn(rust_pg) + " dbname=rp314_d", autocommit=True) as c:
        plan = [r[0] for r in c.execute("explain select * from pg_class where relkind = 'v'")]
    assert plan == [
        "Seq Scan on pg_class  (cost=0.00..19.12 rows=140 width=265)",
        "  Filter: (relkind = 'v'::\"char\")",
    ]


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
# column's attribute (fixed: it counts every attnum the table has used);
# RP-294, an oidvector would not cast to regtype[] (fixed: COPY input and a
# literal beside a vector read it by `oidvectorin`, as an array).
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
        [
            "select proargtypes::regtype[]::text from pg_proc where oid = 1317",
            "select proargtypes = '16 16', proargtypes::oid[]::text from pg_proc where oid = 60",
            "select count(*) from pg_index where indexrelid = 'pg_class_oid_index'::regclass"
            " and indkey = '1'",
        ],
    ),
}

# The FOUND items closed since: their cases are regression tests now.
FOUND_FIXED = {"RP-293", "RP-294"}

EXPECTED_FOUND: dict[str, Any] = {
    "relnatts counts dropped columns": [None, None, None, [(2,)], None],
    "an oidvector casts to regtype[]": [
        [("[0:0]={text}",)],
        [(True, "[0:1]={16,16}")],
        [(1,)],
    ],
}


def found(dsn: str, steps: list[str]) -> list[Any]:
    out: list[Any] = []
    with _connect(dsn, "found") as c:
        for sql in steps:
            # A refusal is an answer, compared like rows: the gap is the
            # wrong answer, not a test that could not run.
            try:
                cur = c.execute(sql)
            except psycopg.Error as e:
                out.append(("err", e.sqlstate))
                continue
            out.append(cur.fetchall() if cur.description else None)
    return out


@pytest.mark.parametrize(
    "name",
    [
        pytest.param(
            n,
            marks=(pytest.mark.backlog_fixed if item in FOUND_FIXED else pytest.mark.backlog)(item),
            id=n,
        )
        for n, (item, _) in FOUND.items()
    ],
)
def test_found(name: str, rust_pg: Any) -> None:
    assert found(_dsn(rust_pg), FOUND[name][1]) == EXPECTED_FOUND[name]


if __name__ == "__main__" and sys.argv[2:] == ["heap"]:
    print(
        "EXPECTED_CATALOG_BLOCKS: list[dict[str, int]] = "
        + pprint.pformat(catalog_blocks(sys.argv[1]), width=100, sort_dicts=False)
    )
    print(
        "EXPECTED_HEAP_BLOCKS: list[dict[str, int]] = "
        + pprint.pformat(heap_blocks(sys.argv[1]), width=100, sort_dicts=False)
    )
elif __name__ == "__main__":
    measured = catalog_reads(sys.argv[1])
    print("EXPECTED: dict[str, Any] = " + pprint.pformat(measured, width=100, sort_dicts=False))
    gaps = {n: catalog_reads(sys.argv[1], {n: steps})[n] for n, (_, _, steps) in GAPS.items()}
    print("EXPECTED_GAPS: dict[str, Any] = " + pprint.pformat(gaps, width=100, sort_dicts=False))
    print(
        "EXPECTED_FOUND: dict[str, Any] = "
        + pprint.pformat({n: found(sys.argv[1], st) for n, (_, st) in FOUND.items()}, width=100)
    )
