"""Differential-probe the ROW ORDER of joins against PostgreSQL 15 (RP-028).

A join with no ORDER BY has no promised order, and a client still sees one:
the order of the plan PostgreSQL's join search picks (join order, join
method, sorted or hashed sides). Under a LIMIT that order decides WHICH rows
come back, and under an ORDER BY it decides how ties are left. So the same
join shapes are run on PostgreSQL and on `secantusd-pg` and the rows are
compared exactly, in order.

The shapes:

- three to five tables `m0..m4` (`a, b, c` holding `i, i % m, i % n`),
  joined in chains, stars and two-hub trees on `m{p}.{b|c} = m{k}.a` --
  one, two or no equivalence classes -- INNER, the last join LEFT, or every
  join LEFT, with no LIMIT, a LIMIT, a LIMIT and OFFSET, and an ORDER BY of
  the first or last table's `a` under a LIMIT;
- two tables under an ORDER BY of one column and a LIMIT (with and without
  OFFSET), INNER or LEFT, on `m0.b = m1.a` or `m0.a = m1.b`;

each over tables of 20, 300 and 3,000 rows, with no index and with one on
`m1.a`. Every group's tables are created inside a transaction that is
rolled back, so neither server analyses them, and the reference never keeps
them. Use a PRIVATE PostgreSQL 15 cluster (the probe creates tables), e.g.
`initdb` a throwaway one on TCP only (`-k ''`).

    PROBE_PG="host=127.0.0.1 port=25917 user=postgres dbname=postgres" \
    PROBE_SERVER="host=127.0.0.1 port=25918 user=postgres dbname=postgres" \
    python tools/probes/pg_join_order_matrix.py [--filter SUBSTR] [--out DIR]

The reference's answers are cached in `DIR/matrix_pg.json` (`--pg`
measures them again); the server's go to `DIR/matrix_rs.json`, and the
shapes that differ to `DIR/matrix_bad.txt`. The summary counts matching
shapes per group: `multiN:limit` / `multiN:nolimit` for N tables, and
`two:order+limit`.
"""

from __future__ import annotations

import itertools
import json
import os
import pathlib
import sys
import time

import psycopg

MODS = [(7, 5), (5, 3), (3, 4), (4, 6), (6, 5)]

# For table k >= 1: (parent table, parent column); clause `m{p}.{col} = m{k}.a`.
PATTERNS = {
    "chain_b": lambda k: (k - 1, "b"),
    "chain_bc": lambda k: (k - 1, "bc"[(k - 1) % 2]),
    "star_b": lambda k: (0, "b"),
    "star_cb": lambda k: (0, "cb"[(k - 1) % 2]),
    "star_bc": lambda k: (0, "bc"[(k - 1) % 2]),
    "two_hubs": lambda k: ([0, 0, 1, 1][k - 1], "b" if k <= 2 else "c"),
    "two_hubs2": lambda k: ([0, 0, 1, 1][k - 1], ["b", "c", "b", "b"][k - 1]),
}
KINDS = {
    "inner": lambda k, n: "join",
    "lastleft": lambda k, n: "left join" if k == n - 1 else "join",
    "allleft": lambda k, n: "left join",
}
SUFFIXES = {
    "none": "",
    "lim5": " limit 5",
    "lim5off3": " limit 5 offset 3",
    "ord0lim5": " order by m0.a limit 5",
    "ordlastlim5": " order by m{last}.a limit 5",
}
SIZES = [20, 300, 3000]
INDEXES = {"plain": [], "idx1": ["create index on m1 (a)"]}


def setup(n_tables: int, size: int, index: str) -> list[str]:
    out = []
    for t in range(n_tables):
        mb, mc = MODS[t]
        out.append(f"create table m{t} (a int, b int, c int)")
        out.append(
            f"insert into m{t} select i, i % {mb}, i % {mc} from generate_series(1, {size}) i"
        )
    out.extend(INDEXES[index])
    return out


def multi_queries(n: int) -> list[tuple[str, str]]:
    out = []
    for (pname, pat), (kname, kind), (sname, suf) in itertools.product(
        PATTERNS.items(), KINDS.items(), SUFFIXES.items()
    ):
        if n == 3 and pname in ("two_hubs", "two_hubs2"):
            continue
        sql = "select " + ", ".join(f"m{t}.a" for t in range(n)) + " from m0"
        for k in range(1, n):
            p, col = pat(k)
            sql += f" {kind(k, n)} m{k} on m{p}.{col} = m{k}.a"
        sql += suf.replace("{last}", str(n - 1))
        out.append((f"{pname}/{kname}/{sname}", sql))
    return out


def two_queries() -> list[tuple[str, str]]:
    out = []
    clauses = {"b_a": "m0.b = m1.a", "a_b": "m0.a = m1.b"}
    orders = ["m0.a", "m1.a", "m0.b", "m1.b", "m0.c"]
    lims = {"lim5": " limit 5", "lim5off2": " limit 5 offset 2"}
    for (cn, cl), kind, order, (ln, lim) in itertools.product(
        clauses.items(), ["join", "left join"], orders, lims.items()
    ):
        sql = f"select m0.a, m1.a from m0 {kind} m1 on {cl} order by {order}{lim}"
        out.append((f"{cn}/{kind.replace(' ', '')}/{order}/{ln}", sql))
    return out


def groups():
    for n in (3, 4, 5):
        for size in SIZES:
            for index in INDEXES:
                if size == 3000 and index != "plain":
                    continue
                yield f"multi{n}", n, size, index, multi_queries(n)
    for size in SIZES:
        for index in INDEXES:
            yield "two", 2, size, index, two_queries()


def answer(conn: psycopg.Connection, sql: str) -> list:
    try:
        return ["ok", [list(r) for r in conn.execute(sql).fetchall()]]
    except psycopg.Error as e:
        conn.rollback()
        conn.execute("begin")
        return ["err", e.sqlstate, str(e).splitlines()[0]]


def run(dsn: str, flt: str | None) -> dict:
    conn = psycopg.connect(dsn, autocommit=True)
    res = {}
    for cat, n, size, index, queries in groups():
        keyed = [
            (f"{cat}/{size}/{index}/{name}", sql)
            for name, sql in queries
            if flt is None or flt in f"{cat}/{size}/{index}/{name}"
        ]
        if not keyed:
            continue
        conn.execute("begin")
        for s in setup(n, size, index):
            conn.execute(s)
        for key, sql in keyed:
            t = time.time()
            res[key] = {"sql": sql, "ans": answer(conn, sql), "secs": round(time.time() - t, 3)}
        conn.execute("rollback")
    conn.close()
    return res


def main() -> None:
    args = sys.argv[1:]
    flt = args[args.index("--filter") + 1] if "--filter" in args else None
    out = pathlib.Path(args[args.index("--out") + 1] if "--out" in args else ".")
    out.mkdir(parents=True, exist_ok=True)
    pg_file = out / "matrix_pg.json"
    if "--pg" in args or not pg_file.exists():
        pg = run(os.environ["PROBE_PG"], flt)
        if flt is None:
            pg_file.write_text(json.dumps(pg))
    else:
        pg = json.loads(pg_file.read_text())
    rs = run(os.environ["PROBE_SERVER"], flt)
    (out / "matrix_rs.json").write_text(json.dumps(rs))
    summary: dict[str, list[int]] = {}
    bad = []
    for key, r in rs.items():
        p = pg[key]
        cat = key.split("/")[0]
        lim = "nolimit" if key.endswith("/none") else "limit"
        bucket = f"{cat}:{lim}" if cat != "two" else "two:order+limit"
        s = summary.setdefault(bucket, [0, 0])
        s[1] += 1
        # A reference error is no answer to match: count it against the probe.
        if p["ans"][0] == "err":
            bad.append(("REFERENCE-ERROR", key, p["ans"]))
        elif r["ans"] == p["ans"]:
            s[0] += 1
        else:
            bad.append(("DIFF", key, r["ans"][:3] if r["ans"][0] == "err" else ""))
    for b, (ok, tot) in sorted(summary.items()):
        print(f"{b:22s} {ok:5d} / {tot}")
    (out / "matrix_bad.txt").write_text("\n".join(f"{k}\t{key}\t{x}" for k, key, x in bad) + "\n")
    print(f"{len(bad)} differences -> {out / 'matrix_bad.txt'}")


if __name__ == "__main__":
    main()
