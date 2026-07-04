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
MATVIEW_COLLECTION = "__sql_matviews__"
SEQUENCE_COLLECTION = "__sql_sequences__"
ROLE_COLLECTION = "__sql_roles__"
ENUM_COLLECTION = "__sql_enums__"
DOMAIN_COLLECTION = "__sql_domains__"


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
    # The sequence this column draws its default from (SERIAL columns and
    # ``DEFAULT nextval('seq')``). When set and the column is omitted at INSERT,
    # the executor assigns the sequence's next value.
    sequence: str | None = None
    # Identity mode for a ``GENERATED … AS IDENTITY`` column: ``"always"`` (a
    # user-supplied value is rejected) or ``"by_default"`` (like SERIAL). None for
    # a plain SERIAL or non-identity column.
    identity: str | None = None
    # The enum type name for a column declared with a ``CREATE TYPE … AS ENUM``
    # type. Stored as ``text`` (``type_tag``) but validated against the enum's
    # labels on write and reflected with the enum's type oid.
    enum_type: str | None = None
    # The domain type name for a column declared with a ``CREATE DOMAIN`` type.
    # ``type_tag`` holds the domain's base type; the domain's NOT NULL / CHECK
    # constraints are enforced on write and it reflects with the domain's type oid
    # (pg_type ``typtype = 'd'``).
    domain_type: str | None = None
    # The rendered SQL expression of a ``GENERATED ALWAYS AS (expr) STORED``
    # column. Computed from the row's other columns on every write; a user value
    # can't be supplied. Reflected as ``attgenerated = 's'``.
    generated: str | None = None


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
    deferrable: bool = False  # DEFERRABLE — the check can be postponed to COMMIT
    initially_deferred: bool = False  # INITIALLY DEFERRED — deferred by default


@dataclass(frozen=True)
class CheckConstraint:
    """A declared CHECK constraint. ``expression`` is the rendered SQL of the
    predicate (e.g. ``age >= 0``); it is enforced on write."""

    name: str
    expression: str


@dataclass(frozen=True)
class UniqueConstraint:
    """A declared UNIQUE constraint over one or more columns, enforced on write."""

    name: str
    columns: tuple[str, ...]
    deferrable: bool = False
    initially_deferred: bool = False


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
    check_constraints: list[CheckConstraint] = field(default_factory=list)
    unique_constraints: list[UniqueConstraint] = field(default_factory=list)
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

    @property
    def pk_columns(self) -> list[Column]:
        """All PRIMARY KEY columns in declaration order. A composite PK maps to a
        subdocument ``_id`` (each column's field is ``_id.<name>``); a single PK
        maps directly to ``_id``."""
        return [c for c in self.columns if c.pk]

    @property
    def composite_pk(self) -> bool:
        return len(self.pk_columns) > 1


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
                "sequence": c.sequence,
                "identity": c.identity,
                "enum_type": c.enum_type,
                "domain_type": c.domain_type,
                "generated": c.generated,
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
                "deferrable": fk.deferrable,
                "initially_deferred": fk.initially_deferred,
            }
            for fk in table.foreign_keys
        ],
        "check_constraints": [
            {"name": ck.name, "expression": ck.expression} for ck in table.check_constraints
        ],
        "unique_constraints": [
            {
                "name": uq.name,
                "columns": list(uq.columns),
                "deferrable": uq.deferrable,
                "initially_deferred": uq.initially_deferred,
            }
            for uq in table.unique_constraints
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
                sequence=c.get("sequence"),
                identity=c.get("identity"),
                enum_type=c.get("enum_type"),
                domain_type=c.get("domain_type"),
                generated=c.get("generated"),
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
                deferrable=bool(fk.get("deferrable", False)),
                initially_deferred=bool(fk.get("initially_deferred", False)),
            )
            for fk in doc.get("foreign_keys", [])
        ],
        check_constraints=[
            CheckConstraint(name=ck["name"], expression=ck["expression"])
            for ck in doc.get("check_constraints", [])
        ],
        unique_constraints=[
            UniqueConstraint(
                name=uq["name"],
                columns=tuple(uq["columns"]),
                deferrable=bool(uq.get("deferrable", False)),
                initially_deferred=bool(uq.get("initially_deferred", False)),
            )
            for uq in doc.get("unique_constraints", [])
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

    # -- materialized views ------------------------------------------------- #
    # A materialized view stores its SELECT text here and a snapshot of rows in a
    # backing collection of the same name (queried through schema-on-read
    # reflection); REFRESH recomputes the snapshot.

    def put_matview(self, db: str, name: str, definition: str, populated: bool = True) -> None:
        self._storage.delete_matching(db, MATVIEW_COLLECTION, {"_id": name})
        self._storage.insert(
            db,
            MATVIEW_COLLECTION,
            [{"_id": name, "matview": name, "definition": definition, "populated": populated}],
        )

    def get_matview(self, db: str, name: str) -> str | None:
        docs = self._storage.find_matching(db, MATVIEW_COLLECTION, {"_id": name}, limit=1)
        return docs[0]["definition"] if docs else None

    def matview_populated(self, db: str, name: str) -> bool:
        """Whether a materialized view holds data. A ``WITH NO DATA`` matview is
        unpopulated (not scannable) until its first ``REFRESH``."""
        docs = self._storage.find_matching(db, MATVIEW_COLLECTION, {"_id": name}, limit=1)
        return bool(docs[0].get("populated", True)) if docs else False

    def set_matview_populated(self, db: str, name: str, populated: bool) -> None:
        definition = self.get_matview(db, name)
        if definition is not None:
            self.put_matview(db, name, definition, populated=populated)

    def drop_matview(self, db: str, name: str) -> bool:
        return self._storage.delete_matching(db, MATVIEW_COLLECTION, {"_id": name}) > 0

    def list_matviews(self, db: str) -> list[str]:
        docs = self._storage.find_matching(db, MATVIEW_COLLECTION, {})
        return sorted(d["matview"] for d in docs)

    # -- sequences ---------------------------------------------------------- #
    # A sequence is a persisted monotonic counter (``CREATE SEQUENCE`` and the
    # implicit sequence behind a SERIAL column). State lives in a per-db
    # ``__sql_sequences__`` collection, one doc per sequence.

    def create_sequence(
        self,
        db: str,
        name: str,
        *,
        start: int = 1,
        increment: int = 1,
        minvalue: int | None = None,
        maxvalue: int | None = None,
        cycle: bool = False,
        owned_by: str | None = None,
    ) -> None:
        """Create (or overwrite) a sequence's persisted state. ``owned_by`` is the
        ``table.column`` a SERIAL/identity sequence belongs to (dropped with it)."""
        self._storage.delete_matching(db, SEQUENCE_COLLECTION, {"_id": name})
        self._storage.insert(
            db,
            SEQUENCE_COLLECTION,
            [
                {
                    "_id": name,
                    "sequence": name,
                    "last_value": start,
                    "start": start,
                    "increment": increment,
                    "min_value": minvalue,
                    "max_value": maxvalue,
                    "cycle": cycle,
                    "is_called": False,
                    "owned_by": owned_by,
                }
            ],
        )

    def get_sequence(self, db: str, name: str) -> dict[str, Any] | None:
        docs = self._storage.find_matching(db, SEQUENCE_COLLECTION, {"_id": name}, limit=1)
        return docs[0] if docs else None

    def sequence_exists(self, db: str, name: str) -> bool:
        return self.get_sequence(db, name) is not None

    def drop_sequence(self, db: str, name: str) -> bool:
        return self._storage.delete_matching(db, SEQUENCE_COLLECTION, {"_id": name}) > 0

    def list_sequences(self, db: str) -> list[str]:
        docs = self._storage.find_matching(db, SEQUENCE_COLLECTION, {})
        return sorted(d["sequence"] for d in docs)

    def sequence_nextval(self, db: str, name: str) -> int:
        """Advance ``name`` and return its new value. The first ``nextval`` returns
        the sequence's ``start``; subsequent calls add ``increment`` (raising on
        overflow past ``max_value`` unless ``cycle``, when it wraps to the bound)."""
        doc = self.get_sequence(db, name)
        if doc is None:
            raise errors.SQLError("42P01", f'relation "{name}" does not exist')
        inc = int(doc.get("increment", 1))
        if not doc.get("is_called", False):
            # First draw returns the current value as-is — ``start`` for a fresh
            # sequence, or the value a ``setval(…, false)`` planted.
            value = int(doc["last_value"])
        else:
            value = int(doc["last_value"]) + inc
            bound = doc.get("max_value") if inc > 0 else doc.get("min_value")
            if bound is not None and (value > bound if inc > 0 else value < bound):
                if not doc.get("cycle", False):
                    raise errors.SQLError(
                        "2200H", f'nextval: reached maximum value of sequence "{name}"'
                    )
                other = doc.get("min_value") if inc > 0 else doc.get("max_value")
                value = other if other is not None else int(doc.get("start", 1))
        self._storage.update_matching(
            db,
            SEQUENCE_COLLECTION,
            {"_id": name},
            {"$set": {"last_value": value, "is_called": True}},
        )
        return value

    # -- roles -------------------------------------------------------------- #
    # SQL-level roles (``CREATE ROLE`` / ``CREATE USER``). Recorded for reflection
    # (``pg_roles`` / ``\du``) and DDL acceptance; these are distinct from the
    # wire server's SCRAM auth users (which remain constructor config) — a SQL
    # role does not by itself grant a login credential.

    # Default role attributes, overlaid by the CREATE/ALTER option list.
    ROLE_DEFAULTS = {
        "login": False,
        "superuser": False,
        "createdb": False,
        "createrole": False,
        "inherit": True,
        "replication": False,
        "connlimit": -1,
        "password_set": False,
    }

    def put_role(self, db: str, name: str, attrs: dict[str, Any]) -> None:
        merged = {**self.ROLE_DEFAULTS, **attrs}
        self._storage.delete_matching(db, ROLE_COLLECTION, {"_id": name})
        self._storage.insert(db, ROLE_COLLECTION, [{"_id": name, "role": name, **merged}])

    def get_role(self, db: str, name: str) -> dict[str, Any] | None:
        docs = self._storage.find_matching(db, ROLE_COLLECTION, {"_id": name}, limit=1)
        return docs[0] if docs else None

    def role_exists(self, db: str, name: str) -> bool:
        return self.get_role(db, name) is not None

    def drop_role(self, db: str, name: str) -> bool:
        return self._storage.delete_matching(db, ROLE_COLLECTION, {"_id": name}) > 0

    def list_roles(self, db: str) -> list[str]:
        docs = self._storage.find_matching(db, ROLE_COLLECTION, {})
        return sorted(d["role"] for d in docs)

    def sequence_setval(self, db: str, name: str, value: int, is_called: bool = True) -> int:
        """Set ``name``'s current value. With ``is_called`` (default) the next
        ``nextval`` returns ``value + increment``; without it, ``nextval`` returns
        ``value`` itself (Postgres ``setval(seq, v, false)`` semantics)."""
        if not self.sequence_exists(db, name):
            raise errors.SQLError("42P01", f'relation "{name}" does not exist')
        self._storage.update_matching(
            db,
            SEQUENCE_COLLECTION,
            {"_id": name},
            {"$set": {"last_value": value, "is_called": is_called}},
        )
        return value

    # -- enum types --------------------------------------------------------- #
    # ``CREATE TYPE name AS ENUM (...)`` — the label list is stored here; an
    # enum-typed column validates its value against it and reflects via pg_enum.

    def create_enum(self, db: str, name: str, labels: list[str]) -> None:
        self._storage.delete_matching(db, ENUM_COLLECTION, {"_id": name})
        self._storage.insert(
            db, ENUM_COLLECTION, [{"_id": name, "enum": name, "labels": list(labels)}]
        )

    def get_enum(self, db: str, name: str) -> dict[str, Any] | None:
        docs = self._storage.find_matching(db, ENUM_COLLECTION, {"_id": name}, limit=1)
        return docs[0] if docs else None

    def enum_exists(self, db: str, name: str) -> bool:
        return self.get_enum(db, name) is not None

    def drop_enum(self, db: str, name: str) -> bool:
        return self._storage.delete_matching(db, ENUM_COLLECTION, {"_id": name}) > 0

    def alter_enum_add_value(
        self,
        db: str,
        name: str,
        label: str,
        *,
        before: str | None = None,
        after: str | None = None,
        if_not_exists: bool = False,
    ) -> None:
        """``ALTER TYPE name ADD VALUE 'label' [BEFORE/AFTER 'other']`` — insert a
        new label into the enum's ordered label list. Position defaults to the end;
        ``BEFORE`` / ``AFTER`` place it relative to an existing label. Raises
        ``42704`` if the enum (or a referenced neighbour) doesn't exist, and
        ``42710`` if the label already exists (unless ``if_not_exists``)."""
        doc = self.get_enum(db, name)
        if doc is None:
            raise errors.SQLError("42704", f'type "{name}" does not exist')
        labels = list(doc["labels"])
        if label in labels:
            if if_not_exists:
                return
            raise errors.SQLError("42710", f'enum label "{label}" already exists in type "{name}"')
        if before is not None or after is not None:
            neighbour = before if before is not None else after
            if neighbour not in labels:
                raise errors.SQLError(
                    "42704", f'"{neighbour}" is not an existing enum label of type "{name}"'
                )
            idx = labels.index(neighbour)
            labels.insert(idx if before is not None else idx + 1, label)
        else:
            labels.append(label)
        self._storage.delete_matching(db, ENUM_COLLECTION, {"_id": name})
        self._storage.insert(db, ENUM_COLLECTION, [{"_id": name, "enum": name, "labels": labels}])

    def list_enums(self, db: str) -> list[str]:
        docs = self._storage.find_matching(db, ENUM_COLLECTION, {})
        return sorted(d["enum"] for d in docs)

    # -- domain types ------------------------------------------------------- #
    # ``CREATE DOMAIN name AS base [DEFAULT expr] [NOT NULL] [CHECK (...)]`` — a
    # named base type carrying its own constraints. A domain-typed column stores
    # as the base tag, enforces the domain's NOT NULL / CHECK on write, and
    # reflects via pg_type (``typtype = 'd'``).

    def create_domain(
        self,
        db: str,
        name: str,
        base_tag: str,
        *,
        not_null: bool = False,
        checks: list[dict[str, Any]] | None = None,
        has_default: bool = False,
        default: Any = None,
    ) -> None:
        self._storage.delete_matching(db, DOMAIN_COLLECTION, {"_id": name})
        self._storage.insert(
            db,
            DOMAIN_COLLECTION,
            [
                {
                    "_id": name,
                    "domain": name,
                    "base_tag": base_tag,
                    "not_null": bool(not_null),
                    "checks": list(checks or []),
                    "has_default": bool(has_default),
                    "default": default,
                }
            ],
        )

    def get_domain(self, db: str, name: str) -> dict[str, Any] | None:
        docs = self._storage.find_matching(db, DOMAIN_COLLECTION, {"_id": name}, limit=1)
        return docs[0] if docs else None

    def domain_exists(self, db: str, name: str) -> bool:
        return self.get_domain(db, name) is not None

    def drop_domain(self, db: str, name: str) -> bool:
        return self._storage.delete_matching(db, DOMAIN_COLLECTION, {"_id": name}) > 0

    def list_domains(self, db: str) -> list[str]:
        docs = self._storage.find_matching(db, DOMAIN_COLLECTION, {})
        return sorted(d["domain"] for d in docs)

    def alter_sequence(self, db: str, name: str, changes: dict[str, Any]) -> None:
        """Apply ``ALTER SEQUENCE`` changes. ``changes`` may set ``increment`` /
        ``min_value`` / ``max_value`` / ``cycle`` / ``start``, and a ``restart``
        key (the value to restart at, or None → the sequence's ``start``) resets
        ``last_value`` with ``is_called`` cleared so the next ``nextval`` returns
        it. Raises ``42P01`` if the sequence doesn't exist."""
        doc = self.get_sequence(db, name)
        if doc is None:
            raise errors.SQLError("42P01", f'relation "{name}" does not exist')
        update: dict[str, Any] = {}
        for key in ("increment", "min_value", "max_value", "cycle", "start"):
            if key in changes:
                update[key] = changes[key]
        if "restart" in changes:
            restart = changes["restart"]
            update["last_value"] = (
                int(restart)
                if restart is not None
                else int(changes.get("start", doc.get("start", 1)))
            )
            update["is_called"] = False
        if update:
            self._storage.update_matching(db, SEQUENCE_COLLECTION, {"_id": name}, {"$set": update})
