"""Dump PostgreSQL 15.19's system views and static catalog rows for the Rust
PG server (RP-070).

Run against a FRESH database on a PostgreSQL 15.19 cluster (nothing created
in it), so every row is one ``initdb`` made:

    python tools/probes/pg15_system_catalog_dump.py "host=... port=... dbname=fresh"

Writes three files the server compiles in:

* ``crates/secantus-pgplan/src/pg15_system_views.tsv`` -- ``schema.view``,
  its oid, then ``pg_get_viewdef(view, true)`` with ``\\``, tab and newline escaped.
  The planner expands a system view from it as the rewriter expands any
  view.
* ``crates/secantus-pgplan/src/pg15_information_schema_functions.tsv`` --
  ``information_schema``'s scalar SQL helpers, which the planner inlines.
* ``crates/secantus-pg/src/pg15_catalog_rows.txt`` -- for each table in
  ``STATIC_TABLES``, a ``\\relation schema.table`` line and then the table's
  rows in COPY text format, in ``ctid`` order, a ``reg*`` column as its oid.
"""

from __future__ import annotations

import pathlib
import sys

import psycopg

ROOT = pathlib.Path(__file__).resolve().parents[2]

# The system views the server does not compute itself: each is served from
# PostgreSQL's own definition, over the server's catalogs.
VIEWS = [
    "information_schema._pg_foreign_data_wrappers",
    "information_schema._pg_foreign_servers",
    "information_schema._pg_foreign_table_columns",
    "information_schema._pg_foreign_tables",
    "information_schema._pg_user_mappings",
    "information_schema.administrable_role_authorizations",
    "information_schema.applicable_roles",
    "information_schema.attributes",
    "information_schema.character_sets",
    "information_schema.check_constraint_routine_usage",
    "information_schema.check_constraints",
    "information_schema.collation_character_set_applicability",
    "information_schema.collations",
    "information_schema.column_column_usage",
    "information_schema.column_domain_usage",
    "information_schema.column_options",
    "information_schema.column_privileges",
    "information_schema.column_udt_usage",
    "information_schema.constraint_column_usage",
    "information_schema.constraint_table_usage",
    "information_schema.data_type_privileges",
    "information_schema.domain_constraints",
    "information_schema.domain_udt_usage",
    "information_schema.domains",
    "information_schema.element_types",
    "information_schema.enabled_roles",
    "information_schema.foreign_data_wrapper_options",
    "information_schema.foreign_server_options",
    "information_schema.foreign_table_options",
    "information_schema.information_schema_catalog_name",
    "information_schema.parameters",
    "information_schema.referential_constraints",
    "information_schema.role_column_grants",
    "information_schema.role_routine_grants",
    "information_schema.role_udt_grants",
    "information_schema.role_usage_grants",
    "information_schema.routine_column_usage",
    "information_schema.routine_privileges",
    "information_schema.routine_routine_usage",
    "information_schema.routine_sequence_usage",
    "information_schema.routine_table_usage",
    "information_schema.routines",
    "information_schema.schemata",
    "information_schema.transforms",
    "information_schema.triggered_update_columns",
    "information_schema.triggers",
    "information_schema.udt_privileges",
    "information_schema.usage_privileges",
    "information_schema.user_defined_types",
    "information_schema.user_mapping_options",
    "information_schema.user_mappings",
    "information_schema.view_column_usage",
    "information_schema.view_routine_usage",
    "information_schema.view_table_usage",
    "pg_catalog.pg_group",
    "pg_catalog.pg_seclabels",
    "pg_catalog.pg_shadow",
]

# Tables whose rows are PostgreSQL's built-in objects and nothing else on a
# fresh database.
STATIC_TABLES = [
    "information_schema.sql_features",
    "information_schema.sql_implementation_info",
    "information_schema.sql_parts",
    "information_schema.sql_sizing",
    "pg_catalog.pg_aggregate",
    "pg_catalog.pg_amop",
    "pg_catalog.pg_amproc",
    "pg_catalog.pg_conversion",
    "pg_catalog.pg_init_privs",
    "pg_catalog.pg_opfamily",
    # Every built-in function, type and catalog constraint (RP-102); the
    # server lists its users' objects after them.
    "pg_catalog.pg_proc",
    "pg_catalog.pg_type",
    "pg_catalog.pg_constraint",
]


def escape(s: str) -> str:
    return s.replace("\\", "\\\\").replace("\t", "\\t").replace("\n", "\\n")


def main(dsn: str) -> None:
    with psycopg.connect(dsn, autocommit=True) as conn:
        version = conn.execute("show server_version").fetchone()[0]
        if version.split()[0] != "15.19":
            raise SystemExit(f"need PostgreSQL 15.19, connected to {version}")
        lines = []
        for view in VIEWS:
            oid, definition = conn.execute(
                "select %s::regclass::oid, pg_get_viewdef(%s::regclass, true)", (view, view)
            ).fetchone()
            lines.append(f"{view}\t{oid}\t{escape(definition.strip())}\n")
        (ROOT / "crates/secantus-pgplan/src/pg15_system_views.tsv").write_text("".join(lines))

        # information_schema's scalar SQL helpers, for the planner to inline:
        # name, parameter names, argument count, body as an expression.
        helpers = []
        for name, params, nargs, body in conn.execute(
            "select p.proname, coalesce(array_to_string(p.proargnames, ','), ''),"
            " p.pronargs, pg_get_function_sqlbody(p.oid) from pg_proc p"
            " where p.pronamespace = 'information_schema'::regnamespace"
            " and not p.proretset order by 1"
        ):
            body = body.strip()
            if body.startswith("RETURN "):
                expr = body.removeprefix("RETURN ")
            else:
                select = body.removeprefix("BEGIN ATOMIC").removesuffix("END")
                expr = f"({select.strip().rstrip(';')})"
            helpers.append(f"{name}\t{params}\t{nargs}\t{escape(expr)}\n")
        (ROOT / "crates/secantus-pgplan/src/pg15_information_schema_functions.tsv").write_text(
            "".join(helpers)
        )

        out = []
        for table in STATIC_TABLES:
            out.append(f"\\relation {table}\n")
            # A reg* column as its oid, which the server renders by name; an
            # expression (pg_node_tree) as the SQL text the server keeps for
            # its own objects' expressions.
            columns = conn.execute(
                "select case when atttypid::regtype::text like 'reg%%'"
                " then quote_ident(attname) || '::oid'"
                " when atttypid = 'pg_node_tree'::regtype then"
                " case attname when 'conbin' then 'pg_get_expr(conbin, conrelid)'"
                " when 'prosqlbody' then 'pg_get_function_sqlbody(oid)'"
                " else 'pg_get_expr(' || quote_ident(attname) || ', 0)' end"
                " else quote_ident(attname) end"
                " from pg_attribute where attrelid = %s::regclass and attnum > 0"
                " order by attnum",
                (table,),
            ).fetchall()
            select = ", ".join(c for (c,) in columns)
            query = f"copy (select {select} from {table} order by ctid) to stdout"
            with conn.cursor().copy(query) as copy:
                for block in copy:
                    out.append(bytes(block).decode())
        (ROOT / "crates/secantus-pg/src/pg15_catalog_rows.txt").write_text("".join(out))


if __name__ == "__main__":
    main(sys.argv[1])
