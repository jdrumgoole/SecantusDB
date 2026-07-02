"""The SQL catalog: table definitions persisted as documents.

Declared tables (``CREATE TABLE``) record their columns, types, and primary key
in a per-db ``__sql_catalog__`` collection — one document per table, keyed by
table name. This is what makes a schemaless Mongo collection answerable as a
typed SQL relation, and (in a later phase) what ``information_schema`` reads
from.

A table maps 1:1 to a collection of the same name. A column maps to a document
*field*; the single PRIMARY KEY column maps to the document ``_id`` (so SQL PK
uniqueness rides the storage layer's ``_id`` index for free), every other
column maps to a field of its own name.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Protocol

from secantus.sql import errors

CATALOG_COLLECTION = "__sql_catalog__"
VIEW_COLLECTION = "__sql_views__"


class _StorageLike(Protocol):
    """The slice of ``Storage`` the catalog uses (duck-typed for testability)."""

    def insert(
        self, db: str, coll: str, docs: Any, *, ordered: bool = ..., journal: bool = ...
    ) -> tuple[int, list[dict[str, Any]]]: ...

    def find_matching(
        self, db: str, coll: str, filter: Any = ..., **kw: Any
    ) -> list[dict[str, Any]]: ...

    def delete_matching(self, db: str, coll: str, filter: Any, **kw: Any) -> int: ...


@dataclass(frozen=True)
class Column:
    name: str
    type_tag: str
    field: str  # "_id" for the PK column, else == name
    pk: bool
    nullable: bool
    # A literal column DEFAULT (applied when an INSERT omits the column).
    # ``has_default`` disambiguates "DEFAULT NULL" from "no default".
    has_default: bool = False
    default: Any = None
    comment: str | None = None  # COMMENT ON COLUMN (reflected via pg_description)


@dataclass(frozen=True)
class ForeignKey:
    """A declared (never enforced) foreign-key constraint.

    Recorded so reflection (``information_schema`` / ``pg_catalog`` / SQLAlchemy's
    inspector) can see it. SecantusDB does not check referential integrity on
    write — this is a schema-shape record, not a runtime guard."""

    name: str  # constraint name, e.g. "orders_user_id_fkey"
    columns: tuple[str, ...]  # local column(s)
    ref_table: str
    ref_columns: tuple[str, ...]
    on_delete: str | None = None  # "CASCADE" / "SET NULL" / ... (informational)
    on_update: str | None = None


@dataclass
class TableDef:
    name: str
    collection: str
    columns: list[Column]
    # Reflected tables have a sampled, schema-on-read shape: any column name
    # resolves to a field of the same name, and an un-sampled column reads as
    # the permissive ``any`` type rather than erroring.
    reflected: bool = False
    foreign_keys: list[ForeignKey] = field(default_factory=list)
    comment: str | None = None  # COMMENT ON TABLE (reflected via pg_description)

    def column(self, name: str) -> Column | None:
        for c in self.columns:
            if c.name == name:
                return c
        return None

    def field_for(self, name: str) -> str:
        c = self.column(name)
        if c is not None:
            return c.field
        if self.reflected:
            return name
        raise errors.undefined_column(name)

    def type_for(self, name: str) -> str:
        c = self.column(name)
        if c is not None:
            return c.type_tag
        if self.reflected:
            return "any"
        raise errors.undefined_column(name)

    @property
    def pk_column(self) -> Column | None:
        for c in self.columns:
            if c.pk:
                return c
        return None


def _to_doc(table: TableDef) -> dict[str, Any]:
    return {
        "_id": table.name,
        "table": table.name,
        "collection": table.collection,
        "columns": [
            {
                "name": c.name,
                "type": c.type_tag,
                "field": c.field,
                "pk": c.pk,
                "nullable": c.nullable,
                "has_default": c.has_default,
                "default": c.default,
                "comment": c.comment,
            }
            for c in table.columns
        ],
        "comment": table.comment,
        "foreign_keys": [
            {
                "name": fk.name,
                "columns": list(fk.columns),
                "ref_table": fk.ref_table,
                "ref_columns": list(fk.ref_columns),
                "on_delete": fk.on_delete,
                "on_update": fk.on_update,
            }
            for fk in table.foreign_keys
        ],
    }


def _from_doc(doc: dict[str, Any]) -> TableDef:
    return TableDef(
        name=doc["table"],
        collection=doc["collection"],
        columns=[
            Column(
                name=c["name"],
                type_tag=c["type"],
                field=c["field"],
                pk=bool(c["pk"]),
                nullable=bool(c["nullable"]),
                has_default=bool(c.get("has_default", False)),
                default=c.get("default"),
                comment=c.get("comment"),
            )
            for c in doc["columns"]
        ],
        comment=doc.get("comment"),
        foreign_keys=[
            ForeignKey(
                name=fk["name"],
                columns=tuple(fk["columns"]),
                ref_table=fk["ref_table"],
                ref_columns=tuple(fk["ref_columns"]),
                on_delete=fk.get("on_delete"),
                on_update=fk.get("on_update"),
            )
            for fk in doc.get("foreign_keys", [])
        ],
    )


class Catalog:
    """Reads/writes table definitions in ``__sql_catalog__``."""

    def __init__(self, storage: _StorageLike) -> None:
        self._storage = storage

    def get(self, db: str, table: str) -> TableDef | None:
        docs = self._storage.find_matching(db, CATALOG_COLLECTION, {"_id": table}, limit=1)
        return _from_doc(docs[0]) if docs else None

    def exists(self, db: str, table: str) -> bool:
        return self.get(db, table) is not None

    def put(self, db: str, table: TableDef) -> None:
        self._storage.insert(db, CATALOG_COLLECTION, [_to_doc(table)])

    def replace(self, db: str, table: TableDef, *, old_name: str | None = None) -> None:
        """Overwrite a table's catalog doc (for ALTER). ``old_name`` lets a
        RENAME drop the entry under the previous name before writing the new."""
        self._storage.delete_matching(db, CATALOG_COLLECTION, {"_id": old_name or table.name})
        self._storage.insert(db, CATALOG_COLLECTION, [_to_doc(table)])

    def drop(self, db: str, table: str) -> bool:
        return self._storage.delete_matching(db, CATALOG_COLLECTION, {"_id": table}) > 0

    def list_tables(self, db: str) -> list[str]:
        docs = self._storage.find_matching(db, CATALOG_COLLECTION, {})
        return sorted(d["table"] for d in docs)

    # -- views ------------------------------------------------------------- #
    # A view is just a stored SELECT definition; querying one expands it as a
    # subquery. Kept in a separate collection so it never shadows a real table.

    def put_view(self, db: str, name: str, definition: str) -> None:
        self._storage.delete_matching(db, VIEW_COLLECTION, {"_id": name})
        self._storage.insert(
            db, VIEW_COLLECTION, [{"_id": name, "view": name, "definition": definition}]
        )

    def get_view(self, db: str, name: str) -> str | None:
        docs = self._storage.find_matching(db, VIEW_COLLECTION, {"_id": name}, limit=1)
        return docs[0]["definition"] if docs else None

    def drop_view(self, db: str, name: str) -> bool:
        return self._storage.delete_matching(db, VIEW_COLLECTION, {"_id": name}) > 0

    def list_views(self, db: str) -> list[str]:
        docs = self._storage.find_matching(db, VIEW_COLLECTION, {})
        return sorted(d["view"] for d in docs)
