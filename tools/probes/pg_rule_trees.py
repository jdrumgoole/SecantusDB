"""Compare every rule's node trees (``pg_rewrite.ev_qual`` / ``ev_action``)
between PostgreSQL and the Rust ``secantusd-pg`` (RP-203).

Two populations:

* the system rules ``initdb`` leaves (the 142 of PostgreSQL 15.19): their
  trees embed fixed catalog oids, so they compare byte for byte;
* the database's own rules, from the scenarios below, each run on both
  servers in a transaction that is rolled back. Their trees embed the oids of
  the objects the scenario made, which differ between the servers, so every
  number in an oid-bearing field that names a user object (oid >= 16384) is
  replaced by that object's name before comparing.

The reference must be a PRIVATE PostgreSQL 15.19 cluster (the scenarios
create objects, inside a transaction): ``--reference "host=127.0.0.1
port=5497 user=postgres dbname=postgres"``.

``--record`` writes what PostgreSQL holds -- the sha256 of each system
rule's trees, and of each scenario's normalised trees -- to
``tools/probes/pg_rule_trees.expected.json``, which
``tests/test_rust_pg_rule_trees.py`` holds the server to in CI, where there
is no PostgreSQL to ask.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import threading

import psycopg

REPO_ROOT = pathlib.Path(__file__).resolve().parents[2]
if str(REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(REPO_ROOT))

from tools.provenance import (  # noqa: E402
    PGSERVER_REL,
    require_fresh_pgserver,
    resolve_binary,
)

EXPECTED = pathlib.Path(__file__).with_name("pg_rule_trees.expected.json")

SYSTEM_SQL = (
    "select n.nspname::text, c.relname::text, r.rulename::text, "
    "r.ev_qual::text, r.ev_action::text from pg_rewrite r "
    "join pg_class c on c.oid = r.ev_class "
    "join pg_namespace n on n.oid = c.relnamespace "
    "where r.oid < 16384 order by 1, 2, 3"
)

USER_SQL = (
    "select c.relname::text, r.rulename::text, r.ev_qual::text, "
    "r.ev_action::text from pg_rewrite r join pg_class c on c.oid = r.ev_class "
    "where r.oid >= 16384 order by 1, 2"
)

# Each scenario: statements run in order, each sent as its own query.
SCENARIOS: dict[str, list[str]] = {
    "a view over a table": [
        "create table rt_t (a int primary key, b text, c numeric(10,2))",
        "create view rt_v as select a, b from rt_t where a > 1",
    ],
    "a view with a column list, a cast and a function": [
        "create table rt_t (a int, b text)",
        "create view rt_v (x, y) as select a + 1, upper(b)::varchar(20) from rt_t",
    ],
    "a view with a join, ORDER BY and an aggregate": [
        "create table rt_t (a int, b text)",
        "create table rt_u (a int, d date)",
        "create view rt_v as select t.b, count(*) as n, max(u.d) from rt_t t "
        "join rt_u u using (a) group by t.b order by t.b",
    ],
    "a view with a CASE, COALESCE and IN": [
        "create table rt_t (a int, b text)",
        "create view rt_v as select case when a in (1, 2, 3) then 'small' "
        "else coalesce(b, 'none') end as k from rt_t",
    ],
    "a view with a subquery and EXISTS": [
        "create table rt_t (a int, b text)",
        "create view rt_v as select a, (select max(a) from rt_t) as m from rt_t t "
        "where exists (select 1 from rt_t s where s.a = t.a)",
    ],
    "a view with UNION": [
        "create table rt_t (a int, b text)",
        "create view rt_v as select a from rt_t union select 2 union all select 3",
    ],
    "a view over a system catalog": [
        "create view rt_v as select relname, relkind from pg_class "
        "where relnamespace = 'pg_catalog'::regnamespace",
    ],
    "a rule DO INSTEAD NOTHING": [
        "create table rt_t (a int, b text)",
        "create rule rt_r as on delete to rt_t do instead nothing",
    ],
    "a rule with a qualification and an INSERT action": [
        "create table rt_t (a int, b text)",
        "create table rt_log (a int, note text)",
        "create rule rt_r as on update to rt_t where new.a <> old.a "
        "do also insert into rt_log values (old.a, 'changed')",
    ],
    "a rule with a NOTIFY action": [
        "create table rt_t (a int, b text)",
        "create rule rt_r as on insert to rt_t do also notify rt_chan",
    ],
    "two statements in one query string": [
        "create table rt_t (a int, b text); create view rt_v as select b from rt_t",
    ],
    "a view with SELECT *, DISTINCT and LIMIT": [
        "create table rt_t (a int, b text)",
        "create view rt_v as select distinct * from rt_t order by a limit 10 offset 2",
    ],
    "a view with outer and natural joins": [
        "create table rt_t (a int, b text)",
        "create table rt_u (a int, c text)",
        "create view rt_v as select * from rt_t left join rt_u using (a) "
        "natural full join (select 1 as a) s",
    ],
    "a view with a function in FROM and LATERAL": [
        "create table rt_t (a int, b text)",
        "create view rt_v as select t.a, g from rt_t t, lateral generate_series(1, t.a) g",
    ],
    "a view with a window function": [
        "create table rt_t (a int, b text)",
        "create view rt_v as select a, row_number() over (partition by b order by a desc) "
        "as rn, sum(a) over w from rt_t window w as (order by a)",
    ],
    "a view with operators of every form": [
        "create table rt_t (a int, b text, c numeric, d timestamptz)",
        "create view rt_v as select a between 1 and 5 as x, b like 'a%' as y, "
        "b is distinct from 'q' as z, nullif(a, 0) as n, greatest(a, 2, 3.5) as g, "
        "-a as neg, a = any (array[1, 2]) as arr, c * 2.5 + 1 as num, "
        "d > now() - interval '1 day' as recent, b || '!' as cat, "
        "(a, b) = (1, 'x') as rowcmp, not (a > 1 or a < 0) as logic from rt_t",
    ],
    "a view with casts and typmods": [
        "create table rt_t (a int, b text)",
        "create view rt_v as select b::varchar(10) as v, a::numeric(8, 2) as n, "
        "'x'::char(3) as c, a::text as t, cast(b as name) as nm, '{1,2}'::int[] as ar from rt_t",
    ],
    "a view over a view": [
        "create table rt_t (a int, b text)",
        "create view rt_v1 as select a, b from rt_t",
        "create view rt_v as select v.a from rt_v1 v where v.b is not null",
    ],
    "a view with a subquery in FROM and IN (SELECT)": [
        "create table rt_t (a int, b text)",
        "create view rt_v as select s.x from (select a as x from rt_t) s "
        "where s.x in (select a from rt_t where b = 'q') and s.x > all (select 1)",
    ],
    "a view with COLLATE and an array subscript": [
        "create table rt_t (a int[], b text)",
        'create view rt_v as select a[1] as first, a[1:2] as sl, b collate "C" as bc from rt_t',
    ],
    "a view with a CTE": [
        "create table rt_t (a int, b text)",
        "create view rt_v as with w as (select a from rt_t) select * from w",
    ],
    "a view with several CTEs, aliases and an outer-level reference": [
        "create table rt_t (a int, b text)",
        "create view rt_v as with w (x, y) as materialized (select a, b from rt_t), "
        "u as not materialized (select x from w where y > 'm') "
        "select w.x, u2.z from w join u as u2 (z) on w.x = u2.z "
        "where w.x in (select x from u) "
        "union all (with v as (select 2 as k) select k, k from v)",
    ],
    "a rule with an UPDATE action and several actions": [
        "create table rt_t (a int, b text)",
        "create table rt_log (a int, note text)",
        "create rule rt_r as on update to rt_t do instead (update rt_log set note = new.b "
        "where a = old.a; delete from rt_log where a = old.a + 1)",
    ],
    "a rule with INSERT ... SELECT": [
        "create table rt_t (a int, b text)",
        "create table rt_log (a int, note text)",
        "create rule rt_r as on insert to rt_t do also insert into rt_log "
        "select new.a, b from rt_t where a = new.a",
    ],
    "CREATE OR REPLACE VIEW replaces the trees": [
        "create table rt_t (a int, b text)",
        "create view rt_v as select a from rt_t",
        "create or replace view rt_v as select a, b || 'x' as c from rt_t where a > 0",
    ],
    "a materialized view": [
        "create table rt_t (a int, b text)",
        "create materialized view rt_v as select a, upper(b) as u from rt_t",
    ],
    "a materialized view with a column list, refreshed and renamed": [
        "create table rt_t (a int, b text)",
        "create materialized view rt_m (x) as select a, b from rt_t where a > 0 with no data",
        "refresh materialized view rt_m",
        "alter materialized view rt_m rename to rt_v",
    ],
    "a view with check option and a security barrier": [
        "create table rt_t (a int, b text)",
        "create view rt_v with (security_barrier) as select a from rt_t where a > 1 "
        "with local check option",
    ],
    "views created by PL/pgSQL, statically and by EXECUTE": [
        "create table rt_t (a int, b text)",
        "do $$ begin create view rt_v as select a + 1 as n from rt_t; end $$",
        "do $$ begin execute 'create view rt_w as select b from rt_t where a = 2'; end $$",
    ],
    "a view with date, time, timestamp, uuid and bytea literals": [
        "create table rt_t (a int, d date, t time, ts timestamp, tz timestamptz, u uuid, by bytea)",
        "set local timezone = 'America/New_York'",
        "create view rt_v as select * from rt_t where d > date '2020-02-29' "
        "and d < '1999-12-31' and d <> 'infinity' and t > '10:11:12.5' "
        "and ts < '2020-01-01 10:00' and ts > timestamp '1970-01-01' "
        "and tz >= '2021-06-01 12:00' and tz < '2021-01-01 00:00+02' "
        "and u = '{A0EEBC99-9C0B-4EF8-BB6D-6BB9BD380A11}' and by = '\\x00ff' "
        "and by <> 'ab\\000' and ts::timestamp(0) > '2000-01-01'",
    ],
    "a view with window frames in ROWS, GROUPS and RANGE offsets": [
        "create table rt_t (a int, b text, c numeric, d date, ts timestamptz)",
        "create view rt_v as select "
        "sum(a) over (order by a rows between 1 preceding and current row) as r1, "
        "sum(a) over (order by a groups between 2 preceding and 1 following "
        "exclude ties) as g1, "
        "sum(a) over (order by c range between 1.5 preceding and current row) as n1, "
        "count(*) over (order by d desc nulls last range between "
        "interval '3 days' preceding and unbounded following) as d1, "
        "count(*) over (order by ts range '1 hour' preceding) as t1, "
        "sum(a) over (order by a range between 2 preceding and 2 following) as i1 "
        "from rt_t",
    ],
    "a view with ordered-set and hypothetical-set aggregates": [
        "create table rt_t (a int, b text, c numeric)",
        "create view rt_v as select b, "
        "percentile_cont(0.5) within group (order by a) as med, "
        "percentile_disc(array[0.25, 0.75]) within group (order by c desc) as q, "
        "mode() within group (order by b) as m, "
        "rank(3) within group (order by a) as r, "
        "dense_rank(2) within group (order by a nulls first) as dr, "
        "percent_rank(1.5) within group (order by a) as pr, "
        "rank('m' collate \"C\") within group (order by b) as rc "
        "from rt_t group by b",
    ],
    "a view with WITH RECURSIVE": [
        "create table rt_t (id int, parent int, name text)",
        "create view rt_v as with recursive tree (id, path, depth) as ("
        "select id, name, 1 from rt_t where parent is null "
        "union all select t.id, tree.path || '/' || t.name, tree.depth + 1 "
        "from rt_t t join tree on t.parent = tree.id), "
        "leaf as (select id from rt_t where id not in (select parent from rt_t "
        "where parent is not null)), "
        "nums as (select 1 as n union select n + 1 from nums where n < 5) "
        "select tree.*, n from tree join leaf using (id) cross join nums",
    ],
    "a view with GROUPING SETS, ROLLUP, CUBE and GROUPING()": [
        "create table rt_t (a int, b text, c int, d int)",
        "create view rt_v as select a, b, c, grouping(a, b) as g, sum(d) from rt_t "
        "group by grouping sets ((a, b), (c), ()), rollup (a, (b, c)), cube (d) "
        "order by a nulls first",
    ],
    "a view whose grouping sets fold to a plain GROUP BY": [
        "create table rt_t (a int, b text, c int)",
        "create view rt_v as select a, grouping(a), count(*) from rt_t "
        "group by grouping sets ((a)) having grouping(a) = 0",
    ],
    "a view with GROUP BY DISTINCT and an empty set": [
        "create table rt_t (a int, b text, c int)",
        "create view rt_v as select a, b, count(*) from rt_t "
        "group by distinct rollup (a, b), rollup (a)",
    ],
    "a view grouped through a join's merged column": [
        "create table rt_p (id int primary key, name text)",
        "create table rt_c (id int, v int)",
        "create view rt_v as select id, grouping(id), count(*) from rt_p "
        "full join rt_c using (id) group by rollup (id)",
    ],
    "rules with ON CONFLICT DO NOTHING and a multiple-column SET": [
        "create table rt_t (a int primary key, b text, c int default 5)",
        "create table rt_l (a int primary key, b text, n int)",
        "create rule rt_r1 as on insert to rt_t do also insert into rt_l (a, b) "
        "values (new.a, new.b) on conflict do nothing",
        "create rule rt_r2 as on delete to rt_t do also insert into rt_l (a) "
        "values (old.a) on conflict (a) where a > 0 do nothing",
        "create rule rt_r3 as on update to rt_t do also update rt_l "
        "set (a, b, n) = (new.a, new.b || 'x', default) where a = old.a",
    ],
    "a view in another schema with search_path": [
        "create schema rt_s",
        "create table rt_s.t (a int, b text)",
        "set local search_path = rt_s, public",
        "create view rt_v as select a, count(*) filter (where b > 'm') from t group by a "
        "having count(*) > 1",
    ],
}


class RustServer:
    def __init__(self) -> None:
        binary = resolve_binary(REPO_ROOT / PGSERVER_REL)
        require_fresh_pgserver(binary)
        self.dir = tempfile.mkdtemp(prefix=f"secantus-probe-{os.getpid()}-rule-trees-")
        self.proc = subprocess.Popen(
            [str(binary), self.dir, "127.0.0.1:0", "--trust-unknown-users"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        line = self.proc.stdout.readline() if self.proc.stdout else ""
        m = re.search(r"listening on \S+:(\d+)", line)
        if not m:
            self.stop()
            raise SystemExit(f"secantusd-pg did not start: {line!r}")
        self.dsn = f"host=127.0.0.1 port={m.group(1)} user=postgres dbname=postgres"
        threading.Thread(target=self._drain, daemon=True).start()

    def _drain(self) -> None:
        for _ in self.proc.stdout or []:
            pass

    def stop(self) -> None:
        self.proc.terminate()
        self.proc.wait(timeout=30)
        shutil.rmtree(self.dir, ignore_errors=True)


def first_difference(a: str | None, b: str | None) -> str:
    if a is None or b is None:
        return (
            f"reference {'NULL' if a is None else 'set'}, server {'NULL' if b is None else 'set'}"
        )
    for i, (x, y) in enumerate(zip(a, b, strict=False)):
        if x != y:
            lo, hi = max(0, i - 80), i + 80
            return f"at {i}: reference …{a[lo:hi]!r}\n        server    …{b[lo:hi]!r}"
    return f"lengths {len(a)} vs {len(b)}"


OID_FIELDS = re.compile(
    r":(relid|resorigtbl|vartype|consttype|paramtype|funcid|funcresulttype|opno|opfuncid|"
    r"opresulttype|aggfnoid|aggtype|resulttype|casetype|array_typeid|element_typeid|"
    r"row_typeid|coalescetype|minmaxtype|typeId|refcontainertype|refelemtype|refrestype) (\d+)"
)


def normalise(tree: str | None, names: dict[int, str]) -> str | None:
    if tree is None:
        return None

    def sub(m: re.Match) -> str:
        n = int(m.group(2))
        if n >= 16384:
            return f":{m.group(1)} <{names.get(n, '?')}>"
        return m.group(0)

    return OID_FIELDS.sub(sub, tree)


def object_names(cur) -> dict[int, str]:
    out: dict[int, str] = {}
    for sql in (
        "select oid::int8, 'rel:' || relname from pg_class where oid >= 16384",
        "select oid::int8, 'type:' || typname from pg_type where oid >= 16384",
        "select oid::int8, 'proc:' || proname from pg_proc where oid >= 16384",
    ):
        for o, n in cur.execute(sql).fetchall():
            out[int(o)] = n
    return out


def run_scenario(dsn: str, stmts: list[str]) -> tuple[list[tuple], str | None]:
    with psycopg.connect(dsn) as conn:
        cur = conn.cursor()
        try:
            for s in stmts:
                cur.execute(s)
            names = object_names(cur)
            rows = [
                (rel, rule, normalise(q, names), normalise(a, names))
                for rel, rule, q, a in cur.execute(USER_SQL).fetchall()
            ]
            return rows, None
        except Exception as e:  # noqa: BLE001
            return [], f"{type(e).__name__}: {e}"
        finally:
            conn.rollback()


def digest(tree: str | None) -> str | None:
    """A tree's sha256, ``None`` for a NULL tree."""
    return None if tree is None else hashlib.sha256(tree.encode()).hexdigest()


def system_digests(rows: list[tuple]) -> list[list]:
    return [[nsp, rel, rule, digest(q), digest(a)] for nsp, rel, rule, q, a in rows]


def scenario_digests(rows: list[tuple]) -> list[list]:
    return [[rel, rule, digest(q), digest(a)] for rel, rule, q, a in rows]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--reference", required=True, help="a PRIVATE PostgreSQL 15.19 DSN")
    ap.add_argument(
        "--record", action="store_true", help=f"write PostgreSQL's digests to {EXPECTED.name}"
    )
    ap.add_argument("--verbose", "-v", action="store_true")
    args = ap.parse_args()

    ref = psycopg.connect(args.reference, autocommit=True)
    ref_rows = ref.execute(SYSTEM_SQL).fetchall()
    if args.record:
        version = ref.execute("show server_version").fetchone()[0]
        scenarios = {}
        for name, stmts in SCENARIOS.items():
            rows, err = run_scenario(args.reference, stmts)
            if err:
                raise SystemExit(f"scenario {name!r} fails on the reference: {err}")
            scenarios[name] = scenario_digests(rows)
        EXPECTED.write_text(
            json.dumps(
                {
                    "postgresql": version,
                    "system": system_digests(ref_rows),
                    "scenarios": scenarios,
                },
                indent=1,
            )
            + "\n"
        )
        print(f"recorded {len(ref_rows)} system rules and {len(scenarios)} scenarios to {EXPECTED}")

    server = RustServer()
    try:
        with psycopg.connect(server.dsn, autocommit=True) as rust:
            got = {(r[0], r[1], r[2]): (r[3], r[4]) for r in rust.execute(SYSTEM_SQL).fetchall()}
        match = 0
        for nsp, rel, rule, q, a in ref_rows:
            mine = got.get((nsp, rel, rule))
            if mine == (q, a):
                match += 1
            elif args.verbose:
                print(f"DIFF {nsp}.{rel} {rule}")
                if mine is None:
                    print("    no such rule on the server")
                else:
                    if mine[0] != q:
                        print("    ev_qual  " + first_difference(q, mine[0]))
                    if mine[1] != a:
                        print("    ev_action " + first_difference(a, mine[1]))
        print(f"system rules: {match} of {len(ref_rows)} match byte for byte")

        smatch = 0
        for name, stmts in SCENARIOS.items():
            want, werr = run_scenario(args.reference, stmts)
            have, herr = run_scenario(server.dsn, stmts)
            if werr or herr:
                print(f"SCENARIO {name}: reference {werr or 'ok'}; server {herr or 'ok'}")
                continue
            if want == have:
                smatch += 1
            elif args.verbose:
                print(f"SCENARIO DIFF {name}")
                for w, h in zip(want, have, strict=False):
                    if w != h:
                        print(f"    {w[0]} {w[1]} ev_qual   " + first_difference(w[2], h[2]))
                        print(f"    {w[0]} {w[1]} ev_action " + first_difference(w[3], h[3]))
                if len(want) != len(have):
                    print(f"    {len(want)} rules vs {len(have)}")
        print(f"scenarios: {smatch} of {len(SCENARIOS)} match")
    finally:
        server.stop()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
