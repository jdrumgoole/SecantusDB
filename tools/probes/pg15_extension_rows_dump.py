"""Dump the objects each extension the Rust PostgreSQL server offers makes on
PostgreSQL 15.19 (RP-105): its member functions, types, operators, casts,
operator classes and families, and their families' operators and support
functions, as ``CREATE EXTENSION`` leaves them in a fresh database.

Writes ``crates/secantus-pg/src/pg15_extension_rows.txt``: for each
extension a ``\\extension name`` line, then a ``\\relation catalog`` block
per catalog in ``pg15_catalog_rows.txt``'s form (a ``reg*`` column as its
oid, COPY text), then a ``\\signatures`` block of its functions'
``oid<TAB>arguments<TAB>identity<TAB>result``. The server copies them
into a database when it installs the extension, each oid the dump drew
moved to one of its own.

    PYTHONPATH=. python tools/probes/pg15_extension_rows_dump.py \
        "host=127.0.0.1 port=5499 user=postgres"

Needs a PostgreSQL 15.19 it may create and drop a scratch database on.
"""

from __future__ import annotations

import pathlib
import sys

import psycopg

ROOT = pathlib.Path(__file__).resolve().parents[2]

EXTENSIONS = ["citext", "hstore", "pg_trgm", "pgcrypto", "btree_gin", "btree_gist"]

# The member rows of each catalog, as the WHERE over it: `members` is the
# extension's own objects (pg_depend deptype 'e').
CATALOGS = {
    "pg_type": "oid in (select objid from members where classid = 'pg_type'::regclass)"
    " or typelem in (select objid from members where classid = 'pg_type'::regclass)"
    " and typcategory = 'A'",
    "pg_proc": "oid in (select objid from members where classid = 'pg_proc'::regclass)",
    "pg_operator": "oid in (select objid from members where classid = 'pg_operator'::regclass)",
    "pg_cast": "oid in (select objid from members where classid = 'pg_cast'::regclass)",
    "pg_opfamily": "oid in (select objid from members where classid = 'pg_opfamily'::regclass)",
    "pg_opclass": "oid in (select objid from members where classid = 'pg_opclass'::regclass)",
    "pg_amop": "amopfamily in (select objid from members where classid = 'pg_opfamily'::regclass)",
    "pg_amproc": "amprocfamily in (select objid from members"
    " where classid = 'pg_opfamily'::regclass)",
}


def escape(s: str) -> str:
    return s.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n")


def main(dsn: str) -> None:
    with psycopg.connect(dsn + " dbname=postgres", autocommit=True) as admin:
        version = admin.execute("show server_version").fetchone()[0]
        if version.split()[0] != "15.19":
            raise SystemExit(f"need PostgreSQL 15.19, connected to {version}")
        out = []
        for ext in EXTENSIONS:
            admin.execute("drop database if exists rp105_dump")
            admin.execute("create database rp105_dump")
            try:
                with psycopg.connect(dsn + " dbname=rp105_dump", autocommit=True) as conn:
                    conn.execute(f"create extension {ext}")
                    out.append(f"\\extension {ext}\n")
                    for catalog, where in CATALOGS.items():
                        columns = conn.execute(
                            "select case when atttypid::regtype::text like 'reg%%'"
                            " then quote_ident(attname) || '::oid'"
                            " when atttypid = 'pg_node_tree'::regtype"
                            " then 'pg_get_expr(' || quote_ident(attname) || ', 0)'"
                            " else quote_ident(attname) end"
                            " from pg_attribute where attrelid = %s::regclass and attnum > 0"
                            " order by attnum",
                            (catalog,),
                        ).fetchall()
                        select = ", ".join(c for (c,) in columns)
                        out.append(f"\\relation pg_catalog.{catalog}\n")
                        query = (
                            "copy (with members as (select classid, objid from pg_depend"
                            " where refclassid = 'pg_extension'::regclass and deptype = 'e'"
                            " and refobjid = (select oid from pg_extension"
                            f" where extname = '{ext}'))"
                            f" select {select} from pg_catalog.{catalog} where {where}"
                            " order by oid) to stdout"
                        )
                        with conn.cursor().copy(query) as copy:
                            for block in copy:
                                out.append(bytes(block).decode())
                    # Its functions' signatures as `\\df` prints them (RP-228).
                    out.append("\\signatures\n")
                    for o, args, ident, result, definition in conn.execute(
                        "with members as (select objid from pg_depend"
                        " where refclassid = 'pg_extension'::regclass and deptype = 'e'"
                        " and classid = 'pg_proc'::regclass and refobjid ="
                        " (select oid from pg_extension where extname = %s))"
                        " select oid, pg_get_function_arguments(oid),"
                        " pg_get_function_identity_arguments(oid),"
                        " coalesce(pg_get_function_result(oid), ''),"
                        " case when prokind = 'a' then '!' || proname"
                        " else pg_get_functiondef(oid) end"
                        " from pg_proc where oid in (select objid from members) order by oid",
                        (ext,),
                    ):
                        out.append(
                            f"{o}\t{escape(args)}\t{escape(ident)}\t{escape(result)}"
                            f"\t{escape(definition)}\n"
                        )
            finally:
                admin.execute("drop database if exists rp105_dump")
        (ROOT / "crates/secantus-pg/src/pg15_extension_rows.txt").write_text("".join(out))


if __name__ == "__main__":
    main(sys.argv[1])
