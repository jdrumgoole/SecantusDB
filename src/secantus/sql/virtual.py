"""Virtual catalog tables: ``information_schema`` + ``pg_catalog`` (no-join).

Postgres clients introspect schemas by querying system catalogs. P2 serves the
no-join subset of those reads — ``information_schema.tables`` / ``.columns`` /
``.schemata`` and a few ``pg_catalog`` relations — computed on demand from the
SQL catalog. Each is exposed as a synthetic table whose rows are plain dicts;
the ordinary ``SELECT`` planner/executor then applies ``WHERE`` / ``ORDER BY`` /
``LIMIT`` against them via a tiny in-memory backend, so no new query code is
needed.

Queries that *join* these catalogs (what interactive ``psql``'s ``\\d`` emits)
need the join + function machinery of a later phase; those still fall through to
an undefined-table error rather than a wrong answer.
"""

from __future__ import annotations

import re
from collections.abc import Callable
from typing import Any

from secantus.paths import get_path
from secantus.query import matches
from secantus.sql import typemap
from secantus.sql.catalog import (
    COMPOSITE_TYPE_OID_BASE,
    DOMAIN_TYPE_OID_BASE,
    ENUM_TYPE_OID_BASE,
    USER_TYPE_ARRAY_OID_OFFSET,
    Catalog,
    Column,
    ForeignKey,
    TableDef,
    fold_type_name,
)
from secantus.sql.session import Session

# Stable, fictional OIDs for the namespaces we advertise.
_NS_OIDS = {"pg_catalog": 11, "public": 2200, "information_schema": 13000}

# A virtual table: a column spec + a builder that returns row dicts.
ColumnsSpec = list[tuple[str, str]]  # (name, type_tag)
RowsBuilder = Callable[[str, "Session", Any, Catalog], list[dict[str, Any]]]


class VirtualTable:
    def __init__(self, schema: str, name: str, columns: ColumnsSpec, builder: RowsBuilder) -> None:
        self.schema = schema
        self.name = name
        self.columns = columns
        self.builder = builder

    def table_def(self) -> TableDef:
        return TableDef(
            name=self.name,
            collection=self.name,
            columns=[
                Column(name=c, type_tag=t, field=c, pk=False, nullable=True)
                for c, t in self.columns
            ],
        )


# --------------------------------------------------------------------------- #
# Row builders
# --------------------------------------------------------------------------- #


def _user_tables(db: str, catalog: Catalog) -> list[TableDef]:
    return [t for name in catalog.list_tables(db) if (t := catalog.get(db, name)) is not None]


def _tables_with_oids(db: str, catalog: Catalog) -> tuple[list[TableDef], dict[str, int]]:
    """The user tables and their OIDs from a *single* enumeration.

    A builder that needs both must not call ``_user_tables`` and ``_table_oids``
    separately: another session's DDL landing between the two calls leaves the
    second list holding a table the OID map never saw, and the builder dies with
    a ``KeyError`` part-way through a catalog scan."""
    tables = _user_tables(db, catalog)
    return tables, {t.name: 16384 + i for i, t in enumerate(tables)}


def _table_oids(db: str, catalog: Catalog) -> dict[str, int]:
    """Stable, fictional pg_class OIDs per table — shared by every catalog that
    keys off ``relid`` (pg_class.oid, pg_attribute.attrelid) so joins line up."""
    return _tables_with_oids(db, catalog)[1]


# Table row types (typtype 'c'): pg_type oids derived from the table's
# pg_class oid, with the paired array oid one offset above.
_ROWTYPE_OID_BASE = 250000
_ROWTYPE_ARRAY_OID_OFFSET = 100000


def _table_rowtype_oids(db: str, catalog: Catalog) -> dict[str, int]:
    return {
        name: _ROWTYPE_OID_BASE + (toid - 16384) for name, toid in _table_oids(db, catalog).items()
    }


_VIEW_OID_BASE = 50000
_SEQUENCE_OID_BASE = 55000
_ROLE_OID_BASE = 60000
_FUNCTION_OID_BASE = 65000
_SQL_LANG_OID = 14  # pg_language oid for LANGUAGE sql


def _view_names(db: str, catalog: Catalog) -> list[str]:
    lister = getattr(catalog, "list_views", None)
    return lister(db) if lister is not None else []


def _sequence_names(db: str, catalog: Catalog) -> list[str]:
    lister = getattr(catalog, "list_sequences", None)
    return lister(db) if lister is not None else []


def _sequence_oids(db: str, catalog: Catalog) -> dict[str, int]:
    """Stable pg_class OIDs per sequence — a distinct range so relkind='S' rows
    never collide with tables / indexes / views."""
    return {name: _SEQUENCE_OID_BASE + i for i, name in enumerate(_sequence_names(db, catalog))}


def _matview_names(db: str, catalog: Catalog) -> set[str]:
    lister = getattr(catalog, "list_matviews", None)
    return set(lister(db)) if lister is not None else set()


def _view_oids(db: str, catalog: Catalog) -> dict[str, int]:
    """Stable pg_class OIDs per view — a distinct range from tables/indexes/FKs so
    ``pg_get_viewdef(oid)`` and relkind='v' rows never collide with a real table."""
    return {name: _VIEW_OID_BASE + i for i, name in enumerate(_view_names(db, catalog))}


# Access-method / opclass OIDs (the real Postgres values for btree).
_BTREE_AM_OID = 403
_HEAP_AM_OID = 2
_DEFAULT_OPCLASS_OID = 1978
_INDEX_OID_BASE = 24576


def _index_coldef(field: str, direction: Any, field_to_name: dict[str, str]) -> str:
    """Render one indexed column for ``pg_get_indexdef`` — its SQL name plus
    ``DESC`` when the index stores it descending."""
    name = field_to_name.get(field, field)
    desc = str(direction).upper() in ("-1", "DESC")
    return f"{name} DESC" if desc else name


def _indexes(db: str, storage: Any, catalog: Catalog) -> list[dict[str, Any]]:
    """Every index relation (implicit PK index + each ``CREATE INDEX`` + the index
    backing each UNIQUE constraint) with everything reflection needs: a stable
    ``indexrelid`` / ``indrelid``, ``indkey`` attnums, unique/primary flags, and —
    for ``pg_get_indexdef`` / ``pg_indexes`` — the owning ``table`` name and the
    rendered ``columns`` (with ``DESC``). ``partial`` flags a partial index (its
    predicate isn't reversed back to SQL)."""
    tables, table_oids = _tables_with_oids(db, catalog)
    out: list[dict[str, Any]] = []
    oid = _INDEX_OID_BASE
    for t in tables:
        relid = table_oids[t.name]
        field_to_attnum = {col.field: i for i, col in enumerate(t.columns, start=1)}
        field_to_name = {col.field: col.name for col in t.columns}
        # An expression index is stored over a hidden ``__expr_<name>`` field; map
        # that field back to its source SQL so the index reflects like Postgres'
        # (indkey attnum 0 marks an expression column).
        field_to_expr = {ei.field: ei for ei in getattr(t, "expr_indexes", [])}
        pk_cols = t.ordered_pk_columns()
        if pk_cols:
            out.append(
                {
                    "indexrelid": oid,
                    "indrelid": relid,
                    "relname": t.pk_constraint_name(),
                    "indkey": [field_to_attnum.get(c.field, 1) for c in pk_cols],
                    "unique": True,
                    "primary": True,
                    "conname": t.pk_constraint_name(),
                    "table": t.name,
                    "columns": [c.name for c in pk_cols],
                    "partial": False,
                }
            )
            oid += 1
        constraint_backed = {uq.name for uq in getattr(t, "unique_constraints", [])}
        for ix in storage.list_indexes(db, t.collection):
            if ix.get("name") == "_id_":
                continue  # WiredTiger's physical _id index — the PK is shown as <t>_pkey
            if ix.get("name") in constraint_backed:
                # The storage index enforcing a declared UNIQUE constraint. It
                # is reported below from the constraint itself, which carries
                # the conname / conkey a client expects, so listing it here as
                # well would show the same index twice.
                continue
            key = ix.get("key") or {}
            expr_fields = [f for f in key if f in field_to_expr]
            if expr_fields:
                # Expression index: single hidden field → attnum 0 + rendered SQL.
                ei = field_to_expr[expr_fields[0]]
                out.append(
                    {
                        "indexrelid": oid,
                        "indrelid": relid,
                        "relname": ix["name"],
                        "indkey": [0],
                        "unique": bool(ix.get("unique")),
                        "primary": False,
                        "conname": None,
                        "table": t.name,
                        "columns": [f"({ei.expr_sql.lower()})"],
                        "partial": bool(ix.get("partialFilterExpression")),
                    }
                )
                oid += 1
                continue
            indkey = [field_to_attnum.get(f) for f in key]
            if not indkey or any(a is None for a in indkey):
                continue  # index over a non-column field — not reflectable as SQL
            nkeyatts = len(indkey)
            # ``INCLUDE (cols)`` covering columns ride indkey beyond
            # indnkeyatts, exactly how real pg_index encodes them.
            for inc in ix.get("include") or []:
                col = t.column(inc)
                attnum = field_to_attnum.get(col.field) if col is not None else None
                if attnum is not None:
                    indkey.append(attnum)
            out.append(
                {
                    "indexrelid": oid,
                    "indrelid": relid,
                    "relname": ix["name"],
                    "indkey": indkey,
                    "nkeyatts": nkeyatts,
                    "unique": bool(ix.get("unique")),
                    "primary": False,
                    "conname": None,
                    "table": t.name,
                    "columns": [_index_coldef(f, d, field_to_name) for f, d in key.items()],
                    "partial": bool(ix.get("partialFilterExpression")),
                }
            )
            oid += 1
    # The implicit unique index backing each declared UNIQUE constraint.
    for uq in _unique_constraints(db, catalog):
        out.append(
            {
                "indexrelid": uq["conindid"],
                "indrelid": uq["conrelid"],
                "relname": uq["conname"],
                "indkey": uq["conkey"],
                "unique": True,
                "primary": False,
                "conname": uq["conname"],
                "table": uq["table"].name,
                "columns": list(uq["columns"]),
                "partial": False,
            }
        )
    return out


_INDEX_RELATION_KEYS = (
    "indexrelid",
    "indrelid",
    "relname",
    "indkey",
    "unique",
    "primary",
    "conname",
)


def _relation_row(ix: dict[str, Any]) -> dict[str, Any]:
    row = {k: ix[k] for k in _INDEX_RELATION_KEYS}
    row["nkeyatts"] = ix.get("nkeyatts", len(ix["indkey"]))
    return row


def _index_relations(db: str, storage: Any, catalog: Catalog) -> list[dict[str, Any]]:
    """The pg_index / pg_class / pg_constraint projection of :func:`_indexes` — a
    stable ``indexrelid``, its table ``indrelid``, the ``indkey`` attnum array, and
    unique/primary flags. (Kept as the historical shape those builders consume.)"""
    return [_relation_row(ix) for ix in _indexes(db, storage, catalog)]


def indexdef_for_oid(db: str, storage: Any, catalog: Catalog, oid: int) -> str | None:
    """``pg_get_indexdef(oid)`` — reconstruct the ``CREATE INDEX`` statement for an
    index relation oid, or None when the oid isn't a known index."""
    for ix in _indexes(db, storage, catalog):
        if ix["indexrelid"] == oid:
            unique = "UNIQUE " if ix["unique"] else ""
            cols = ", ".join(ix["columns"])
            return (
                f"CREATE {unique}INDEX {ix['relname']} ON public.{ix['table']} USING btree ({cols})"
            )
    return None


def _info_tables(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    # Materialized views are pg_catalog relations only — Postgres does not list
    # them in information_schema.tables, so neither do we.
    matviews = _matview_names(db, catalog)
    rows = [
        {
            "table_catalog": db,
            # Real PG homes a temp table in its session's pg_temp_N schema
            # with type LOCAL TEMPORARY, so schema-filtered reflection
            # (table_schema = 'public') never lists it. The schema is the
            # name's own per-session pg_temp_<n> prefix.
            "table_schema": (_table_schema_name(t.name) if "." in t.name else "pg_temp_1")
            if t.temp
            else _table_schema_name(t.name),
            "table_name": _bare_table_name(t.name),
            "table_type": "LOCAL TEMPORARY" if t.temp else "BASE TABLE",
        }
        for t in _user_tables(db, catalog)
        if t.name not in matviews
    ]
    # Views appear in information_schema.tables with table_type 'VIEW'.
    rows.extend(
        {
            "table_catalog": db,
            "table_schema": _table_schema_name(name),
            "table_name": _bare_table_name(name),
            "table_type": "VIEW",
        }
        for name in _view_names(db, catalog)
    )
    return rows


def _info_views(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    getter = getattr(catalog, "get_view", None)
    co_getter = getattr(catalog, "get_view_check_option", None)
    rows: list[dict] = []
    for name in _view_names(db, catalog):
        check = co_getter(db, name) if co_getter is not None else None
        rows.append(
            {
                "table_catalog": db,
                "table_schema": "public",
                "table_name": name,
                "view_definition": getter(db, name) if getter is not None else None,
                "check_option": check or "NONE",
                "is_updatable": "NO",
                "is_insertable_into": "NO",
            }
        )
    return rows


def _info_columns(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    rows: list[dict] = []
    for t in _user_tables(db, catalog):
        for i, col in enumerate(t.columns, start=1):
            rows.append(
                {
                    "table_catalog": db,
                    "table_schema": "public",
                    "table_name": t.name,
                    "column_name": col.name,
                    "ordinal_position": i,
                    "data_type": (
                        "ARRAY"
                        if typemap.is_array_tag(col.type_tag)
                        else typemap.SQL_TYPE_NAME.get(col.type_tag, "text")
                    ),
                    "is_nullable": "NO" if not col.nullable else "YES",
                    "column_default": _column_default_text(col),
                }
            )
    return rows


def _column_default_text(col: Any) -> str | None:
    """The ``information_schema.columns.column_default`` text for a column: the
    expression default's SQL, a ``nextval`` for a sequence-backed column, or the
    rendered literal. None for a column with no default."""
    if getattr(col, "default_expr", None) is not None:
        return col.default_expr
    if col.sequence is not None:
        return f"nextval('{col.sequence}'::regclass)"
    if not col.has_default:
        return None
    v = col.default
    if v is None:
        return "NULL"
    if isinstance(v, bool):
        return "true" if v else "false"
    if isinstance(v, str):
        return "'" + v.replace("'", "''") + "'::text"
    return str(v)


def _info_schemata(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    return [
        {"catalog_name": db, "schema_name": s}
        for s in ("public", "information_schema", "pg_catalog")
    ]


def _info_column_grants(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``information_schema.column_privileges`` — one row per (grantee, table,
    column, privilege) recorded by a column-scoped GRANT (#131)."""
    lister = getattr(catalog, "list_column_grants", None)
    if lister is None:
        return []
    rows: list[dict] = []
    for doc in lister(db):
        for priv in doc.get("privileges", ()):
            rows.append(
                {
                    "grantor": session.user,
                    "grantee": doc["grantee"],
                    "table_catalog": db,
                    "table_schema": "public",
                    "table_name": doc["table"],
                    "column_name": doc["column"],
                    "privilege_type": priv,
                    "is_grantable": "NO",
                }
            )
    return rows


def _info_table_grants(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``information_schema.role_table_grants`` / ``.table_privileges`` — one row per
    ``(grantee, table, privilege)`` recorded by GRANT. The grantor isn't tracked
    (single-node dev surface), so it's reported as the connected user."""
    lister = getattr(catalog, "list_table_grants", None)
    if lister is None:
        return []
    rows: list[dict] = []
    for doc in lister(db):
        for priv in doc.get("privileges", ()):
            rows.append(
                {
                    "grantor": session.user,
                    "grantee": doc["grantee"],
                    "table_catalog": db,
                    "table_schema": "public",
                    "table_name": doc["table"],
                    "privilege_type": priv,
                    "is_grantable": "YES" if doc.get("grant_option") else "NO",
                    "with_hierarchy": "YES" if priv == "SELECT" else "NO",
                }
            )
    return rows


def _pk_constraints(db: str, catalog: Catalog) -> list[tuple[TableDef, str, list[str]]]:
    """Each table's PRIMARY KEY constraint as ``(table, constraint_name, [cols])``.
    The PK is the only real constraint in our model (a ``CREATE UNIQUE INDEX`` is
    an index, not a constraint), so these builders surface PK rows only."""
    out: list[tuple[TableDef, str, list[str]]] = []
    for t in _user_tables(db, catalog):
        pk_cols = t.ordered_pk_columns()
        if pk_cols:
            out.append((t, t.pk_constraint_name(), [c.name for c in pk_cols]))
    return out


_PK_CON_OID_BASE = 30000
_FK_OID_BASE = 40000


_SAFE_IDENT_RE = re.compile(r"^[a-z_][a-z0-9_$]*$")


def _quote_ident(name: str) -> str:
    """``quote_ident`` semantics: bare when already lowercase-safe, else
    double-quoted with embedded quotes doubled — matching how real
    ``pg_get_constraintdef`` renders identifiers (SQLAlchemy regex-parses
    that text, and an unquoted ``i need quotes`` breaks its parser)."""
    if _SAFE_IDENT_RE.match(name):
        return name
    return '"' + name.replace('"', '""') + '"'


def _fk_condef(fk: ForeignKey, ref_cols: list[str]) -> str:
    """Render a foreign key the way ``pg_get_constraintdef`` does — SQLAlchemy's
    inspector regex-parses exactly this string to reflect the constraint."""
    cols = ", ".join(_quote_ident(c) for c in fk.columns)
    rcols = ", ".join(_quote_ident(c) for c in ref_cols)
    # A cross-schema target is stored dotted; render schema and relation as
    # separately-quoted identifiers so the inspector's regex parses them.
    if "." in fk.ref_table:
        rschema, rname = fk.ref_table.split(".", 1)
        ref_sql = f"{_quote_ident(rschema)}.{_quote_ident(rname)}"
    else:
        ref_sql = _quote_ident(fk.ref_table)
    text = f"FOREIGN KEY ({cols}) REFERENCES {ref_sql}({rcols})"
    if fk.on_update:
        text += f" ON UPDATE {fk.on_update}"
    if fk.on_delete:
        text += f" ON DELETE {fk.on_delete}"
    return text


def _foreign_keys(db: str, catalog: Catalog) -> list[dict[str, Any]]:
    """Every declared foreign key with the fields the ``pg_constraint`` /
    ``information_schema`` / ``pg_get_constraintdef`` reflection paths need: a
    stable ``oid``, owner/referenced table OIDs, ``conkey``/``confkey`` attnum
    arrays, the resolved referenced columns, and the rendered ``condef``."""
    table_list, table_oids = _tables_with_oids(db, catalog)
    tables = {t.name: t for t in table_list}
    out: list[dict[str, Any]] = []
    oid = _FK_OID_BASE
    for t in table_list:
        owner_attnum = {c.name: i for i, c in enumerate(t.columns, start=1)}
        for fk in t.foreign_keys:
            ref = tables.get(fk.ref_table)
            ref_cols = list(fk.ref_columns)
            if not ref_cols and ref is not None and ref.pk_columns:
                ref_cols = [c.name for c in ref.pk_columns]  # REFERENCES t → its PRIMARY KEY
            ref_attnum = {c.name: i for i, c in enumerate(ref.columns, start=1)} if ref else {}
            out.append(
                {
                    "oid": oid,
                    "conname": fk.name,
                    "table": t,
                    "fk": fk,
                    "conrelid": table_oids.get(t.name, 0),
                    "confrelid": table_oids.get(fk.ref_table, 0),
                    "conkey": [owner_attnum.get(c, 0) for c in fk.columns],
                    "confkey": [ref_attnum.get(c, 0) for c in ref_cols],
                    "ref_cols": ref_cols,
                    "ref_pk_name": ref.pk_constraint_name() if ref is not None else None,
                    "condef": _fk_condef(fk, ref_cols),
                }
            )
            oid += 1
    return out


_UNIQUE_CON_OID_BASE = 45000
_UNIQUE_IDX_OID_BASE = 46000
_CHECK_CON_OID_BASE = 47000


def _unique_constraints(db: str, catalog: Catalog) -> list[dict[str, Any]]:
    """Declared UNIQUE constraints with the fields ``pg_constraint`` /
    ``information_schema`` / ``pg_get_constraintdef`` need: a stable ``oid``, the
    owner table OID, ``conkey`` attnums, columns, the rendered ``condef``, and the
    OID of its backing unique index (``conindid``) — SQLAlchemy reflects a UNIQUE
    constraint by joining ``pg_constraint.conindid = pg_index.indexrelid``."""
    tables, table_oids = _tables_with_oids(db, catalog)
    out: list[dict[str, Any]] = []
    oid = _UNIQUE_CON_OID_BASE
    idx_oid = _UNIQUE_IDX_OID_BASE
    for t in tables:
        attnum = {c.name: i for i, c in enumerate(t.columns, start=1)}
        for uq in t.unique_constraints:
            out.append(
                {
                    "oid": oid,
                    "conindid": idx_oid,
                    "conname": uq.name,
                    "table": t,
                    "conrelid": table_oids.get(t.name, 0),
                    "conkey": [attnum.get(c, 0) for c in uq.columns],
                    "columns": list(uq.columns),
                    "condef": f"UNIQUE ({', '.join(uq.columns)})",
                    "deferrable": uq.deferrable,
                    "initially_deferred": uq.initially_deferred,
                }
            )
            oid += 1
            idx_oid += 1
    return out


def _pg_indexes(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``pg_catalog.pg_indexes`` — one row per index with its rendered ``indexdef``
    (what psql's ``\\d`` and SQLAlchemy read to list a table's indexes). (#134)"""
    rows = []
    for ix in _indexes(db, storage, catalog):
        rows.append(
            {
                "schemaname": "public",
                "tablename": ix["table"],
                "indexname": ix["relname"],
                "tablespace": None,
                "indexdef": indexdef_for_oid(db, storage, catalog, ix["indexrelid"]),
            }
        )
    return rows


def _check_constraints(db: str, catalog: Catalog) -> list[dict[str, Any]]:
    """Declared CHECK constraints with the fields reflection needs. ``condef`` is
    rendered the way Postgres does — ``CHECK ((<expr>))`` — so SQLAlchemy's
    ``get_check_constraints`` regex can peel it back to the predicate text."""
    tables, table_oids = _tables_with_oids(db, catalog)
    out: list[dict[str, Any]] = []
    oid = _CHECK_CON_OID_BASE
    for t in tables:
        for ck in t.check_constraints:
            out.append(
                {
                    "oid": oid,
                    "conname": ck.name,
                    "table": t,
                    "conrelid": table_oids.get(t.name, 0),
                    "expression": ck.expression,
                    "condef": f"CHECK (({ck.expression}))",
                }
            )
            oid += 1
    return out


def constraint_def_for_oid(db: str, catalog: Catalog, oid: int) -> str | None:
    """``pg_get_constraintdef(oid)`` — the rendered constraint definition for a
    primary-key, foreign-key, UNIQUE, or CHECK constraint OID, or ``None`` when
    unknown. The PK oids mirror :func:`_pg_constraint` — ``_PK_CON_OID_BASE`` plus
    the table's position among tables-with-a-PK (both walk ``_user_tables``)."""
    for i, (_t, _conname, cols) in enumerate(_pk_constraints(db, catalog)):
        if _PK_CON_OID_BASE + i == oid:
            return f"PRIMARY KEY ({', '.join(cols)})"
    for fk in _foreign_keys(db, catalog):
        if fk["oid"] == oid:
            return fk["condef"]
    for uq in _unique_constraints(db, catalog):
        if uq["oid"] == oid:
            return uq["condef"]
    for ck in _check_constraints(db, catalog):
        if ck["oid"] == oid:
            return ck["condef"]
    return None


def _info_table_constraints(
    db: str, session: Session, storage: Any, catalog: Catalog
) -> list[dict]:
    return (
        [
            {
                "constraint_catalog": db,
                "constraint_schema": "public",
                "constraint_name": conname,
                "table_catalog": db,
                "table_schema": "public",
                "table_name": t.name,
                "constraint_type": "PRIMARY KEY",
                "is_deferrable": "NO",
                "initially_deferred": "NO",
            }
            for t, conname, _cols in _pk_constraints(db, catalog)
        ]
        + [
            {
                "constraint_catalog": db,
                "constraint_schema": "public",
                "constraint_name": fk["conname"],
                "table_catalog": db,
                "table_schema": "public",
                "table_name": fk["table"].name,
                "constraint_type": "FOREIGN KEY",
                "is_deferrable": "YES" if fk["fk"].deferrable else "NO",
                "initially_deferred": "YES" if fk["fk"].initially_deferred else "NO",
            }
            for fk in _foreign_keys(db, catalog)
        ]
        + [
            {
                "constraint_catalog": db,
                "constraint_schema": "public",
                "constraint_name": con["conname"],
                "table_catalog": db,
                "table_schema": "public",
                "table_name": con["table"].name,
                "constraint_type": ctype,
                "is_deferrable": "YES" if con.get("deferrable") else "NO",
                "initially_deferred": "YES" if con.get("initially_deferred") else "NO",
            }
            for ctype, builder in (("UNIQUE", _unique_constraints), ("CHECK", _check_constraints))
            for con in builder(db, catalog)
        ]
    )


def _info_key_column_usage(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    rows: list[dict] = []
    for t, conname, cols in _pk_constraints(db, catalog):
        for pos, col in enumerate(cols, start=1):
            rows.append(
                {
                    "constraint_catalog": db,
                    "constraint_schema": "public",
                    "constraint_name": conname,
                    "table_catalog": db,
                    "table_schema": "public",
                    "table_name": t.name,
                    "column_name": col,
                    "ordinal_position": pos,
                    "position_in_unique_constraint": None,
                }
            )
    # FK local columns: position_in_unique_constraint points at the ordinal of
    # the matching referenced (unique) column.
    for fk in _foreign_keys(db, catalog):
        for pos, col in enumerate(fk["fk"].columns, start=1):
            rows.append(
                {
                    "constraint_catalog": db,
                    "constraint_schema": "public",
                    "constraint_name": fk["conname"],
                    "table_catalog": db,
                    "table_schema": "public",
                    "table_name": fk["table"].name,
                    "column_name": col,
                    "ordinal_position": pos,
                    "position_in_unique_constraint": pos,
                }
            )
    # UNIQUE constraint columns (CHECK constraints have no key_column_usage rows).
    for uq in _unique_constraints(db, catalog):
        for pos, col in enumerate(uq["columns"], start=1):
            rows.append(
                {
                    "constraint_catalog": db,
                    "constraint_schema": "public",
                    "constraint_name": uq["conname"],
                    "table_catalog": db,
                    "table_schema": "public",
                    "table_name": uq["table"].name,
                    "column_name": col,
                    "ordinal_position": pos,
                    "position_in_unique_constraint": None,
                }
            )
    return rows


def _info_constraint_column_usage(
    db: str, session: Session, storage: Any, catalog: Catalog
) -> list[dict]:
    rows: list[dict] = []
    for t, conname, cols in _pk_constraints(db, catalog):
        for col in cols:
            rows.append(
                {
                    "table_catalog": db,
                    "table_schema": "public",
                    "table_name": t.name,
                    "column_name": col,
                    "constraint_catalog": db,
                    "constraint_schema": "public",
                    "constraint_name": conname,
                }
            )
    # For an FK, constraint_column_usage names the *referenced* table's columns.
    for fk in _foreign_keys(db, catalog):
        for col in fk["ref_cols"]:
            rows.append(
                {
                    "table_catalog": db,
                    "table_schema": "public",
                    "table_name": fk["fk"].ref_table,
                    "column_name": col,
                    "constraint_catalog": db,
                    "constraint_schema": "public",
                    "constraint_name": fk["conname"],
                }
            )
    for uq in _unique_constraints(db, catalog):
        for col in uq["columns"]:
            rows.append(
                {
                    "table_catalog": db,
                    "table_schema": "public",
                    "table_name": uq["table"].name,
                    "column_name": col,
                    "constraint_catalog": db,
                    "constraint_schema": "public",
                    "constraint_name": uq["conname"],
                }
            )
    return rows


def _info_check_constraints(
    db: str, session: Session, storage: Any, catalog: Catalog
) -> list[dict]:
    """``information_schema.check_constraints`` — one row per declared CHECK, with
    the predicate under ``check_clause``."""
    return [
        {
            "constraint_catalog": db,
            "constraint_schema": "public",
            "constraint_name": ck["conname"],
            "check_clause": f"({ck['expression']})",
        }
        for ck in _check_constraints(db, catalog)
    ]


def _info_referential_constraints(
    db: str, session: Session, storage: Any, catalog: Catalog
) -> list[dict]:
    # One row per declared foreign key. The referenced unique constraint is the
    # target table's PRIMARY KEY. Rules default to NO ACTION (Postgres' default)
    # when the FK didn't spell out ON UPDATE / ON DELETE.
    return [
        {
            "constraint_catalog": db,
            "constraint_schema": "public",
            "constraint_name": fk["conname"],
            "unique_constraint_catalog": db,
            "unique_constraint_schema": "public",
            "unique_constraint_name": fk["ref_pk_name"],
            "match_option": "NONE",
            "update_rule": fk["fk"].on_update or "NO ACTION",
            "delete_rule": fk["fk"].on_delete or "NO ACTION",
        }
        for fk in _foreign_keys(db, catalog)
    ]


def _info_sequences(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    rows = []
    for name in _sequence_names(db, catalog):
        seq = catalog.get_sequence(db, name)
        if seq is None:
            continue
        inc = int(seq.get("increment", 1))
        maxv = seq.get("max_value")
        minv = seq.get("min_value")
        rows.append(
            {
                "sequence_catalog": db,
                "sequence_schema": _table_schema_name(name),
                "sequence_name": _bare_table_name(name),
                "data_type": "bigint",
                "numeric_precision": 64,
                "numeric_scale": 0,
                "start_value": str(seq.get("start", 1)),
                "minimum_value": str(minv if minv is not None else (1 if inc > 0 else -(2**63))),
                "maximum_value": str(maxv if maxv is not None else (2**63 - 1 if inc > 0 else -1)),
                "increment": str(inc),
                "cycle_option": "YES" if seq.get("cycle") else "NO",
            }
        )
    return rows


_SCHEMA_OID_BASE = 69000


def _schema_oids(db: str, catalog: Catalog) -> dict[str, int]:
    lister = getattr(catalog, "list_schemas", None)
    names = lister(db) if lister is not None else []
    return {name: _SCHEMA_OID_BASE + i for i, name in enumerate(names)}


def _pg_namespace(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    rows = [{"oid": oid, "nspname": name, "nspowner": 10} for name, oid in _NS_OIDS.items()]
    rows.append({"oid": _PG_TEMP_NS_OID, "nspname": "pg_temp_1", "nspowner": 10})
    rows.extend(
        {"oid": oid, "nspname": name, "nspowner": 10}
        for name, oid in _schema_oids(db, catalog).items()
    )
    return rows


_PG_TEMP_NS_OID = 99  # pg_temp_1's namespace oid (real PG mints one per backend)


def _bare_table_name(name: str) -> str:
    """The relation name without its schema prefix — tables in a user schema
    are stored dotted (``testschema.users``), like user types."""
    return name.split(".", 1)[1] if "." in name else name


def _table_schema_name(name: str) -> str:
    return name.split(".", 1)[0] if "." in name else "public"


def _table_ns_oid(t: Any, schema_oids: dict[str, int]) -> int:
    if getattr(t, "temp", False):
        return _PG_TEMP_NS_OID
    if "." in t.name:
        return schema_oids.get(t.name.split(".", 1)[0], _NS_OIDS["public"])
    return _NS_OIDS["public"]


def _split_user_type_name(name: str, schema_oids: dict[str, int]) -> tuple[str, int]:
    """A stored user-type name split into (bare typname, namespace oid) — types
    in a user schema are stored dotted ("testschema.testcomp")."""
    if "." in name:
        schema, bare = name.split(".", 1)
        oid = schema_oids.get(schema)
        if oid is not None:
            return bare, oid
    return name, _NS_OIDS["public"]


def _pg_class(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    tables, oids = _tables_with_oids(db, catalog)
    matviews = _matview_names(db, catalog)
    schema_oids = _schema_oids(db, catalog)
    # Every relation is owned by the connecting user (they created it). Report
    # that role's oid as relowner — hardcoding PG's bootstrap-superuser oid 10
    # broke pgjdbc's getTablePrivileges join (``c.relowner = r.oid`` against the
    # minted role oid). relacl is the materialized ACL, or NULL when untouched.
    owner_name = getattr(session, "user", None)
    owner_oid = _role_oid_map(db, session, catalog).get(owner_name, 10) if owner_name else 10

    def _relacl(name: str) -> str | None:
        if owner_name is None:
            return None
        return catalog.relation_acl_text(db, _bare_table_name(name), owner_name)

    rows = [
        {
            "oid": oids[t.name],
            "relname": _bare_table_name(t.name),
            "relnamespace": _table_ns_oid(t, schema_oids),
            # A materialized view is a real relation with columns, tracked in the
            # catalog like a table — but it reports relkind 'm', not 'r'.
            "relkind": "m" if t.name in matviews else "r",
            # Temp tables report 't' so SQLAlchemy's get_table_names filter
            # (relpersistence != 't') hides them, exactly as real PG does.
            "relpersistence": "t" if t.temp else "p",
            "relam": _HEAP_AM_OID,
            "relowner": owner_oid,
            "relacl": _relacl(t.name),
            "reltoastrelid": 0,
            "relchecks": len(t.check_constraints),
            "relhasindex": True,
            "relhasrules": False,
            "relhastriggers": False,
            "relrowsecurity": False,
            "relforcerowsecurity": False,
            "relispartition": False,
            "reltablespace": 0,
            "relreplident": "d",
            "reloftype": 0,
            "reloptions": None,
            # -1 = "no estimate yet" (PG's initial value; we never analyze).
            "reltuples": -1.0,
        }
        for t in tables
    ]
    # Index relations are also pg_class rows (relkind 'i') — reflection joins
    # pg_index.indexrelid = pg_class.oid to read an index's name + access method.
    for ix in _index_relations(db, storage, catalog):
        rows.append(
            {
                "oid": ix["indexrelid"],
                "relname": ix["relname"],
                "relnamespace": _NS_OIDS["public"],
                "relkind": "i",
                "relpersistence": "p",
                "relam": _BTREE_AM_OID,
                "reloptions": None,
                "reltuples": -1.0,
            }
        )
    # Views are pg_class rows too (relkind 'v') — SQLAlchemy's get_view_names
    # filters pg_class on relkind IN ('v','m').
    for name, oid in _view_oids(db, catalog).items():
        rows.append(
            {
                "oid": oid,
                "relname": _bare_table_name(name),
                "relnamespace": schema_oids.get(_table_schema_name(name), _NS_OIDS["public"])
                if "." in name
                else _NS_OIDS["public"],
                "relkind": "v",
                "relpersistence": "p",
                "relam": 0,
                "relowner": owner_oid,
                "relacl": _relacl(name),
                "reloptions": None,
            }
        )
    # Sequences are pg_class rows too (relkind 'S').
    for name, oid in _sequence_oids(db, catalog).items():
        rows.append(
            {
                "oid": oid,
                "relname": _bare_table_name(name),
                "relnamespace": schema_oids.get(_table_schema_name(name), _NS_OIDS["public"])
                if "." in name
                else _NS_OIDS["public"],
                "relkind": "S",
                "relpersistence": "p",
                "relam": 0,
                "reloptions": None,
            }
        )
    # Composite types have a backing relation (relkind 'c') whose reltype points
    # back at the pg_type row; its pg_attribute rows are the type's fields.
    type_oids = _composite_oids(db, catalog)
    for name, oid in _composite_rel_oids(db, catalog).items():
        rows.append(
            {
                "oid": oid,
                "relname": name,
                "relnamespace": _NS_OIDS["public"],
                "relkind": "c",
                "relpersistence": "p",
                "relam": 0,
                "reloptions": None,
                "reltype": type_oids.get(name, 0),
            }
        )
    for row in rows:
        row.setdefault("reltype", 0)
    return rows


def viewdef_for_oid(db: str, catalog: Catalog, oid: int) -> str | None:
    """``pg_get_viewdef(oid)`` — the stored SELECT text for a view's or
    materialized view's pg_class OID."""
    for name, vourid in _view_oids(db, catalog).items():
        if vourid == oid:
            getter = getattr(catalog, "get_view", None)
            return getter(db, name) if getter is not None else None
    # Materialized views are catalog tables (their OID comes from _table_oids).
    getter = getattr(catalog, "get_matview", None)
    if getter is not None:
        for name, toid in _table_oids(db, catalog).items():
            if toid == oid and (definition := getter(db, name)) is not None:
                return definition
    return None


def _function_by_oid(db: str, catalog: Catalog, oid: int) -> dict | None:
    oids = _function_oids(db, catalog)
    for fn in _functions(db, catalog):
        if oids.get(f"{fn['name']}/{fn['nargs']}") == oid:
            return fn
    return None


def function_arguments_for_oid(db: str, catalog: Catalog, oid: int) -> str | None:
    """``pg_get_function_arguments(oid)`` — the ``(name type, …)`` list for \\df."""
    fn = _function_by_oid(db, catalog, oid)
    return _function_signature(fn) if fn is not None else None


def function_result_for_oid(db: str, catalog: Catalog, oid: int) -> str | None:
    """``pg_get_function_result(oid)`` — the return type name."""
    fn = _function_by_oid(db, catalog, oid)
    if fn is None:
        return None
    result = _type_name(fn.get("return_tag"))
    return f"SETOF {result}" if fn.get("is_table") else result


def functiondef_for_oid(db: str, catalog: Catalog, oid: int) -> str | None:
    """``pg_get_functiondef(oid)`` — a CREATE FUNCTION reconstruction."""
    fn = _function_by_oid(db, catalog, oid)
    if fn is None:
        return None
    args = _function_signature(fn)
    result = function_result_for_oid(db, catalog, oid)
    lang = str(fn.get("language", "sql")).upper()
    body = fn.get("body") or ""
    return (
        f"CREATE OR REPLACE FUNCTION public.{fn['name']}({args})\n"
        f" RETURNS {result}\n"
        f" LANGUAGE {lang.lower()}\n"
        f"AS $function$\n{body}\n$function$\n"
    )


def _pg_attribute(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """One row per column of every declared table — the pg_catalog column surface
    tools (and ``\\d``-style queries) read. attrelid lines up with pg_class.oid."""
    tables, oids = _tables_with_oids(db, catalog)
    enum_oids = _enum_oids(db, catalog)
    domain_oids = _domain_oids(db, catalog)
    rows: list[dict] = []
    for t in tables:
        for i, col in enumerate(t.columns, start=1):
            if col.domain_type is not None:
                typoid = domain_oids.get(col.domain_type, 25)
            elif col.enum_type is not None:
                typoid = enum_oids.get(col.enum_type, 25)
            elif getattr(col, "composite_type", None) is not None:
                # A composite (or composite-array) column reports its type's
                # minted oid, not generic RECORD/2249 — so getColumns' typname
                # join resolves ``custom`` / ``_custom`` (pgjdbc's
                # customArrayTypeInfo; psycopg composite reflection).
                ct = col.composite_type
                minted = _composite_oids(db, catalog).get(ct) or _table_rowtype_oids(
                    db, catalog
                ).get(ct)
                if minted is not None:
                    typoid = (
                        minted + USER_TYPE_ARRAY_OID_OFFSET
                        if typemap.is_array_tag(col.type_tag)
                        else minted
                    )
                else:
                    typoid = col.decl_oid or typemap.PG_OID.get(col.type_tag, 25)
            else:
                typoid = col.decl_oid or typemap.PG_OID.get(col.type_tag, 25)
            rows.append(
                {
                    "attrelid": oids[t.name],
                    "attname": col.name,
                    "atttypid": typoid,
                    "atttypmod": col.typmod,
                    "attnum": i,
                    "attnotnull": not col.nullable,
                    "atthasdef": col.has_default or col.sequence is not None,
                    "attisdropped": False,
                    "attidentity": {"always": "a", "by_default": "d"}.get(col.identity or "", ""),
                    "attgenerated": "s" if col.generated is not None else "",
                    "attcollation": 0,
                    "attlen": -1,
                }
            )
    # Plain views expose their output columns as pg_attribute rows keyed on the
    # view's pg_class oid (real PG does the same) — this is what SQLAlchemy's
    # get_columns reads for a view. The column shape comes from describing the
    # view's stored SELECT (planning only, never executed); a view whose
    # definition can't be described (e.g. its base table was dropped) simply
    # contributes no rows. Lazy import: engine imports virtual at module level.
    getter = getattr(catalog, "get_view", None)
    if getter is not None:
        import sqlglot as _sqlglot

        from secantus.sql import engine as _engine

        for vname, void in _view_oids(db, catalog).items():
            definition = getter(db, vname)
            if definition is None:
                continue
            try:
                inner = _sqlglot.parse_one(definition, read="postgres")
                descs = _engine.describe_statement(storage, db, inner, session, catalog)
            except Exception:
                continue
            for i, desc in enumerate(descs or [], start=1):
                rows.append(
                    {
                        "attrelid": void,
                        "attname": desc.name,
                        "atttypid": desc.pg_oid,
                        "atttypmod": desc.typmod,
                        "attnum": i,
                        "attnotnull": False,
                        "atthasdef": False,
                        "attisdropped": False,
                        "attidentity": "",
                        "attgenerated": "",
                        "attcollation": 0,
                        "attlen": -1,
                    }
                )
    # Composite-type fields are pg_attribute rows keyed on the type's relkind='c'
    # relation oid, so pg_type.typrelid -> pg_class.oid -> pg_attribute resolves.
    rel_oids = _composite_rel_oids(db, catalog)
    comp_oids = _composite_oids(db, catalog)
    getter = getattr(catalog, "get_composite", None)
    for name, rel_oid in rel_oids.items():
        fields = getter(db, name) if getter is not None else None
        for i, entry in enumerate(fields or [], start=1):
            fname, tag = entry[0], entry[1]
            sub = entry[2] if len(entry) > 2 else None
            # A field whose type is another composite reflects that type's oid.
            atttypid = (
                comp_oids.get(tag, typemap.PG_OID.get(tag, 25))
                if sub
                else (typemap.PG_OID.get(tag, 25))
            )
            rows.append(
                {
                    "attrelid": rel_oid,
                    "attname": fname,
                    "atttypid": atttypid,
                    "atttypmod": -1,
                    "attnum": i,
                    "attnotnull": False,
                    "atthasdef": False,
                    "attisdropped": False,
                    "attidentity": "",
                    "attgenerated": "",
                    "attcollation": 0,
                    "attlen": -1,
                }
            )
    return rows


def _pg_attrdef(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    # One row per column that carries a DEFAULT. ``adbin`` holds the default's SQL
    # text (Postgres stores a nodeToString there and tools call pg_get_expr; we
    # store the rendered text directly, which is what our pg_get_expr passes
    # through). ``adnum`` is the column's attnum; ``oid`` is synthesised per row.
    tables, oids = _tables_with_oids(db, catalog)
    rows: list[dict] = []
    for t in tables:
        relid = oids[t.name]
        for attnum, col in enumerate(t.columns, start=1):
            adbin = _column_default_text(col)
            if adbin is None:
                continue
            rows.append(
                {
                    "oid": relid * 1000 + attnum,
                    "adrelid": relid,
                    "adnum": attnum,
                    "adbin": adbin,
                }
            )
    return rows


_PG_CLASS_OID = 1259  # the OID of the pg_class catalog itself (classoid for relations)
_PG_CONSTRAINT_CLASSOID = 2606  # pg_constraint catalog OID (classoid for constraint comments)
_PG_PROC_CLASSOID = 1255  # pg_proc catalog OID (classoid for function comments)
_PG_TYPE_CLASSOID = 1247  # pg_type catalog OID (classoid for type/domain comments)


def _pg_description(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    # Object comments from COMMENT ON TABLE / COLUMN. A table comment has
    # objsubid 0; a column comment's objsubid is the column's attnum. classoid is
    # pg_class so SQLAlchemy's get_columns / get_table_comment joins line up.
    tables, oids = _tables_with_oids(db, catalog)
    rows: list[dict] = []
    for t in tables:
        relid = oids[t.name]
        if t.comment is not None:
            rows.append(
                {
                    "objoid": relid,
                    "classoid": _PG_CLASS_OID,
                    "objsubid": 0,
                    "description": t.comment,
                }
            )
        for attnum, col in enumerate(t.columns, start=1):
            if col.comment is not None:
                rows.append(
                    {
                        "objoid": relid,
                        "classoid": _PG_CLASS_OID,
                        "objsubid": attnum,
                        "description": col.comment,
                    }
                )
    # COMMENT ON CONSTRAINT rows, keyed by the pg_constraint oid so
    # SQLAlchemy's constraint-comment outer join (objoid = pg_constraint.oid)
    # resolves. The comments live on the catalog's constraint records.
    relid_to_table = {oids[t.name]: t for t in tables}
    for con in _pg_constraint(db, session, storage, catalog):
        t = relid_to_table.get(con["conrelid"])
        if t is None:
            continue
        comment = None
        if con["contype"] == "p" and con["conname"] == t.pk_constraint_name():
            comment = t.pk_comment
        else:
            for group in (t.check_constraints, t.unique_constraints, t.foreign_keys):
                for c in group:
                    if c.name == con["conname"]:
                        comment = c.comment
                        break
        if comment is not None:
            rows.append(
                {
                    "objoid": con["oid"],
                    "classoid": _PG_CONSTRAINT_CLASSOID,
                    "objsubid": 0,
                    "description": comment,
                }
            )
    # COMMENT ON FUNCTION rows (classoid pg_proc), keyed by the minted pg_proc
    # oid so ``objoid = 'name'::regproc`` predicates resolve.
    fn_oids = _function_oids(db, catalog)
    for f in _functions(db, catalog):
        comment = f.get("comment")
        if comment is not None:
            rows.append(
                {
                    "objoid": fn_oids[f"{f['name']}/{f['nargs']}"],
                    "classoid": _PG_PROC_CLASSOID,
                    "objsubid": 0,
                    "description": comment,
                }
            )
    # COMMENT ON DOMAIN rows (classoid pg_type) — obj_description(oid,
    # 'pg_type') is how pgjdbc's getUDTs reads a domain's REMARKS.
    lister = getattr(catalog, "get_domain", None)
    if lister is not None:
        for dom_name, dom_oid in catalog.domain_type_oids(db).items():
            dom = catalog.get_domain(db, dom_name)
            comment = dom.get("comment") if dom else None
            if comment is not None:
                rows.append(
                    {
                        "objoid": dom_oid,
                        "classoid": _PG_TYPE_CLASSOID,
                        "objsubid": 0,
                        "description": comment,
                    }
                )
    # COMMENT ON INDEX rows — stored by name, resolved to the index relation's
    # oid here (minted oids can reshuffle as indexes come and go).
    index_comments = getattr(catalog, "index_comments", lambda _db: {})(db)
    if index_comments:
        for ix in _index_relations(db, storage, catalog):
            comment = index_comments.get(ix["relname"])
            if comment is not None:
                rows.append(
                    {
                        "objoid": ix["indexrelid"],
                        "classoid": _PG_CLASS_OID,
                        "objsubid": 0,
                        "description": comment,
                    }
                )
    # Direct DML against pg_description (DatabaseMetaDataTest's setup moves a
    # function comment onto a table's oid to manufacture a duplicate row) is
    # persisted as a delta over the derived rows: suppressed original keys
    # plus replacement/inserted rows.
    from secantus.sql.catalog import DESCRIPTION_DELTA_COLLECTION

    delta = storage.find_matching(db, DESCRIPTION_DELTA_COLLECTION, {})
    if delta:
        suppressed = {d["key"] for d in delta if d.get("kind") == "suppress"}
        rows = [
            r for r in rows if f"{r['objoid']}/{r['classoid']}/{r['objsubid']}" not in suppressed
        ]
        for d in delta:
            if d.get("kind") == "extra":
                rows.append(
                    {
                        "objoid": d["objoid"],
                        "classoid": d["classoid"],
                        "objsubid": d["objsubid"],
                        "description": d["description"],
                    }
                )
    return rows


def _pg_sequence(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    oids = _sequence_oids(db, catalog)
    rows = []
    for name in _sequence_names(db, catalog):
        seq = catalog.get_sequence(db, name)
        if seq is None:
            continue
        inc = int(seq.get("increment", 1))
        maxv = seq.get("max_value")
        minv = seq.get("min_value")
        rows.append(
            {
                "seqrelid": oids[name],
                "seqstart": int(seq.get("start", 1)),
                "seqincrement": inc,
                "seqmax": int(maxv) if maxv is not None else (2**63 - 1 if inc > 0 else -1),
                "seqmin": int(minv) if minv is not None else (1 if inc > 0 else -(2**63)),
                "seqcache": 1,
                "seqcycle": bool(seq.get("cycle", False)),
            }
        )
    return rows


def _role_oid_map(db: str, session: Session, catalog: Catalog) -> dict[str, int]:
    """Canonical ``role name -> oid`` used by both ``pg_roles`` and
    ``pg_auth_members`` so their oids join consistently. Declared roles (sorted)
    get ``_ROLE_OID_BASE + i``; the connecting user gets ``_ROLE_OID_BASE - 1``."""
    lister = getattr(catalog, "list_roles", None)
    names = lister(db) if lister is not None else []
    out = {name: _ROLE_OID_BASE + i for i, name in enumerate(names)}
    if session.user and session.user not in out:
        out[session.user] = _ROLE_OID_BASE - 1
    return out


def _pg_roles(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``pg_catalog.pg_roles`` — SQL-declared roles plus the connection's own user
    (a superuser login, like Postgres' bootstrap role) when it isn't one already."""
    oids = _role_oid_map(db, session, catalog)
    rows = []
    for name, oid in oids.items():
        if oid == _ROLE_OID_BASE - 1:
            # The connecting user — a superuser login not declared via CREATE ROLE.
            rows.append(
                _role_row(
                    oid,
                    name,
                    {"login": True, "superuser": True, "createdb": True, "createrole": True},
                )
            )
        else:
            rows.append(_role_row(oid, name, catalog.get_role(db, name) or {}))
    return rows


def _pg_auth_members(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``pg_catalog.pg_auth_members`` — one row per role membership (#138):
    ``roleid`` (the group role), ``member``, ``grantor``, ``admin_option``. Oids
    come from the shared :func:`_role_oid_map` so they join to ``pg_roles.oid``."""
    lister = getattr(catalog, "list_role_memberships", None)
    if lister is None:
        return []
    oids = _role_oid_map(db, session, catalog)
    grantor = oids.get(session.user, _ROLE_OID_BASE - 1)
    rows = []
    for m in lister(db):
        rows.append(
            {
                "roleid": oids.get(m["role"], 0),
                "member": oids.get(m["member"], 0),
                "grantor": grantor,
                "admin_option": bool(m.get("admin_option", False)),
            }
        )
    return rows


def _functions(db: str, catalog: Catalog) -> list[dict]:
    lister = getattr(catalog, "list_functions", None)
    return sorted(lister(db), key=lambda f: (f["name"], f["nargs"])) if lister is not None else []


def _function_oids(db: str, catalog: Catalog) -> dict[str, int]:
    """Stable pg_proc oid per function key (``name/nargs``)."""
    return {
        f"{f['name']}/{f['nargs']}": _FUNCTION_OID_BASE + i
        for i, f in enumerate(_functions(db, catalog))
    }


def _type_oid(tag: str | None) -> int:
    if tag is None:
        return 2278  # void
    return typemap.PG_OID.get(tag, typemap.PG_OID.get("text", 25))


def _type_name(tag: str | None) -> str:
    if tag is None:
        return "void"
    return typemap.SQL_TYPE_NAME.get(tag, tag)


def _function_signature(fn: dict) -> str:
    """The ``(argname argtype, …)`` argument list for pg_get_function_arguments /
    a CREATE FUNCTION reconstruction."""
    names = fn.get("params") or []
    types = fn.get("param_types") or []
    parts = []
    for i in range(fn.get("nargs", 0)):
        nm = names[i] if i < len(names) else None
        tt = types[i] if i < len(types) else None
        typ = _type_name(tt)
        parts.append(f"{nm} {typ}" if nm else typ)
    return ", ".join(parts)


#: Built-in large-object functions, reflected so pgjdbc's LargeObjectManager
#: can resolve their OIDs (it queries pg_proc joined to the pg_catalog
#: namespace by name, then calls via the Fastpath sub-protocol — see
#: ``secantus.sql.largeobjects``). ``(name, oid, rettype_oid, argtype_oids)``.
_LO_PROCS = [
    ("lo_open", 952, 23, "26 23"),
    ("lo_close", 953, 23, "23"),
    ("loread", 954, 17, "23 23"),
    ("lowrite", 955, 23, "23 17"),
    ("lo_lseek", 956, 23, "23 23 23"),
    ("lo_creat", 957, 26, "23"),
    ("lo_create", 715, 26, "26"),
    ("lo_tell", 958, 23, "23"),
    ("lo_unlink", 964, 23, "26"),
    ("lo_truncate", 1004, 23, "23 23"),
    ("lo_lseek64", 3170, 20, "23 20 23"),
    ("lo_tell64", 3171, 20, "23"),
    ("lo_truncate64", 3172, 23, "23 20"),
]


def _pg_proc(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``pg_catalog.pg_proc`` — one row per user-defined ``CREATE FUNCTION``
    (#130), plus the built-in large-object functions in ``pg_catalog``."""
    oids = _function_oids(db, catalog)
    rows = [
        {
            "oid": oid,
            "proname": name,
            "pronamespace": _NS_OIDS["pg_catalog"],
            "proowner": 10,
            "prolang": _SQL_LANG_OID,
            "prorettype": rettype,
            "pronargs": len(argtypes.split()),
            "pronargdefaults": 0,
            "proargtypes": argtypes,
            "proargnames": None,
            "proargmodes": None,
            "proallargtypes": None,
            "prosrc": name,
            "prokind": "f",
            "proretset": False,
            "provariadic": 0,
        }
        for name, oid, rettype, argtypes in _LO_PROCS
    ]
    for fn in _functions(db, catalog):
        key = f"{fn['name']}/{fn['nargs']}"
        argtypes = " ".join(str(_type_oid(t)) for t in (fn.get("param_types") or []))
        names = [n for n in (fn.get("params") or []) if n is not None]
        rows.append(
            {
                "oid": oids[key],
                "proname": fn["name"],
                "pronamespace": _NS_OIDS["public"],
                "proowner": 10,
                "prolang": _SQL_LANG_OID,
                "prorettype": _type_oid(fn.get("return_tag")),
                "pronargs": fn.get("nargs", 0),
                "pronargdefaults": 0,
                "proargtypes": argtypes,
                "proargnames": names or None,
                "proargmodes": None,
                "proallargtypes": None,
                "prosrc": fn.get("body"),
                "prokind": "f",
                "proretset": bool(fn.get("is_table")),
                "provariadic": 0,
            }
        )
    return rows


def _specific_name(fn: dict, oids: dict[str, int]) -> str:
    """The information_schema ``specific_name`` for a function (name + its oid)."""
    key = "{}/{}".format(fn["name"], fn["nargs"])
    return "{}_{}".format(fn["name"], oids[key])


def _info_routines(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``information_schema.routines`` — one row per user function."""
    oids = _function_oids(db, catalog)
    rows = []
    for fn in _functions(db, catalog):
        rows.append(
            {
                "specific_catalog": db,
                "specific_schema": "public",
                "specific_name": _specific_name(fn, oids),
                "routine_catalog": db,
                "routine_schema": "public",
                "routine_name": fn["name"],
                "routine_type": "FUNCTION",
                "data_type": _type_name(fn.get("return_tag")),
                "routine_body": "EXTERNAL",
                "routine_definition": fn.get("body"),
                "external_language": str(fn.get("language", "sql")).upper(),
                "is_deterministic": "NO",
            }
        )
    return rows


def _info_parameters(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``information_schema.parameters`` — one row per function parameter."""
    oids = _function_oids(db, catalog)
    rows = []
    for fn in _functions(db, catalog):
        specific = _specific_name(fn, oids)
        names = fn.get("params") or []
        types = fn.get("param_types") or []
        for i in range(fn.get("nargs", 0)):
            rows.append(
                {
                    "specific_catalog": db,
                    "specific_schema": "public",
                    "specific_name": specific,
                    "ordinal_position": i + 1,
                    "parameter_mode": "IN",
                    "parameter_name": names[i] if i < len(names) else None,
                    "data_type": _type_name(types[i] if i < len(types) else None),
                }
            )
    return rows


def _pg_policies(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``pg_catalog.pg_policies`` — one row per RLS policy (#129)."""
    lister = getattr(catalog, "list_policies", None)
    if lister is None:
        return []
    rows = []
    for p in lister(db):
        roles = p.get("roles") or ["public"]
        rows.append(
            {
                "schemaname": "public",
                "tablename": p["table"],
                "policyname": p["name"],
                "permissive": "PERMISSIVE" if p.get("permissive", True) else "RESTRICTIVE",
                "roles": "{" + ",".join(str(r) for r in roles) + "}",
                "cmd": str(p.get("command") or "ALL").upper(),
                "qual": p.get("using"),
                "with_check": p.get("check"),
            }
        )
    return rows


def _pg_locks(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``pg_catalog.pg_locks`` — the advisory locks (#135) the connection holds.
    Single-node dev surface: reflects *this* session's held advisory locks (one
    row per key+mode, always ``granted``). Non-advisory lock types and other
    backends aren't tracked."""
    held = getattr(session, "held_advisory_locks", None)
    if held is None:
        return []
    pid = getattr(session, "backend_pid", 0)
    rows = []
    for classid, objid, objsubid, mode in held():
        rows.append(
            {
                "locktype": "advisory",
                "database": None,
                "relation": None,
                "page": None,
                "tuple": None,
                "virtualxid": None,
                "transactionid": None,
                "classid": classid,
                "objid": objid,
                "objsubid": objsubid,
                "virtualtransaction": f"{pid}/0",
                "pid": pid,
                "mode": mode,
                "granted": True,
                "fastpath": False,
            }
        )
    return rows


def _guc_vartype(value: str) -> str:
    """The ``pg_settings.vartype`` for a GUC value, inferred from its text."""
    if value.lower() in ("on", "off", "true", "false", "yes", "no"):
        return "bool"
    try:
        int(value)
        return "integer"
    except ValueError:
        pass
    try:
        float(value)
        return "real"
    except ValueError:
        return "string"


def _pg_settings(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``pg_catalog.pg_settings`` — one row per GUC (defaults overlaid with the
    session's ``SET`` overrides), the subset psql's ``\\dconfig`` and ORMs read.
    (#136)"""
    from secantus.sql.session import GUC_DEFAULTS

    overrides = getattr(session, "settings", {}) or {}
    merged = session.all_settings()
    rows = []
    for name in sorted(merged):
        setting = merged[name]
        boot = GUC_DEFAULTS.get(name, setting)
        rows.append(
            {
                "name": name,
                "setting": setting,
                "unit": None,
                "category": "Client Connection Defaults",
                "short_desc": "",
                "context": "user",
                "vartype": _guc_vartype(setting),
                "source": "session" if name in overrides else "default",
                "min_val": None,
                "max_val": None,
                "enumvals": None,
                "boot_val": boot,
                "reset_val": boot,
                "pending_restart": False,
            }
        )
    return rows


def _live_sessions(session: Session) -> list:
    """The live connection sessions to reflect — the server's ``ActivityRegistry``
    snapshot when connected over the wire, else just this (embedded) session."""
    reg = getattr(session, "activity_registry", None)
    return reg.snapshot() if reg is not None else [session]


def _pg_stat_activity(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``pg_catalog.pg_stat_activity`` — one row per live backend (#137). Reflects
    the wire server's live-session registry; a client running this query sees its
    own row as ``state = 'active'`` with this query text."""
    rows = []
    for s in _live_sessions(session):
        app = s.get_setting("application_name") if hasattr(s, "get_setting") else ""
        rows.append(
            {
                "datid": None,
                "datname": s.database,
                "pid": s.backend_pid,
                "usesysid": None,
                "usename": s.user,  # session user (the authenticated login)
                "application_name": app,
                "client_addr": getattr(s, "client_addr", None),
                "client_port": None,
                "backend_start": getattr(s, "backend_start", None),
                "xact_start": None,
                "query_start": getattr(s, "query_start", None),
                "state_change": None,
                "wait_event_type": None,
                "wait_event": None,
                "state": getattr(s, "state", "idle"),
                "query": getattr(s, "current_query", "") or "",
                "backend_type": "client backend",
            }
        )
    return rows


def _pg_stat_database(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``pg_catalog.pg_stat_database`` — one row per database with a live backend
    count (#137). Cumulative counters are single-node dev stubs (zero)."""
    counts: dict[str, int] = {}
    for s in _live_sessions(session):
        counts[s.database] = counts.get(s.database, 0) + 1
    rows = []
    for name, n in sorted(counts.items()):
        rows.append(
            {
                "datid": None,
                "datname": name,
                "numbackends": n,
                "xact_commit": 0,
                "xact_rollback": 0,
                "blks_read": 0,
                "blks_hit": 0,
                "tup_returned": 0,
                "tup_fetched": 0,
                "tup_inserted": 0,
                "tup_updated": 0,
                "tup_deleted": 0,
            }
        )
    return rows


def _pg_cursors(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``pg_catalog.pg_cursors`` — this connection's open DECLAREd cursors."""
    return [
        {
            "name": c.name,
            "statement": getattr(c, "statement", "") or "",
            "is_holdable": bool(getattr(c, "hold", False)),
            "is_binary": False,
            "is_scrollable": True,
            "creation_time": getattr(c, "created", None),
        }
        for c in session.cursors.values()
    ]


def _pg_prepared_statements(
    db: str, session: Session, storage: Any, catalog: Catalog
) -> list[dict]:
    """``pg_catalog.pg_prepared_statements`` — SQL-level ``PREPARE``d statements
    plus this connection's wire-level (extended Parse) ones."""

    def _regtype_names(oids: Any) -> list[str]:
        # An unresolved (0) parameter reports ``text`` — PG's parse analysis
        # defaults unknowns to text by Describe time.
        return [typemap.regtype_from_oid(o or 25) or "text" for o in (oids or [])]

    rows = []
    for name, entry in (getattr(session, "prepared", None) or {}).items():
        stmt = entry[0] if isinstance(entry, tuple) else entry
        text = stmt.sql(dialect="postgres") if hasattr(stmt, "sql") else str(stmt)
        rows.append(
            {
                "name": name,
                "statement": f"PREPARE {name} AS {text}",
                "prepare_time": getattr(session, "backend_start", None),
                "parameter_types": [],
                "from_sql": True,
            }
        )
    for name, prep in (getattr(session, "wire_prepared", None) or {}).items():
        if not name:
            continue  # the unnamed statement isn't listed
        stmt = getattr(prep, "stmt", None)
        # The ORIGINAL query text, exactly as parsed (psycopg matches its own
        # cache keys against it; a re-render changes keyword case).
        text = getattr(prep, "query", "") or (
            stmt.sql(dialect="postgres") if stmt is not None else ""
        )
        rows.append(
            {
                "name": name,
                "statement": text,
                "prepare_time": getattr(prep, "created", None)
                or getattr(session, "backend_start", None),
                "parameter_types": _regtype_names(getattr(prep, "param_oids", ())),
                "from_sql": False,
            }
        )
    return rows


def _pg_prepared_xacts(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``pg_catalog.pg_prepared_xacts`` — one row per prepared two-phase
    transaction (#139), from the server-wide ``PreparedXactRegistry``. ``transaction``
    is a synthetic xid (Postgres shows the real prepared xid; single-node we hand out
    a stable small integer per gid ordinal)."""
    reg = getattr(session, "prepared_xacts", None)
    if reg is None:
        return []
    rows = []
    for i, x in enumerate(sorted(reg.snapshot(), key=lambda x: x.gid), start=1):
        rows.append(
            {
                "transaction": i,
                "gid": x.gid,
                "prepared": x.prepared_at,
                "owner": x.owner,
                "database": x.database,
            }
        )
    return rows


def _role_row(oid: int, name: str, role: dict) -> dict:
    return {
        "oid": oid,
        "rolname": name,
        "rolsuper": bool(role.get("superuser", False)),
        "rolinherit": bool(role.get("inherit", True)),
        "rolcreaterole": bool(role.get("createrole", False)),
        "rolcreatedb": bool(role.get("createdb", False)),
        "rolcanlogin": bool(role.get("login", False)),
        "rolreplication": bool(role.get("replication", False)),
        "rolconnlimit": int(role.get("connlimit", -1)),
        "rolbypassrls": False,
    }


def _pg_collation(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    # No non-default collations — present-but-empty.
    return []


_ENUM_OID_BASE = ENUM_TYPE_OID_BASE


def _enum_oids(db: str, catalog: Catalog) -> dict[str, int]:
    # The mint lives on Catalog because RowDescription must report the same
    # oids this module reflects through pg_type / pg_enum / pg_attribute.
    return catalog.enum_type_oids(db)


_DOMAIN_OID_BASE = DOMAIN_TYPE_OID_BASE


def _domain_oids(db: str, catalog: Catalog) -> dict[str, int]:
    # Allocation-stable mint on Catalog (see _enum_oids).
    return catalog.domain_type_oids(db)


_COMPOSITE_OID_BASE = COMPOSITE_TYPE_OID_BASE


def _composite_oids(db: str, catalog: Catalog) -> dict[str, int]:
    # Allocation-stable mint on Catalog (see _enum_oids).
    return catalog.composite_type_oids(db)


_COMPOSITE_REL_OID_BASE = 68000


def _composite_rel_oids(db: str, catalog: Catalog) -> dict[str, int]:
    """pg_class relation OIDs for composite types (relkind 'c'); the type's
    ``pg_type.typrelid`` points here and its fields are pg_attribute rows."""
    lister = getattr(catalog, "list_composites", None)
    names = lister(db) if lister is not None else []
    return {name: _COMPOSITE_REL_OID_BASE + i for i, name in enumerate(names)}


def _pg_type(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    rows = [
        {
            "oid": typemap.PG_OID[tag],
            "typname": typname,
            "typcollation": 0,
            "typnamespace": _NS_OIDS["pg_catalog"],
            "typbasetype": 0,  # not a domain
            "typtypmod": -1,
            "typnotnull": False,
            "typdefault": None,
            # Range types report typtype 'r'; everything else is a base type 'b'.
            "typtype": "r" if tag in typemap._RANGE_TAGS else "b",
            # The paired ``_<type>`` array type's oid — 0 when we don't model
            # one (drivers treat 0 as "no array type"). psycopg's
            # TypeInfo.fetch reads it as array_oid.
            "typarray": typemap._ARRAY_PG_OID.get(tag, 0),
            "typdelim": ",",
        }
        for tag, typname in typemap.PG_TYPENAME.items()
    ]
    # Every table has a composite row type (typtype 'c') like real Postgres —
    # psycopg's ``TypeInfo.fetch(conn, "<table>")`` resolves it (and its
    # ``typarray``) to register the table-row array loader.
    table_oids = _table_oids(db, catalog)
    schema_oids = _schema_oids(db, catalog)
    for tname, rowtype_oid in _table_rowtype_oids(db, catalog).items():
        # A schema-qualified table's row type: bare typname + its schema's
        # namespace oid — pgjdbc's TypeInfoCache resolves ``typname = 'x'``
        # against the search_path's nspnames (SearchPathLookupTest).
        rows.append(
            {
                "oid": rowtype_oid,
                "typname": _bare_table_name(tname),
                "typcollation": 0,
                "typnamespace": schema_oids.get(_table_schema_name(tname), _NS_OIDS["public"])
                if "." in tname
                else _NS_OIDS["public"],
                "typbasetype": 0,
                "typtypmod": -1,
                "typnotnull": False,
                "typdefault": None,
                "typtype": "c",
                "typrelid": table_oids.get(tname, 0),
                "typarray": rowtype_oid + _ROWTYPE_ARRAY_OID_OFFSET,
                "typdelim": ",",
            }
        )
    # User-declared enum types (typtype 'e') live in their schema's namespace
    # (public unless created schema-qualified).
    schema_oids = _schema_oids(db, catalog)
    for name, oid in _enum_oids(db, catalog).items():
        typname, nsoid = _split_user_type_name(name, schema_oids)
        rows.append(
            {
                "oid": oid,
                "typname": typname,
                "typcollation": 0,
                "typnamespace": nsoid,
                "typbasetype": 0,
                "typtypmod": -1,
                "typnotnull": False,
                "typdefault": None,
                "typtype": "e",
                # A real server pairs every user type with a ``_name`` array
                # type; reporting 0 here let psycopg's TypeInfo registration
                # paths touch oid 0 = INVALID_OID (its own suite pops the
                # global unknown-oid fallback loader through array_oid).
                "typarray": oid + USER_TYPE_ARRAY_OID_OFFSET,
            }
        )
    # User-declared range types (typtype 'r') and their auto-created companion
    # multirange types (typtype 'm').
    range_lister = getattr(catalog, "list_range_types", None)
    for doc in range_lister(db) if range_lister is not None else []:
        for key, oid_key, typtype in (("range", "oid", "r"), ("multirange", "multirange_oid", "m")):
            tname = doc.get(key)
            toid = doc.get(oid_key)
            if not tname or not toid:
                continue
            typname, nsoid = _split_user_type_name(tname, schema_oids)
            rows.append(
                {
                    "oid": toid,
                    "typname": typname,
                    "typcollation": 0,
                    "typnamespace": nsoid,
                    "typbasetype": 0,
                    "typtypmod": -1,
                    "typnotnull": False,
                    "typdefault": None,
                    "typtype": typtype,
                    "typarray": toid + USER_TYPE_ARRAY_OID_OFFSET,
                }
            )
    # User-declared domain types (typtype 'd') carry their base type's oid in
    # typbasetype and the domain's NOT NULL in typnotnull.
    getter = getattr(catalog, "get_domain", None)
    for name, oid in _domain_oids(db, catalog).items():
        domain = getter(db, name) if getter is not None else None
        base_tag = domain.get("base_tag") if domain else None
        default = domain.get("default") if domain else None
        typname, nsoid = _split_user_type_name(name, schema_oids)
        base_oid = (domain.get("base_oid") if domain else None) or typemap.PG_OID.get(
            base_tag or "", 25
        )
        rows.append(
            {
                "oid": oid,
                "typname": typname,
                "typcollation": 0,
                "typnamespace": nsoid,
                "typbasetype": base_oid,
                # The base type's declared typmod (``varbit(3)`` → 3), which
                # getColumns reads as a domain column's COLUMN_SIZE.
                "typtypmod": int(domain.get("typmod", -1)) if domain else -1,
                "typnotnull": bool(domain.get("not_null")) if domain else False,
                "typdefault": None if default is None else str(default),
                "typtype": "d",
                "typarray": oid + USER_TYPE_ARRAY_OID_OFFSET,
            }
        )
    # User-declared composite types (typtype 'c') live in the public namespace;
    # typrelid points at the relkind='c' pg_class row whose pg_attribute rows are
    # the type's fields.
    rel_oids = _composite_rel_oids(db, catalog)
    for name, oid in _composite_oids(db, catalog).items():
        typname, nsoid = _split_user_type_name(name, schema_oids)
        rows.append(
            {
                "oid": oid,
                "typname": typname,
                "typcollation": 0,
                "typnamespace": nsoid,
                "typbasetype": 0,
                "typtypmod": -1,
                "typnotnull": False,
                "typdefault": None,
                "typtype": "c",
                "typrelid": rel_oids.get(name, 0),
                "typarray": oid + USER_TYPE_ARRAY_OID_OFFSET,
            }
        )
    # Non-composite types have no backing relation; built-in types without a
    # modelled ``_type`` pair report typarray 0 ("no array type").
    for row in rows:
        row.setdefault("typrelid", 0)
        row.setdefault("typarray", 0)
        row.setdefault("typdelim", ",")
        # Scalar / composite / enum rows are not arrays: no element type.
        row.setdefault("typelem", 0)
        # typinput is the type's input function. Drivers do not call it; they
        # compare it to array_in to decide whether a type is an array —
        # pgjdbc's TypeInfoCache asks for ``typinput = 'pg_catalog.array_in'
        # ::regproc as is_array``. Array types therefore must report exactly
        # ``array_in``; for the rest the conventional ``<typname>in`` name is
        # both harmless and what real Postgres shows for the common types.
        row.setdefault(
            "typinput",
            "array_in" if str(row.get("typname", "")).startswith("_") else f"{row['typname']}in",
        )
    # Every type that advertises a ``typarray`` gets the paired array-type ROW
    # — real pg_type has one per scalar (``_int4`` etc.), and a driver
    # resolving an array type by the oid it read from ``typarray`` (pgjdbc's
    # TypeInfoCache, psycopg's TypeInfo.fetch) found nothing here before.
    # ``typelem`` points back at the element; arrays of arrays don't exist in
    # PG, so the array row's own typarray is 0.
    #
    # Array type names are ``_<element>`` — but when that collides with an
    # EXISTING type name (a user composite literally named ``_custom`` shadows
    # the array type of ``custom``), real PG prepends more underscores until
    # unique (``__custom``). pgjdbc's customArrayTypeInfo reads exactly this.
    # Assign array names in oid order — PG names an array type when its element
    # type is created, so an earlier-created (lower-oid) element claims the
    # shorter name (``custom`` created before ``_custom`` → ``custom``'s array
    # is ``__custom``, ``_custom``'s array is ``___custom``).
    taken = {r["typname"] for r in rows}
    array_name_by_oid: dict[int, str] = {}
    for row in sorted(rows, key=lambda r: r["oid"]):
        name = f"_{row['typname']}"
        while name in taken:
            name = f"_{name}"
        taken.add(name)
        array_name_by_oid[row["typarray"]] = name

    array_rows = [
        {
            "oid": row["typarray"],
            "typname": array_name_by_oid[row["typarray"]],
            "typcollation": 0,
            "typnamespace": row.get("typnamespace", _NS_OIDS["pg_catalog"]),
            "typbasetype": 0,
            "typtypmod": -1,
            "typnotnull": False,
            "typdefault": None,
            "typtype": "b",
            "typrelid": 0,
            "typarray": 0,
            "typelem": row["oid"],
            "typdelim": ",",
            "typinput": "array_in",
        }
        for row in rows
        if row.get("typarray")
    ]
    rows.extend(array_rows)
    return rows


def _range_type_oids(db: str, catalog: Catalog) -> dict[str, int]:
    """Range AND companion multirange names -> minted oids."""
    lister = getattr(catalog, "list_range_types", None)
    out: dict[str, int] = {}
    for doc in lister(db) if lister is not None else []:
        out[doc["range"]] = doc["oid"]
        if doc.get("multirange") and doc.get("multirange_oid"):
            out[doc["multirange"]] = doc["multirange_oid"]
    return out


def user_type_name(db: str, catalog: Catalog, oid: int) -> str | None:
    """The name of a user-declared type (enum / domain / composite / range) by
    oid, or None — the ``oid::regtype`` tail for oids the built-in tables don't
    know."""
    for lookup in (_enum_oids, _domain_oids, _composite_oids, _range_type_oids):
        for name, type_oid in lookup(db, catalog).items():
            if type_oid == oid:
                return name
    for name, type_oid in _table_rowtype_oids(db, catalog).items():
        if type_oid == oid:
            return name
    return None


_BARE_IDENT_RE = re.compile(r"[a-z_][a-z0-9_$]*\Z")

# Reserved words that ``regtype`` output must double-quote even when lowercase
# (``create type "order"`` renders as ``"order"``, never bare). The subset of
# PG's fully-reserved keywords likely to appear as type names.
_RESERVED_TYPE_WORDS = frozenset(
    [
        "all",
        "analyse",
        "analyze",
        "and",
        "any",
        "array",
        "as",
        "asc",
        "asymmetric",
        "both",
        "case",
        "cast",
        "check",
        "collate",
        "column",
        "constraint",
        "create",
        "current_date",
        "current_role",
        "current_time",
        "current_timestamp",
        "current_user",
        "default",
        "deferrable",
        "desc",
        "distinct",
        "do",
        "else",
        "end",
        "except",
        "false",
        "fetch",
        "for",
        "foreign",
        "from",
        "grant",
        "group",
        "having",
        "in",
        "initially",
        "intersect",
        "into",
        "lateral",
        "leading",
        "limit",
        "localtime",
        "localtimestamp",
        "not",
        "null",
        "offset",
        "on",
        "only",
        "or",
        "order",
        "placing",
        "primary",
        "references",
        "returning",
        "select",
        "session_user",
        "some",
        "symmetric",
        "table",
        "then",
        "to",
        "trailing",
        "true",
        "union",
        "unique",
        "user",
        "using",
        "variadic",
        "when",
        "where",
        "window",
        "with",
    ]
)


def quote_type_name(name: str) -> str:
    """Render a user-type name the way ``oid::regtype`` prints it: each dotted
    part double-quoted unless it is a plain lower-case identifier that isn't a
    reserved word (``CamelCaseEnum`` → ``"CamelCaseEnum"``, ``order`` →
    ``"order"``). psycopg's ClientCursor pastes this string verbatim as a cast
    suffix, so an unquoted mixed-case or reserved name would misparse."""

    def _q(part: str) -> str:
        if _BARE_IDENT_RE.fullmatch(part) and part not in _RESERVED_TYPE_WORDS:
            return part
        return '"' + part.replace('"', '""') + '"'

    return ".".join(_q(part) for part in name.split("."))


def user_type_oid(db: str, catalog: Catalog, name: str) -> int | None:
    """The oid of a user-declared type (enum / domain / composite) by name, or
    None — the ``to_regtype()`` tail for names the built-in tables don't know.
    A ``public.`` schema qualifier (psycopg's TypeInfo passes the name as
    typed by the user) is accepted."""

    # Normalize per dotted part ('"testschema"."testtype"' — psycopg's
    # sql.Identifier spelling) with Postgres identifier folding: a quoted part
    # keeps its case, an unquoted part folds to lowercase ('StrTestEnum' ==
    # strtestenum). Then strip the default public namespace.
    text = fold_type_name(name)
    if text.lower().startswith("public."):
        text = text[len("public.") :]
    for lookup in (_enum_oids, _domain_oids, _composite_oids, _range_type_oids):
        oid = lookup(db, catalog).get(text)
        if oid is not None:
            return oid
    # A table's name resolves to its composite row type, like real Postgres.
    rowtype = _table_rowtype_oids(db, catalog).get(text)
    if rowtype is not None:
        return rowtype
    return None


# Range tag -> the *declared* subtype tag (tsrange's bounds are stored as
# timestamptz internally, but its pg_range row must advertise ``timestamp``).
_RANGE_SUBTYPE: dict[str, str] = {
    "int4range": "int4",
    "int8range": "int8",
    "numrange": "numeric",
    "tsrange": "timestamp",
    "tstzrange": "timestamptz",
    "daterange": "date",
}


def _pg_range(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``pg_catalog.pg_range`` — one row per built-in range type, mapping the
    range oid to its element (subtype) oid. psycopg's ``RangeInfo.fetch`` joins
    it (``pg_range r ON t.oid = r.rngtypid``); the multirange fetch reads
    ``rngmultitypid`` too."""
    from secantus.sql.ranges import RANGE_TO_MULTIRANGE

    rows = [
        {
            "rngtypid": typemap.PG_OID[range_tag],
            "rngsubtype": typemap.PG_OID[elem_tag],
            "rngmultitypid": typemap.PG_OID.get(RANGE_TO_MULTIRANGE.get(range_tag, ""), 0),
            "rngcollation": 0,
        }
        for range_tag, elem_tag in _RANGE_SUBTYPE.items()
        if range_tag in typemap.PG_OID
    ]
    lister = getattr(catalog, "list_range_types", None)
    for doc in lister(db) if lister is not None else []:
        rows.append(
            {
                "rngtypid": doc["oid"],
                "rngsubtype": typemap.PG_OID.get(doc.get("subtype_tag", ""), 25),
                "rngmultitypid": doc.get("multirange_oid", 0),
                "rngcollation": 0,
            }
        )
    return rows


def _fk_action_code(action: str | None) -> str:
    """pg_constraint's one-letter referential-action code."""
    return {
        "CASCADE": "c",
        "SET NULL": "n",
        "SET DEFAULT": "d",
        "RESTRICT": "r",
    }.get((action or "").upper(), "a")


def _pg_constraint(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    # Primary-key constraints (contype 'p'), one per table with a PK, keyed to
    # the implicit PK index via conindid, plus declared foreign keys (contype
    # 'f'). No check / unique *constraints* in our model (a CREATE UNIQUE INDEX
    # is an index, not a constraint), so contype 'u'/'c' rows are absent.
    rows: list[dict] = []
    oid = _PK_CON_OID_BASE
    # A foreign key's conindid points at the referenced table's PK index —
    # pgjdbc's getImportedKeys joins ``pkic.oid = con.conindid`` to read the
    # PK_NAME, so a 0 here silently empties every FK metadata result.
    pk_index_by_rel = {
        ix["indrelid"]: ix["indexrelid"]
        for ix in _index_relations(db, storage, catalog)
        if ix["primary"]
    }
    for ix in _index_relations(db, storage, catalog):
        if not ix["primary"]:
            continue
        rows.append(
            {
                "oid": oid,
                "conname": ix["conname"],
                "conrelid": ix["indrelid"],
                "confrelid": 0,
                "conindid": ix["indexrelid"],
                "contype": "p",
                "contypid": 0,  # not a domain constraint
                "condeferrable": False,
                "condeferred": False,
                "conkey": list(ix["indkey"]),
                "confkey": None,
            }
        )
        oid += 1
    for fk in _foreign_keys(db, catalog):
        rows.append(
            {
                "oid": fk["oid"],
                "conname": fk["conname"],
                "conrelid": fk["conrelid"],
                "confrelid": fk["confrelid"],
                "conindid": pk_index_by_rel.get(fk["confrelid"], 0),
                "contype": "f",
                "contypid": 0,
                "condeferrable": fk["fk"].deferrable,
                "condeferred": fk["fk"].initially_deferred,
                "conkey": fk["conkey"],
                "confkey": fk["confkey"],
                "confupdtype": _fk_action_code(fk["fk"].on_update),
                "confdeltype": _fk_action_code(fk["fk"].on_delete),
            }
        )
    for uq in _unique_constraints(db, catalog):
        rows.append(
            {
                "oid": uq["oid"],
                "conname": uq["conname"],
                "conrelid": uq["conrelid"],
                "confrelid": 0,
                "conindid": uq["conindid"],
                "contype": "u",
                "contypid": 0,
                "condeferrable": uq["deferrable"],
                "condeferred": uq["initially_deferred"],
                "conkey": uq["conkey"],
                "confkey": None,
            }
        )
    for ck in _check_constraints(db, catalog):
        rows.append(
            {
                "oid": ck["oid"],
                "conname": ck["conname"],
                "conrelid": ck["conrelid"],
                "confrelid": 0,
                "conindid": 0,
                "contype": "c",
                "contypid": 0,
                "condeferrable": False,
                "condeferred": False,
                "conkey": None,
                "confkey": None,
            }
        )
    # Domain CHECK constraints (contype 'c', keyed to the domain via contypid).
    getter = getattr(catalog, "get_domain", None)
    doid = 33000
    for name, type_oid in _domain_oids(db, catalog).items():
        domain = getter(db, name) if getter is not None else None
        for check in (domain or {}).get("checks") or []:
            rows.append(
                {
                    "oid": doid,
                    "conname": check["name"],
                    "conrelid": 0,
                    "confrelid": 0,
                    "conindid": 0,
                    "contype": "c",
                    "contypid": type_oid,
                    "condeferrable": False,
                    "condeferred": False,
                    "conkey": None,
                    "confkey": None,
                }
            )
            doid += 1
    return rows


def _pg_index(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    rows: list[dict] = []
    for ix in _index_relations(db, storage, catalog):
        n = len(ix["indkey"])
        rows.append(
            {
                "indexrelid": ix["indexrelid"],
                "indrelid": ix["indrelid"],
                "indkey": list(ix["indkey"]),
                "indclass": [_DEFAULT_OPCLASS_OID] * n,
                "indoption": [0] * n,
                "indnatts": n,
                "indnkeyatts": ix.get("nkeyatts", n),
                "indisunique": ix["unique"],
                "indisprimary": ix["primary"],
                "indisclustered": False,
                "indisvalid": True,
                "indisreplident": False,
                "indnullsnotdistinct": False,
                "indpred": None,
                "indexprs": None,
            }
        )
    return rows


def _pg_am(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    return [{"oid": _BTREE_AM_OID, "amname": "btree"}, {"oid": _HEAP_AM_OID, "amname": "heap"}]


def _pg_opclass(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    return [{"oid": _DEFAULT_OPCLASS_OID, "opcname": "default_ops", "opcdefault": True}]


def _pg_enum(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """One row per enum label — ``enumtypid`` points at the pg_type enum oid,
    ``enumsortorder`` is the 1-based label position (SQLAlchemy reads these)."""
    oids = _enum_oids(db, catalog)
    lister = getattr(catalog, "list_enums", None)
    rows: list[dict] = []
    oid = _ENUM_OID_BASE + 10000
    for name in lister(db) if lister is not None else []:
        enum = catalog.get_enum(db, name)
        for order, label in enumerate(enum["labels"] if enum else [], start=1):
            rows.append(
                {
                    "oid": oid,
                    "enumtypid": oids[name],
                    "enumsortorder": float(order),
                    "enumlabel": label,
                }
            )
            oid += 1
    return rows


def _pg_database(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    # The connected database plus ``postgres`` — the maintenance database every
    # real PG cluster carries and the one JDBC clients enumerate through
    # (pgjdbc's getCatalogs asserts both are present). We deliberately do NOT
    # enumerate ``storage.list_databases()`` here: that set is the MongoDB-wire
    # namespace and includes cross-protocol names like ``local`` that a PG
    # client must never see as a connectable catalog.
    names = [db] if db == "postgres" else [db, "postgres"]
    return [
        {
            "oid": 1 + i,
            "datname": name,
            "datallowconn": True,
            "datdba": 10,
            "encoding": 6,
            "datcollate": "C",
            "datctype": "C",
            "datacl": None,
        }
        for i, name in enumerate(names)
    ]


def _pg_tables(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """The ``pg_tables`` system view — one row per user table (psql's ``\\dt``
    and various clients' bootstrap queries read it)."""
    return [
        {
            "schemaname": "public",
            "tablename": t.name,
            "tableowner": session.user,
            "tablespace": None,
            "hasindexes": bool(getattr(t, "indexes", None)),
            "hasrules": False,
            "hastriggers": False,
            "rowsecurity": False,
        }
        for t in _user_tables(db, catalog)
    ]


# --------------------------------------------------------------------------- #
# Registry
# --------------------------------------------------------------------------- #

_REGISTRY: dict[tuple[str, str], VirtualTable] = {}


def _register(schema: str, name: str, columns: ColumnsSpec, builder: RowsBuilder) -> None:
    _REGISTRY[(schema, name)] = VirtualTable(schema, name, columns, builder)


_register(
    "information_schema",
    "tables",
    [
        ("table_catalog", "text"),
        ("table_schema", "text"),
        ("table_name", "text"),
        ("table_type", "text"),
    ],
    _info_tables,
)
_register(
    "information_schema",
    "columns",
    [
        ("table_catalog", "text"),
        ("table_schema", "text"),
        ("table_name", "text"),
        ("column_name", "text"),
        ("ordinal_position", "int4"),
        ("data_type", "text"),
        ("is_nullable", "text"),
        ("column_default", "text"),
    ],
    _info_columns,
)
_register(
    "information_schema",
    "views",
    [
        ("table_catalog", "text"),
        ("table_schema", "text"),
        ("table_name", "text"),
        ("view_definition", "text"),
        ("check_option", "text"),
        ("is_updatable", "text"),
        ("is_insertable_into", "text"),
    ],
    _info_views,
)
_register(
    "information_schema",
    "schemata",
    [("catalog_name", "text"), ("schema_name", "text")],
    _info_schemata,
)
_TABLE_GRANT_COLUMNS: ColumnsSpec = [
    ("grantor", "text"),
    ("grantee", "text"),
    ("table_catalog", "text"),
    ("table_schema", "text"),
    ("table_name", "text"),
    ("privilege_type", "text"),
    ("is_grantable", "text"),
    ("with_hierarchy", "text"),
]
_register("information_schema", "role_table_grants", _TABLE_GRANT_COLUMNS, _info_table_grants)
_register("information_schema", "table_privileges", _TABLE_GRANT_COLUMNS, _info_table_grants)
_register(
    "information_schema",
    "column_privileges",
    [
        ("grantor", "text"),
        ("grantee", "text"),
        ("table_catalog", "text"),
        ("table_schema", "text"),
        ("table_name", "text"),
        ("column_name", "text"),
        ("privilege_type", "text"),
        ("is_grantable", "text"),
    ],
    _info_column_grants,
)
_register(
    "information_schema",
    "table_constraints",
    [
        ("constraint_catalog", "text"),
        ("constraint_schema", "text"),
        ("constraint_name", "text"),
        ("table_catalog", "text"),
        ("table_schema", "text"),
        ("table_name", "text"),
        ("constraint_type", "text"),
        ("is_deferrable", "text"),
        ("initially_deferred", "text"),
    ],
    _info_table_constraints,
)
_register(
    "information_schema",
    "key_column_usage",
    [
        ("constraint_catalog", "text"),
        ("constraint_schema", "text"),
        ("constraint_name", "text"),
        ("table_catalog", "text"),
        ("table_schema", "text"),
        ("table_name", "text"),
        ("column_name", "text"),
        ("ordinal_position", "int4"),
        ("position_in_unique_constraint", "int4"),
    ],
    _info_key_column_usage,
)
_register(
    "information_schema",
    "constraint_column_usage",
    [
        ("table_catalog", "text"),
        ("table_schema", "text"),
        ("table_name", "text"),
        ("column_name", "text"),
        ("constraint_catalog", "text"),
        ("constraint_schema", "text"),
        ("constraint_name", "text"),
    ],
    _info_constraint_column_usage,
)
_register(
    "information_schema",
    "check_constraints",
    [
        ("constraint_catalog", "text"),
        ("constraint_schema", "text"),
        ("constraint_name", "text"),
        ("check_clause", "text"),
    ],
    _info_check_constraints,
)
_register(
    "information_schema",
    "referential_constraints",
    [
        ("constraint_catalog", "text"),
        ("constraint_schema", "text"),
        ("constraint_name", "text"),
        ("unique_constraint_catalog", "text"),
        ("unique_constraint_schema", "text"),
        ("unique_constraint_name", "text"),
        ("match_option", "text"),
        ("update_rule", "text"),
        ("delete_rule", "text"),
    ],
    _info_referential_constraints,
)
_register(
    "information_schema",
    "sequences",
    [
        ("sequence_catalog", "text"),
        ("sequence_schema", "text"),
        ("sequence_name", "text"),
        ("data_type", "text"),
        ("numeric_precision", "int4"),
        ("numeric_scale", "int4"),
        ("start_value", "text"),
        ("minimum_value", "text"),
        ("maximum_value", "text"),
        ("increment", "text"),
        ("cycle_option", "text"),
    ],
    _info_sequences,
)
_register(
    "pg_catalog",
    "pg_namespace",
    [("oid", "int4"), ("nspname", "text"), ("nspowner", "int4")],
    _pg_namespace,
)
_register(
    "pg_catalog",
    "pg_tables",
    [
        ("schemaname", "text"),
        ("tablename", "text"),
        ("tableowner", "text"),
        ("tablespace", "text"),
        ("hasindexes", "bool"),
        ("hasrules", "bool"),
        ("hastriggers", "bool"),
        ("rowsecurity", "bool"),
    ],
    _pg_tables,
)
_register(
    "pg_catalog",
    "pg_class",
    [
        ("oid", "int4"),
        ("relname", "text"),
        ("relnamespace", "int4"),
        ("relowner", "int4"),
        ("reltoastrelid", "int4"),
        ("relchecks", "int2"),
        ("relhasindex", "bool"),
        ("relhasrules", "bool"),
        ("relhastriggers", "bool"),
        ("relrowsecurity", "bool"),
        ("relforcerowsecurity", "bool"),
        ("relispartition", "bool"),
        ("reltablespace", "int4"),
        ("relreplident", "text"),
        ("reloftype", "int4"),
        ("relkind", "text"),
        ("relacl", "text"),
        ("relpersistence", "text"),
        ("relam", "int4"),
        ("reloptions", "text"),
        ("reltype", "int4"),
        ("reltuples", "float4"),
    ],
    _pg_class,
)
_register(
    "pg_catalog",
    "pg_attribute",
    [
        ("attrelid", "int4"),
        ("attname", "text"),
        ("atttypid", "int4"),
        ("atttypmod", "int4"),
        ("attnum", "int4"),
        ("attnotnull", "bool"),
        ("atthasdef", "bool"),
        ("attisdropped", "bool"),
        ("attidentity", "text"),
        ("attgenerated", "text"),
        ("attcollation", "int4"),
        ("attlen", "int4"),
    ],
    _pg_attribute,
)
_register(
    "pg_catalog",
    "pg_attrdef",
    [("oid", "int4"), ("adrelid", "int4"), ("adnum", "int4"), ("adbin", "text")],
    _pg_attrdef,
)
_register(
    "pg_catalog",
    "pg_description",
    [("objoid", "int4"), ("classoid", "int4"), ("objsubid", "int4"), ("description", "text")],
    _pg_description,
)
_register(
    "pg_catalog",
    "pg_sequence",
    [
        ("seqrelid", "int4"),
        ("seqstart", "int8"),
        ("seqincrement", "int8"),
        ("seqmax", "int8"),
        ("seqmin", "int8"),
        ("seqcache", "int8"),
        ("seqcycle", "bool"),
    ],
    _pg_sequence,
)
_register(
    "pg_catalog",
    "pg_collation",
    [("oid", "int4"), ("collname", "text"), ("collnamespace", "int4")],
    _pg_collation,
)
_register(
    "pg_catalog",
    "pg_roles",
    [
        ("oid", "int4"),
        ("rolname", "text"),
        ("rolsuper", "bool"),
        ("rolinherit", "bool"),
        ("rolcreaterole", "bool"),
        ("rolcreatedb", "bool"),
        ("rolcanlogin", "bool"),
        ("rolreplication", "bool"),
        ("rolconnlimit", "int4"),
        ("rolbypassrls", "bool"),
    ],
    _pg_roles,
)
_register(
    "pg_catalog",
    "pg_auth_members",
    [
        ("roleid", "int4"),
        ("member", "int4"),
        ("grantor", "int4"),
        ("admin_option", "bool"),
    ],
    _pg_auth_members,
)
_register(
    "pg_catalog",
    "pg_policies",
    [
        ("schemaname", "text"),
        ("tablename", "text"),
        ("policyname", "text"),
        ("permissive", "text"),
        ("roles", "text"),
        ("cmd", "text"),
        ("qual", "text"),
        ("with_check", "text"),
    ],
    _pg_policies,
)
_register(
    "pg_catalog",
    "pg_stat_activity",
    [
        ("datid", "int4"),
        ("datname", "text"),
        ("pid", "int4"),
        ("usesysid", "int4"),
        ("usename", "text"),
        ("application_name", "text"),
        ("client_addr", "text"),
        ("client_port", "int4"),
        ("backend_start", "timestamptz"),
        ("xact_start", "timestamptz"),
        ("query_start", "timestamptz"),
        ("state_change", "timestamptz"),
        ("wait_event_type", "text"),
        ("wait_event", "text"),
        ("state", "text"),
        ("query", "text"),
        ("backend_type", "text"),
    ],
    _pg_stat_activity,
)
_register(
    "pg_catalog",
    "pg_stat_database",
    [
        ("datid", "int4"),
        ("datname", "text"),
        ("numbackends", "int4"),
        ("xact_commit", "int8"),
        ("xact_rollback", "int8"),
        ("blks_read", "int8"),
        ("blks_hit", "int8"),
        ("tup_returned", "int8"),
        ("tup_fetched", "int8"),
        ("tup_inserted", "int8"),
        ("tup_updated", "int8"),
        ("tup_deleted", "int8"),
    ],
    _pg_stat_database,
)
_register(
    "pg_catalog",
    "pg_prepared_xacts",
    [
        ("transaction", "int8"),
        ("gid", "text"),
        ("prepared", "timestamptz"),
        ("owner", "text"),
        ("database", "text"),
    ],
    _pg_prepared_xacts,
)
_register(
    "pg_catalog",
    "pg_cursors",
    [
        ("name", "text"),
        ("statement", "text"),
        ("is_holdable", "bool"),
        ("is_binary", "bool"),
        ("is_scrollable", "bool"),
        ("creation_time", "timestamptz"),
    ],
    _pg_cursors,
)
_register(
    "pg_catalog",
    "pg_prepared_statements",
    [
        ("name", "text"),
        ("statement", "text"),
        ("prepare_time", "timestamptz"),
        ("parameter_types", "text[]"),
        ("from_sql", "bool"),
    ],
    _pg_prepared_statements,
)
_register(
    "pg_catalog",
    "pg_settings",
    [
        ("name", "text"),
        ("setting", "text"),
        ("unit", "text"),
        ("category", "text"),
        ("short_desc", "text"),
        ("context", "text"),
        ("vartype", "text"),
        ("source", "text"),
        ("min_val", "text"),
        ("max_val", "text"),
        ("enumvals", "text"),
        ("boot_val", "text"),
        ("reset_val", "text"),
        ("pending_restart", "bool"),
    ],
    _pg_settings,
)
_register(
    "pg_catalog",
    "pg_locks",
    [
        ("locktype", "text"),
        ("database", "int4"),
        ("relation", "int4"),
        ("page", "int4"),
        ("tuple", "int4"),
        ("virtualxid", "text"),
        ("transactionid", "int4"),
        ("classid", "int4"),
        ("objid", "int4"),
        ("objsubid", "int4"),
        ("virtualtransaction", "text"),
        ("pid", "int4"),
        ("mode", "text"),
        ("granted", "bool"),
        ("fastpath", "bool"),
    ],
    _pg_locks,
)
_register(
    "pg_catalog",
    "pg_indexes",
    [
        ("schemaname", "text"),
        ("tablename", "text"),
        ("indexname", "text"),
        ("tablespace", "text"),
        ("indexdef", "text"),
    ],
    _pg_indexes,
)
_register(
    "pg_catalog",
    "pg_proc",
    [
        ("oid", "int4"),
        ("proname", "text"),
        ("pronamespace", "int4"),
        ("proowner", "int4"),
        ("prolang", "int4"),
        ("prorettype", "int4"),
        ("pronargs", "int4"),
        ("pronargdefaults", "int4"),
        ("proargtypes", "text"),
        ("proargnames", "text[]"),
        ("proargmodes", "text[]"),
        ("proallargtypes", "text[]"),
        ("prosrc", "text"),
        ("prokind", "text"),
        ("proretset", "bool"),
        ("provariadic", "int4"),
    ],
    _pg_proc,
)
_register(
    "information_schema",
    "routines",
    [
        ("specific_catalog", "text"),
        ("specific_schema", "text"),
        ("specific_name", "text"),
        ("routine_catalog", "text"),
        ("routine_schema", "text"),
        ("routine_name", "text"),
        ("routine_type", "text"),
        ("data_type", "text"),
        ("routine_body", "text"),
        ("routine_definition", "text"),
        ("external_language", "text"),
        ("is_deterministic", "text"),
    ],
    _info_routines,
)
_register(
    "information_schema",
    "parameters",
    [
        ("specific_catalog", "text"),
        ("specific_schema", "text"),
        ("specific_name", "text"),
        ("ordinal_position", "int4"),
        ("parameter_mode", "text"),
        ("parameter_name", "text"),
        ("data_type", "text"),
    ],
    _info_parameters,
)
_register(
    "pg_catalog",
    "pg_type",
    [
        ("oid", "int4"),
        ("typname", "text"),
        ("typcollation", "int4"),
        ("typnamespace", "int4"),
        ("typbasetype", "int4"),
        ("typtypmod", "int4"),
        ("typnotnull", "bool"),
        ("typdefault", "text"),
        ("typtype", "text"),
        ("typrelid", "int4"),
        ("typarray", "int4"),
        ("typelem", "int4"),
        ("typdelim", "text"),
        ("typinput", "text"),
    ],
    _pg_type,
)
_register(
    "pg_catalog",
    "pg_range",
    [
        ("rngtypid", "int4"),
        ("rngsubtype", "int4"),
        ("rngmultitypid", "int4"),
        ("rngcollation", "int4"),
    ],
    _pg_range,
)
_register(
    "pg_catalog",
    "pg_constraint",
    [
        ("oid", "int4"),
        ("contypid", "int4"),
        ("conname", "text"),
        ("conrelid", "int4"),
        ("confrelid", "int4"),
        ("conindid", "int4"),
        ("contype", "text"),
        ("condeferrable", "bool"),
        ("condeferred", "bool"),
        ("conkey", "json"),
        ("confkey", "json"),
        ("confupdtype", "text"),
        ("confdeltype", "text"),
    ],
    _pg_constraint,
)


def _pg_policy(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    # Row-level-security policies: psql's ``\d`` joins this; one row per
    # declared policy, keyed to the owning table's pg_class oid.
    oids = _table_oids(db, catalog)
    rows: list[dict] = []
    lister = getattr(catalog, "list_policies", None)
    if lister is None:
        return rows
    for i, pol in enumerate(lister(db)):
        relid = oids.get(pol.get("table"))
        if relid is None:
            continue
        rows.append(
            {
                "oid": 75000 + i,
                "polname": pol.get("name"),
                "polrelid": relid,
                "polcmd": (pol.get("cmd") or "*")[:1].lower(),
                "polpermissive": bool(pol.get("permissive", True)),
                "polroles": [0],
                "polqual": pol.get("using"),
                "polwithcheck": pol.get("check"),
            }
        )
    return rows


def _empty_rows(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    return []


# Present-but-empty catalogs psql's ``\d`` family joins: no extended
# statistics, triggers, rules, inheritance, or publications in our model —
# but the relations must exist so the queries run.
_register(
    "pg_catalog",
    "pg_statistic_ext",
    [
        ("oid", "int4"),
        ("stxrelid", "int4"),
        ("stxname", "text"),
        ("stxnamespace", "int4"),
        ("stxkeys", "int2vector"),
        ("stxkind", "text[]"),
        ("stxstattarget", "int4"),
    ],
    _empty_rows,
)
_register(
    "pg_catalog",
    "pg_trigger",
    [
        ("oid", "int4"),
        ("tgrelid", "int4"),
        ("tgname", "text"),
        ("tgfoid", "int4"),
        ("tgtype", "int4"),
        ("tgenabled", "text"),
        ("tgisinternal", "bool"),
        ("tgconstraint", "int4"),
        ("tgdeferrable", "bool"),
        ("tginitdeferred", "bool"),
    ],
    _empty_rows,
)
_register(
    "pg_catalog",
    "pg_rewrite",
    [("oid", "int4"), ("rulename", "text"), ("ev_class", "int4"), ("ev_type", "text")],
    _empty_rows,
)
_register(
    "pg_catalog",
    "pg_inherits",
    [("inhrelid", "int4"), ("inhparent", "int4"), ("inhseqno", "int4")],
    _empty_rows,
)
_register(
    "pg_catalog",
    "pg_publication",
    [
        ("oid", "int4"),
        ("pubname", "text"),
        ("puballtables", "bool"),
        ("pubinsert", "bool"),
        ("pubupdate", "bool"),
        ("pubdelete", "bool"),
        ("pubtruncate", "bool"),
        ("pubviaroot", "bool"),
    ],
    _empty_rows,
)
_register(
    "pg_catalog",
    "pg_publication_rel",
    [("oid", "int4"), ("prpubid", "int4"), ("prrelid", "int4")],
    _empty_rows,
)
_register(
    "pg_catalog",
    "pg_publication_namespace",
    [("oid", "int4"), ("pnpubid", "int4"), ("pnnspid", "int4")],
    _empty_rows,
)
_register(
    "pg_catalog",
    "pg_policy",
    [
        ("oid", "int4"),
        ("polname", "text"),
        ("polrelid", "int4"),
        ("polcmd", "text"),
        ("polpermissive", "bool"),
        ("polroles", "int4[]"),
        ("polqual", "text"),
        ("polwithcheck", "text"),
    ],
    _pg_policy,
)
_register(
    "pg_catalog",
    "pg_index",
    [
        ("indexrelid", "int4"),
        ("indrelid", "int4"),
        ("indkey", "int2vector"),
        ("indclass", "oidvector"),
        ("indoption", "int2vector"),
        ("indnatts", "int4"),
        ("indnkeyatts", "int4"),
        ("indisclustered", "bool"),
        ("indisvalid", "bool"),
        ("indisreplident", "bool"),
        ("indisunique", "bool"),
        ("indisprimary", "bool"),
        ("indnullsnotdistinct", "bool"),
        ("indpred", "text"),
        ("indexprs", "text"),
    ],
    _pg_index,
)
_register(
    "pg_catalog",
    "pg_am",
    [("oid", "int4"), ("amname", "text")],
    _pg_am,
)
_register(
    "pg_catalog",
    "pg_opclass",
    [("oid", "int4"), ("opcname", "text"), ("opcdefault", "bool")],
    _pg_opclass,
)
_register(
    "pg_catalog",
    "pg_enum",
    [
        ("oid", "int4"),
        ("enumtypid", "int4"),
        ("enumsortorder", "float8"),
        ("enumlabel", "text"),
    ],
    _pg_enum,
)
_register(
    "pg_catalog",
    "pg_database",
    [
        ("oid", "int4"),
        ("datname", "text"),
        ("datallowconn", "bool"),
        ("datdba", "int4"),
        ("encoding", "int4"),
        ("datcollate", "text"),
        ("datctype", "text"),
        ("datacl", "text[]"),
    ],
    _pg_database,
)


def lookup(schema: str | None, name: str) -> VirtualTable | None:
    """Find a virtual table by (schema, name), or by name across schemas."""
    if schema is not None:
        return _REGISTRY.get((schema, name))
    for (_sch, nm), vt in _REGISTRY.items():
        if nm == name:
            return vt
    return None


# --------------------------------------------------------------------------- #
# In-memory backend so the normal SELECT executor can run over virtual rows
# --------------------------------------------------------------------------- #


def _sortkey(value: Any) -> tuple:
    return (0,) if value is None else (1, value)


class MemoryBackend:
    """Read-only ``Storage``-shaped view over a fixed list of row dicts."""

    def __init__(self, rows: list[dict[str, Any]]) -> None:
        self._rows = rows

    def find_matching(
        self,
        db: str,
        coll: str,
        filter: Any = None,
        *,
        skip: int = 0,
        limit: int = 0,
        sort: Any = None,
        projection: Any = None,
        **kw: Any,
    ) -> list[dict[str, Any]]:
        out = [dict(r) for r in self._rows if matches(r, filter or {})]
        if sort:
            for field, direction in reversed(list(sort.items())):
                out.sort(key=lambda d, f=field: _sortkey(get_path(d, f)), reverse=(direction == -1))
        if skip:
            out = out[skip:]
        if limit:
            out = out[:limit]
        return out


class CatalogBackend:
    """``Storage``-shaped proxy that serves virtual catalog tables in-memory and
    delegates everything else to the real ``Storage``.

    This is what lets a JOIN / GROUP BY span ``pg_catalog`` /
    ``information_schema`` relations: when the aggregation pipeline reads a
    virtual collection (the base ``find_matching`` or a ``$lookup`` foreign
    collection), the rows are built on demand and filtered in memory; a real
    user collection passes straight through. All other ``Storage`` methods the
    aggregation engine needs are forwarded unchanged via ``__getattr__``.
    """

    def __init__(self, storage: Any, catalog: Catalog, session: Session, db: str) -> None:
        self._storage = storage
        self._catalog = catalog
        self._session = session
        self._db = db
        # Materialized derived tables (a (SELECT ...) AS alias join source), keyed
        # by their alias; served like a virtual table for the duration of a query.
        self._ephemeral: dict[str, list[dict[str, Any]]] = {}

    def register_ephemeral(self, name: str, rows: list[dict[str, Any]]) -> None:
        self._ephemeral[name] = rows

    def _virtual_rows(self, coll: str) -> list[dict[str, Any]] | None:
        if coll in self._ephemeral:
            return self._ephemeral[coll]
        vt = lookup(None, coll)
        if vt is None:
            return None
        return vt.builder(self._db, self._session, self._storage, self._catalog)

    def find_matching(
        self,
        db: str,
        coll: str,
        filter: Any = None,
        *,
        skip: int = 0,
        limit: int = 0,
        sort: Any = None,
        projection: Any = None,
        **kw: Any,
    ) -> list[dict[str, Any]]:
        rows = self._virtual_rows(coll)
        if rows is not None:
            return MemoryBackend(rows).find_matching(
                db, coll, filter, skip=skip, limit=limit, sort=sort, projection=projection, **kw
            )
        return self._storage.find_matching(
            db, coll, filter, skip=skip, limit=limit, sort=sort, projection=projection, **kw
        )

    def list_indexes(self, db: str, coll: str) -> list[Any]:
        if coll in self._ephemeral or lookup(None, coll) is not None:
            return []  # virtual / ephemeral tables are never indexed — force hash-join.
        return self._storage.list_indexes(db, coll)

    def __getattr__(self, name: str) -> Any:
        # Forward any other Storage method (count_matching, get_collection_options,
        # ...) to the real storage. find_matching / list_indexes are overridden
        # above so virtual collections never reach the WT layer.
        return getattr(self._storage, name)
