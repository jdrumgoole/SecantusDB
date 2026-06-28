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

from dataclasses import dataclass
from typing import Any, Protocol

from secantus.sql import errors

CATALOG_COLLECTION = "__sql_catalog__"


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


@dataclass
class TableDef:
    name: str
    collection: str
    columns: list[Column]

    def column(self, name: str) -> Column | None:
        for c in self.columns:
            if c.name == name:
                return c
        return None

    def field_for(self, name: str) -> str:
        c = self.column(name)
        if c is None:
            raise errors.undefined_column(name)
        return c.field

    def type_for(self, name: str) -> str:
        c = self.column(name)
        if c is None:
            raise errors.undefined_column(name)
        return c.type_tag

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
            }
            for c in table.columns
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
            )
            for c in doc["columns"]
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

    def drop(self, db: str, table: str) -> bool:
        return self._storage.delete_matching(db, CATALOG_COLLECTION, {"_id": table}) > 0

    def list_tables(self, db: str) -> list[str]:
        docs = self._storage.find_matching(db, CATALOG_COLLECTION, {})
        return sorted(d["table"] for d in docs)
