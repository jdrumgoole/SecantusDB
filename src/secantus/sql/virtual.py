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

from collections.abc import Callable
from typing import Any

from secantus.paths import get_path
from secantus.query import matches
from secantus.sql import typemap
from secantus.sql.catalog import Catalog, Column, ForeignKey, TableDef
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


def _table_oids(db: str, catalog: Catalog) -> dict[str, int]:
    """Stable, fictional pg_class OIDs per table — shared by every catalog that
    keys off ``relid`` (pg_class.oid, pg_attribute.attrelid) so joins line up."""
    return {t.name: 16384 + i for i, t in enumerate(_user_tables(db, catalog))}


_VIEW_OID_BASE = 50000
_SEQUENCE_OID_BASE = 55000
_ROLE_OID_BASE = 60000


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


def _index_relations(db: str, storage: Any, catalog: Catalog) -> list[dict[str, Any]]:
    """Enumerate every index relation (the implicit primary-key index plus each
    user ``CREATE INDEX``) with the fields pg_index / pg_class / pg_constraint
    reflection needs: a stable ``indexrelid``, its table ``indrelid``, the
    ``indkey`` attnum array, and unique/primary flags."""
    table_oids = _table_oids(db, catalog)
    out: list[dict[str, Any]] = []
    oid = _INDEX_OID_BASE
    for t in _user_tables(db, catalog):
        relid = table_oids[t.name]
        field_to_attnum = {col.field: i for i, col in enumerate(t.columns, start=1)}
        pk_cols = t.pk_columns
        if pk_cols:
            out.append(
                {
                    "indexrelid": oid,
                    "indrelid": relid,
                    "relname": f"{t.name}_pkey",
                    "indkey": [field_to_attnum.get(c.field, 1) for c in pk_cols],
                    "unique": True,
                    "primary": True,
                    "conname": f"{t.name}_pkey",
                }
            )
            oid += 1
        for ix in storage.list_indexes(db, t.collection):
            key = ix.get("key") or {}
            indkey = [field_to_attnum.get(f) for f in key]
            if not indkey or any(a is None for a in indkey):
                continue  # index over a non-column field — not reflectable as SQL
            out.append(
                {
                    "indexrelid": oid,
                    "indrelid": relid,
                    "relname": ix["name"],
                    "indkey": indkey,
                    "unique": bool(ix.get("unique")),
                    "primary": False,
                    "conname": None,
                }
            )
            oid += 1
    # The implicit unique index backing each declared UNIQUE constraint.
    out.extend(_unique_constraint_index_relations(db, catalog))
    return out


def _info_tables(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    # Materialized views are pg_catalog relations only — Postgres does not list
    # them in information_schema.tables, so neither do we.
    matviews = _matview_names(db, catalog)
    rows = [
        {
            "table_catalog": db,
            "table_schema": "public",
            "table_name": t.name,
            "table_type": "BASE TABLE",
        }
        for t in _user_tables(db, catalog)
        if t.name not in matviews
    ]
    # Views appear in information_schema.tables with table_type 'VIEW'.
    rows.extend(
        {
            "table_catalog": db,
            "table_schema": "public",
            "table_name": name,
            "table_type": "VIEW",
        }
        for name in _view_names(db, catalog)
    )
    return rows


def _info_views(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    getter = getattr(catalog, "get_view", None)
    rows: list[dict] = []
    for name in _view_names(db, catalog):
        rows.append(
            {
                "table_catalog": db,
                "table_schema": "public",
                "table_name": name,
                "view_definition": getter(db, name) if getter is not None else None,
                "check_option": "NONE",
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
                }
            )
    return rows


def _info_schemata(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    return [
        {"catalog_name": db, "schema_name": s}
        for s in ("public", "information_schema", "pg_catalog")
    ]


def _pk_constraints(db: str, catalog: Catalog) -> list[tuple[TableDef, str, list[str]]]:
    """Each table's PRIMARY KEY constraint as ``(table, constraint_name, [cols])``.
    The PK is the only real constraint in our model (a ``CREATE UNIQUE INDEX`` is
    an index, not a constraint), so these builders surface PK rows only."""
    out: list[tuple[TableDef, str, list[str]]] = []
    for t in _user_tables(db, catalog):
        pk_cols = t.pk_columns
        if pk_cols:
            out.append((t, f"{t.name}_pkey", [c.name for c in pk_cols]))
    return out


_FK_OID_BASE = 40000


def _fk_condef(fk: ForeignKey, ref_cols: list[str]) -> str:
    """Render a foreign key the way ``pg_get_constraintdef`` does — SQLAlchemy's
    inspector regex-parses exactly this string to reflect the constraint."""
    cols = ", ".join(fk.columns)
    rcols = ", ".join(ref_cols)
    text = f"FOREIGN KEY ({cols}) REFERENCES {fk.ref_table}({rcols})"
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
    table_oids = _table_oids(db, catalog)
    tables = {t.name: t for t in _user_tables(db, catalog)}
    out: list[dict[str, Any]] = []
    oid = _FK_OID_BASE
    for t in _user_tables(db, catalog):
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
                    "ref_pk_name": f"{fk.ref_table}_pkey" if ref is not None else None,
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
    table_oids = _table_oids(db, catalog)
    out: list[dict[str, Any]] = []
    oid = _UNIQUE_CON_OID_BASE
    idx_oid = _UNIQUE_IDX_OID_BASE
    for t in _user_tables(db, catalog):
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


def _unique_constraint_index_relations(db: str, catalog: Catalog) -> list[dict[str, Any]]:
    """The implicit unique index backing each UNIQUE constraint — a pg_index /
    pg_class relation whose ``indexrelid`` matches the constraint's ``conindid``."""
    return [
        {
            "indexrelid": uq["conindid"],
            "indrelid": uq["conrelid"],
            "relname": uq["conname"],
            "indkey": uq["conkey"],
            "unique": True,
            "primary": False,
            "conname": uq["conname"],
        }
        for uq in _unique_constraints(db, catalog)
    ]


def _check_constraints(db: str, catalog: Catalog) -> list[dict[str, Any]]:
    """Declared CHECK constraints with the fields reflection needs. ``condef`` is
    rendered the way Postgres does — ``CHECK ((<expr>))`` — so SQLAlchemy's
    ``get_check_constraints`` regex can peel it back to the predicate text."""
    table_oids = _table_oids(db, catalog)
    out: list[dict[str, Any]] = []
    oid = _CHECK_CON_OID_BASE
    for t in _user_tables(db, catalog):
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
    foreign-key, UNIQUE, or CHECK constraint OID, or ``None`` when unknown."""
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
                "sequence_schema": "public",
                "sequence_name": name,
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


def _pg_namespace(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    return [{"oid": oid, "nspname": name} for name, oid in _NS_OIDS.items()]


def _pg_class(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    oids = _table_oids(db, catalog)
    matviews = _matview_names(db, catalog)
    rows = [
        {
            "oid": oids[t.name],
            "relname": t.name,
            "relnamespace": _NS_OIDS["public"],
            # A materialized view is a real relation with columns, tracked in the
            # catalog like a table — but it reports relkind 'm', not 'r'.
            "relkind": "m" if t.name in matviews else "r",
            "relpersistence": "p",  # permanent (never temp/unlogged)
            "relam": _HEAP_AM_OID,
            "reloptions": None,
        }
        for t in _user_tables(db, catalog)
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
            }
        )
    # Views are pg_class rows too (relkind 'v') — SQLAlchemy's get_view_names
    # filters pg_class on relkind IN ('v','m').
    for name, oid in _view_oids(db, catalog).items():
        rows.append(
            {
                "oid": oid,
                "relname": name,
                "relnamespace": _NS_OIDS["public"],
                "relkind": "v",
                "relpersistence": "p",
                "relam": 0,
                "reloptions": None,
            }
        )
    # Sequences are pg_class rows too (relkind 'S').
    for name, oid in _sequence_oids(db, catalog).items():
        rows.append(
            {
                "oid": oid,
                "relname": name,
                "relnamespace": _NS_OIDS["public"],
                "relkind": "S",
                "relpersistence": "p",
                "relam": 0,
                "reloptions": None,
            }
        )
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


def _pg_attribute(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """One row per column of every declared table — the pg_catalog column surface
    tools (and ``\\d``-style queries) read. attrelid lines up with pg_class.oid."""
    oids = _table_oids(db, catalog)
    enum_oids = _enum_oids(db, catalog)
    domain_oids = _domain_oids(db, catalog)
    rows: list[dict] = []
    for t in _user_tables(db, catalog):
        for i, col in enumerate(t.columns, start=1):
            if col.domain_type is not None:
                typoid = domain_oids.get(col.domain_type, 25)
            elif col.enum_type is not None:
                typoid = enum_oids.get(col.enum_type, 25)
            else:
                typoid = typemap.PG_OID.get(col.type_tag, 25)
            rows.append(
                {
                    "attrelid": oids[t.name],
                    "attname": col.name,
                    "atttypid": typoid,
                    "atttypmod": -1,
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
    return rows


def _pg_attrdef(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    # No column DEFAULTs in our model — the relation exists (so joins resolve)
    # but is always empty.
    return []


_PG_CLASS_OID = 1259  # the OID of the pg_class catalog itself (classoid for relations)


def _pg_description(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    # Object comments from COMMENT ON TABLE / COLUMN. A table comment has
    # objsubid 0; a column comment's objsubid is the column's attnum. classoid is
    # pg_class so SQLAlchemy's get_columns / get_table_comment joins line up.
    oids = _table_oids(db, catalog)
    rows: list[dict] = []
    for t in _user_tables(db, catalog):
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


def _pg_roles(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """``pg_catalog.pg_roles`` — SQL-declared roles plus the connection's own user
    (a superuser login, like Postgres' bootstrap role) when it isn't one already."""
    lister = getattr(catalog, "list_roles", None)
    names = lister(db) if lister is not None else []
    rows = []
    seen = set()
    for i, name in enumerate(names):
        role = catalog.get_role(db, name) or {}
        seen.add(name)
        rows.append(_role_row(_ROLE_OID_BASE + i, name, role))
    # The connecting user is always a role (superuser login).
    if session.user and session.user not in seen:
        rows.append(
            _role_row(
                _ROLE_OID_BASE - 1,
                session.user,
                {"login": True, "superuser": True, "createdb": True, "createrole": True},
            )
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


_ENUM_OID_BASE = 65000


def _enum_oids(db: str, catalog: Catalog) -> dict[str, int]:
    lister = getattr(catalog, "list_enums", None)
    names = lister(db) if lister is not None else []
    return {name: _ENUM_OID_BASE + i for i, name in enumerate(names)}


_DOMAIN_OID_BASE = 66000


def _domain_oids(db: str, catalog: Catalog) -> dict[str, int]:
    lister = getattr(catalog, "list_domains", None)
    names = lister(db) if lister is not None else []
    return {name: _DOMAIN_OID_BASE + i for i, name in enumerate(names)}


_COMPOSITE_OID_BASE = 67000


def _composite_oids(db: str, catalog: Catalog) -> dict[str, int]:
    lister = getattr(catalog, "list_composites", None)
    names = lister(db) if lister is not None else []
    return {name: _COMPOSITE_OID_BASE + i for i, name in enumerate(names)}


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
            "typtype": "b",  # base type (never a domain 'd')
        }
        for tag, typname in typemap.PG_TYPENAME.items()
    ]
    # User-declared enum types (typtype 'e') live in the public namespace.
    for name, oid in _enum_oids(db, catalog).items():
        rows.append(
            {
                "oid": oid,
                "typname": name,
                "typcollation": 0,
                "typnamespace": _NS_OIDS["public"],
                "typbasetype": 0,
                "typtypmod": -1,
                "typnotnull": False,
                "typdefault": None,
                "typtype": "e",
            }
        )
    # User-declared domain types (typtype 'd') carry their base type's oid in
    # typbasetype and the domain's NOT NULL in typnotnull.
    getter = getattr(catalog, "get_domain", None)
    for name, oid in _domain_oids(db, catalog).items():
        domain = getter(db, name) if getter is not None else None
        base_tag = domain.get("base_tag") if domain else None
        default = domain.get("default") if domain else None
        rows.append(
            {
                "oid": oid,
                "typname": name,
                "typcollation": 0,
                "typnamespace": _NS_OIDS["public"],
                "typbasetype": typemap.PG_OID.get(base_tag or "", 25),
                "typtypmod": -1,
                "typnotnull": bool(domain.get("not_null")) if domain else False,
                "typdefault": None if default is None else str(default),
                "typtype": "d",
            }
        )
    # User-declared composite types (typtype 'c') live in the public namespace;
    # their fields are reflected as pg_attribute rows keyed on this oid.
    for name, oid in _composite_oids(db, catalog).items():
        rows.append(
            {
                "oid": oid,
                "typname": name,
                "typcollation": 0,
                "typnamespace": _NS_OIDS["public"],
                "typbasetype": 0,
                "typtypmod": -1,
                "typnotnull": False,
                "typdefault": None,
                "typtype": "c",
            }
        )
    return rows


def _pg_constraint(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    # Primary-key constraints (contype 'p'), one per table with a PK, keyed to
    # the implicit PK index via conindid, plus declared foreign keys (contype
    # 'f'). No check / unique *constraints* in our model (a CREATE UNIQUE INDEX
    # is an index, not a constraint), so contype 'u'/'c' rows are absent.
    rows: list[dict] = []
    oid = 30000
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
                "conindid": 0,
                "contype": "f",
                "contypid": 0,
                "condeferrable": fk["fk"].deferrable,
                "condeferred": fk["fk"].initially_deferred,
                "conkey": fk["conkey"],
                "confkey": fk["confkey"],
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
                "indnkeyatts": n,
                "indisunique": ix["unique"],
                "indisprimary": ix["primary"],
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
    return [{"oid": 1, "datname": db, "datallowconn": True}]


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
    [("oid", "int4"), ("nspname", "text")],
    _pg_namespace,
)
_register(
    "pg_catalog",
    "pg_class",
    [
        ("oid", "int4"),
        ("relname", "text"),
        ("relnamespace", "int4"),
        ("relkind", "text"),
        ("relpersistence", "text"),
        ("relam", "int4"),
        ("reloptions", "text"),
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
    ],
    _pg_type,
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
    ],
    _pg_constraint,
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
    [("oid", "int4"), ("datname", "text"), ("datallowconn", "bool")],
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
