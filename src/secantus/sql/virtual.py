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
        pk = t.pk_column
        if pk is not None:
            out.append(
                {
                    "indexrelid": oid,
                    "indrelid": relid,
                    "relname": f"{t.name}_pkey",
                    "indkey": [field_to_attnum.get(pk.field, 1)],
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
    return out


def _info_tables(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    return [
        {
            "table_catalog": db,
            "table_schema": "public",
            "table_name": t.name,
            "table_type": "BASE TABLE",
        }
        for t in _user_tables(db, catalog)
    ]


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
                    "data_type": typemap.SQL_TYPE_NAME.get(col.type_tag, "text"),
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
        pk = t.pk_column
        if pk is not None:
            out.append((t, f"{t.name}_pkey", [pk.name]))
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
            if not ref_cols and ref is not None and ref.pk_column is not None:
                ref_cols = [ref.pk_column.name]  # REFERENCES t → its PRIMARY KEY
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


def constraint_def_for_oid(db: str, catalog: Catalog, oid: int) -> str | None:
    """``pg_get_constraintdef(oid)`` — the rendered constraint definition for a
    foreign-key constraint OID, or ``None`` when the OID isn't an FK we know."""
    for fk in _foreign_keys(db, catalog):
        if fk["oid"] == oid:
            return fk["condef"]
    return None


def _info_table_constraints(
    db: str, session: Session, storage: Any, catalog: Catalog
) -> list[dict]:
    return [
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
    ] + [
        {
            "constraint_catalog": db,
            "constraint_schema": "public",
            "constraint_name": fk["conname"],
            "table_catalog": db,
            "table_schema": "public",
            "table_name": fk["table"].name,
            "constraint_type": "FOREIGN KEY",
            "is_deferrable": "NO",
            "initially_deferred": "NO",
        }
        for fk in _foreign_keys(db, catalog)
    ]


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
    return rows


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
    # No sequences / identity columns — present-but-empty for the same reason.
    return []


def _pg_namespace(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    return [{"oid": oid, "nspname": name} for name, oid in _NS_OIDS.items()]


def _pg_class(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    oids = _table_oids(db, catalog)
    rows = [
        {
            "oid": oids[t.name],
            "relname": t.name,
            "relnamespace": _NS_OIDS["public"],
            "relkind": "r",
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
    return rows


def _pg_attribute(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    """One row per column of every declared table — the pg_catalog column surface
    tools (and ``\\d``-style queries) read. attrelid lines up with pg_class.oid."""
    oids = _table_oids(db, catalog)
    rows: list[dict] = []
    for t in _user_tables(db, catalog):
        for i, col in enumerate(t.columns, start=1):
            rows.append(
                {
                    "attrelid": oids[t.name],
                    "attname": col.name,
                    "atttypid": typemap.PG_OID.get(col.type_tag, 25),
                    "atttypmod": -1,
                    "attnum": i,
                    "attnotnull": not col.nullable,
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
    # No sequences / identity columns in our model — present-but-empty so the
    # identity-options subquery in SQLAlchemy's get_columns resolves to NULL.
    return []


def _pg_collation(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    # No non-default collations — present-but-empty.
    return []


def _pg_type(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    return [
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
                "conkey": fk["conkey"],
                "confkey": fk["confkey"],
            }
        )
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
    # No enum types — present-but-empty so SQLAlchemy's enum-label subquery resolves.
    return []


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
