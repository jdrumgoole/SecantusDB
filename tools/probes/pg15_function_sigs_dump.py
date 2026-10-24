"""Dump PostgreSQL 15.19's own signatures for its built-in functions (RP-228):
``pg_get_function_arguments``, ``pg_get_function_identity_arguments`` and
``pg_get_function_result`` of every function a fresh database has, as
``oid<TAB>arguments<TAB>identity<TAB>result`` lines in
``crates/secantus-pgplan/src/pg15_function_sigs.tsv``, and each one's
``pg_get_functiondef`` (RP-229) in ``pg15_function_defs.tsv``.

    PYTHONPATH=. python tools/probes/pg15_function_sigs_dump.py \
        "host=127.0.0.1 port=5499 user=postgres"
"""

from __future__ import annotations

import pathlib
import sys

import psycopg

ROOT = pathlib.Path(__file__).resolve().parents[2]


def escape(s: str) -> str:
    return s.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n")


def main(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as conn:
        version = conn.execute("show server_version").fetchone()[0]
        if version.split()[0] != "15.19":
            raise SystemExit(f"need PostgreSQL 15.19, connected to {version}")
        rows = conn.execute(
            "select oid, pg_get_function_arguments(oid), pg_get_function_identity_arguments(oid),"
            " coalesce(pg_get_function_result(oid), '') from pg_proc where oid < 16384 order by oid"
        ).fetchall()
        out = "".join(f"{o}\t{escape(a)}\t{escape(i)}\t{escape(r)}\n" for o, a, i, r in rows)
        (ROOT / "crates/secantus-pgplan/src/pg15_function_sigs.tsv").write_text(out)
        # Each one's `pg_get_functiondef` (RP-229); an aggregate, which it
        # refuses, as `!` and its name.
        defs = conn.execute(
            "select oid, case when prokind = 'a' then '!' || proname"
            " else pg_get_functiondef(oid) end from pg_proc where oid < 16384 order by oid"
        ).fetchall()
        (ROOT / "crates/secantus-pgplan/src/pg15_function_defs.tsv").write_text(
            "".join(f"{o}\t{escape(d)}\n" for o, d in defs)
        )


if __name__ == "__main__":
    main(sys.argv[1])
