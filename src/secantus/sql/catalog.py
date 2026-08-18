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
from weakref import WeakKeyDictionary

from secantus.sql import errors

CATALOG_COLLECTION = "__sql_catalog__"
VIEW_COLLECTION = "__sql_views__"
MATVIEW_COLLECTION = "__sql_matviews__"
TRIGGER_COLLECTION = "__sql_triggers__"
SEQUENCE_COLLECTION = "__sql_sequences__"

#: How many sequence values one persisted write pre-allocates (see the
#: sequences section of ``Catalog``). 128 cuts the per-``nextval`` storage
#: writes by ~99% on bulk SERIAL ingest while keeping the worst-case
#: restart gap small.
SEQUENCE_ALLOC_BATCH = 128

#: Pre-allocated sequence values, server-wide per storage object:
#: ``storage -> {(db, name): [pending (reversed for pop), last_handed]}``.
#: ``last_handed`` lets invalidation write the true position back to the
#: stored doc, so ``setval`` / ``ALTER`` / reflection-after-mutation see
#: CACHE-1-observable state, not the batch high-water mark. Weakly keyed so a
#: closed storage's cache dies with it; guarded by the executor's
#: statement-write lock (every reader/writer of an entry holds it).
_SEQ_ALLOC_CACHE: WeakKeyDictionary[Any, dict[tuple[str, str], list[Any]]] = WeakKeyDictionary()


def _precompute_sequence_values(doc: dict[str, Any], count: int) -> list[int]:
    """The next up-to-``count`` values ``doc``'s sequence will yield, applying
    the same first-draw / increment / bound / cycle rules one step at a time.
    Stops early at an uncycled bound (returning what fits); raises 2200H only
    when not even one value is available."""
    inc = int(doc.get("increment", 1))
    values: list[int] = []
    last = int(doc["last_value"])
    called = bool(doc.get("is_called", False))
    for _ in range(count):
        if not called:
            # First draw returns the current value as-is — ``start`` for a
            # fresh sequence, or the value a ``setval(…, false)`` planted.
            value = last
            called = True
        else:
            value = last + inc
            bound = doc.get("max_value") if inc > 0 else doc.get("min_value")
            if bound is not None and (value > bound if inc > 0 else value < bound):
                if not doc.get("cycle", False):
                    if values:
                        break
                    raise errors.SQLError(
                        "2200H",
                        f'nextval: reached maximum value of sequence "{doc["_id"]}"',
                    )
                other = doc.get("min_value") if inc > 0 else doc.get("max_value")
                value = other if other is not None else int(doc.get("start", 1))
        values.append(value)
        last = value
    return values


ROLE_COLLECTION = "__sql_roles__"
ROLE_MEMBER_COLLECTION = "__sql_role_members__"
GRANT_COLLECTION = "__sql_grants__"
SCHEMA_COLLECTION = "__sql_schemas__"
# Per-database GUC defaults set by ``ALTER DATABASE … SET <guc>`` — applied to
# every NEW session at connect (PG semantics), never to already-open ones.
DB_SETTINGS_COLLECTION = "__sql_db_settings__"
ENUM_COLLECTION = "__sql_enums__"
# The enum oid counter lives outside ENUM_COLLECTION so list_enums stays a plain scan.
ENUM_META_COLLECTION = "__sql_enum_meta__"
# Bases for minted user-type pg_type oids (see ``Catalog.enum_type_oids`` /
# ``domain_type_oids`` / ``composite_type_oids``).
ENUM_TYPE_OID_BASE = 65000
DOMAIN_TYPE_OID_BASE = 66000
COMPOSITE_TYPE_OID_BASE = 67000
RANGE_TYPE_OID_BASE = 69000
RANGE_TYPE_COLLECTION = "__sql_range_types__"
# A user type's paired array-type oid (pg_type ``typarray``) is its own oid
# plus this offset — derived, never stored, and clear of every other minted-oid
# base (functions 65000+, domains 66000+, composites 67000+). Reporting a real
# ``typarray`` is load-bearing: psycopg's TypeInfo keys array registrations on
# it, and an ``array_oid`` of 0 lets client code touch oid 0 = INVALID_OID —
# psycopg's own test suite pops the global unknown-oid fallback loader that way.
USER_TYPE_ARRAY_OID_OFFSET = 100_000
DOMAIN_COLLECTION = "__sql_domains__"
COMPOSITE_COLLECTION = "__sql_composites__"
FUNCTION_COLLECTION = "__sql_functions__"
POLICY_COLLECTION = "__sql_policies__"
# Direct DML against the pg_description virtual relation (suppressed derived
# rows + extra rows) — see ``virtual._pg_description``.
DESCRIPTION_DELTA_COLLECTION = "__sql_description_delta__"
# User-defined operators (CREATE OPERATOR) — registered so the DDL round-trips;
# expression evaluation does not consult them.
OPERATOR_COLLECTION = "__sql_operators__"
# COMMENT ON INDEX comments, keyed by index name (resolved to the index
# relation's oid at pg_description read time — minted oids can reshuffle).
INDEX_COMMENT_COLLECTION = "__sql_index_comments__"
# Per-relation ACL materialization state. A relation the user never GRANTed /
# REVOKEd on has NO row here and reports ``relacl`` NULL — real PG's "default"
# ACL, which a driver reads as the owner holding every privilege implicitly.
# The first grant/revoke *materializes* the ACL: a row appears whose
# ``owner_privs`` is the owner's retained privilege set (``REVOKE ALL FROM
# <owner>`` empties it), and per-grantee privileges come from GRANT_COLLECTION.
RELATION_ACL_COLLECTION = "__sql_relation_acl__"
RLS_COLLECTION = "__sql_rls__"
COLUMN_GRANT_COLLECTION = "__sql_column_grants__"

#: Every catalog collection DDL can touch — snapshotted together before a DDL
#: statement inside a savepoint so ``ROLLBACK TO SAVEPOINT`` reverts the schema
#: change (a CREATE TYPE / CREATE TABLE / … undone by the enclosing savepoint).
ALL_CATALOG_COLLECTIONS = (
    CATALOG_COLLECTION,
    VIEW_COLLECTION,
    MATVIEW_COLLECTION,
    SEQUENCE_COLLECTION,
    ROLE_COLLECTION,
    ROLE_MEMBER_COLLECTION,
    GRANT_COLLECTION,
    SCHEMA_COLLECTION,
    DB_SETTINGS_COLLECTION,
    ENUM_COLLECTION,
    ENUM_META_COLLECTION,
    RANGE_TYPE_COLLECTION,
    DOMAIN_COLLECTION,
    COMPOSITE_COLLECTION,
    FUNCTION_COLLECTION,
    TRIGGER_COLLECTION,
    POLICY_COLLECTION,
    RLS_COLLECTION,
    COLUMN_GRANT_COLLECTION,
    DESCRIPTION_DELTA_COLLECTION,
    OPERATOR_COLLECTION,
    INDEX_COMMENT_COLLECTION,
    RELATION_ACL_COLLECTION,
)


def fold_type_name(name: str) -> str:
    """Normalize a user-type name as Postgres resolves identifiers: each dotted
    part folds to lowercase unless double-quoted (a quoted part keeps its case,
    quotes stripped). ``'StrTestEnum'`` → ``strtestenum``; ``'"CamelCaseEnum"'``
    → ``CamelCaseEnum``. Shared by ``::regtype`` resolution and cast targets."""

    def _fold(part: str) -> str:
        p = part.strip()
        if len(p) >= 2 and p.startswith('"') and p.endswith('"'):
            return p[1:-1]
        return p.lower()

    return ".".join(_fold(part) for part in name.strip().split("."))


def _ser_composite_fields(fields: Any) -> list | None:
    """Serialize composite fields ``(name, tag, subfields)`` to plain lists (bson-
    safe), recursing into a nested composite field's own subfields. Tolerates
    legacy two-element ``(name, tag)`` entries."""
    if fields is None:
        return None
    out = []
    for f in fields:
        name, tag = f[0], f[1]
        sub = f[2] if len(f) > 2 else None
        out.append([name, tag, _ser_composite_fields(sub)])
    return out


def _deser_composite_fields(raw: Any) -> tuple | None:
    """Inverse of ``_ser_composite_fields`` — plain lists back to nested tuples."""
    if raw is None:
        return None
    out = []
    for f in raw:
        name, tag = f[0], f[1]
        sub = f[2] if len(f) > 2 else None
        out.append((name, tag, _deser_composite_fields(sub)))
    return tuple(out)


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
    # A non-literal column DEFAULT — the rendered SQL of an expression default
    # (``now()``, ``gen_random_uuid()``, arithmetic, …). Evaluated per omitted row
    # at INSERT via ``scalar.evaluate``. None for a literal / no / sequence default.
    default_expr: str | None = None
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
    # The composite type name for a column declared with a ``CREATE TYPE … AS
    # (…)`` type. The value is stored as a subdocument keyed by the type's field
    # names; ``composite_fields`` carries ``[[name, type_tag], …]`` (copied from
    # the type at CREATE TABLE) so the INSERT path can map a positional ``ROW(…)``
    # onto the named fields and ``(col).field`` access can type its result.
    composite_type: str | None = None
    composite_fields: tuple[tuple[str, str], ...] | None = None
    # Declared PG type identity for reflection: the oid when it differs from
    # the storage tag's (``varchar``/``bpchar`` fold to the ``text`` tag) and
    # the ``atttypmod`` (``varchar(52)`` → 56, ``numeric(18,5)`` → ((18<<16)|5)+4).
    decl_oid: int | None = None
    typmod: int = -1
    # True for a column declared ``json`` (not ``jsonb``): the value behaviour
    # is identical (both store parsed JSON under type_tag "json") but the wire
    # identity differs — RowDescription/COPY report oid 114, whose binary form
    # has no jsonb version byte.
    json_plain: bool = False


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
    comment: str | None = None  # COMMENT ON CONSTRAINT


@dataclass(frozen=True)
class CheckConstraint:
    """A declared CHECK constraint. ``expression`` is the rendered SQL of the
    predicate (e.g. ``age >= 0``); it is enforced on write."""

    name: str
    expression: str
    comment: str | None = None  # COMMENT ON CONSTRAINT


@dataclass(frozen=True)
class UniqueConstraint:
    """A declared UNIQUE constraint over one or more columns, enforced on write."""

    name: str
    columns: tuple[str, ...]
    deferrable: bool = False
    initially_deferred: bool = False
    comment: str | None = None  # COMMENT ON CONSTRAINT
    # An ``EXCLUDE (col WITH =, ...)`` constraint: equality-only exclusion is
    # unique enforcement with a different violation (23P01, PG's
    # exclusion_violation) and reflected contype 'x'.
    exclusion: bool = False


@dataclass(frozen=True)
class ExprIndex:
    """An expression (functional) index — ``CREATE INDEX … ((a + b))``. The
    expression is materialised into a hidden storage doc field (``field``, a
    ``__``-prefixed key that is *not* a table column, so it never appears in
    ``SELECT *`` / reflection) recomputed on every write; the storage B-tree indexes
    that field. Queries whose WHERE / ORDER BY name the same expression are rewritten
    onto ``field`` so the index lights up. ``expr_sql`` is the normalised SQL of the
    indexed expression (the match key); ``type_tag`` is its inferred result type."""

    name: str
    expr_sql: str
    field: str
    type_tag: str
    direction: int = 1


@dataclass
class TableDef:
    name: str
    collection: str
    columns: list[Column]
    # Reflected tables have a sampled, schema-on-read shape: any column name
    # resolves to a field of the same name, and an un-sampled column reads as
    # the permissive ``any`` type rather than erroring.
    reflected: bool = False
    # CREATE TEMP TABLE — reflected in error diagnostics (schema pg_temp_1).
    temp: bool = False
    foreign_keys: list[ForeignKey] = field(default_factory=list)
    check_constraints: list[CheckConstraint] = field(default_factory=list)
    unique_constraints: list[UniqueConstraint] = field(default_factory=list)
    comment: str | None = None  # COMMENT ON TABLE (reflected via pg_description)
    # Declared PK constraint name (``CONSTRAINT <name> PRIMARY KEY (…)``);
    # reflection surfaces it instead of the synthesized ``<table>_pkey``.
    pk_name: str | None = None
    pk_comment: str | None = None  # COMMENT ON CONSTRAINT for the PK
    # The PK constraint's declared column order (``PRIMARY KEY (name, id)``),
    # when it differs from table-column order — reflection surfaces it; the
    # storage ``_id`` mapping is untouched.
    pk_column_order: tuple[str, ...] | None = None
    # Expression (functional) indexes. Their hidden ``field`` keys resolve like
    # columns (so a query rewritten onto one plans through the normal index path)
    # but are NOT in ``columns`` (so ``SELECT *`` / reflection never surface them).
    expr_indexes: list[ExprIndex] = field(default_factory=list)

    def ordered_pk_columns(self) -> list[Column]:
        """PK columns in the constraint's declared order (reflection order);
        falls back to table-column order when no explicit order was declared."""
        cols = self.pk_columns
        if not self.pk_column_order:
            return cols
        by_name = {c.name: c for c in cols}
        ordered = [by_name[n] for n in self.pk_column_order if n in by_name]
        return ordered if len(ordered) == len(cols) else cols

    def pk_constraint_name(self) -> str:
        """The PK constraint's reflected name: the declared one, else the
        Postgres default ``<table>_pkey``."""
        return self.pk_name or f"{self.name}_pkey"

    def column(self, name: str) -> Column | None:
        for c in self.columns:
            if c.name == name:
                return c
        return None

    def _expr_index_field(self, name: str) -> ExprIndex | None:
        for ei in self.expr_indexes:
            if ei.field == name:
                return ei
        return None

    def field_for(self, name: str) -> str:
        c = self.column(name)
        if c is not None:
            return c.field
        if self._expr_index_field(name) is not None:
            return name
        if self.reflected:
            return name
        raise errors.undefined_column(name)

    def type_for(self, name: str) -> str:
        c = self.column(name)
        if c is not None:
            return c.type_tag
        ei = self._expr_index_field(name)
        if ei is not None:
            return ei.type_tag
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
                "default_expr": c.default_expr,
                "comment": c.comment,
                "sequence": c.sequence,
                "identity": c.identity,
                "enum_type": c.enum_type,
                "domain_type": c.domain_type,
                "generated": c.generated,
                "composite_type": c.composite_type,
                "composite_fields": _ser_composite_fields(c.composite_fields),
                "json_plain": c.json_plain,
                "decl_oid": c.decl_oid,
                "typmod": c.typmod,
            }
            for c in table.columns
        ],
        "comment": table.comment,
        "pk_name": table.pk_name,
        "pk_comment": table.pk_comment,
        "pk_column_order": list(table.pk_column_order) if table.pk_column_order else None,
        "temp": table.temp,
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
                "comment": fk.comment,
            }
            for fk in table.foreign_keys
        ],
        "check_constraints": [
            {"name": ck.name, "expression": ck.expression, "comment": ck.comment}
            for ck in table.check_constraints
        ],
        "unique_constraints": [
            {
                "name": uq.name,
                "columns": list(uq.columns),
                "deferrable": uq.deferrable,
                "initially_deferred": uq.initially_deferred,
                "comment": uq.comment,
                "exclusion": uq.exclusion,
            }
            for uq in table.unique_constraints
        ],
        "expr_indexes": [
            {
                "name": ei.name,
                "expr_sql": ei.expr_sql,
                "field": ei.field,
                "type_tag": ei.type_tag,
                "direction": ei.direction,
            }
            for ei in table.expr_indexes
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
                default_expr=c.get("default_expr"),
                comment=c.get("comment"),
                sequence=c.get("sequence"),
                identity=c.get("identity"),
                enum_type=c.get("enum_type"),
                domain_type=c.get("domain_type"),
                generated=c.get("generated"),
                composite_type=c.get("composite_type"),
                composite_fields=_deser_composite_fields(c.get("composite_fields")),
                json_plain=bool(c.get("json_plain", False)),
                decl_oid=c.get("decl_oid"),
                typmod=int(c.get("typmod", -1)),
            )
            for c in doc["columns"]
        ],
        comment=doc.get("comment"),
        pk_name=doc.get("pk_name"),
        pk_comment=doc.get("pk_comment"),
        pk_column_order=(tuple(doc["pk_column_order"]) if doc.get("pk_column_order") else None),
        temp=bool(doc.get("temp", False)),
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
                comment=fk.get("comment"),
            )
            for fk in doc.get("foreign_keys", [])
        ],
        check_constraints=[
            CheckConstraint(name=ck["name"], expression=ck["expression"], comment=ck.get("comment"))
            for ck in doc.get("check_constraints", [])
        ],
        unique_constraints=[
            UniqueConstraint(
                name=uq["name"],
                columns=tuple(uq["columns"]),
                deferrable=bool(uq.get("deferrable", False)),
                initially_deferred=bool(uq.get("initially_deferred", False)),
                comment=uq.get("comment"),
                exclusion=bool(uq.get("exclusion", False)),
            )
            for uq in doc.get("unique_constraints", [])
        ],
        expr_indexes=[
            ExprIndex(
                name=ei["name"],
                expr_sql=ei["expr_sql"],
                field=ei["field"],
                type_tag=ei["type_tag"],
                direction=int(ei.get("direction", 1)),
            )
            for ei in doc.get("expr_indexes", [])
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

    def put_view(
        self, db: str, name: str, definition: str, check_option: str | None = None
    ) -> None:
        self._storage.delete_matching(db, VIEW_COLLECTION, {"_id": name})
        self._storage.insert(
            db,
            VIEW_COLLECTION,
            [
                {
                    "_id": name,
                    "view": name,
                    "definition": definition,
                    "check_option": check_option,
                }
            ],
        )

    def get_view(self, db: str, name: str) -> str | None:
        docs = self._storage.find_matching(db, VIEW_COLLECTION, {"_id": name}, limit=1)
        return docs[0]["definition"] if docs else None

    def get_view_check_option(self, db: str, name: str) -> str | None:
        """A view's ``WITH CHECK OPTION`` mode (``"LOCAL"`` / ``"CASCADED"``), or
        None if the view has no check option (or doesn't exist)."""
        docs = self._storage.find_matching(db, VIEW_COLLECTION, {"_id": name}, limit=1)
        return docs[0].get("check_option") if docs else None

    def drop_view(self, db: str, name: str) -> bool:
        return self._storage.delete_matching(db, VIEW_COLLECTION, {"_id": name}) > 0

    # -- triggers ----------------------------------------------------------- #
    # BEFORE INSERT FOR EACH ROW triggers (the supported shape). Keyed by
    # (table, name) — PG trigger names are per-table.

    def put_trigger(self, db: str, doc: dict[str, Any]) -> None:
        key = f"{doc['table']}::{doc['name']}"
        self._storage.delete_matching(db, TRIGGER_COLLECTION, {"_id": key})
        self._storage.insert(db, TRIGGER_COLLECTION, [{**doc, "_id": key}])

    def trigger_exists(self, db: str, table: str, name: str) -> bool:
        key = f"{table}::{name}"
        return bool(self._storage.find_matching(db, TRIGGER_COLLECTION, {"_id": key}))

    def triggers_for_table(self, db: str, table: str) -> list[dict[str, Any]]:
        return sorted(
            self._storage.find_matching(db, TRIGGER_COLLECTION, {"table": table}),
            key=lambda t: t.get("name", ""),
        )

    def drop_triggers_for_table(self, db: str, table: str) -> None:
        self._storage.delete_matching(db, TRIGGER_COLLECTION, {"table": table})

    def drop_trigger(self, db: str, table: str, name: str) -> bool:
        key = f"{table}::{name}"
        if not self._storage.find_matching(db, TRIGGER_COLLECTION, {"_id": key}):
            return False
        self._storage.delete_matching(db, TRIGGER_COLLECTION, {"_id": key})
        return True

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
    #
    # ``nextval`` allocates in BATCHES: one storage write persists the batch's
    # high-water mark, then values are handed out from memory (guarded by the
    # same statement-write lock that already serializes ``nextval``). This is
    # PG's own ``CACHE`` mechanism applied server-side — without it every
    # SERIAL insert paid a full read + durable-update transaction, which
    # dominated bulk-ingest profiles (a 100k-row ``COPY`` into a SERIAL table
    # spent ~75% of its samples inside ``nextval``'s update). Consequences,
    # both PG-faithful for a cached sequence: values are gapless while the
    # server runs (the cache is server-wide, not per-backend), and a restart
    # resumes from the persisted high-water mark, skipping unhanded values —
    # exactly the gap PG's ``CACHE``/crash semantics produce. Every other
    # sequence write path (create / drop / setval / ALTER) invalidates the
    # cached run so its effect is immediate.

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
        self._invalidate_sequence_cache(db, name)
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
        self._invalidate_sequence_cache(db, name)
        return self._storage.delete_matching(db, SEQUENCE_COLLECTION, {"_id": name}) > 0

    def list_sequences(self, db: str) -> list[str]:
        docs = self._storage.find_matching(db, SEQUENCE_COLLECTION, {})
        return sorted(d["sequence"] for d in docs)

    def sequence_nextval(self, db: str, name: str) -> int:
        """Advance ``name`` and return its new value. The first ``nextval`` returns
        the sequence's ``start``; subsequent calls add ``increment`` (raising on
        overflow past ``max_value`` unless ``cycle``, when it wraps to the bound).

        Serialized under the storage's statement-write lock: the read-advance-
        persist below spans two storage calls, and a bare ``SELECT nextval(…)``
        runs outside the DML executors, so two connections could otherwise draw
        the same value. Lazy import — executor already imports this module."""
        from secantus.sql.executor import _write_lock

        with _write_lock(self._storage):
            return self._sequence_nextval_locked(db, name)

    def _sequence_nextval_locked(self, db: str, name: str) -> int:
        cache = _SEQ_ALLOC_CACHE.setdefault(self._storage, {})
        entry = cache.get((db, name))
        if entry is not None and entry[0]:
            value = entry[0].pop()
            entry[1] = value
            return value
        doc = self.get_sequence(db, name)
        if doc is None:
            raise errors.SQLError("42P01", f'relation "{name}" does not exist')
        values = _precompute_sequence_values(doc, SEQUENCE_ALLOC_BATCH)
        # One write persists the whole batch's high-water mark BEFORE any
        # value is handed out, so a crash can only skip values, never repeat.
        self._storage.update_matching(
            db,
            SEQUENCE_COLLECTION,
            {"_id": name},
            {"$set": {"last_value": values[-1], "is_called": True}},
        )
        first = values[0]
        rest = values[1:]
        rest.reverse()
        cache[(db, name)] = [rest, first]
        return first

    def _invalidate_sequence_cache(self, db: str, name: str) -> None:
        """Retire ``name``'s pre-allocated run, writing the last value actually
        handed out back to the stored doc first — so ``setval`` / ``ALTER`` /
        drop-and-recreate proceed from the sequence's true position, exactly as
        an uncached (CACHE 1) sequence would. Every sequence write path other
        than ``nextval`` itself must call this before its own write."""
        cache = _SEQ_ALLOC_CACHE.get(self._storage)
        entry = cache.pop((db, name), None) if cache is not None else None
        if entry is not None:
            self._storage.update_matching(
                db,
                SEQUENCE_COLLECTION,
                {"_id": name},
                {"$set": {"last_value": entry[1], "is_called": True}},
            )

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

    # -- Role membership (GRANT <role> TO <member>) ------------------------- #
    # ``GRANT readers TO alice`` records that ``alice`` is a member of ``readers``.
    # One document per ``(role, member)``; ``admin_option`` marks WITH ADMIN OPTION
    # (the member may grant the role onward). Reflected via ``pg_auth_members``.

    @staticmethod
    def _member_key(role: str, member: str) -> str:
        return f"{role}\x00{member}"

    def grant_role_membership(
        self, db: str, role: str, member: str, *, admin_option: bool = False
    ) -> None:
        """Record that ``member`` is a member of ``role`` (idempotent). Once set, the
        admin option is only cleared by REVOKE ADMIN OPTION FOR — a re-grant without
        WITH ADMIN OPTION keeps an existing one (Postgres semantics)."""
        key = self._member_key(role, member)
        existing = self._storage.find_matching(db, ROLE_MEMBER_COLLECTION, {"_id": key}, limit=1)
        admin = bool(admin_option) or (bool(existing) and existing[0].get("admin_option", False))
        self._storage.delete_matching(db, ROLE_MEMBER_COLLECTION, {"_id": key})
        self._storage.insert(
            db,
            ROLE_MEMBER_COLLECTION,
            [{"_id": key, "role": role, "member": member, "admin_option": admin}],
        )

    def revoke_role_membership(self, db: str, role: str, member: str) -> bool:
        """Remove ``member`` from ``role``. Returns whether a membership existed."""
        key = self._member_key(role, member)
        return self._storage.delete_matching(db, ROLE_MEMBER_COLLECTION, {"_id": key}) > 0

    def revoke_role_admin_option(self, db: str, role: str, member: str) -> bool:
        """REVOKE ADMIN OPTION FOR — clear the admin option but keep the membership.
        Returns whether a membership existed."""
        key = self._member_key(role, member)
        existing = self._storage.find_matching(db, ROLE_MEMBER_COLLECTION, {"_id": key}, limit=1)
        if not existing:
            return False
        self._storage.delete_matching(db, ROLE_MEMBER_COLLECTION, {"_id": key})
        self._storage.insert(
            db,
            ROLE_MEMBER_COLLECTION,
            [{"_id": key, "role": role, "member": member, "admin_option": False}],
        )
        return True

    def list_role_memberships(self, db: str) -> list[dict[str, Any]]:
        """Every ``(role, member, admin_option)`` membership, sorted."""
        docs = self._storage.find_matching(db, ROLE_MEMBER_COLLECTION, {})
        return sorted(
            (
                {
                    "role": d["role"],
                    "member": d["member"],
                    "admin_option": bool(d.get("admin_option", False)),
                }
                for d in docs
            ),
            key=lambda m: (m["role"], m["member"]),
        )

    # -- Table-level privileges (GRANT / REVOKE) ---------------------------- #
    # ``GRANT SELECT ON t TO alice`` / ``REVOKE INSERT ON t FROM bob`` — persisted
    # here, one document per ``(table, grantee)`` carrying the set of privileges
    # that grantee holds on that table. The authz gate (``authz.py``) reads these
    # to allow a data operation a user's Mongo role wouldn't otherwise cover, and
    # ``information_schema.role_table_grants`` / ``has_table_privilege()`` reflect
    # them. The four enforced privileges are SELECT / INSERT / UPDATE / DELETE;
    # PG's other table privileges (TRUNCATE / REFERENCES / TRIGGER) are recorded
    # for reflection fidelity but not enforced (the operations don't exist here).

    # The order PG lists them for ``GRANT ALL`` / ``has_table_privilege`` fidelity.
    TABLE_PRIVILEGES = ("SELECT", "INSERT", "UPDATE", "DELETE", "TRUNCATE", "REFERENCES", "TRIGGER")

    @staticmethod
    def _grant_key(table: str, grantee: str) -> str:
        # NUL-joined so a table or grantee containing a delimiter can't collide.
        return f"{table}\x00{grantee}"

    def grant_table_privileges(
        self,
        db: str,
        table: str,
        grantee: str,
        privileges: list[str],
        *,
        grant_option: bool = False,
    ) -> None:
        """Add ``privileges`` to what ``grantee`` holds on ``table`` (union with any
        existing grant). ``grant_option`` marks the grant as re-grantable."""
        key = self._grant_key(table, grantee)
        existing = self._storage.find_matching(db, GRANT_COLLECTION, {"_id": key}, limit=1)
        held = set(existing[0]["privileges"]) if existing else set()
        held.update(p.upper() for p in privileges)
        option = bool(grant_option) or (bool(existing) and existing[0].get("grant_option", False))
        doc = {
            "_id": key,
            "table": table,
            "grantee": grantee,
            "privileges": [p for p in self.TABLE_PRIVILEGES if p in held],
            "grant_option": option,
        }
        self._storage.delete_matching(db, GRANT_COLLECTION, {"_id": key})
        self._storage.insert(db, GRANT_COLLECTION, [doc])

    def revoke_table_privileges(
        self, db: str, table: str, grantee: str, privileges: list[str]
    ) -> None:
        """Remove ``privileges`` from what ``grantee`` holds on ``table``; the
        grant document is deleted once no privileges remain."""
        key = self._grant_key(table, grantee)
        existing = self._storage.find_matching(db, GRANT_COLLECTION, {"_id": key}, limit=1)
        if not existing:
            return
        held = set(existing[0]["privileges"]) - {p.upper() for p in privileges}
        self._storage.delete_matching(db, GRANT_COLLECTION, {"_id": key})
        if held:
            self._storage.insert(
                db,
                GRANT_COLLECTION,
                [
                    {
                        "_id": key,
                        "table": table,
                        "grantee": grantee,
                        "privileges": [p for p in self.TABLE_PRIVILEGES if p in held],
                        "grant_option": existing[0].get("grant_option", False),
                    }
                ],
            )

    def get_table_grants(self, db: str, table: str) -> list[dict[str, Any]]:
        """Every grant recorded on ``table`` (one dict per grantee)."""
        return self._storage.find_matching(db, GRANT_COLLECTION, {"table": table})

    def list_table_grants(self, db: str) -> list[dict[str, Any]]:
        """Every table grant in ``db`` (for ``information_schema`` reflection)."""
        return self._storage.find_matching(db, GRANT_COLLECTION, {})

    #: PG aclitem privilege letters (pg_class.relacl text form) — the subset a
    #: table can carry, in the order real PG emits them.
    _ACL_LETTERS = (
        ("INSERT", "a"),
        ("SELECT", "r"),
        ("UPDATE", "w"),
        ("DELETE", "d"),
        ("TRUNCATE", "D"),
        ("REFERENCES", "x"),
        ("TRIGGER", "t"),
    )

    def materialize_relation_owner_privileges(
        self, db: str, table: str, owner: str, privileges: list[str] | None
    ) -> None:
        """Record the owner's retained privileges on ``table`` — the act of
        touching the ACL. ``privileges=None`` seeds the owner's full implicit
        set (first GRANT to a third party); a list (possibly empty) is the set
        left after a REVOKE / GRANT that targeted the owner. Presence of the
        row is what flips ``relacl`` from NULL to a materialized array."""
        privs = list(self.TABLE_PRIVILEGES) if privileges is None else list(privileges)
        ordered = [p for p in self.TABLE_PRIVILEGES if p in {x.upper() for x in privs}]
        self._storage.delete_matching(db, RELATION_ACL_COLLECTION, {"_id": table})
        self._storage.insert(
            db, RELATION_ACL_COLLECTION, [{"_id": table, "owner": owner, "owner_privs": ordered}]
        )

    def _relation_acl_state(self, db: str, table: str) -> dict[str, Any] | None:
        docs = self._storage.find_matching(db, RELATION_ACL_COLLECTION, {"_id": table}, limit=1)
        return docs[0] if docs else None

    def relation_acl_text(self, db: str, table: str, owner: str) -> str | None:
        """The ``pg_class.relacl`` text (aclitem[] literal) for ``table``, or
        None when the ACL was never touched (real PG's default → a driver reads
        the owner as implicitly holding everything). Once materialized, emit the
        owner's retained privileges plus every recorded per-grantee grant."""
        grants = self.get_table_grants(db, table)
        state = self._relation_acl_state(db, table)
        if not grants and state is None:
            return None  # untouched — implicit owner privileges
        owner_privs = state["owner_privs"] if state is not None else list(self.TABLE_PRIVILEGES)

        def item(grantee: str, privs: list[str]) -> str:
            held = {p.upper() for p in privs}
            letters = "".join(ch for name, ch in self._ACL_LETTERS if name in held)
            # An empty grantee name is PUBLIC in the aclitem form (``=r/owner``).
            who = "" if grantee.upper() == "PUBLIC" else grantee
            return f"{who}={letters}/{owner}"

        entries: list[str] = []
        if owner_privs:
            entries.append(item(owner, owner_privs))
        for doc in grants:
            if doc["grantee"] == owner:
                continue  # the owner's row is driven by owner_privs above
            entries.append(item(doc["grantee"], doc.get("privileges", [])))
        return "{" + ",".join(entries) + "}"

    def has_table_privilege(self, db: str, table: str, grantees: set[str], privilege: str) -> bool:
        """Whether any identity in ``grantees`` (a user + its role names, plus
        ``PUBLIC``) holds ``privilege`` on ``table`` via a recorded grant."""
        want = privilege.upper()
        for doc in self.get_table_grants(db, table):
            if doc["grantee"] in grantees and want in doc.get("privileges", ()):
                return True
        return False

    # -- Column-level privileges (GRANT SELECT (col) …) --------------------- #
    # Finer-grained than table grants: ``GRANT SELECT (a, b) ON t TO alice``
    # authorizes alice for exactly columns a / b. Persisted per-
    # ``(table, grantee, column)``; the authz gate reads them when a table grant
    # or Mongo role doesn't already cover the statement's columns.

    @staticmethod
    def _col_grant_key(table: str, grantee: str, column: str) -> str:
        return f"{table}\x00{grantee}\x00{column}"

    def grant_column_privileges(
        self, db: str, table: str, grantee: str, column: str, privileges: list[str]
    ) -> None:
        key = self._col_grant_key(table, grantee, column)
        existing = self._storage.find_matching(db, COLUMN_GRANT_COLLECTION, {"_id": key}, limit=1)
        held = set(existing[0]["privileges"]) if existing else set()
        held.update(p.upper() for p in privileges)
        self._storage.delete_matching(db, COLUMN_GRANT_COLLECTION, {"_id": key})
        self._storage.insert(
            db,
            COLUMN_GRANT_COLLECTION,
            [
                {
                    "_id": key,
                    "table": table,
                    "grantee": grantee,
                    "column": column,
                    "privileges": [p for p in self.TABLE_PRIVILEGES if p in held],
                }
            ],
        )

    def revoke_column_privileges(
        self, db: str, table: str, grantee: str, column: str, privileges: list[str]
    ) -> None:
        key = self._col_grant_key(table, grantee, column)
        existing = self._storage.find_matching(db, COLUMN_GRANT_COLLECTION, {"_id": key}, limit=1)
        if not existing:
            return
        held = set(existing[0]["privileges"]) - {p.upper() for p in privileges}
        self._storage.delete_matching(db, COLUMN_GRANT_COLLECTION, {"_id": key})
        if held:
            self._storage.insert(
                db,
                COLUMN_GRANT_COLLECTION,
                [
                    {
                        "_id": key,
                        "table": table,
                        "grantee": grantee,
                        "column": existing[0]["column"],
                        "privileges": [p for p in self.TABLE_PRIVILEGES if p in held],
                    }
                ],
            )

    def get_column_grants(self, db: str, table: str) -> list[dict[str, Any]]:
        return self._storage.find_matching(db, COLUMN_GRANT_COLLECTION, {"table": table})

    def list_column_grants(self, db: str) -> list[dict[str, Any]]:
        return self._storage.find_matching(db, COLUMN_GRANT_COLLECTION, {})

    def has_column_privilege(
        self, db: str, table: str, grantees: set[str], column: str, privilege: str
    ) -> bool:
        """Whether any identity in ``grantees`` holds ``privilege`` on
        ``table.column`` — via a column grant *or* a whole-table grant."""
        want = privilege.upper()
        if self.has_table_privilege(db, table, grantees, want):
            return True
        for doc in self.get_column_grants(db, table):
            if (
                doc["column"] == column
                and doc["grantee"] in grantees
                and want in doc.get("privileges", ())
            ):
                return True
        return False

    # -- Row-level security (RLS) ------------------------------------------- #
    # ``ALTER TABLE t ENABLE ROW LEVEL SECURITY`` records a per-table flag in
    # ``__sql_rls__``; ``CREATE POLICY`` records a per-``(table, name)`` policy in
    # ``__sql_policies__``. The authz gate (``rls.py``) injects a policy's USING
    # predicate into the query WHERE and validates its WITH CHECK on writes.

    def set_rls(self, db: str, table: str, *, enabled: bool, forced: bool = False) -> None:
        self._storage.delete_matching(db, RLS_COLLECTION, {"_id": table})
        self._storage.insert(
            db,
            RLS_COLLECTION,
            [{"_id": table, "table": table, "enabled": bool(enabled), "forced": bool(forced)}],
        )

    def get_rls(self, db: str, table: str) -> dict[str, Any]:
        docs = self._storage.find_matching(db, RLS_COLLECTION, {"_id": table}, limit=1)
        if docs:
            return {"enabled": bool(docs[0].get("enabled")), "forced": bool(docs[0].get("forced"))}
        return {"enabled": False, "forced": False}

    def create_policy(self, db: str, doc: dict[str, Any]) -> None:
        """Persist a policy. ``doc`` carries ``name`` / ``table`` / ``command`` /
        ``roles`` / ``permissive`` / ``using`` / ``check``. Errors 42710 on a
        duplicate ``(table, name)``."""
        key = f"{doc['table']}\x00{doc['name']}"
        if self._storage.find_matching(db, POLICY_COLLECTION, {"_id": key}, limit=1):
            raise errors.SQLError(
                "42710", f'policy "{doc["name"]}" for table "{doc["table"]}" already exists'
            )
        self._storage.insert(db, POLICY_COLLECTION, [{"_id": key, **doc}])

    def drop_policy(self, db: str, table: str, name: str) -> bool:
        return (
            self._storage.delete_matching(db, POLICY_COLLECTION, {"_id": f"{table}\x00{name}"}) > 0
        )

    def get_policies(self, db: str, table: str) -> list[dict[str, Any]]:
        return self._storage.find_matching(db, POLICY_COLLECTION, {"table": table})

    def list_policies(self, db: str) -> list[dict[str, Any]]:
        return self._storage.find_matching(db, POLICY_COLLECTION, {})

    # -- SQL functions ------------------------------------------------------ #
    # ``CREATE FUNCTION name(params) RETURNS t AS $$ body $$ LANGUAGE sql`` —
    # persisted here (keyed ``name/nargs`` so overloads by arity coexist) and
    # invoked by the scalar evaluator when a call resolves no builtin.

    @staticmethod
    def _function_key(name: str, nargs: int) -> str:
        return f"{name.lower()}/{nargs}"

    def put_function(self, db: str, doc: dict[str, Any]) -> None:
        key = self._function_key(doc["name"], doc["nargs"])
        self._storage.delete_matching(db, FUNCTION_COLLECTION, {"_id": key})
        self._storage.insert(db, FUNCTION_COLLECTION, [{"_id": key, **doc}])

    @staticmethod
    def _operator_key(name: str, left: str, right: str) -> str:
        return f"{name}/{left.lower()}/{right.lower()}"

    def put_operator(self, db: str, doc: dict[str, Any]) -> None:
        key = self._operator_key(doc["name"], doc["leftarg"], doc["rightarg"])
        self._storage.delete_matching(db, OPERATOR_COLLECTION, {"_id": key})
        self._storage.insert(db, OPERATOR_COLLECTION, [{"_id": key, **doc}])

    def get_operator(self, db: str, name: str, left: str, right: str) -> dict[str, Any] | None:
        key = self._operator_key(name, left, right)
        docs = self._storage.find_matching(db, OPERATOR_COLLECTION, {"_id": key}, limit=1)
        return docs[0] if docs else None

    def drop_operator(self, db: str, name: str, left: str, right: str) -> bool:
        key = self._operator_key(name, left, right)
        return self._storage.delete_matching(db, OPERATOR_COLLECTION, {"_id": key}) > 0

    def get_function(self, db: str, name: str, nargs: int) -> dict[str, Any] | None:
        key = self._function_key(name, nargs)
        docs = self._storage.find_matching(db, FUNCTION_COLLECTION, {"_id": key}, limit=1)
        return docs[0] if docs else None

    def function_exists(self, db: str, name: str, nargs: int) -> bool:
        return self.get_function(db, name, nargs) is not None

    def drop_function(self, db: str, name: str, nargs: int) -> bool:
        key = self._function_key(name, nargs)
        return self._storage.delete_matching(db, FUNCTION_COLLECTION, {"_id": key}) > 0

    def list_functions(self, db: str) -> list[dict[str, Any]]:
        return self._storage.find_matching(db, FUNCTION_COLLECTION, {})

    def sequence_setval(self, db: str, name: str, value: int, is_called: bool = True) -> int:
        """Set ``name``'s current value. With ``is_called`` (default) the next
        ``nextval`` returns ``value + increment``; without it, ``nextval`` returns
        ``value`` itself (Postgres ``setval(seq, v, false)`` semantics)."""
        if not self.sequence_exists(db, name):
            raise errors.SQLError("42P01", f'relation "{name}" does not exist')
        self._invalidate_sequence_cache(db, name)
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
            db,
            ENUM_COLLECTION,
            [{"_id": name, "enum": name, "labels": list(labels), "oid": self._mint_enum_oid(db)}],
        )

    def _mint_enum_oid(self, db: str) -> int:
        return self._mint_user_type_oid(db, "oid_counter", ENUM_TYPE_OID_BASE, ENUM_COLLECTION)

    def _mint_user_type_oid(self, db: str, counter_key: str, base: int, collection: str) -> int:
        """Allocate the next pg_type oid for a user-type kind — monotonic, never
        reused, like a real server's oid counter. Positional minting (base +
        sorted-name index) is NOT an option: a later CREATE/DROP TYPE would
        renumber every other type of the kind, and a client that registered a
        loader for the old oid would silently decode a different type."""
        docs = self._storage.find_matching(db, ENUM_META_COLLECTION, {"_id": counter_key}, limit=1)
        if docs:
            oid = docs[0]["next"]
        else:
            # First mint (or a pre-counter database): start above every oid
            # already in use so legacy positionally-minted types keep theirs.
            existing = self._storage.find_matching(db, collection, {})
            taken = [d["oid"] for d in existing if "oid" in d]
            oid = max([base + len(existing) - 1, *taken]) + 1 if existing else base
        self._storage.delete_matching(db, ENUM_META_COLLECTION, {"_id": counter_key})
        self._storage.insert(db, ENUM_META_COLLECTION, [{"_id": counter_key, "next": oid + 1}])
        return oid

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
        # Rewrite the whole doc, preserving the minted oid — ALTER TYPE must not
        # renumber the type out from under a client's registered loader.
        self._storage.insert(db, ENUM_COLLECTION, [{**doc, "labels": labels}])

    def list_enums(self, db: str) -> list[str]:
        docs = self._storage.find_matching(db, ENUM_COLLECTION, {})
        return sorted(d["enum"] for d in docs)

    def enum_type_oids(self, db: str) -> dict[str, int]:
        """The minted pg_type oid per enum. The single mint shared by pg_type /
        pg_enum / pg_attribute reflection AND the wire layer's RowDescription —
        both sides must agree or a client that registered the type from the
        catalog (psycopg's ``EnumInfo.fetch``) won't recognise result columns.
        Oids are stored on the enum doc at CREATE TYPE; the positional form is
        only a fallback for docs written before oids were persisted."""
        docs = self._storage.find_matching(db, ENUM_COLLECTION, {})
        out: dict[str, int] = {}
        for i, doc in enumerate(sorted(docs, key=lambda d: d["enum"])):
            out[doc["enum"]] = doc.get("oid", ENUM_TYPE_OID_BASE + i)
        return out

    # -- user schemas -------------------------------------------------------- #
    # ``CREATE SCHEMA name`` — a namespace for user-declared types (and, later,
    # tables). Types created in a schema are stored under their dotted
    # qualified name ("testschema.testcomp"); pg_namespace / pg_type surface
    # the schema with a minted namespace oid.

    # -- per-database GUC defaults (ALTER DATABASE … SET) -------------------- #

    def set_db_setting(self, db: str, name: str, value: str | None) -> None:
        """Set (or, with ``value=None``, reset) a database-level GUC default."""
        self._storage.delete_matching(db, DB_SETTINGS_COLLECTION, {"_id": name})
        if value is not None:
            self._storage.insert(db, DB_SETTINGS_COLLECTION, [{"_id": name, "value": value}])

    def db_settings(self, db: str) -> dict[str, str]:
        return {
            d["_id"]: d["value"]
            for d in self._storage.find_matching(db, DB_SETTINGS_COLLECTION, {})
        }

    def create_schema(self, db: str, name: str) -> None:
        if not self.schema_exists(db, name):
            self._storage.insert(db, SCHEMA_COLLECTION, [{"_id": name, "schema": name}])

    def schema_exists(self, db: str, name: str) -> bool:
        return bool(self._storage.find_matching(db, SCHEMA_COLLECTION, {"_id": name}, limit=1))

    def drop_schema(self, db: str, name: str) -> bool:
        return self._storage.delete_matching(db, SCHEMA_COLLECTION, {"_id": name}) > 0

    def list_schemas(self, db: str) -> list[str]:
        docs = self._storage.find_matching(db, SCHEMA_COLLECTION, {})
        return sorted(d["schema"] for d in docs)

    # -- composite types ---------------------------------------------------- #
    # ``CREATE TYPE name AS (field type, …)`` — the ordered ``(field, type_tag)``
    # list is stored here. A composite-typed column stores its value as a
    # subdocument keyed by the field names and reflects via pg_type
    # (``typtype = 'c'``) + pg_attribute.

    def create_composite(self, db: str, name: str, fields: list[tuple]) -> None:
        self._storage.delete_matching(db, COMPOSITE_COLLECTION, {"_id": name})
        self._storage.insert(
            db,
            COMPOSITE_COLLECTION,
            [
                {
                    "_id": name,
                    "composite": name,
                    "fields": _ser_composite_fields(fields),
                    "oid": self._mint_user_type_oid(
                        db, "composite_oid_counter", COMPOSITE_TYPE_OID_BASE, COMPOSITE_COLLECTION
                    ),
                }
            ],
        )

    # -- user range types ---------------------------------------------------- #
    # ``CREATE TYPE name AS RANGE (subtype = X)``. Postgres auto-creates the
    # companion multirange type (``testrange`` → ``testmultirange``); both get
    # allocation-stable minted oids.

    @staticmethod
    def multirange_name_for(name: str) -> str:
        head, sep, tail = name.rpartition("range")
        return f"{head}multirange{tail}" if sep else f"{name}_multirange"

    def create_range_type(self, db: str, name: str, subtype_tag: str) -> None:
        self._storage.delete_matching(db, RANGE_TYPE_COLLECTION, {"_id": name})
        oid = self._mint_user_type_oid(
            db, "range_oid_counter", RANGE_TYPE_OID_BASE, RANGE_TYPE_COLLECTION
        )
        mr_oid = self._mint_user_type_oid(
            db, "range_oid_counter", RANGE_TYPE_OID_BASE, RANGE_TYPE_COLLECTION
        )
        self._storage.insert(
            db,
            RANGE_TYPE_COLLECTION,
            [
                {
                    "_id": name,
                    "range": name,
                    "subtype_tag": subtype_tag,
                    "oid": oid,
                    "multirange": self.multirange_name_for(name),
                    "multirange_oid": mr_oid,
                }
            ],
        )

    def get_range_type(self, db: str, name: str) -> dict[str, Any] | None:
        """The range-type doc by its range OR companion multirange name."""
        docs = self._storage.find_matching(db, RANGE_TYPE_COLLECTION, {"_id": name}, limit=1)
        if docs:
            return docs[0]
        docs = self._storage.find_matching(db, RANGE_TYPE_COLLECTION, {"multirange": name}, limit=1)
        return docs[0] if docs else None

    def range_type_exists(self, db: str, name: str) -> bool:
        return self.get_range_type(db, name) is not None

    def drop_range_type(self, db: str, name: str) -> bool:
        return self._storage.delete_matching(db, RANGE_TYPE_COLLECTION, {"_id": name}) > 0

    def list_range_types(self, db: str) -> list[dict[str, Any]]:
        return self._storage.find_matching(db, RANGE_TYPE_COLLECTION, {})

    def composite_type_oids(self, db: str) -> dict[str, int]:
        """Minted pg_type oid per composite — allocation-stable (see
        ``enum_type_oids``); positional fallback for pre-oid docs."""
        docs = self._storage.find_matching(db, COMPOSITE_COLLECTION, {})
        out: dict[str, int] = {}
        for i, doc in enumerate(sorted(docs, key=lambda d: d["composite"])):
            out[doc["composite"]] = doc.get("oid", COMPOSITE_TYPE_OID_BASE + i)
        return out

    def get_composite(self, db: str, name: str) -> list[tuple] | None:
        docs = self._storage.find_matching(db, COMPOSITE_COLLECTION, {"_id": name}, limit=1)
        if not docs:
            return None
        return list(_deser_composite_fields(docs[0]["fields"]) or ())

    def composite_exists(self, db: str, name: str) -> bool:
        return self.get_composite(db, name) is not None

    def drop_composite(self, db: str, name: str) -> bool:
        return self._storage.delete_matching(db, COMPOSITE_COLLECTION, {"_id": name}) > 0

    def list_composites(self, db: str) -> list[str]:
        docs = self._storage.find_matching(db, COMPOSITE_COLLECTION, {})
        return sorted(d["composite"] for d in docs)

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
        typmod: int = -1,
        base_oid: int | None = None,
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
                    # The base type's declared typmod / oid (``varbit(3)`` →
                    # typmod 3), surfaced as the domain's pg_type.typtypmod /
                    # typbasetype so getColumns reports COLUMN_SIZE.
                    "typmod": int(typmod),
                    "base_oid": base_oid,
                    "oid": self._mint_user_type_oid(
                        db, "domain_oid_counter", DOMAIN_TYPE_OID_BASE, DOMAIN_COLLECTION
                    ),
                }
            ],
        )

    def domain_type_oids(self, db: str) -> dict[str, int]:
        """Minted pg_type oid per domain — allocation-stable (see
        ``enum_type_oids``); positional fallback for pre-oid docs."""
        docs = self._storage.find_matching(db, DOMAIN_COLLECTION, {})
        out: dict[str, int] = {}
        for i, doc in enumerate(sorted(docs, key=lambda d: d["domain"])):
            out[doc["domain"]] = doc.get("oid", DOMAIN_TYPE_OID_BASE + i)
        return out

    def set_domain_comment(self, db: str, name: str, comment: str | None) -> bool:
        doc = self.get_domain(db, name)
        if doc is None:
            return False
        doc = {k: v for k, v in doc.items() if k != "_id"}
        doc["comment"] = comment
        self._storage.delete_matching(db, DOMAIN_COLLECTION, {"_id": name})
        self._storage.insert(db, DOMAIN_COLLECTION, [{"_id": name, **doc}])
        return True

    def set_index_comment(self, db: str, name: str, comment: str | None) -> None:
        self._storage.delete_matching(db, INDEX_COMMENT_COLLECTION, {"_id": name})
        if comment is not None:
            self._storage.insert(db, INDEX_COMMENT_COLLECTION, [{"_id": name, "comment": comment}])

    def index_comments(self, db: str) -> dict[str, str]:
        return {
            d["_id"]: d["comment"]
            for d in self._storage.find_matching(db, INDEX_COMMENT_COLLECTION, {})
        }

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

    def update_domain(self, db: str, name: str, doc: dict[str, Any]) -> None:
        """Overwrite a domain's stored definition (for ALTER DOMAIN). ``doc`` is a
        full replacement, keyed under the given ``name``."""
        self._storage.delete_matching(db, DOMAIN_COLLECTION, {"_id": name})
        self._storage.insert(db, DOMAIN_COLLECTION, [{**doc, "_id": name, "domain": name}])

    def alter_sequence(self, db: str, name: str, changes: dict[str, Any]) -> None:
        """Apply ``ALTER SEQUENCE`` changes. ``changes`` may set ``increment`` /
        ``min_value`` / ``max_value`` / ``cycle`` / ``start``, and a ``restart``
        key (the value to restart at, or None → the sequence's ``start``) resets
        ``last_value`` with ``is_called`` cleared so the next ``nextval`` returns
        it. Raises ``42P01`` if the sequence doesn't exist."""
        doc = self.get_sequence(db, name)
        if doc is None:
            raise errors.SQLError("42P01", f'relation "{name}" does not exist')
        self._invalidate_sequence_cache(db, name)
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
