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
from secantus.sql.catalog import Catalog, Column, TableDef
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


def _pg_namespace(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    return [{"oid": oid, "nspname": name} for name, oid in _NS_OIDS.items()]


def _pg_class(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    rows: list[dict] = []
    for i, t in enumerate(_user_tables(db, catalog), start=16384):
        rows.append(
            {
                "oid": i,
                "relname": t.name,
                "relnamespace": _NS_OIDS["public"],
                "relkind": "r",
                "relpersistence": "p",  # permanent (never temp/unlogged)
            }
        )
    return rows


def _pg_type(db: str, session: Session, storage: Any, catalog: Catalog) -> list[dict]:
    return [
        {"oid": typemap.PG_OID[tag], "typname": typname}
        for tag, typname in typemap.PG_TYPENAME.items()
    ]


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
    ],
    _pg_class,
)
_register(
    "pg_catalog",
    "pg_type",
    [("oid", "int4"), ("typname", "text")],
    _pg_type,
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

    def _virtual_rows(self, coll: str) -> list[dict[str, Any]] | None:
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
        if lookup(None, coll) is not None:
            return []  # virtual tables are never indexed — force the hash-join path.
        return self._storage.list_indexes(db, coll)

    def __getattr__(self, name: str) -> Any:
        # Forward any other Storage method (count_matching, get_collection_options,
        # ...) to the real storage. find_matching / list_indexes are overridden
        # above so virtual collections never reach the WT layer.
        return getattr(self._storage, name)
