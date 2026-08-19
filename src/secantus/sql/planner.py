"""Translate a parsed SQL statement into operations over the Mongo engines.

This is the heart of the spike: it walks a ``sqlglot`` AST and lowers it to the
exact structures ``Storage`` already consumes — a ``query``-style filter dict, a
sort spec, an ``update``-style ``$set`` document, or a list of documents to
insert. The executor then just hands those to ``Storage``. Because WHERE becomes
a real Mongo filter, SQL inherits the storage layer's index acceleration and
matching semantics with no separate execution engine.

Only the P0 subset is handled; anything outside it raises a
``feature_not_supported`` SQLError rather than silently diverging.
"""

from __future__ import annotations

import contextvars
import dataclasses
import datetime as _dt
import functools
import json
import logging
import math as _math
import re
import threading
from collections import OrderedDict
from collections.abc import Callable
from dataclasses import dataclass, field, replace
from decimal import Decimal as _Decimal
from typing import Any

import sqlglot
from sqlglot import exp

from secantus.paths import set_path
from secantus.sql import errors, ranges, subms, typemap
from secantus.sql.catalog import (
    CheckConstraint,
    Column,
    ExprIndex,
    ForeignKey,
    TableDef,
    UniqueConstraint,
)

# sqlglot logs a WARNING when it falls back to parsing ``SHOW`` / ``RESET`` as a
# generic ``Command`` node — which is exactly how we consume them. Quiet it so
# the server log isn't spammed for statements we handle on purpose.
logging.getLogger("sqlglot").setLevel(logging.ERROR)

# ---------------------------------------------------------------------------
# Plan objects — ready-to-execute structures over Storage.
# ---------------------------------------------------------------------------


@dataclass
class CreateTablePlan:
    table: TableDef
    if_not_exists: bool
    # Sequences to auto-create alongside the table (SERIAL columns). Each is
    # ``{"name", "column", "increment", "start"}`` — the executor creates them.
    sequences: list[dict[str, Any]] = field(default_factory=list)


@dataclass
class DropTablePlan:
    name: str
    if_exists: bool


@dataclass
class AlterTablePlan:
    name: str
    if_exists: bool
    actions: list[Any]  # the raw sqlglot action nodes, applied by the executor


@dataclass
class CreateIndexPlan:
    collection: str
    name: str
    key_spec: dict[str, int]
    unique: bool
    if_not_exists: bool
    # A partial-index predicate (``CREATE INDEX … WHERE …``) lowered to a Mongo
    # filter, passed to storage as ``partialFilterExpression``. None = full index.
    partial_filter: dict[str, Any] | None = None
    # An expression (functional) index — ``CREATE INDEX … ((expr))``. Carries the
    # ``ExprIndex`` metadata to register on the table (its hidden ``field`` is the
    # key indexed by ``key_spec``) and the raw expression SQL to backfill/maintain.
    expr_index: Any = None  # catalog.ExprIndex | None
    # ``CREATE INDEX … INCLUDE (cols)`` — covering columns, stored as metadata
    # only (a surrogate needs no physical INCLUDE payload) and reflected via
    # pg_index's indnkeyatts/indkey split.
    include: list[str] = field(default_factory=list)


@dataclass
class DropIndexPlan:
    name: str
    if_exists: bool


@dataclass
class OnConflict:
    """An ``ON CONFLICT`` clause lowered for execution.

    ``conflict_fields`` are the storage field names of the conflict target
    (``ON CONFLICT (a, b)``); empty when none was given — a bare
    ``ON CONFLICT DO NOTHING`` matches *any* unique conflict, which the executor
    handles by inserting and swallowing the duplicate-key error. For
    ``action == "update"``, ``set_exprs`` is the list of
    ``(field, type_tag, raw expr)`` SET assignments — evaluated per conflicting
    row with ``EXCLUDED`` bound to the proposed insert row and the target table
    bound to the existing row — and ``where`` is an optional predicate that gates
    the update."""

    action: str  # "nothing" | "update"
    conflict_fields: list[str]
    set_exprs: list[tuple[str, str, Any]] = field(default_factory=list)
    where: Any = None


@dataclass
class InsertPlan:
    table: TableDef
    docs: list[dict[str, Any]]
    returning: list[tuple[str, Column, Any]] | None = None
    on_conflict: OnConflict | None = None
    # An auto-updatable view's ``WITH CHECK OPTION`` predicate (an sqlglot
    # expression over base columns): every inserted row must satisfy it or the
    # write raises ``44000``. None for a direct table write / a view without the
    # option.
    check_option: Any = None


@dataclass
class ConstantSelectPlan:
    # A FROM-less ``SELECT <expr>, ...`` — no storage access. The headline P1 case
    # (``SELECT 1``), ``SELECT version()``, and constant expressions (``SELECT
    # 1 + 1``). ``emit`` is False when a constant ``WHERE`` evaluates false, so the
    # result has the column shape but zero rows.
    columns: list[tuple[str, str, Any]]  # (out_name, type_tag, python_value)
    emit: bool = True
    # Per-column RowDescription oid overrides, parallel to ``columns`` (None =
    # derive from the tag). Carries a ``'ok'::mood`` cast's enum oid — the tag
    # stays ``text`` (the value form IS the label text) but the descriptor must
    # report the minted enum oid or a client's registered loader won't fire.
    pg_oids: list[int | None] = field(default_factory=list)
    # Per-column PG type modifiers, parallel to ``columns`` (-1 = none).
    # ``select null::varchar(42)`` describes with typmod 46 like real PG.
    typmods: list[int] = field(default_factory=list)


@dataclass
class SelectPlan:
    table: TableDef
    filter: dict[str, Any]
    # ORDER BY as (field_path, direction, nulls_first); realized by a
    # Postgres-semantics Python sort in the executor.
    order: list[tuple[str, int, bool]]
    limit: int
    skip: int
    out_columns: list[tuple[str, Column]] = field(default_factory=list)
    count_star: bool = False
    count_alias: str = "count"
    # For an ORDER BY field that is an enum column: field_path -> the enum's
    # declared label list, so the executor sorts by declared order not lexically.
    enum_orders: dict[str, list[str]] = field(default_factory=dict)
    # ORDER BY field paths that are citext columns: the executor folds their string
    # values to lower case before comparing, so the sort is case-insensitive.
    citext_orders: set[str] = field(default_factory=set)


@dataclass
class CorrelatedSelectPlan:
    """A single-table SELECT whose WHERE references the outer row (EXISTS /
    correlated subquery), so it can't lower to a pushdown Mongo filter — the
    executor evaluates ``where`` per candidate row."""

    table: TableDef
    where: Any  # exp.Expression — the raw WHERE predicate
    out_columns: list[tuple[str, Column]] = field(default_factory=list)
    order: list[tuple[str, int, bool]] = field(default_factory=list)
    limit: int = 0
    skip: int = 0
    count_star: bool = False
    count_alias: str = "count"
    outer_alias: str | None = None
    enum_orders: dict[str, list[str]] = field(default_factory=dict)


@dataclass
class UpdatePlan:
    table: TableDef
    filter: dict[str, Any]
    update: dict[str, Any]
    returning: list[tuple[str, Column, Any]] | None = None
    # True when the SET changes a primary-key column, so the executor must re-key
    # (delete + re-insert under the new ``_id``) rather than update in place.
    rekey: bool = False
    # ``SET col = <expr>`` targets whose RHS is a per-row expression (arithmetic, a
    # column reference, ``||``, a function …) rather than a literal — each a
    # ``(storage_field, type_tag, expr_node)`` evaluated against the old row by the
    # executor. Empty for a pure-literal UPDATE (the fast bulk ``$set`` path).
    computed: list[tuple[str, str, Any]] = field(default_factory=list)
    # An auto-updatable view's ``WITH CHECK OPTION`` predicate (an sqlglot
    # expression over base columns): every updated row's post-image must satisfy it
    # or the write raises ``44000``. None for a direct table write.
    check_option: Any = None


@dataclass
class DeletePlan:
    table: TableDef
    filter: dict[str, Any]
    returning: list[tuple[str, Column, Any]] | None = None


Plan = CreateTablePlan | DropTablePlan | InsertPlan | SelectPlan | UpdatePlan | DeletePlan


# ---------------------------------------------------------------------------
# Literal / column extraction
# ---------------------------------------------------------------------------


def _literal(node: exp.Expression) -> Any:
    """Extract a Python value from a literal-ish AST node."""
    if isinstance(node, exp.Paren):
        return _literal(node.this)
    if isinstance(node, exp.Cast):
        # ``'x'::varchar`` / ``CAST($1 AS SMALLINT)`` — drivers (and SQLAlchemy's
        # reflection) annotate values with a target type. Honour a *numeric* cast
        # so a text-bound param (extended protocol decodes ``$1`` as a string)
        # compares numerically rather than as a string (Mongo orders numbers
        # before strings, so ``attnum > '0'`` would be wrongly false).
        return _coerce_cast(_literal(node.this), node.to)
    if isinstance(node, exp.Neg):
        inner = _literal(node.this)
        if inner is None:
            return None  # - NULL is NULL (``- CAST(NULL AS REAL)``)
        if isinstance(inner, dict) and "interval" in inner:
            from secantus.sql import intervals as _intervals

            return _intervals.neg(inner)
        return typemap.negate(inner)
    if isinstance(node, exp.Null):
        return None
    if isinstance(node, exp.Boolean):
        return bool(node.this)
    if isinstance(node, exp.Array):  # ARRAY[1, 2, 3] -> [1, 2, 3]
        return [_literal(e) for e in node.expressions]
    if isinstance(node, exp.Literal):
        if node.is_string:
            return node.this
        return typemap.number_literal(node.this)
    if isinstance(node, exp.BitString):  # ``B'1010'`` -> the canonical '0'/'1' string
        return str(node.this)
    if isinstance(node, exp.ByteString):
        # ``E'…'`` escape-string literal (psycopg's ClientCursor emits it for
        # any string containing a backslash) — sqlglot's postgres dialect lexes
        # it as a ByteString and keeps the escapes raw.
        from secantus.sql.scalar import _unescape_estring

        return _unescape_estring(str(node.this))
    if isinstance(node, exp.Interval):  # ``interval '1 day'`` -> an interval subdoc
        from secantus.sql import intervals as _intervals

        raw = _literal(node.this) if node.this is not None else ""
        unit = node.args.get("unit")
        if unit is not None:
            return _intervals.from_unit(float(raw), unit.name)
        return _intervals.parse(str(raw))
    if getattr(exp, "Uuid", None) is not None and isinstance(node, exp.Uuid):
        from secantus.sql import uuidtype as _uuidtype

        return _uuidtype.generate()
    if isinstance(node, exp.Anonymous) and str(node.this).lower() in (
        "uuid_generate_v4",
        "uuid_generate_v1",
    ):
        from secantus.sql import uuidtype as _uuidtype

        return _uuidtype.generate()
    if isinstance(node, exp.Anonymous) and str(node.this).lower() == "row":
        # ``ROW(a, b, …)`` composite constructor -> a positional list; the INSERT
        # path maps it onto a composite column's named fields.
        return [_literal(e) for e in node.expressions]
    if isinstance(node, exp.Anonymous) and str(node.this).lower() in typemap._RANGE_TAGS:
        # ``int4range(lo, hi [, bounds])`` -> a normalised range subdocument.
        from secantus.sql import ranges as _ranges

        tag = str(node.this).lower()
        elem, _discrete = _ranges.RANGE_TYPES[tag]
        args = [_literal(e) for e in node.expressions]
        lo = typemap.coerce(args[0], elem) if len(args) > 0 and args[0] is not None else None
        hi = typemap.coerce(args[1], elem) if len(args) > 1 and args[1] is not None else None
        bounds = str(args[2]) if len(args) > 2 and args[2] is not None else "[)"
        return _ranges.make_range(lo, hi, bounds, tag)
    if isinstance(node, exp.Anonymous) and str(node.this).lower() in typemap._MULTIRANGE_TAGS:
        # ``int4multirange(r1, r2, …)`` -> a coalesced multirange subdocument.
        from secantus.sql import ranges as _ranges

        members = [_literal(e) for e in node.expressions]
        return _ranges.make_multirange([m for m in members if m is not None])
    if isinstance(node, exp.Anonymous) and str(node.this).lower() in (
        "to_tsvector",
        "to_tsquery",
        "plainto_tsquery",
        "phraseto_tsquery",
        "websearch_to_tsquery",
    ):
        # Full-text constructors -> a tsvector / tsquery subdocument (a two-arg form
        # passes the fixed text-search config first, which we ignore).
        from secantus.sql import fts as _fts

        fname = str(node.this).lower()
        text = _literal(node.expressions[-1]) if node.expressions else None
        if text is None:
            return None
        if fname == "to_tsvector":
            return _fts.to_tsvector(str(text))
        if fname == "plainto_tsquery":
            return _fts.plainto_tsquery(str(text))
        if fname == "phraseto_tsquery":
            return _fts.phraseto_tsquery(str(text))
        if fname == "websearch_to_tsquery":
            return _fts.websearch_to_tsquery(str(text))
        return _fts.to_tsquery(str(text))
    if isinstance(node, exp.Anonymous) and str(node.this).lower() == "to_regtype":
        # ``to_regtype('name')`` resolves a type name to its OID (NULL if unknown).
        # SQLAlchemy's psycopg dialect probes ``t.oid = to_regtype('hstore')`` at
        # connect time; an unknown type must yield NULL → matches no pg_type row.
        arg = node.expressions[0] if node.expressions else None
        return _to_regtype(_literal(arg)) if arg is not None else None
    raise errors.feature_not_supported(f"unsupported value expression: {node.sql()}")


def _to_regtype(name: Any) -> int | None:
    """Map a type name (as ``to_regtype`` takes) to its OID, or None if unknown.

    User-declared types (enum / domain / composite) resolve through the
    planning ``_pipeline_subctx`` catalog when one is live — psycopg's
    ``EnumInfo.fetch`` keys ``WHERE t.oid = to_regtype('mood')`` on it."""
    if not isinstance(name, str):
        return None
    key = name.strip()
    if len(key) >= 2 and key.startswith('"') and key.endswith('"'):
        # psycopg's sql.Identifier spelling ('"text"') — quoted names keep
        # their case; built-ins only match when already lowercase.
        key = key[1:-1]
        if key != key.lower():
            key = None
    if key is not None:
        key = key.lower()
        for tag, typname in typemap.PG_TYPENAME.items():
            if key in (typname, tag, typemap.SQL_TYPE_NAME.get(tag)):
                return typemap.PG_OID.get(tag)
    sub = _pipeline_subctx.get()
    if sub is not None and getattr(sub, "catalog", None) is not None:
        from secantus.sql import virtual

        return virtual.user_type_oid(sub.db, sub.catalog, name)
    return None


# Catalog-relation OIDs for ``'pg_catalog.pg_class'::regclass``-style casts (used
# by SQLAlchemy's get_table_comment join on ``pg_description.classoid``).
_REGCLASS_OIDS = {
    "pg_class": 1259,
    "pg_type": 1247,
    "pg_attribute": 1249,
    "pg_constraint": 2606,
    "pg_namespace": 2615,
    "pg_index": 2610,
    "pg_description": 2609,
    "pg_proc": 1255,
}


def _regclass_oid(value: Any) -> Any:
    """``<name>::regclass`` → the catalog relation's OID (unchanged if unknown)."""
    if not isinstance(value, str):
        return value
    name = value.rsplit(".", 1)[-1].strip('"')
    return _REGCLASS_OIDS.get(name, value)


def _coerce_cast(value: Any, datatype: exp.Expression | None) -> Any:
    """Coerce a value to a Python number when a CAST targets a numeric type.

    Non-numeric casts (varchar/text/etc.) leave the value unchanged — the
    column-type coercion downstream handles those.
    """
    if value is None or datatype is None:
        return value
    if isinstance(datatype, exp.ObjectIdentifier) and str(datatype.this).upper() == "REGCLASS":
        return _regclass_oid(value)
    if isinstance(datatype, exp.ObjectIdentifier) and str(datatype.this).upper() == "REGPROC":
        # ``'pg_catalog.array_in'::regproc`` in a pushdown constant — PG
        # renders search-path-visible functions UNQUALIFIED, which is what
        # pg_type.typinput stores (pgjdbc's is_array probe compares them).
        if isinstance(value, int) and not isinstance(value, bool):
            return value
        return str(value).rsplit(".", 1)[-1]
    if isinstance(datatype, exp.ObjectIdentifier) and str(datatype.this).upper() == "REGTYPE":
        # ``'name'::regtype`` in a pushdown constant — resolve to the type oid
        # (built-ins and, via the planning subctx, user-declared types).
        # Unlike to_regtype(), the cast errors on an unknown name, like PG.
        oid = _to_regtype(value)
        if oid is None:
            raise errors.SQLError("42704", f'type "{value}" does not exist')
        return oid
    tag = typemap.type_tag_for_sql(datatype) if isinstance(datatype, exp.DataType) else None
    try:
        if tag in ("int2", "int4", "int8"):
            return int(value)
        if tag in ("float4", "float8"):
            return float(value)
        if tag == "numeric":
            from decimal import Decimal

            return value if isinstance(value, Decimal) else Decimal(str(value))
        if tag == "timestamptz" and not isinstance(value, _dt.datetime):
            # Resolve to an INSTANT here. A timestamptz-declared bound
            # parameter is substituted as ``CAST('…' AS timestamptz)``, and
            # leaving that as text lost the declared type: storing it into a
            # ``timestamp`` column then applied Postgres' literal rule (drop
            # the offset, keep the wall clock) where Postgres converts the
            # value through the session zone. A JDBC client's timestamps were
            # shifted by the session offset as a result.
            return typemap.coerce(value, "timestamptz")
    except (TypeError, ValueError):
        return value
    return value


def _column_name(node: exp.Expression) -> str:
    if isinstance(node, exp.Column):
        return node.name
    if isinstance(node, exp.Identifier):
        return node.name
    raise errors.feature_not_supported(f"expected a column, got: {node.sql()}")


def _literal_default(node: exp.Expression, tag: str) -> tuple[bool, Any]:
    """A column DEFAULT expression → ``(has_default, coerced_value)``. Only literal
    defaults (number / string / bool / NULL) are stored; a function / expression
    default (e.g. ``now()``) is not modeled — it reads as "no static default"."""
    if isinstance(node, exp.Null):
        return True, None
    if isinstance(node, (exp.Literal, exp.Boolean, exp.Neg)):
        return True, typemap.coerce(_literal(node), tag)
    return False, None


def _column_default(coldef: exp.ColumnDef, tag: str) -> tuple[bool, Any]:
    for c in coldef.args.get("constraints") or []:
        if type(c.kind).__name__ == "DefaultColumnConstraint":
            return _literal_default(c.kind.this, tag)
    return False, None


# SERIAL pseudo-types → the integer tag they store as, plus the sequence increment
# implied. (SERIAL is not a real type — it's an int column with an owned sequence.)
_SERIAL_TAGS = {"SERIAL": "int4", "BIGSERIAL": "int8", "SMALLSERIAL": "int2"}


def _serial_tag(datatype: exp.Expression) -> str | None:
    """The integer type tag a SERIAL/BIGSERIAL/SMALLSERIAL column stores as, or
    None when ``datatype`` isn't a serial pseudo-type."""
    if isinstance(datatype, exp.DataType):
        # ``datatype.this`` is normally a DataType.Type enum, but sqlglot keeps a
        # plain string for some keyword types (``OID``).
        this = datatype.this
        return _SERIAL_TAGS.get(getattr(this, "name", str(this)) if this else "")
    return None


def _enum_type_name(datatype: exp.Expression) -> str | None:
    """The user-defined type name of a column declared with a non-builtin type
    (a candidate ``CREATE TYPE … AS ENUM``), or None. Existence is verified at
    execution time (the planner is storage-free)."""
    if isinstance(datatype, exp.DataType) and datatype.this and datatype.this.name == "USERDEFINED":
        return datatype.sql(dialect="postgres").strip('"')
    return None


def _enum_array_element_name(datatype: exp.Expression) -> str | None:
    """The user-defined element type name of an ``ARRAY`` column declaration
    (``mood[]`` — a candidate enum-array column), or None."""
    if (
        isinstance(datatype, exp.DataType)
        and datatype.this
        and datatype.this.name == "ARRAY"
        and datatype.expressions
    ):
        return _enum_type_name(datatype.expressions[0])
    return None


def _identity_spec(coldef: exp.ColumnDef) -> dict[str, Any] | None:
    """Parse a ``GENERATED { ALWAYS | BY DEFAULT } AS IDENTITY [(START WITH n
    INCREMENT BY n)]`` column constraint into ``{mode, start, increment}``, or
    None. ``mode`` is ``"always"`` (a user value is rejected) or ``"by_default"``
    (like SERIAL)."""
    for c in coldef.args.get("constraints") or []:
        kind = c.kind
        if type(kind).__name__ != "GeneratedAsIdentityColumnConstraint":
            continue
        always = bool(kind.args.get("this"))  # this=True → ALWAYS, False → BY DEFAULT
        start = kind.args.get("start")
        increment = kind.args.get("increment")
        return {
            "mode": "always" if always else "by_default",
            "start": int(typemap.unwrap_numeric(_literal(start))) if start is not None else 1,
            "increment": (
                int(typemap.unwrap_numeric(_literal(increment))) if increment is not None else 1
            ),
        }
    return None


def _generated_expr(coldef: exp.ColumnDef) -> str | None:
    """The rendered SQL of a ``GENERATED ALWAYS AS (expr) STORED`` column's
    expression, or None. (Postgres only supports STORED; VIRTUAL is not a thing.)"""
    for c in coldef.args.get("constraints") or []:
        if type(c.kind).__name__ == "ComputedColumnConstraint" and c.kind.this is not None:
            return c.kind.this.sql(dialect="postgres")
    return None


def _default_sequence(coldef: exp.ColumnDef) -> str | None:
    """The sequence name a column's ``DEFAULT nextval('seq')`` draws from, or None.
    Only the ``nextval('literal')`` form is recognised (regclass cast included)."""
    for c in coldef.args.get("constraints") or []:
        if type(c.kind).__name__ != "DefaultColumnConstraint":
            continue
        expr = c.kind.this
        if isinstance(expr, exp.Cast):  # nextval('s'::regclass)
            expr = expr.this
        if isinstance(expr, exp.Anonymous) and str(expr.this).lower() == "nextval":
            args = expr.expressions
            if args:
                target = args[0]
                if isinstance(target, exp.Cast):
                    target = target.this
                return str(_literal(target))
    return None


def _default_expr(coldef: exp.ColumnDef) -> str | None:
    """The rendered SQL of a *non-literal, non-nextval* column DEFAULT (``now()``,
    ``gen_random_uuid()``, ``CURRENT_TIMESTAMP``, an arithmetic expression, …),
    evaluated per omitted row at INSERT. None when the default is a static literal
    (handled by ``_literal_default``), a ``nextval`` sequence (``_default_sequence``),
    or absent."""
    for c in coldef.args.get("constraints") or []:
        if type(c.kind).__name__ != "DefaultColumnConstraint":
            continue
        node = c.kind.this
        if isinstance(node, (exp.Null, exp.Literal, exp.Boolean, exp.Neg)):
            return None  # static literal — handled by _literal_default
        probe = node.this if isinstance(node, exp.Cast) else node
        if isinstance(probe, exp.Anonymous) and str(probe.this).lower() == "nextval":
            return None  # sequence default — handled by _default_sequence
        return node.sql(dialect="postgres")
    return None


@functools.lru_cache(maxsize=256)
def _parse_default_expr(text: str) -> Any:
    import sqlglot

    return sqlglot.parse_one(text, read="postgres")


def _default_col_scope(node: Any) -> Any:
    # A column DEFAULT can't reference table columns (Postgres rejects it at
    # CREATE TABLE); a stray reference surfaces faithfully rather than silently
    # evaluating to NULL.
    raise errors.SQLError("0A000", "column references in a DEFAULT expression are not supported")


# ---------------------------------------------------------------------------
# WHERE -> Mongo filter
# ---------------------------------------------------------------------------

_CMP_OPS: dict[type, tuple[str, str]] = {
    # exp class -> (operator, operator-when-column-is-on-the-right)
    exp.GT: ("$gt", "$lt"),
    exp.GTE: ("$gte", "$lte"),
    exp.LT: ("$lt", "$gt"),
    exp.LTE: ("$lte", "$gte"),
}


_EXPLICIT_OPERATORS = {
    "~": exp.RegexpLike,
    "~*": exp.RegexpILike,
}


def _rewrite_explicit_operator(node: exp.Operator) -> exp.Expression | None:
    """``a OPERATOR(pg_catalog.~) b`` (psql's ``\\d`` family emits it) → the
    equivalent regex node; a COLLATE wrapper on the pattern is dropped (the
    default collation changes nothing here). Unknown operators return None."""
    op = str(node.args.get("operator") or "").rsplit(".", 1)[-1]
    negated = op.startswith("!")
    cls = _EXPLICIT_OPERATORS.get(op.lstrip("!"))
    if cls is None:
        return None
    rhs = node.expression
    if isinstance(rhs, exp.Collate):
        rhs = rhs.this
    out: exp.Expression = cls(this=node.this, expression=rhs)
    return exp.Not(this=out) if negated else out


def _like_to_regex(pattern: str, escape: str | None = None) -> str:
    """Translate a SQL LIKE pattern to an anchored regex.

    ``%`` -> ``.*`` and ``_`` -> ``.``; every other character is escaped so it
    matches literally. With an ``ESCAPE`` character, ``<esc>X`` matches ``X``
    literally (PG semantics — the escape applies to the next character).
    """
    if escape is not None and len(escape) > 1:
        raise errors.SQLError("22025", "invalid escape string")
    if not escape:
        escape = None  # ``ESCAPE ''`` disables escaping, like PG
    out = ["^"]
    i = 0
    while i < len(pattern):
        ch = pattern[i]
        if escape and ch == escape and i + 1 < len(pattern):
            out.append(re.escape(pattern[i + 1]))
            i += 2
            continue
        if ch == "%":
            out.append(".*")
        elif ch == "_":
            out.append(".")
        else:
            out.append(re.escape(ch))
        i += 1
    out.append("$")
    return "".join(out)


# A resolver maps a column AST node to (mongo_field_path, type_tag). The
# single-table path ignores any table qualifier; the join path uses it to route
# ``alias.column`` to the right side of the lookup.
Resolve = Callable[[exp.Expression], tuple[str, str]]


def table_resolver(table: TableDef) -> Resolve:
    def resolve(node: exp.Expression) -> tuple[str, str]:
        col = _column_name(node)
        return table.field_for(col), table.type_for(col)

    # Stashed so composite ``(col).field`` typing can look up the type's fields.
    resolve.table = table  # type: ignore[attr-defined]
    return resolve


def _field_parts(entry: Any) -> tuple[str, str, Any]:
    """Unpack a composite-field entry into ``(name, tag, subfields)``. ``subfields``
    is None for a scalar field, or the nested field list for a composite field.
    Tolerates legacy two-element ``(name, tag)`` entries."""
    name, tag = entry[0], entry[1]
    sub = entry[2] if len(entry) > 2 else None
    return name, tag, sub


def _composite_walk(node: exp.Expression, resolve: Resolve) -> tuple[str, str, Any] | None:
    """Resolve a (possibly nested) composite access ``((col).a).b`` to
    ``(dotted_path, field_tag, subfields)``. ``field_tag`` is the accessed field's
    declared type (a composite field's tag is its type name); ``subfields`` is that
    field's nested field list when it is itself composite, else None. Returns None
    when ``node`` isn't a composite access or the base isn't a resolvable
    composite."""
    if not (
        isinstance(node, exp.Dot)
        and isinstance(node.this, exp.Paren)
        and isinstance(node.expression, exp.Identifier)
    ):
        return None
    field = node.expression.name
    inner = node.this.this
    if isinstance(inner, exp.Column):
        table = getattr(resolve, "table", None)
        if table is None:
            return None
        col = table.column(_column_name(inner))
        if col is None or col.composite_fields is None:
            return None
        base_path, _ = resolve(inner)
        fields = col.composite_fields
    else:
        parent = _composite_walk(inner, resolve)
        if parent is None or parent[2] is None:
            return None
        base_path, _ptag, fields = parent
    for entry in fields:
        fname, tag, sub = _field_parts(entry)
        if fname == field:
            return f"{base_path}.{field}", tag, sub
    return None


def _composite_field_tag(node: exp.Expression, resolve: Resolve) -> str | None:
    """Output type of a composite field-access ``(col).field`` (or nested
    ``((col).a).b``). A field that is itself composite types as ``composite`` so it
    renders as a record on the wire; a scalar field keeps its declared tag."""
    walked = _composite_walk(node, resolve)
    if walked is None:
        return None
    _path, tag, sub = walked
    return "composite" if sub is not None else tag


# jsonb navigation: ->, ->>, #>, #>> parse to these nodes. ``Scalar`` variants
# (->> / #>>) return text; the others return jsonb.
_JSONB_CLASSES = (exp.JSONExtract, exp.JSONExtractScalar, exp.JSONBExtract, exp.JSONBExtractScalar)
_JSONB_SCALAR = (exp.JSONExtractScalar, exp.JSONBExtractScalar)


def _composite_access_parts(node: exp.Expression) -> tuple[exp.Column, str] | None:
    """A single-level composite field-access ``(col).field`` ->
    ``(inner_column, subfield)``, or None. Requires the parenthesised form
    (``Dot(Paren(Column), Identifier)``) so a schema-qualified ``pg_catalog.x`` Dot
    is never mistaken for field access. Nested accesses (``((col).a).b``) are
    handled by ``_composite_walk``, not here."""
    if not (
        isinstance(node, exp.Dot)
        and isinstance(node.this, exp.Paren)
        and isinstance(node.expression, exp.Identifier)
    ):
        return None
    inner = node.this.this
    return (inner, node.expression.name) if isinstance(inner, exp.Column) else None


def _is_composite_access_shape(node: exp.Expression) -> bool:
    """Shape-only test for a (possibly nested) composite access ``((col).a).b``."""
    if not (
        isinstance(node, exp.Dot)
        and isinstance(node.this, exp.Paren)
        and isinstance(node.expression, exp.Identifier)
    ):
        return False
    inner = node.this.this
    return isinstance(inner, exp.Column) or _is_composite_access_shape(inner)


def _is_field_node(node: exp.Expression) -> bool:
    return isinstance(node, (exp.Column, *_JSONB_CLASSES)) or _is_composite_access_shape(node)


def _json_keys(expr: exp.Expression) -> list[str]:
    """Extract the path keys from a ->/#> right-hand side. Integer subscripts
    (``-> 1`` — a JSON array index) come back as their digit strings; the
    evaluator's array branch converts them."""
    if isinstance(expr, exp.JSONPath):
        return [
            str(p.this)
            for p in expr.expressions
            if isinstance(p, (exp.JSONPathKey, exp.JSONPathSubscript))
        ]
    if isinstance(expr, exp.Literal):
        # ``#> '{a,b}'`` — a Postgres text[] path literal.
        return [k for k in str(expr.this).strip("{}").split(",") if k]
    raise errors.feature_not_supported(f"unsupported jsonb path: {expr.sql()}")


def _field(node: exp.Expression, resolve: Resolve) -> tuple[str, str]:
    """Resolve a column or jsonb-path node to (dotted_field_path, type_tag)."""
    if isinstance(node, exp.Column):
        return resolve(node)
    if isinstance(node, _JSONB_CLASSES):
        base_path, base_tag = _field(node.this, resolve)
        keys = _json_keys(node.expression)
        # ``hstore -> key`` is a flat text lookup: the value lives one level below
        # the ``hstore`` tag wrapper (``{"hstore": {...}}``), and the result is text.
        if base_tag == "hstore":
            path = base_path + ".hstore" + ("." + ".".join(keys) if keys else "")
            return path, "text"
        path = base_path + ("." + ".".join(keys) if keys else "")
        return path, ("text" if isinstance(node, _JSONB_SCALAR) else "json")
    walked = _composite_walk(node, resolve)
    if walked is not None:
        # ``(col).field`` / ``((col).a).b`` -> the storage path into the (possibly
        # nested) subdocument; a composite field types as ``composite`` so it
        # renders as a record on the wire.
        path, tag, sub = walked
        return path, ("composite" if sub is not None else (tag or "text"))
    raise errors.feature_not_supported(f"expected a column or jsonb path: {node.sql()}")


def _array_elements(node: exp.Expression) -> list[exp.Expression]:
    """Unwrap an ``ARRAY[...]`` (possibly parenthesised) to its element nodes.
    An array *literal* string (a substituted list parameter — ``= any(%s)``)
    parses into literal element nodes."""
    if isinstance(node, exp.Paren):
        return _array_elements(node.this)
    if isinstance(node, exp.Array):
        return list(node.expressions)
    if isinstance(node, exp.Cast):
        return _array_elements(node.this)
    if isinstance(node, exp.Literal) and node.is_string and str(node.this).startswith("{"):
        try:
            items = typemap._parse_pg_array_literal(str(node.this))
        except ValueError:
            raise errors.feature_not_supported(f"unsupported array operand: {node.sql()}") from None
        out: list[exp.Expression] = []
        for v in items:
            if v is None:
                out.append(exp.Null())
            elif isinstance(v, str) and _NUMERIC_TOKEN_RE.fullmatch(v):
                out.append(exp.Literal.number(v))
            else:
                out.append(exp.Literal.string(str(v)))
        return out
    raise errors.feature_not_supported(f"unsupported array operand: {node.sql()}")


_LITERAL_SENTINEL = object()

# A bare numeric array-literal element (``{1,2}``) — becomes a number node so
# ``id = any('{1,2}')`` compares numerically.
_NUMERIC_TOKEN_RE = re.compile(r"-?\d+(\.\d+)?([eE][+-]?\d+)?")


def _try_literal(node: exp.Expression) -> Any:
    """``_literal(node)`` or the sentinel if it isn't a constant expression."""
    try:
        return _literal(node)
    except errors.SQLError:
        return _LITERAL_SENTINEL


def _is_literalish(node: exp.Expression) -> bool:
    return _try_literal(node) is not _LITERAL_SENTINEL


def _field_literal_pair(
    left: exp.Expression, right: exp.Expression
) -> tuple[exp.Expression, exp.Expression] | None:
    """For ``field OP const`` (either order), return ``(field_node, const_node)``."""
    if _is_field_node(left) and _is_literalish(right):
        return (left, right)
    if _is_field_node(right) and _is_literalish(left):
        return (right, left)
    return None


def _citext_cmp_filter(field: str, mongo_op: str, value: Any) -> dict[str, Any]:
    """Lower a ``citext_col OP literal`` comparison to a case-insensitive Mongo
    filter — both sides folded with ``$toLower`` inside ``$expr`` so equality,
    inequality, and range all compare case-insensitively (citext's defining
    behaviour). ``value`` is a plain Python string (or None)."""
    folded = value.lower() if isinstance(value, str) else value
    return {"$expr": {mongo_op: [{"$toLower": f"${field}"}, folded]}}


# Comparison op -> the aggregation-expression operator used inside ``$expr`` when
# neither side is a constant (column-to-column / arithmetic predicates).
_EXPR_CMP: dict[type, str] = {
    exp.EQ: "$eq",
    exp.NEQ: "$ne",
    exp.GT: "$gt",
    exp.GTE: "$gte",
    exp.LT: "$lt",
    exp.LTE: "$lte",
}
_ARITH_OPS: dict[type, str] = {
    exp.Add: "$add",
    exp.Sub: "$subtract",
    exp.Mul: "$multiply",
    exp.Div: "$divide",
}


def _int_division_operands(node: exp.Expression, resolve: Resolve) -> bool:
    """Whether both ``/`` operands type as integers (→ truncating PG division).
    An unresolvable operand tag counts as non-integer (real division, the safe
    default for schema-on-read shapes)."""
    for operand in (node.this, node.expression):
        try:
            if _infer_scalar_tag(operand, resolve) not in _INT_TAG_ORDER:
                return False
        except errors.SQLError:
            return False
    return True


def _to_agg_expr(node: exp.Expression, resolve: Resolve) -> Any:
    """Lower a scalar WHERE operand to a Mongo aggregation expression for ``$expr``.

    Columns / jsonb paths become ``$field`` refs, arithmetic nests, and constants
    pass through (strings wrapped in ``$literal`` so a leading ``$`` isn't read as
    a field path). Anything else (function calls, etc.) raises — those predicates
    aren't supported yet."""
    if isinstance(node, exp.Paren):
        return _to_agg_expr(node.this, resolve)
    if _is_field_node(node):
        return "$" + _field(node, resolve)[0]
    if isinstance(node, exp.Cast):
        # The cast itself is dropped — the operand's own value is what the
        # aggregation engine compares. That only holds for types the engine
        # models; an ``interval`` has no BSON counterpart at all, so dropping
        # the cast would hand ``$multiply`` / ``$add`` the raw literal text and
        # produce a wrong answer (or a crash). Refuse it, and the WHERE falls
        # back to the scalar evaluator, which does understand intervals.
        if node.to is not None and node.to.sql(dialect="postgres").lower().strip() == "interval":
            raise errors.feature_not_supported("interval arithmetic in a pushed-down predicate")
        return _to_agg_expr(node.this, resolve)
    if isinstance(node, exp.Neg) and not isinstance(node.this, (exp.Literal, exp.Null)):
        # Unary minus over a non-literal (``- col2``) — a negative literal
        # falls through to ``_literal`` instead.
        return {"$multiply": [-1, _to_agg_expr(node.this, resolve)]}
    if type(node) in _ARITH_OPS:
        lowered = {
            _ARITH_OPS[type(node)]: [
                _to_agg_expr(node.this, resolve),
                _to_agg_expr(node.expression, resolve),
            ]
        }
        if isinstance(node, exp.Div) and _int_division_operands(node, resolve):
            # PG integer ``/`` truncates toward zero; Mongo's $divide is real.
            return {"$trunc": [lowered, 0]}
        return lowered
    if isinstance(node, exp.Bracket) and node.expressions:
        idx_node = node.expressions[0]
        if not isinstance(idx_node, exp.Slice):
            # ``arr[i]`` -> ``$arrayElemAt``. sqlglot folds a constant index to
            # 0-based; a runtime (column-bearing) index stays 1-based, so subtract.
            idx_expr = _to_agg_expr(idx_node, resolve)
            if any(True for _ in idx_node.find_all(exp.Column)):
                idx_expr = {"$subtract": [idx_expr, 1]}
            return {"$arrayElemAt": [_to_agg_expr(node.this, resolve), idx_expr]}
    func = _func_to_agg_expr(node, resolve)
    if func is not None:
        return func
    val = _literal(node)
    return {"$literal": val} if isinstance(val, str) else val


# Scalar functions that lower 1:1 to a single-argument Mongo aggregation operator.
_UNARY_FUNC_AGG: dict[type, str] = {
    exp.Lower: "$toLower",
    exp.Upper: "$toUpper",
    exp.Abs: "$abs",
    exp.Floor: "$floor",
    exp.Ceil: "$ceil",
    exp.Length: "$strLenCP",
}


def _func_to_agg_expr(node: exp.Expression, resolve: Resolve) -> Any | None:
    """Lower a scalar *function call* to a Mongo aggregation expression, or ``None``
    when the node isn't a supported function. Used for computed GROUP BY keys (and
    any other place that lowers a scalar to ``$expr``). Only functions the Python
    aggregation engine can evaluate are mapped; anything else returns ``None`` so
    the caller falls through (ultimately a ``feature_not_supported`` for a group key)."""
    op = _UNARY_FUNC_AGG.get(type(node))
    if op is not None:
        return {op: _to_agg_expr(node.this, resolve)}
    if isinstance(node, exp.Round):
        place = _to_agg_expr(node.expression, resolve) if node.expression is not None else 0
        return {"$round": [_to_agg_expr(node.this, resolve), place]}
    if isinstance(node, exp.Mod):
        return {"$mod": [_to_agg_expr(node.this, resolve), _to_agg_expr(node.expression, resolve)]}
    if isinstance(node, exp.Coalesce):
        args = [_to_agg_expr(node.this, resolve)]
        args.extend(_to_agg_expr(a, resolve) for a in (node.expressions or []))
        if len(args) >= 2:
            return {"$ifNull": args}
        return args[0]
    if isinstance(node, exp.DPipe):
        # Postgres ``||`` string concatenation — coerce each side to text.
        return {
            "$concat": [
                {"$toString": _to_agg_expr(node.this, resolve)},
                {"$toString": _to_agg_expr(node.expression, resolve)},
            ]
        }
    return None


# Catalog predicates that are functions of visibility/scope which, on a
# single-node SecantusDB where every relation lives in the default search path,
# are always true. SQLAlchemy's reflection emits these in its catalog WHEREs.
# ``pg_table_is_visible`` is NOT here: a temp relation is invisible to every
# session but its creator (and the reflecting connection is never the creator),
# so it translates to ``relpersistence != 't'`` on the pg_class row instead.
_ALWAYS_TRUE_PREDICATES = {"pg_type_is_visible"}


@dataclass
class SubqueryCtx:
    """Carries what a WHERE subquery needs to evaluate itself (it runs the inner
    SELECT through the engine, so aggregates / WHERE / etc. all work)."""

    storage: Any
    db: str
    catalog: Any
    session: Any


# The single-table pushdown path threads a SubqueryCtx explicitly. The pipeline
# planners (join / GROUP BY / evaluated / DISTINCT) call `_where_filter` from many
# places, so `plan_pipeline_select` publishes the context here for the duration of
# planning and every `_where_filter` picks it up — one set-point, no signature
# churn. Planning-scoped and reset in a finally.
_pipeline_subctx: contextvars.ContextVar[SubqueryCtx | None] = contextvars.ContextVar(
    "pipeline_subctx", default=None
)


def _subquery_select(node: exp.Expression) -> exp.Expression:
    return node.this if isinstance(node, exp.Subquery) else node


def _subquery_has_outer_ref(select: exp.Select) -> bool:
    """Heuristic correlation check: a column qualified with an alias not defined
    inside the subquery itself references the outer query (correlated)."""
    inner: set[str | None] = set()
    from_node = select.find(exp.From)
    if from_node is not None:
        src = from_node.this
        inner.add(src.alias or getattr(src, "name", None))
    for jn in select.args.get("joins") or []:
        src = jn.this
        inner.add(src.alias or getattr(src, "name", None))
    return any(col.table and col.table not in inner for col in select.find_all(exp.Column))


def _validate_scalar_subquery(select: exp.Expression) -> exp.Select:
    if not isinstance(select, exp.Select):
        raise errors.feature_not_supported("unsupported subquery")
    exprs = select.expressions
    bare = (
        exprs[0].this
        if exprs and isinstance(exprs[0], exp.Alias)
        else (exprs[0] if exprs else None)
    )
    if len(exprs) != 1 or isinstance(bare, exp.Star):
        raise errors.feature_not_supported("a subquery here must select exactly one column")
    if _subquery_has_outer_ref(select):
        raise errors.feature_not_supported("correlated subqueries are not supported")
    return select


def _run_inner_select(select: exp.Select, subctx: SubqueryCtx) -> Any:
    # Lazy import to avoid a planner<->engine import cycle.
    from secantus.sql import engine

    return engine.run_inner_select(
        select, subctx.storage, subctx.db, subctx.catalog, subctx.session
    )


def _eval_in_subquery(query_node: exp.Expression, subctx: SubqueryCtx, tag: str) -> list[Any]:
    select = _validate_scalar_subquery(_subquery_select(query_node))
    res = _run_inner_select(select, subctx)
    return [typemap.coerce(row[0], tag) for row in res.rows]


def _constant_in_result(node: exp.In, subctx: SubqueryCtx | None) -> bool | None:
    """Fold a constant-LHS ``IN`` (value list or subquery) to its three-valued
    outcome: True / False / None-for-unknown. ``IN`` over an empty set is FALSE
    even for a NULL left side; a NULL left side or a NULL candidate otherwise
    makes a non-match unknown (matters under NOT: ``1 NOT IN (NULL, 2)`` is
    unknown, not true)."""
    left = _literal(node.this)
    if node.args.get("query") is not None:
        if subctx is None:
            raise errors.feature_not_supported("IN (subquery) is not supported")
        candidates = _eval_in_subquery(node.args["query"], subctx, "any")
    else:
        candidates = [_literal(e) for e in node.expressions]
    if not candidates:
        return False
    if left is None:
        return None
    if any(c == left for c in candidates if c is not None):
        return True
    return None if any(c is None for c in candidates) else False


def _eval_scalar_subquery(query_node: exp.Expression, subctx: SubqueryCtx, tag: str) -> Any:
    select = _validate_scalar_subquery(_subquery_select(query_node))
    res = _run_inner_select(select, subctx)
    return typemap.coerce(res.rows[0][0], tag) if res.rows else None


_FLIP_CMP = {"$gt": "$lt", "$gte": "$lte", "$lt": "$gt", "$lte": "$gte", "$eq": "$eq", "$ne": "$ne"}


def _comparison_subquery_filter(
    node: exp.Expression, resolve: Resolve, subctx: SubqueryCtx | None
) -> dict[str, Any] | None:
    """``field OP (SELECT scalar ...)`` → ``{field: {op: value}}`` (None if neither
    side is a subquery, so the caller falls through to the normal handling)."""
    left, right = node.this, node.expression
    if not (isinstance(left, exp.Subquery) or isinstance(right, exp.Subquery)):
        return None
    if subctx is None:
        raise errors.feature_not_supported("scalar subquery is not supported here")
    if isinstance(right, exp.Subquery) and _is_field_node(left):
        fld, sub, flip = left, right, False
    elif isinstance(left, exp.Subquery) and _is_field_node(right):
        fld, sub, flip = right, left, True
    else:
        raise errors.feature_not_supported(f"unsupported subquery comparison: {node.sql()}")
    field, tag = _field(fld, resolve)
    value = _eval_scalar_subquery(sub, subctx, tag)
    op = _EXPR_CMP[type(node)]
    if flip:
        op = _FLIP_CMP[op]
    return {field: value} if op == "$eq" else {field: {op: value}}


def _null_guarded_expr_cmp(
    op: str, left: exp.Expression, right: exp.Expression, resolve: Resolve
) -> dict[str, Any]:
    """A computed comparison lowered to ``$expr`` with SQL three-valued
    semantics. Mongo's aggregation comparisons use BSON total order (NULL sorts
    below numbers, two-valued: ``NULL <> 19`` is true, ``NULL <= 0`` is true),
    so guard both sides non-null — a NULL operand makes the comparison unknown,
    and unknown never satisfies a WHERE."""
    lexpr = _to_agg_expr(left, resolve)
    rexpr = _to_agg_expr(right, resolve)
    return {
        "$expr": {"$and": [{"$ne": [lexpr, None]}, {"$ne": [rexpr, None]}, {op: [lexpr, rexpr]}]}
    }


def _always_unknown_predicate(core: exp.Expression) -> bool:
    """Whether a (NOT/paren-stripped) predicate is *always unknown* because of
    a NULL-literal operand: a NULL side of a comparison, a NULL left side of a
    non-empty ``IN`` list, or a NULL BETWEEN subject (or both bounds NULL).
    Unknown never satisfies a filter, and NOT preserves unknown — callers fold
    to match-nothing regardless of NOT nesting."""
    if isinstance(core, (exp.EQ, exp.NEQ, exp.GT, exp.GTE, exp.LT, exp.LTE)):
        return _null_literal_operand(core.this) or _null_literal_operand(core.expression)
    if isinstance(core, exp.In) and core.args.get("query") is None and core.expressions:
        return _null_literal_operand(core.this)
    if isinstance(core, exp.Between):
        return _null_literal_operand(core.this) or (
            _null_literal_operand(core.args["low"]) and _null_literal_operand(core.args["high"])
        )
    return False


def _null_literal_operand(side: exp.Expression) -> bool:
    """Whether a comparison operand is a literal that folds to SQL NULL —
    ``NULL``, ``(NULL)``, ``- CAST(NULL AS INTEGER)``, … A *function call*
    (``to_regtype('mood')``) is never a NULL literal: its value needs a live
    catalog/session context the fold doesn't have, so a context-free
    evaluation to None must not turn the comparison into match-nothing."""
    if isinstance(side, exp.Null):
        return True
    if not _is_literalish(side):
        return False
    if next(side.find_all(exp.Anonymous), None) is not None:
        return False
    try:
        return _literal(side) is None
    except errors.SQLError:
        return False


def _expr_to_filter(
    node: exp.Expression, resolve: Resolve, subctx: SubqueryCtx | None = None
) -> dict[str, Any]:
    if isinstance(node, exp.Paren):
        return _expr_to_filter(node.this, resolve, subctx)

    # Boolean literals — ``WHERE TRUE`` matches all, ``WHERE FALSE`` matches none
    # (the latter is how a default-deny RLS policy renders). ``$nor`` of match-all
    # is the empty-result filter.
    if isinstance(node, exp.Boolean):
        return {} if node.this else {"$nor": [{}]}

    # A comparison against a NULL literal (``col <> NULL`` / ``51 <> (NULL)`` /
    # ``- CAST(NULL AS INT) <> x`` — NOT the ``IS NULL`` form, which parses as
    # exp.Is) is always unknown in SQL, and unknown never satisfies a WHERE:
    # match nothing. Mongo's ``$ne: null`` (or a BSON-order ``$expr``
    # comparison) would instead match rows — the opposite.
    if isinstance(node, (exp.EQ, exp.NEQ, exp.GT, exp.GTE, exp.LT, exp.LTE)) and (
        _null_literal_operand(node.this) or _null_literal_operand(node.expression)
    ):
        return {"$nor": [{}]}

    # A schema-qualified function predicate (``pg_catalog.pg_table_is_visible(...)``)
    # parses as Dot(Identifier, Anonymous); unwrap to the function call.
    if isinstance(node, exp.Dot) and isinstance(node.expression, exp.Anonymous):
        node = node.expression
    if isinstance(node, exp.Anonymous) and node.name.lower() in _ALWAYS_TRUE_PREDICATES:
        return {}
    if isinstance(node, exp.Anonymous) and node.name.lower() == "pg_table_is_visible":
        # Real PG: a temp relation is visible only to its creating session.
        # Sessions track the temp tables they created (``Session.temp_tables``),
        # so the predicate lowers to ``relpersistence != 't' OR relname IN
        # (<this session's temp tables>)`` on the same pg_class row the oid
        # argument points at (same alias, so joins resolve).
        arg = (node.expressions or [None])[0]
        alias = arg.table if isinstance(arg, exp.Column) else None
        vis: exp.Expression = exp.NEQ(
            this=exp.column("relpersistence", table=alias or None),
            expression=exp.Literal.string("t"),
        )
        session = getattr(subctx, "session", None)
        # temp_tables carries the pg_temp_<n>. prefix; pg_class relname is bare.
        own = sorted(
            name.split(".", 1)[1] if "." in name else name
            for (tdb, name) in (getattr(session, "temp_tables", None) or ())
            if subctx is None or tdb == subctx.db
        )
        if own:
            vis = exp.Or(
                this=vis,
                expression=exp.In(
                    this=exp.column("relname", table=alias or None),
                    expressions=[exp.Literal.string(n) for n in own],
                ),
            )
        # Search-path visibility: the relation's namespace must be on the
        # session's search_path (plus the implicit pg_catalog /
        # information_schema / session pg_temp). The path is resolved to
        # namespace oids per-session — a hardcoded default-path list hid every
        # user-schema relation the moment ``SET search_path TO schema1`` ran
        # (pgjdbc's same-table-name-in-two-schemas updatable-resultset probe).
        # Default namespace oids mirror virtual._NS_OIDS / _PG_TEMP_NS_OID.
        visible_ns = [11, 13000, 99]
        path = list(getattr(session, "search_path", None) or ["public"])
        if subctx is not None and getattr(subctx, "catalog", None) is not None:
            from secantus.sql import virtual as _virtual

            schema_oid_map = _virtual._schema_oids(subctx.db, subctx.catalog)
        else:
            schema_oid_map = {}
        for schema in path:
            if schema == "public":
                visible_ns.append(2200)
            elif schema in schema_oid_map:
                visible_ns.append(schema_oid_map[schema])
        vis = exp.And(
            this=vis,
            expression=exp.In(
                this=exp.column("relnamespace", table=alias or None),
                expressions=[exp.Literal.number(o) for o in visible_ns],
            ),
        )
        return _expr_to_filter(vis, resolve, subctx)

    if isinstance(node, exp.And):
        parts = [
            _expr_to_filter(node.this, resolve, subctx),
            _expr_to_filter(node.expression, resolve, subctx),
        ]
        return _merge_and(parts)

    if isinstance(node, exp.Or):
        return {
            "$or": [
                _expr_to_filter(node.this, resolve, subctx),
                _expr_to_filter(node.expression, resolve, subctx),
            ]
        }

    if isinstance(node, exp.Not):
        inner = node.this
        # IS NOT NULL parses as Not(Is(col, Null)).
        if isinstance(inner, exp.Is) and isinstance(inner.expression, exp.Null):
            field, _ = _field(inner.this, resolve)
            return {field: {"$ne": None}}
        return _negated_filter(inner, resolve, subctx)

    if isinstance(node, exp.Exists):
        raise errors.feature_not_supported("EXISTS (subquery) is not supported")

    if isinstance(node, exp.Is):
        field, _ = _field(node.this, resolve)
        if isinstance(node.expression, exp.Null):
            return {field: None}
        raise errors.feature_not_supported(f"unsupported IS predicate: {node.sql()}")

    if isinstance(node, (exp.EQ, exp.NEQ, *_CMP_OPS.keys())):
        sub = _comparison_subquery_filter(node, resolve, subctx)
        if sub is not None:
            return sub

    if isinstance(node, exp.EQ):
        left, right = node.this, node.expression
        if isinstance(left, exp.Any) or isinstance(right, exp.Any):
            anynode, other = (left, right) if isinstance(left, exp.Any) else (right, left)
            inner = anynode.this
            while isinstance(inner, exp.Paren):  # ANY(col) wraps the operand in a Paren
                inner = inner.this
            if _is_field_node(inner):
                # ``<value> = ANY(array_col)`` — array membership. Mongo's
                # array-aware equality matches a doc whose array contains the value.
                field, tag = _field(inner, resolve)
                elem = typemap.array_element_tag(tag) if typemap.is_array_tag(tag) else tag
                return {field: typemap.coerce(_literal(other), elem)}
            # ``col = ANY(ARRAY[...])`` is Postgres' IN — SQLAlchemy's reflection
            # emits ``relkind = ANY(ARRAY['r','p',...])``.
            field, tag = _field(other, resolve)
            try:
                elements = _array_elements(inner)
            except errors.SQLError:
                elements = None
            if elements is None:
                # ``col = ANY(<expr>)`` where the operand is a function or other
                # expression yielding an array (``ANY(current_schemas(true))`` —
                # pgjdbc's DatabaseMetaData namespace filter): evaluate it.
                from secantus.sql import scalar as _scalar

                sub = subctx or _pipeline_subctx.get()
                ctx = _scalar.ScalarContext(
                    storage=getattr(sub, "storage", None),
                    catalog=getattr(sub, "catalog", None),
                    db=getattr(sub, "db", None) or "",
                    session=getattr(sub, "session", None),
                )
                value = _scalar.evaluate(inner, _const_scope, ctx)
                if not isinstance(value, (list, tuple)):
                    raise errors.feature_not_supported(f"unsupported ANY operand: {inner.sql()}")
                return {field: {"$in": [typemap.coerce(v, tag) for v in value]}}
            values = [typemap.coerce(_literal(e), tag) for e in elements]
            return {field: {"$in": values}}
        pair = _field_literal_pair(left, right)
        if pair is not None:
            field, tag = _field(pair[0], resolve)
            if tag == "citext":
                return _citext_cmp_filter(field, "$eq", _literal(pair[1]))
            return {field: typemap.coerce(_literal(pair[1]), tag)}
        return _null_guarded_expr_cmp("$eq", left, right, resolve)

    if isinstance(node, exp.NEQ):
        left, right = node.this, node.expression
        pair = _field_literal_pair(left, right)
        if pair is not None:
            field, tag = _field(pair[0], resolve)
            if tag == "citext":
                return _citext_cmp_filter(field, "$ne", _literal(pair[1]))
            # SQL ``<>`` is unknown (not true) for a NULL operand; Mongo's bare
            # ``$ne`` would match NULL/missing rows, so guard the field non-null.
            value = typemap.coerce(_literal(pair[1]), tag)
            return {"$and": [{field: {"$ne": value}}, {field: {"$ne": None}}]}
        return _null_guarded_expr_cmp("$ne", left, right, resolve)

    for cls, (op, flipped) in _CMP_OPS.items():
        if isinstance(node, cls):
            left, right = node.this, node.expression
            if _is_field_node(left) and _is_literalish(right):
                field, tag = _field(left, resolve)
                if tag == "citext":
                    return _citext_cmp_filter(field, op, _literal(right))
                return {field: {op: typemap.coerce(_literal(right), tag)}}
            if _is_field_node(right) and _is_literalish(left):
                field, tag = _field(right, resolve)
                if tag == "citext":
                    return _citext_cmp_filter(field, flipped, _literal(left))
                return {field: {flipped: typemap.coerce(_literal(left), tag)}}
            return _null_guarded_expr_cmp(_EXPR_CMP[cls], left, right, resolve)

    if isinstance(node, exp.In):
        if not _is_field_node(node.this) and _is_literalish(node.this):
            # Constant-LHS membership (``1 IN (2)`` / ``1 IN (SELECT 1)``) folds to
            # match-all / match-nothing (unknown never satisfies a WHERE, and IN
            # over an empty set is FALSE even for a NULL left side).
            return {} if _constant_in_result(node, subctx) is True else {"$nor": [{}]}
        field, tag = _field(node.this, resolve)
        if node.args.get("query") is not None:
            if subctx is None:
                raise errors.feature_not_supported("IN (subquery) is not supported")
            values = _eval_in_subquery(node.args["query"], subctx, tag)
        else:
            if tag == "citext":
                # Case-insensitive membership: fold the field and every candidate.
                folded = [str(_literal(e)).lower() for e in node.expressions]
                return {"$expr": {"$in": [{"$toLower": f"${field}"}, folded]}}
            values = [typemap.coerce(_literal(e), tag) for e in node.expressions]
        # A NULL candidate can only turn a non-match unknown — and unknown never
        # satisfies a WHERE — so drop it; Mongo's ``$in`` with ``None`` would
        # instead match NULL rows.
        return {field: {"$in": [v for v in values if v is not None]}}

    if isinstance(node, exp.Between):
        field, tag = _field(node.this, resolve)
        if tag == "citext":
            low = _citext_cmp_filter(field, "$gte", _literal(node.args["low"]))
            high = _citext_cmp_filter(field, "$lte", _literal(node.args["high"]))
            return _merge_and([low, high])
        low = typemap.coerce(_literal(node.args["low"]), tag)
        high = typemap.coerce(_literal(node.args["high"]), tag)
        return {field: {"$gte": low, "$lte": high}}

    if isinstance(node, exp.Operator):
        rewritten = _rewrite_explicit_operator(node)
        if rewritten is not None:
            return _expr_to_filter(rewritten, resolve, subctx)
    if isinstance(node, exp.Escape) and isinstance(node.this, (exp.Like, exp.ILike)):
        # ``LIKE <pattern> ESCAPE <char>`` — same lowering with the escape
        # character honored in the regex translation.
        like = node.this
        esc = _literal(node.expression)
        field, tag = _field(like.this, resolve)
        pattern = _literal(like.expression)
        spec_e: dict[str, Any] = {"$regex": _like_to_regex(str(pattern), escape=str(esc))}
        if isinstance(like, exp.ILike) or tag == "citext":
            spec_e["$options"] = "i"
        if like.args.get("negate"):
            return {field: {"$not": spec_e}}
        return {field: spec_e}

    if isinstance(node, (exp.Like, exp.ILike)):
        field, tag = _field(node.this, resolve)
        pattern = _literal(node.expression)
        spec: dict[str, Any] = {"$regex": _like_to_regex(str(pattern))}
        # ``citext LIKE`` is case-insensitive (equivalent to ILIKE).
        if isinstance(node, exp.ILike) or tag == "citext":
            spec["$options"] = "i"
        # sqlglot parses ``NOT LIKE`` as ``Like(negate=True)``, not Not(Like).
        if node.args.get("negate"):
            return {field: {"$not": spec}}
        return {field: spec}

    if isinstance(node, (exp.RegexpLike, exp.RegexpILike)):
        # POSIX regex-match operators: ``~`` / ``~*`` (and their negations ``!~`` /
        # ``!~*``, which parse as ``Not(RegexpLike/RegexpILike)`` and route through
        # the ``exp.Not`` branch above). The pattern is a raw regex — unlike LIKE,
        # it is *not* translated — and matches unanchored (Mongo ``$regex`` uses
        # ``re.search`` semantics, matching Postgres' ``~``).
        field, _ = _field(node.this, resolve)
        spec = {"$regex": str(_literal(node.expression))}
        if isinstance(node, exp.RegexpILike):
            spec["$options"] = "i"
        return {field: spec}

    if isinstance(node, exp.ArrayOverlaps):  # array && (overlaps) → {field: {$in: …}}
        arr = _array_index_filter(node, resolve)
        if arr is not None:
            return arr
        raise errors.feature_not_supported(f"unsupported operator: {node.sql()}")

    if isinstance(node, exp.ArrayContainsAll):  # array @> → {field: {$all: …}}, else jsonb @>
        arr = _array_index_filter(node, resolve)
        if arr is not None:
            return arr
        field, _ = _field(node.this, resolve)
        return _jsonb_contains_filter(field, _json_value(node.expression))

    if isinstance(node, exp.ArrayContainedBy):  # jsonb <@ (contained by)
        # ``const <@ field`` is exactly ``field @> const`` (the field contains the
        # constant), which pushes down. ``field <@ const`` is a subset constraint
        # on the stored value's whole shape — not a value lookup — so it can't
        # lower to a Mongo filter; faithful not-supported beats a silent divergence.
        if _is_field_node(node.expression) and _is_literalish(node.this):
            field, _ = _field(node.expression, resolve)
            return _jsonb_contains_filter(field, _json_value(node.this))
        raise errors.feature_not_supported(
            "the jsonb <@ (contained by) operator is only supported as "
            "<constant> <@ field (equivalently field @> <constant>)"
        )

    if isinstance(node, exp.JSONBContains):  # jsonb ? (top-level key / element exists)
        field, _ = _field(node.this, resolve)
        return {"$or": _jsonb_key_exists_clauses(field, str(_literal(node.expression)))}

    if isinstance(node, exp.JSONBContainsAnyTopKeys):  # jsonb ?| (any key exists)
        field, _ = _field(node.this, resolve)
        clauses: list[dict[str, Any]] = []
        for e in _array_elements(node.expression):
            clauses.extend(_jsonb_key_exists_clauses(field, str(_literal(e))))
        return {"$or": clauses}

    if isinstance(node, exp.JSONBContainsAllTopKeys):  # jsonb ?& (all keys exist)
        field, _ = _field(node.this, resolve)
        keys = [str(_literal(e)) for e in _array_elements(node.expression)]
        return {"$and": [{"$or": _jsonb_key_exists_clauses(field, k)} for k in keys]}

    # A bare boolean column used as a predicate (``WHERE flag`` / ``WHERE NOT
    # flag``) — Postgres treats it as ``flag IS TRUE``.
    if isinstance(node, exp.Column):
        field, _ = resolve(node)
        return {field: True}

    raise errors.feature_not_supported(f"unsupported WHERE clause: {node.sql()}")


_NEG_CMP_NODE: dict[type, type] = {
    exp.EQ: exp.NEQ,
    exp.NEQ: exp.EQ,
    exp.GT: exp.LTE,
    exp.GTE: exp.LT,
    exp.LT: exp.GTE,
    exp.LTE: exp.GT,
}


def _negated_filter(
    inner: exp.Expression, resolve: Resolve, subctx: SubqueryCtx | None
) -> dict[str, Any]:
    """Lower ``NOT <inner>`` with SQL three-valued semantics.

    Mongo's ``$nor`` is two-valued: it matches rows where ``<inner>`` is
    *unknown* (a NULL operand), which SQL's ``NOT`` must not — ``d NOT BETWEEN
    110 AND 150`` excludes a NULL ``d``. So push the negation into the tree —
    De Morgan over AND/OR, operator flips at comparison leaves — and lower the
    positive rewrite. A shape with no exact rewrite gets a null-guarded
    ``$nor`` when it predicates a single field (there unknown ⇔ field IS
    NULL); anything else raises feature_not_supported so the statement routes
    to the per-row evaluated path, whose scalar NOT is three-valued."""
    while isinstance(inner, exp.Paren):
        inner = inner.this
    if isinstance(inner, exp.Boolean):
        return _expr_to_filter(exp.Boolean(this=not inner.this), resolve, subctx)
    if isinstance(inner, exp.Null):
        return {"$nor": [{}]}  # NOT NULL is unknown: match nothing
    if isinstance(inner, exp.Not):
        # NOT NOT p ≡ p under WHERE (unknown is excluded either way).
        return _expr_to_filter(inner.this, resolve, subctx)
    if isinstance(inner, exp.Is):
        # IS [NOT] NULL / IS TRUE / IS DISTINCT FROM are two-valued: $nor is exact.
        return {"$nor": [_expr_to_filter(inner, resolve, subctx)]}
    if isinstance(inner, exp.And):
        return {
            "$or": [
                _negated_filter(inner.this, resolve, subctx),
                _negated_filter(inner.expression, resolve, subctx),
            ]
        }
    if isinstance(inner, exp.Or):
        return _merge_and(
            [
                _negated_filter(inner.this, resolve, subctx),
                _negated_filter(inner.expression, resolve, subctx),
            ]
        )
    neg_cls = _NEG_CMP_NODE.get(type(inner))
    if neg_cls is not None:
        left, right = inner.this, inner.expression
        if _null_literal_operand(left) or _null_literal_operand(right):
            return {"$nor": [{}]}  # a comparison with NULL never turns true under NOT
        if (
            _field_literal_pair(left, right) is None
            and not isinstance(left, exp.Subquery)
            and not isinstance(right, exp.Subquery)
        ):
            # A computed comparison would lower to $expr, whose BSON-order
            # comparisons are two-valued over NULL — per-row instead.
            raise errors.feature_not_supported(f"NOT over a computed comparison: {inner.sql()}")
        return _expr_to_filter(neg_cls(this=left.copy(), expression=right.copy()), resolve, subctx)
    if isinstance(inner, exp.Between):
        this, low, high = inner.this, inner.args["low"], inner.args["high"]
        if not (_is_field_node(this) and _is_literalish(low) and _is_literalish(high)):
            raise errors.feature_not_supported(f"NOT over a computed BETWEEN: {inner.sql()}")
        return {
            "$or": [
                _expr_to_filter(exp.LT(this=this.copy(), expression=low.copy()), resolve, subctx),
                _expr_to_filter(exp.GT(this=this.copy(), expression=high.copy()), resolve, subctx),
            ]
        }
    if isinstance(inner, exp.In):
        if not _is_field_node(inner.this) and _is_literalish(inner.this):
            # NOT IN is true only when IN is definitively false — unknown
            # (``1 NOT IN (NULL, 2)``) stays excluded.
            return {} if _constant_in_result(inner, subctx) is False else {"$nor": [{}]}
        field, tag = _field(inner.this, resolve)
        if inner.args.get("query") is not None:
            if subctx is None:
                raise errors.feature_not_supported("IN (subquery) is not supported")
            values = _eval_in_subquery(inner.args["query"], subctx, tag)
        elif tag == "citext":
            return {
                "$and": [
                    {"$nor": [_expr_to_filter(inner, resolve, subctx)]},
                    {field: {"$ne": None}},
                ]
            }
        else:
            values = [typemap.coerce(_literal(e), tag) for e in inner.expressions]
        if any(v is None for v in values):
            return {"$nor": [{}]}  # x NOT IN (…, NULL) is never true
        return {"$and": [{field: {"$nin": values}}, {field: {"$ne": None}}]}
    # Generic single-field fallback: NOT p ≡ (p is false AND field non-null),
    # sound exactly when p is unknown iff its one field is NULL — no NULL
    # literal, no subquery, and a non-$expr positive lowering ($expr compares
    # in two-valued BSON order).
    pos = _expr_to_filter(inner, resolve, subctx)
    if inner.find(exp.Null) is None and inner.find(exp.Select) is None:
        fields = set()
        for c in inner.find_all(exp.Column):
            try:
                fields.add(resolve(c)[0])
            except errors.SQLError:
                fields.clear()
                break
        if len(fields) == 1 and "$expr" not in str(pos):
            field = next(iter(fields))
            return {"$and": [{"$nor": [pos]}, {field: {"$ne": None}}]}
    raise errors.feature_not_supported(f"unsupported NOT: NOT {inner.sql()}")


def _json_value(node: exp.Expression) -> Any:
    """Decode a jsonb literal operand (``'{"a":1}'`` / ``'[1,2]'::jsonb``)."""
    raw = _literal(node)
    if isinstance(raw, str):
        try:
            return json.loads(raw)
        except (ValueError, TypeError) as exc:
            raise errors.feature_not_supported(f"invalid jsonb literal: {raw!r}") from exc
    return raw


def _jsonb_key_exists_clauses(path: str, key: str) -> list[dict[str, Any]]:
    """Mongo clauses for Postgres ``jsonb ? key`` at ``path``: the value is an
    object with top-level key ``key``, OR an array containing the string ``key``,
    OR the string ``key`` itself (``{path: key}`` matches the array / scalar cases
    by Mongo's array-aware equality)."""
    return [{f"{path}.{key}": {"$exists": True}}, {path: key}]


def _jsonb_contains_filter(path: str, value: Any) -> dict[str, Any]:
    """Translate Postgres ``field @> value`` containment into a Mongo filter.

    An object RHS becomes a conjunction of dotted-path equalities (recursively);
    an array RHS becomes ``$all`` (the field array contains every element); a
    scalar RHS becomes a plain equality."""
    if isinstance(value, dict):
        return _merge_and([_jsonb_contains_filter(f"{path}.{k}", v) for k, v in value.items()])
    if isinstance(value, list):
        return {path: {"$all": value}}
    return {path: value}


def _merge_and(parts: list[dict[str, Any]]) -> dict[str, Any]:
    """Merge conjuncts into one dict when their field keys are disjoint, else $and.

    Merging keeps simple ``a = 1 AND b = 2`` queries as ``{a: 1, b: 2}`` so the
    storage layer's compound-index planning can see them; colliding keys fall
    back to an explicit ``$and`` which the query engine also handles.
    """
    merged: dict[str, Any] = {}
    for part in parts:
        if any(k in merged or k.startswith("$") for k in part):
            return {"$and": parts}
        merged.update(part)
    return merged


def _where_filter(
    stmt: exp.Expression, table: TableDef, subctx: SubqueryCtx | None = None
) -> dict[str, Any]:
    where = stmt.args.get("where")
    if where is None:
        return {}
    # The pipeline planners don't thread `subctx`; fall back to the one published
    # by `plan_pipeline_select` so WHERE subqueries work there too.
    ctx = subctx or _pipeline_subctx.get()
    return _expr_to_filter(where.this, table_resolver(table), ctx)


# ---------------------------------------------------------------------------
# Statement planners
# ---------------------------------------------------------------------------


def _decl_identity(datatype: exp.DataType) -> dict[str, Any]:
    """The declared reflection identity of a column type — ``decl_oid`` when the
    declared oid differs from the storage tag's (``varchar``/``bpchar`` fold to
    ``text``) and the ``atttypmod`` (``varchar(52)``, ``numeric(18,5)``, …), via
    the same ``cast_type_identity`` the cast descriptor uses. JSON's special
    (114, -1) identity is already carried by ``json_plain``."""
    ident = typemap.cast_type_identity(datatype)
    if ident is None or ident == (114, -1):
        return {}
    oid, typmod = ident
    return {"decl_oid": oid, "typmod": typmod}


def plan_create_table(stmt: exp.Create) -> CreateTablePlan:
    schema = stmt.this
    if not isinstance(schema, exp.Schema):
        raise errors.feature_not_supported("CREATE TABLE requires a column list")
    table_name = qualified_table_name(schema.this)
    columns: list[Column] = []
    seq_plans: list[dict[str, Any]] = []
    pk_seen = False
    pk_table_names: list[str] = []
    pk_name: str | None = None
    for coldef in schema.expressions:
        if isinstance(coldef, exp.PrimaryKey):
            # Table-level PRIMARY KEY (col, ...) — mark the named column(s). A
            # composite PK maps to a subdocument ``_id`` (field ``_id.<name>`` per
            # column); a single PK maps directly to ``_id``. Applied post-loop so
            # a PK clause written before its columns still marks them.
            pk_table_names = [_column_name(c) for c in coldef.expressions]
            pk_seen = True
            continue
        if isinstance(coldef, exp.ForeignKey):
            # Table-level FOREIGN KEY — collected by _extract_foreign_keys below.
            continue
        if isinstance(coldef, exp.Constraint):
            # ``CONSTRAINT <name> PRIMARY KEY (col, ...)`` — same as the bare
            # table-level PK, plus the declared constraint name (which reflection
            # and COMMENT ON CONSTRAINT surface instead of ``<table>_pkey``).
            # CHECK / UNIQUE inners are collected by _extract_constraints below.
            inner_pk = next(
                (
                    i
                    for i in (coldef.args.get("expressions") or [])
                    if isinstance(i, exp.PrimaryKey)
                ),
                None,
            )
            if inner_pk is not None:
                pk_table_names = [_column_name(c) for c in inner_pk.expressions]
                pk_name = coldef.this.name if coldef.this else None
                pk_seen = True
            continue
        if isinstance(coldef, (exp.CheckColumnConstraint, exp.UniqueColumnConstraint)):
            # Table-level CHECK / UNIQUE — collected by _extract_constraints below.
            continue
        if isinstance(coldef, exp.ExcludeColumnConstraint):
            # ``EXCLUDE (col WITH =, ...)`` — equality-only exclusion is
            # collected by _extract_constraints; any other operator rejects
            # there (a GiST range exclusion has no unique-index equivalent).
            continue
        if isinstance(coldef, exp.Anonymous) and str(coldef.this).upper() == "INDEX":
            # crdb-style inline ``INDEX (...)`` table element (also MySQL DDL;
            # the pgtest copy corpus creates one on an expression). The table
            # is created without the secondary index — inline index elements
            # are an optimization hint here, not a constraint (see
            # tasks/backlog.md).
            continue
        if not isinstance(coldef, exp.ColumnDef):
            raise errors.feature_not_supported(f"unsupported table element: {coldef.sql()}")
        serial_tag = _serial_tag(coldef.args["kind"])
        tag = serial_tag or typemap.type_tag_for_sql(coldef.args["kind"])
        enum_name = None
        if tag is None:
            named = _enum_type_name(coldef.args["kind"])
            # A *quoted* built-in spelling (``"cidr"`` — psycopg's fixtures emit
            # sql.Identifier(type) DDL) parses as user-defined; resolve it
            # against the built-in registry before the enum fallback.
            builtin = typemap.builtin_tag_for_name(named) if named is not None else None
            if builtin is not None:
                tag = builtin
            elif named is not None:
                # An unknown type name is a candidate enum type (stored as text,
                # validated against the enum's labels at write time). If it isn't
                # a declared enum, execute_create_table raises 42704.
                enum_name = named
                tag = "text"
            else:
                # ``mood[]`` — an array of a user-defined type is a candidate
                # enum-array column: stored as a text array, each element
                # validated against the enum's labels at write time.
                arr_elem = _enum_array_element_name(coldef.args["kind"])
                if arr_elem is not None and typemap.builtin_tag_for_name(arr_elem) is None:
                    enum_name = arr_elem
                    tag = "text[]"
        if tag is None:
            raise errors.feature_not_supported(
                f"unsupported column type for {coldef.name}: {coldef.args['kind'].sql()}"
            )
        constraints = [type(c.kind).__name__ for c in (coldef.args.get("constraints") or [])]
        is_pk = "PrimaryKeyColumnConstraint" in constraints
        identity = _identity_spec(coldef)
        # A SERIAL / IDENTITY column is implicitly NOT NULL with an owned sequence.
        auto = serial_tag is not None or identity is not None
        nullable = not is_pk and not auto and "NotNullColumnConstraint" not in constraints
        if is_pk:
            pk_seen = True
        has_default, default = _column_default(coldef, tag)
        sequence = _default_sequence(coldef)
        default_expr = None if (has_default or sequence) else _default_expr(coldef)
        if auto:
            sequence = f"{table_name}_{coldef.name}_seq"
            start = identity["start"] if identity else 1
            increment = identity["increment"] if identity else 1
            seq_plans.append(
                {"name": sequence, "column": coldef.name, "increment": increment, "start": start}
            )
        columns.append(
            Column(
                name=coldef.name,
                type_tag=tag,
                field="_id" if is_pk else coldef.name,
                pk=is_pk,
                nullable=nullable,
                has_default=has_default,
                default=default,
                default_expr=default_expr,
                sequence=sequence,
                identity=(identity["mode"] if identity else None),
                enum_type=enum_name,
                generated=_generated_expr(coldef),
                # ``json`` (not ``jsonb``): same stored shape, oid 114 on the wire.
                json_plain=(tag == "json" and coldef.args["kind"].this == exp.DataType.Type.JSON),
                **_decl_identity(coldef.args["kind"]),
            )
        )
    if pk_table_names:
        columns = [_with_pk(c, pk_table_names) for c in columns]
    if not pk_seen:
        # No PK: the _id is auto-assigned by storage and not surfaced as a
        # column. Fine for the spike.
        pass
    fks = _extract_foreign_keys(schema, table_name)
    # A SELF-referencing FK captured the target by its spelled name, which for
    # a temp table is the bare pre-rewrite name (``references test_deferred``
    # inside ``CREATE TEMP TABLE test_deferred`` — the target's pg_temp_<n>.
    # rewrite happens on the create target, and the reference can't resolve
    # through the catalog because the table doesn't exist yet). Point it at
    # the table's own final name so enforcement finds the right relation.
    if "." in table_name:
        bare = table_name.split(".", 1)[1]
        fks = [
            dataclasses.replace(fk, ref_table=table_name) if fk.ref_table == bare else fk
            for fk in fks
        ]
    checks, uniques = _extract_constraints(schema, table_name)
    props = stmt.args.get("properties")
    # A pg_temp_<n>-homed name is temp even without the TEMP keyword — CREATE
    # TABLE pg_temp.t is a temp table in real PG (the qualifier was rewritten
    # to the session's namespace by qualify_from_search_path).
    is_temp = (
        bool(props) and any(isinstance(p, exp.TemporaryProperty) for p in props.expressions)
    ) or table_name.startswith("pg_temp_")
    table = TableDef(
        name=table_name,
        collection=table_name,
        columns=columns,
        foreign_keys=fks,
        check_constraints=checks,
        unique_constraints=uniques,
        temp=is_temp,
        pk_name=pk_name,
        pk_column_order=tuple(pk_table_names) if pk_table_names else None,
    )
    return CreateTablePlan(
        table=table, if_not_exists=bool(stmt.args.get("exists")), sequences=seq_plans
    )


def _ref_target(ref: exp.Reference) -> tuple[str, tuple[str, ...]]:
    """A ``REFERENCES`` clause → ``(ref_table, (ref_col, ...))``. An empty column
    list (``REFERENCES t``) points at the target's PRIMARY KEY, left empty here
    and resolved to ``_id`` by reflection."""
    schema = ref.this  # exp.Schema or exp.Table
    if isinstance(schema, exp.Schema):
        return qualified_table_name(schema.this), tuple(_column_name(c) for c in schema.expressions)
    if isinstance(schema, exp.Table):
        return qualified_table_name(schema), ()
    raise errors.feature_not_supported(f"unsupported REFERENCES target: {ref.sql()}")


def _ref_actions(ref: exp.Reference) -> tuple[str | None, str | None]:
    """Parse ``ON DELETE`` / ``ON UPDATE`` referential actions out of a
    ``Reference``'s option strings (e.g. ``"ON DELETE CASCADE"``)."""
    on_delete = on_update = None
    for opt in ref.args.get("options") or []:
        text = str(opt).upper()
        if text.startswith("ON DELETE "):
            on_delete = text[len("ON DELETE ") :].strip()
        elif text.startswith("ON UPDATE "):
            on_update = text[len("ON UPDATE ") :].strip()
    return on_delete, on_update


def _deferrable_flags(options: Any) -> tuple[bool, bool]:
    """``(deferrable, initially_deferred)`` parsed from a constraint's option
    strings (``DEFERRABLE`` / ``NOT DEFERRABLE`` / ``INITIALLY DEFERRED`` /
    ``INITIALLY IMMEDIATE``)."""
    texts = [str(o).upper() for o in (options or [])]
    deferrable = "DEFERRABLE" in texts and "NOT DEFERRABLE" not in texts
    initially_deferred = deferrable and "INITIALLY DEFERRED" in texts
    return deferrable, initially_deferred


def _make_fk(
    table_name: str, cols: tuple[str, ...], ref: exp.Reference, name: str | None = None
) -> ForeignKey:
    ref_table, ref_cols = _ref_target(ref)
    on_delete, on_update = _ref_actions(ref)
    deferrable, initially_deferred = _deferrable_flags(ref.args.get("options"))
    # Postgres' default constraint name: <table>_<firstcol>_fkey (an explicit
    # ``CONSTRAINT <name>`` wins when supplied, e.g. from ALTER TABLE ADD).
    bare = table_name.split(".", 1)[1] if "." in table_name else table_name
    con_name = name or (f"{bare}_{cols[0]}_fkey" if cols else f"{bare}_fkey")
    return ForeignKey(
        name=con_name,
        columns=cols,
        ref_table=ref_table,
        ref_columns=ref_cols,
        on_delete=on_delete,
        on_update=on_update,
        deferrable=deferrable,
        initially_deferred=initially_deferred,
    )


def _fk_from_node(node: exp.ForeignKey, table_name: str, name: str | None) -> ForeignKey | None:
    """Build a ``ForeignKey`` from an ``exp.ForeignKey`` node (its ``expressions``
    are the local columns, ``reference`` the target), or None if it carries no
    reference (a bare ``FOREIGN KEY`` with no ``REFERENCES`` is malformed)."""
    ref = node.args.get("reference")
    if ref is None:
        return None
    cols = tuple(_column_name(c) for c in node.args.get("expressions") or [])
    return _make_fk(table_name, cols, ref, name)


def _extract_foreign_keys(schema: exp.Schema, table_name: str) -> list[ForeignKey]:
    """Collect declared foreign keys from a ``CREATE TABLE`` column list — column-
    level ``col type [CONSTRAINT n] REFERENCES t(c)``, table-level unnamed
    ``FOREIGN KEY (c) REFERENCES t(c)``, and table-level named
    ``CONSTRAINT n FOREIGN KEY (c) REFERENCES t(c)``."""
    fks: list[ForeignKey] = []
    for coldef in schema.expressions:
        if isinstance(coldef, exp.ForeignKey):  # table-level unnamed
            fk = _fk_from_node(coldef, table_name, None)
            if fk is not None:
                fks.append(fk)
        elif isinstance(coldef, exp.Constraint):  # CONSTRAINT n FOREIGN KEY (...) ...
            name = coldef.this.name if coldef.this else None
            for inner in coldef.args.get("expressions") or []:
                if isinstance(inner, exp.ForeignKey):
                    fk = _fk_from_node(inner, table_name, name)
                    if fk is not None:
                        fks.append(fk)
        elif isinstance(coldef, exp.ColumnDef):  # column-level [CONSTRAINT n] REFERENCES
            for con in coldef.args.get("constraints") or []:
                if isinstance(con.kind, exp.Reference):
                    cname = con.this.name if con.args.get("this") else None
                    fks.append(_make_fk(table_name, (coldef.name,), con.kind, cname))
    return fks


def _unique_cols(node: exp.Expression | None, fallback: str | None) -> tuple[str, ...]:
    """Column names a UNIQUE constraint covers. A table-level UNIQUE holds its
    columns in a ``Schema`` (paren list) or a bare column node; a column-level
    UNIQUE has no columns of its own, so it falls back to the owning column."""
    if node is None:
        return (fallback,) if fallback is not None else ()
    if isinstance(node, exp.Schema):
        return tuple(_column_name(c) for c in node.expressions)
    return (_column_name(node),)


def make_check_constraint(
    inner: exp.CheckColumnConstraint, table_name: str, name: str | None, col: str | None = None
) -> CheckConstraint:
    """Build a ``CheckConstraint`` from a parsed ``CHECK (...)`` node. Unnamed
    constraints get Postgres' default name (``<table>_<col>_check`` for a
    column-level check, ``<table>_check`` otherwise)."""
    expr = inner.this.sql(dialect="postgres")
    cname = name or (f"{table_name}_{col}_check" if col else f"{table_name}_check")
    return CheckConstraint(name=cname, expression=expr)


def make_unique_constraint(
    inner: exp.UniqueColumnConstraint, table_name: str, name: str | None, col: str | None = None
) -> UniqueConstraint:
    """Build a ``UniqueConstraint`` from a parsed ``UNIQUE (...)`` node. Unnamed
    constraints get Postgres' default name (``<table>_<col1>_<col2>_key``)."""
    cols = _unique_cols(inner.this, col)
    cname = name or f"{table_name}_{'_'.join(cols)}_key"
    deferrable, initially_deferred = _deferrable_flags(inner.args.get("options"))
    return UniqueConstraint(
        name=cname, columns=cols, deferrable=deferrable, initially_deferred=initially_deferred
    )


def _extract_constraints(
    schema: exp.Schema, table_name: str
) -> tuple[list[CheckConstraint], list[UniqueConstraint]]:
    """Collect declared CHECK / UNIQUE constraints from a ``CREATE TABLE`` column
    list — column-level (``col int CHECK (col > 0)`` / ``col text UNIQUE``),
    table-level named (``CONSTRAINT c CHECK (...)`` / ``... UNIQUE (a, b)``), and
    table-level unnamed. Neither is enforced — recorded for reflection only."""
    checks: list[CheckConstraint] = []
    uniques: list[UniqueConstraint] = []

    for coldef in schema.expressions:
        if isinstance(coldef, exp.ColumnDef):  # column-level
            for con in coldef.args.get("constraints") or []:
                kind = con.kind
                # ``data int CONSTRAINT chk_eq1 CHECK (…)`` — the declared name
                # rides the ColumnConstraint node.
                declared = con.this.name if getattr(con, "this", None) is not None else None
                if isinstance(kind, exp.CheckColumnConstraint):
                    checks.append(make_check_constraint(kind, table_name, declared, coldef.name))
                elif isinstance(kind, exp.UniqueColumnConstraint):
                    uniques.append(make_unique_constraint(kind, table_name, declared, coldef.name))
        elif isinstance(coldef, exp.Constraint):  # CONSTRAINT <name> CHECK/UNIQUE (...)
            name = coldef.this.name if coldef.this else None
            for inner in coldef.args.get("expressions") or []:
                if isinstance(inner, exp.CheckColumnConstraint):
                    checks.append(make_check_constraint(inner, table_name, name))
                elif isinstance(inner, exp.UniqueColumnConstraint):
                    uniques.append(make_unique_constraint(inner, table_name, name))
        elif isinstance(coldef, exp.CheckColumnConstraint):  # table-level unnamed CHECK (...)
            checks.append(make_check_constraint(coldef, table_name, None))
        elif isinstance(coldef, exp.UniqueColumnConstraint):  # table-level unnamed UNIQUE (...)
            uniques.append(make_unique_constraint(coldef, table_name, None))
        elif isinstance(coldef, exp.ExcludeColumnConstraint):
            uniques.append(_make_exclusion_constraint(coldef, table_name))
    return checks, uniques


def _make_exclusion_constraint(
    coldef: exp.ExcludeColumnConstraint, table_name: str
) -> UniqueConstraint:
    """``EXCLUDE (col WITH =, ...)`` — the equality-only form is unique
    enforcement with PG's exclusion identity: violation 23P01, default name
    ``<table>_<col>_excl``. Any non-``=`` operator (a real GiST range
    exclusion) stays unsupported."""
    params = coldef.this
    cols: list[str] = []
    for item in params.args.get("columns") or []:
        target = item.this if isinstance(item, exp.WithOperator) else item
        op = item.args.get("op") if isinstance(item, exp.WithOperator) else None
        op_text = (op.name if op is not None else "=").strip()
        if op_text != "=":
            raise errors.feature_not_supported(
                f"EXCLUDE with operator {op_text} is not supported (equality only)"
            )
        if isinstance(target, exp.Ordered):
            target = target.this
        cols.append(_column_name(target))
    bare = table_name.split(".", 1)[1] if "." in table_name else table_name
    name = f"{bare}_{'_'.join(cols)}_excl"
    return UniqueConstraint(name=name, columns=tuple(cols), exclusion=True)


def _with_pk(col: Column, pk_names: list[str]) -> Column:
    """Mark ``col`` as a PK column if it's named in ``pk_names``. A composite PK
    (>1 name) maps each column to the subdocument field ``_id.<name>``; a single
    PK maps to ``_id``. Preserves the column's other attributes."""
    if col.name not in pk_names:
        return col
    field = f"_id.{col.name}" if len(pk_names) > 1 else "_id"
    return replace(col, field=field, pk=True, nullable=False)


def plan_drop_table(stmt: exp.Drop) -> DropTablePlan:
    return DropTablePlan(
        name=qualified_table_name(stmt.this), if_exists=bool(stmt.args.get("exists"))
    )


def plan_alter_table(stmt: exp.Alter) -> AlterTablePlan:
    kind = str(stmt.args.get("kind") or "TABLE").upper()
    if kind != "TABLE":
        raise errors.feature_not_supported(f"ALTER {kind} is not supported")
    return AlterTablePlan(
        name=stmt.this.name,
        if_exists=bool(stmt.args.get("exists")),
        actions=list(stmt.args.get("actions") or []),
    )


def plan_create_index(stmt: exp.Create, table: TableDef) -> CreateIndexPlan:
    index = stmt.this  # exp.Index
    params = index.args.get("params")
    if params is None or not params.args.get("columns"):
        raise errors.feature_not_supported("CREATE INDEX requires a column list")
    cols = params.args["columns"]
    name_ident = index.this
    key_spec: dict[str, int] = {}
    expr_index = None
    # A single-key expression index (``CREATE INDEX … ((a + b))``) is materialised
    # into a hidden storage field indexed like a column (see ``catalog.ExprIndex``).
    if len(cols) == 1 and not isinstance(
        (cols[0].this if isinstance(cols[0], exp.Ordered) else cols[0]),
        (exp.Column, exp.Identifier),
    ):
        ordered = cols[0] if isinstance(cols[0], exp.Ordered) else None
        expr_node = ordered.this if ordered is not None else cols[0]
        while isinstance(expr_node, exp.Paren):
            expr_node = expr_node.this
        direction = -1 if (ordered is not None and ordered.args.get("desc")) else 1
        try:
            _to_agg_expr(expr_node, table_resolver(table))  # validate it can be evaluated
        except Exception as exc:  # noqa: BLE001
            raise errors.feature_not_supported(
                f"unsupported expression index key: {expr_node.sql()}"
            ) from exc
        index_name = name_ident.name if name_ident is not None else f"{table.name}_expr_idx"
        hidden_field = f"__expr_{index_name}"
        expr_index = ExprIndex(
            name=index_name,
            expr_sql=expr_node.sql(),
            field=hidden_field,
            type_tag=_infer_scalar_tag(expr_node, table_resolver(table)),
            direction=direction,
        )
        key_spec = {hidden_field: direction}
        where = params.args.get("where")
        partial_filter = (
            _expr_to_filter(where.this, table_resolver(table), None) if where is not None else None
        )
        return CreateIndexPlan(
            collection=table.collection,
            name=index_name,
            key_spec=key_spec,
            unique=bool(stmt.args.get("unique")),
            if_not_exists=bool(stmt.args.get("exists")),
            partial_filter=partial_filter,
            expr_index=expr_index,
        )
    for col in cols:
        ordered = col if isinstance(col, exp.Ordered) else None
        col_node = ordered.this if ordered is not None else col
        if not isinstance(col_node, (exp.Column, exp.Identifier)):
            raise errors.feature_not_supported(
                "a multi-key index mixing expressions and columns is not supported"
            )
        name = _column_name(col_node)
        direction = -1 if (ordered is not None and ordered.args.get("desc")) else 1
        key_spec[table.field_for(name)] = direction
    index_name = name_ident.name if name_ident is not None else _default_index_name(key_spec)
    # A partial-index predicate (``WHERE …``) lowers to a Mongo filter.
    where = params.args.get("where")
    partial_filter = (
        _expr_to_filter(where.this, table_resolver(table), None) if where is not None else None
    )
    include = [_column_name(c) for c in (params.args.get("include") or [])]
    return CreateIndexPlan(
        collection=table.collection,
        name=index_name,
        key_spec=key_spec,
        unique=bool(stmt.args.get("unique")),
        if_not_exists=bool(stmt.args.get("exists")),
        partial_filter=partial_filter,
        include=include,
    )


def _default_index_name(key_spec: dict[str, int]) -> str:
    # Mirror mongod's auto-generated index name: field_dir joined by underscores.
    return "_".join(f"{field}_{direction}" for field, direction in key_spec.items())


def plan_drop_index(stmt: exp.Drop) -> DropIndexPlan:
    return DropIndexPlan(name=stmt.this.name, if_exists=bool(stmt.args.get("exists")))


def insert_target_columns(stmt: exp.Insert, table: TableDef) -> list[str]:
    """The target column names for an INSERT: the explicit ``(a, b)`` list, or
    every column of the table when no list is given."""
    schema = stmt.this
    if isinstance(schema, exp.Schema):
        return [_column_name(c) for c in schema.expressions]
    return [c.name for c in table.columns]


def _is_default_cell(cell: exp.Expression) -> bool:
    """A ``DEFAULT`` keyword in a VALUES tuple (sqlglot parses it as
    ``Var('DEFAULT')``)."""
    return isinstance(cell, exp.Var) and cell.name.upper() == "DEFAULT"


def _insert_cell_value(cell: exp.Expression, subctx: Any = None) -> Any:
    """A VALUES cell: a plain literal, else any constant expression PG allows
    there (``nextval('seq')``, arithmetic, casts …) evaluated by the scalar
    engine — with the real storage in scope when the dispatcher provides it."""
    try:
        return _literal(cell)
    except errors.SQLError:
        from secantus.sql import scalar

        ctx = scalar.ScalarContext(
            storage=getattr(subctx, "storage", None),
            catalog=getattr(subctx, "catalog", None),
            db=getattr(subctx, "db", None) or "",
            session=getattr(subctx, "session", None),
        )
        return scalar.evaluate(cell, _const_scope, ctx)


def _insert_doc(col_names: list[str], raw_values: list[Any], table: TableDef) -> dict[str, Any]:
    """Build one insert doc from raw Python values mapped positionally onto
    ``col_names`` — shared by the VALUES and INSERT…SELECT paths. Coerces per the
    target column's type, maps the PK column to ``_id``, and rejects a NULL (or
    omitted) NOT NULL column."""
    doc: dict[str, Any] = {}
    provided = set()
    for name, raw in zip(col_names, raw_values, strict=True):
        col = table.column(name)
        if col is None:
            if table.reflected:
                # Schema-on-read: an un-sampled field is a valid insert target
                # (the ``_id`` field is still the PK / NOT NULL).
                col = Column(name, "any", name, pk=(name == "_id"), nullable=(name != "_id"))
            else:
                raise errors.undefined_column(name)
        if col.identity == "always":
            raise errors.SQLError(
                "428C9",
                f'cannot insert a non-DEFAULT value into column "{name}" — it is an '
                f"identity column defined as GENERATED ALWAYS",
            )
        if col.generated is not None:
            raise errors.SQLError(
                "428C9",
                f'cannot insert a non-DEFAULT value into column "{name}" — it is a '
                f"generated column",
            )
        if raw is None and not col.nullable:
            raise errors.not_null_violation(name, table.name)
        if col.composite_type is not None and raw is not None:
            value = _composite_value(raw, col)
        else:
            value = typemap.coerce(raw, col.type_tag)
            # A declared char(n) / varchar(n) width is enforced, not ignored —
            # storing an over-length value would violate the column's own
            # schema. Trailing-blank overflow trims, like Postgres.
            value = typemap.enforce_declared_length(
                value, getattr(col, "decl_oid", None), getattr(col, "typmod", -1), col.name
            )
        _set_doc_field(doc, col.field, value, col.type_tag)
        provided.add(name)
    # An omitted column takes its DEFAULT if it has one; otherwise a NOT NULL
    # omission is a violation. A sequence-backed column (SERIAL / DEFAULT
    # nextval) is left unset for the executor to fill (planning is storage-free).
    for col in table.columns:
        if col.name in provided:
            continue
        if col.sequence is not None or col.generated is not None:
            continue  # filled by the executor (sequence draw / computed expr)
        if col.has_default:
            _set_doc_field(doc, col.field, typemap.coerce(col.default, col.type_tag), col.type_tag)
        elif col.default_expr is not None:
            from secantus.sql import scalar

            ctx = scalar.ScalarContext(storage=None, catalog=None, db="", session=None)
            val = scalar.evaluate(_parse_default_expr(col.default_expr), _default_col_scope, ctx)
            _set_doc_field(doc, col.field, typemap.coerce(val, col.type_tag), col.type_tag)
        elif not col.nullable:
            raise errors.not_null_violation(col.name, table.name)
    _canonicalize_composite_id(doc, table)
    return doc


def _composite_value(raw: Any, col: Column) -> dict[str, Any]:
    """Map a ``ROW(…)`` positional value (or an already-named subdocument) onto a
    composite column's named fields, coercing each field to its declared type. A
    field that is itself composite recurses (nested ``ROW(...)`` → nested subdoc)."""
    return _build_composite(raw, col.composite_fields or (), col.composite_type or "record")


def _build_composite(raw: Any, fields: Any, type_name: str) -> dict[str, Any]:
    """Build a composite subdocument from a positional ``ROW`` / list, or an
    already-named dict, against ``fields`` (``(name, tag, subfields)`` entries)."""
    if isinstance(raw, str):
        # A record text literal (``'("(foo,10)",20)'`` — a registered psycopg
        # composite dumper's text form) parses into positional tokens; a nested
        # composite field arrives as a string and recurses through the same
        # parse below.
        try:
            raw = typemap.parse_pg_record_literal(raw)
        except ValueError:
            raise errors.SQLError(
                "22P02", f'malformed record literal for type "{type_name}"'
            ) from None
        if not fields and raw in ([], [None]):
            raw = []  # ``'()'`` for a zero-field composite type
    if isinstance(raw, dict):
        pairs = [(_field_parts(f)[0], raw.get(_field_parts(f)[0])) for f in fields]
    elif isinstance(raw, (list, tuple)):
        if len(raw) != len(fields):
            raise errors.SQLError(
                "22P02",
                f'malformed record literal for type "{type_name}": '
                f"expected {len(fields)} fields, got {len(raw)}",
            )
        pairs = list(zip((_field_parts(f)[0] for f in fields), raw, strict=True))
    else:
        raise errors.SQLError("22P02", f'malformed record literal for type "{type_name}"')
    out: dict[str, Any] = {}
    for (fname, val), entry in zip(pairs, fields, strict=True):
        _name, tag, sub = _field_parts(entry)
        if sub is not None and val is not None:
            out[fname] = _build_composite(val, sub, tag)
        else:
            out[fname] = typemap.coerce(val, tag)
    return out


def _set_doc_field(doc: dict[str, Any], field: str, value: Any, tag: str | None = None) -> None:
    """Assign a column's value to its storage field. A composite-PK column has a
    dotted field (``_id.<name>``) that builds a subdocument ``_id``; a plain field
    is a direct key.

    A ``timestamp`` value carries microseconds a BSON date cannot hold, so its
    sub-millisecond remainder is split off into a hidden companion field — see
    `secantus.sql.subms`, and note the invariant there: the companion is
    resolved on EVERY write, never left stale."""
    if tag in subms.SUBMS_TAGS and "." not in field:
        value = subms.carry_subms(doc, field, value)
    if "." in field:
        set_path(doc, field, value)
    else:
        doc[field] = value


def _canonicalize_composite_id(doc: dict[str, Any], table: TableDef) -> None:
    """Rebuild a composite-PK ``_id`` subdocument with its keys in PK-declaration
    order. Mongo treats ``{a:1,b:2}`` and ``{b:2,a:1}`` as distinct ``_id`` values,
    so a stable key order keeps equality / uniqueness deterministic regardless of
    the INSERT's column order."""
    if not table.composite_pk or not isinstance(doc.get("_id"), dict):
        return
    ordered = {c.name: doc["_id"][c.name] for c in table.pk_columns if c.name in doc["_id"]}
    doc["_id"] = ordered


def copy_row_doc(col_names: list[str], values: list[Any], table: TableDef) -> dict[str, Any]:
    """Build one insert doc for a ``COPY … FROM`` row (values already converted
    from copy-stream text). Reuses the INSERT coercion / default-fill / NOT NULL
    machinery so COPY and INSERT enforce the same rules."""
    return _insert_doc(col_names, values, table)


def plan_insert(stmt: exp.Insert, table: TableDef, subctx: Any = None) -> InsertPlan:
    col_names = insert_target_columns(stmt, table)
    values = stmt.expression
    if values is None and stmt.args.get("default"):
        # ``INSERT INTO t DEFAULT VALUES`` — one row, every column defaulted:
        # equivalent to an empty column list with one empty tuple.
        values = exp.Values(expressions=[exp.Tuple(expressions=[])])
        col_names = []
    if not isinstance(values, exp.Values):
        raise errors.feature_not_supported("INSERT requires a VALUES clause")
    explicit_cols = isinstance(stmt.this, exp.Schema)
    docs: list[dict[str, Any]] = []
    for tup in values.expressions:
        cells = tup.expressions
        if len(cells) > len(col_names):
            raise errors.syntax_error("INSERT has more expressions than target columns")
        if len(cells) < len(col_names) and explicit_cols:
            raise errors.syntax_error("INSERT has more target columns than expressions")
        # Without an explicit column list, Postgres lets a shorter row fill a
        # PREFIX of the table's columns — the rest take their DEFAULT / NULL
        # (pgjdbc's rewritten batch inserts and TimeTest lean on this).
        row_col_names = col_names[: len(cells)]
        # A ``DEFAULT`` keyword cell is treated as an omitted column, so the
        # column's DEFAULT / sequence applies (and an identity ALWAYS column
        # accepts DEFAULT while rejecting a real value).
        row_cols, row_vals = [], []
        for name, cell in zip(row_col_names, cells, strict=True):
            if _is_default_cell(cell):
                continue
            row_cols.append(name)
            row_vals.append(_insert_cell_value(cell, subctx))
        docs.append(_insert_doc(row_cols, row_vals, table))
    return InsertPlan(
        table=table,
        docs=docs,
        returning=_returning_columns(stmt, table),
        on_conflict=_plan_on_conflict(stmt, table),
    )


def plan_insert_rows(stmt: exp.Insert, table: TableDef, rows: list[tuple[Any, ...]]) -> InsertPlan:
    """Plan an ``INSERT … SELECT`` from the source query's already-evaluated rows
    (the engine runs the source query, since planning is storage-free). Each
    row's values map positionally onto the target columns."""
    col_names = insert_target_columns(stmt, table)
    docs = [_insert_doc(col_names, list(row), table) for row in rows]
    return InsertPlan(
        table=table,
        docs=docs,
        returning=_returning_columns(stmt, table),
        on_conflict=_plan_on_conflict(stmt, table),
    )


def _ordered_target(node: exp.Expression) -> exp.Expression:
    """Unwrap an ``ON CONFLICT (col)`` key, which sqlglot wraps in ``Ordered``."""
    return node.this if isinstance(node, exp.Ordered) else node


def _fields_for_constraint(name: str, table: TableDef) -> list[str]:
    """The storage fields of the named UNIQUE / PRIMARY KEY constraint, for an
    ``ON CONFLICT ON CONSTRAINT <name>`` arbiter. Matches a declared UNIQUE
    constraint by name, else the primary key (by its Postgres default name
    ``<table>_pkey``). An unknown name raises ``42704``."""
    for uq in table.unique_constraints:
        if uq.name == name:
            return [table.field_for(col) for col in uq.columns]
    if table.pk_columns and name in (table.pk_constraint_name(), f"{table.name}_pkey"):
        return [c.field for c in table.pk_columns]
    raise errors.SQLError("42704", f'constraint "{name}" for table "{table.name}" does not exist')


def _plan_on_conflict(stmt: exp.Insert, table: TableDef) -> OnConflict | None:
    """Lower an ``ON CONFLICT`` clause to an :class:`OnConflict`, or None.

    Supports ``DO NOTHING`` (with or without a conflict target),
    ``DO UPDATE SET … [WHERE …]`` with a column conflict target, and
    ``ON CONSTRAINT <name>`` — resolved to the named UNIQUE / PRIMARY KEY
    constraint's columns via the table's constraint registry."""
    clause = stmt.args.get("conflict")
    if clause is None:
        return None
    action = clause.args.get("action")
    action_text = (action.name if action is not None else "").upper()
    constraint = clause.args.get("constraint")
    if constraint is not None:
        conflict_fields = _fields_for_constraint(constraint.name, table)
    else:
        conflict_fields = [
            table.field_for(_column_name(_ordered_target(k)))
            for k in (clause.args.get("conflict_keys") or [])
        ]
    if "NOTHING" in action_text:
        return OnConflict(action="nothing", conflict_fields=conflict_fields)
    if "UPDATE" not in action_text:
        raise errors.feature_not_supported(f"unsupported ON CONFLICT action: {action_text}")
    if not conflict_fields:
        # Postgres requires an arbiter index for DO UPDATE — i.e. a conflict
        # target — so it knows which row to update.
        raise errors.syntax_error("ON CONFLICT DO UPDATE requires a conflict target")
    set_exprs: list[tuple[str, str, Any]] = []
    for assignment in clause.args.get("expressions") or []:
        if not isinstance(assignment, exp.EQ):
            raise errors.feature_not_supported(
                f"unsupported ON CONFLICT SET assignment: {assignment.sql()}"
            )
        col_name = _column_name(assignment.this)
        set_exprs.append(
            (table.field_for(col_name), table.type_for(col_name), assignment.expression)
        )
    where_node = clause.args.get("where")
    return OnConflict(
        action="update",
        conflict_fields=conflict_fields,
        set_exprs=set_exprs,
        where=where_node.this if where_node is not None else None,
    )


def _nulls_first(o: exp.Ordered) -> bool:
    """Whether NULLs sort ahead of non-NULLs for this ORDER BY term. sqlglot fills
    ``nulls_first`` with Postgres's default when the clause is implicit (DESC →
    first, ASC → last), and with the explicit value for ``NULLS FIRST/LAST``."""
    nf = o.args.get("nulls_first")
    return bool(nf) if nf is not None else bool(o.args.get("desc"))


def _rewrite_order_by_aliases(stmt: exp.Select, table: TableDef) -> None:
    """In the simple pushdown path an ORDER BY may name a SELECT output alias
    (``SELECT a AS s … ORDER BY s``). Postgres resolves a *standalone* output alias
    to its select-list expression; on this path that expression is always a plain
    input column (a computed alias like ``a + b AS s`` would have routed to the
    evaluated path instead). Rewrite each such bare ORDER BY column to its
    underlying input column so ``_order_terms`` / ``_enum_order_map`` /
    ``_citext_order_set`` resolve it — but only when no real base column of that
    name exists, since a real column of the same name wins (Postgres precedence)."""
    order = stmt.args.get("order")
    if order is None:
        return
    alias_cols = {
        e.alias: e.this
        for e in stmt.expressions
        if isinstance(e, exp.Alias) and isinstance(e.this, exp.Column)
    }
    if not alias_cols:
        return
    for o in order.expressions:
        term = o.this
        if (
            isinstance(term, exp.Column)
            and not term.table
            and table.column(term.name) is None
            and term.name in alias_cols
        ):
            o.set("this", alias_cols[term.name].copy())


def _order_terms(stmt: exp.Expression, table: TableDef) -> list[tuple[str, int, bool]]:
    """ORDER BY lowered to ``(field_path, direction, nulls_first)`` triples — the
    single-table / correlated form, realized by a Postgres-semantics Python sort
    in the executor (so NULL placement matches Postgres, not Mongo sort order)."""
    order = stmt.args.get("order")
    if order is None:
        return []
    terms: list[tuple[str, int, bool]] = []
    for o in order.expressions:
        col = _column_name(o.this)
        terms.append((table.field_for(col), -1 if o.args.get("desc") else 1, _nulls_first(o)))
    return terms


def _citext_order_set(stmt: exp.Expression, table: TableDef) -> set[str]:
    """The ORDER BY field paths whose column is citext — the executor folds those
    to lower case before comparing, so citext sorts case-insensitively."""
    order = stmt.args.get("order")
    if order is None:
        return set()
    out: set[str] = set()
    for o in order.expressions:
        if isinstance(o.this, exp.Column):
            col = table.column(_column_name(o.this))
            if col is not None and col.type_tag == "citext":
                out.add(table.field_for(col.name))
    return out


def _enum_order_map(
    stmt: exp.Expression, table: TableDef, subctx: SubqueryCtx | None
) -> dict[str, list[str]]:
    """For each ORDER BY term that names an enum-typed column, map its field path
    to the enum's declared label list (looked up via the catalog on ``subctx``).
    Enum values are stored as their label text, so without this the executor would
    sort them lexically instead of by declared order."""
    order = stmt.args.get("order")
    if order is None or subctx is None or subctx.catalog is None:
        return {}
    out: dict[str, list[str]] = {}
    for o in order.expressions:
        col = table.column(_column_name(o.this))
        if col is None or col.enum_type is None:
            continue
        enum = subctx.catalog.get_enum(subctx.db, col.enum_type)
        if enum is not None:
            out[table.field_for(col.name)] = list(enum["labels"])
    return out


def _enum_labels_for_column(col: Column | None) -> list[str] | None:
    """The declared label list of an enum-typed column (via the planning-scoped
    catalog on ``_pipeline_subctx``), or None if the column isn't enum-typed / no
    catalog is available. Lets a pipeline ``$sort`` order an enum column by its
    declared order instead of lexically."""
    if col is None or col.enum_type is None:
        return None
    ctx = _pipeline_subctx.get()
    if ctx is None or ctx.catalog is None:
        return None
    enum = ctx.catalog.get_enum(ctx.db, col.enum_type)
    return list(enum["labels"]) if enum else None


def _column_for_order_node(
    node: exp.Expression, amap: dict[str, tuple[str, TableDef]]
) -> Column | None:
    """Resolve an ORDER BY expression to its source ``Column`` across a join's
    alias map (qualified ``a.col`` or an unqualified name found in any table), or
    None if the term isn't a bare column (e.g. a function / arithmetic expr)."""
    if not isinstance(node, exp.Column):
        return None
    name = node.name
    if node.table and node.table in amap:
        return amap[node.table][1].column(name)
    for _alias, (_role, tdef) in amap.items():
        col = tdef.column(name)
        if col is not None:
            return col
    return None


def _source_table_attnum(
    node: exp.Expression, amap: dict[str, tuple[str, TableDef]]
) -> tuple[TableDef, int] | None:
    """Resolve a bare column to its ``(TableDef, 1-based attnum)`` across a join's
    alias map, mirroring `_column_for_order_node`'s qualified-then-unqualified
    lookup so provenance names the same table the value came from. None when the
    term isn't a bare column of one of the joined tables."""
    if not isinstance(node, exp.Column):
        return None
    name = node.name
    if node.table:
        entry = amap.get(node.table)
        if entry is None:
            return None
        tdefs = [entry[1]]
    else:
        tdefs = [tdef for _alias, (_role, tdef) in amap.items()]
    for tdef in tdefs:
        for i, c in enumerate(tdef.columns, start=1):
            if c.name == name:
                return tdef, i
    return None


def _emit_pipeline_sort(
    pipeline: list[dict[str, Any]],
    terms: list[tuple[str, int, bool]],
    enum_labels: dict[str, list[str]] | None = None,
) -> None:
    """Append a NULL-aware ``$sort`` for ``terms`` (``(field, direction,
    nulls_first)``). Mongo's ``$sort`` orders NULL/missing as the lowest value, so
    each term gets a companion ``$cond`` null-rank field sorted ahead of it — that
    places NULLs first or last per ``nulls_first``, independent of direction, the
    way Postgres does. A term whose field is an enum column (``enum_labels[field]``
    gives its declared labels) gets a second companion — its ordinal via
    ``$indexOfArray`` — sorted in place of the raw label text, so the order follows
    the declared enum order. All companions are dropped again after the sort."""
    if not terms:
        return
    enum_labels = enum_labels or {}
    companions: dict[str, Any] = {}
    sort: dict[str, int] = {}
    for k, (name, direction, nulls_first) in enumerate(terms):
        nr = f"__nr_{k}"
        companions[nr] = {
            "$cond": [
                {"$eq": [{"$ifNull": [f"${name}", None]}, None]},
                0 if nulls_first else 1,
                1 if nulls_first else 0,
            ]
        }
        sort[nr] = 1
        labels = enum_labels.get(name)
        if labels is not None:
            eo = f"__eo_{k}"
            companions[eo] = {"$indexOfArray": [labels, f"${name}"]}
            sort[eo] = direction
        else:
            sort[name] = direction
    pipeline.append({"$addFields": companions})
    pipeline.append({"$sort": sort})
    pipeline.append({"$unset": list(companions)})


def _limit_skip(stmt: exp.Expression) -> tuple[int, int]:
    limit_node = stmt.args.get("limit")
    offset_node = stmt.args.get("offset")
    limit = _const_int(limit_node.expression) if limit_node is not None else 0
    skip = _const_int(offset_node.expression) if offset_node is not None else 0
    return limit, skip


def _const_int(node: exp.Expression) -> int:
    """An integer LIMIT / OFFSET operand. PG accepts any constant expression
    there (``OFFSET 1 + 1``, ``LIMIT $1::INTEGER``); fall back to the scalar
    evaluator for anything ``_literal`` can't fold."""
    try:
        value = _literal(node)
    except errors.SQLError:
        from secantus.sql import scalar

        value = scalar.evaluate(
            node,
            _const_scope,
            scalar.ScalarContext(storage=None, catalog=None, db="", session=None),
        )
    if value is None:
        raise errors.feature_not_supported(f"non-constant LIMIT/OFFSET: {node.sql()}")
    return int(_unwrap_num(value))


def _unwrap_num(value: Any) -> Any:
    return int(value) if isinstance(value, _Decimal) else value


def _infer_value_tag(value: Any) -> str:
    if value is None:
        return "text"
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, int):
        return "int4" if -(2**31) <= value < 2**31 else "int8"
    if isinstance(value, float):
        return "float8"
    if isinstance(value, dict) and "interval" in value:
        return "interval"
    if isinstance(value, typemap.RecordValue) or (
        isinstance(value, dict) and value and all(k == f"f{i + 1}" for i, k in enumerate(value))
    ):
        # A ``row(…)`` anonymous record — describes as RECORD (2249) and
        # renders as the ``(a,b)`` record literal, like the SELECT path.
        return "composite"
    return "text"


_LITERAL_NODES = (exp.Literal, exp.Boolean, exp.Null, exp.Neg, exp.Paren)


def _is_pure_literal(node: exp.Expression) -> bool:
    """Whether the subtree is literals all the way down (``- ( 5 )``) — a Neg /
    Paren wrapping a function call (``- NULLIF(…)``) is not, and must go to the
    scalar evaluator rather than ``_literal``."""
    if isinstance(node, (exp.Neg, exp.Paren)):
        return _is_pure_literal(node.this)
    return isinstance(node, (exp.Literal, exp.Boolean, exp.Null))


def _const_scope(node: exp.Expression) -> Any:
    """The scope for a FROM-less SELECT: any column reference is undefined."""
    name = node.name if isinstance(node, exp.Column) else node.sql()
    raise errors.undefined_column(name)


_SINGLE_ROW_AGGS = (exp.Count, exp.Sum, exp.Avg, exp.Min, exp.Max)


def _fold_single_row_aggregates(node: exp.Expression, ctx: Any, rows: int = 1) -> exp.Expression:
    """Fold aggregates in a FROM-less SELECT to constants.

    Postgres feeds a FROM-less aggregation exactly one implicit row, so
    ``COUNT(*)`` is 1, ``COUNT(e)`` is 0/1 by ``e``'s NULL-ness, and
    ``SUM`` / ``AVG`` / ``MIN`` / ``MAX`` of ``e`` are ``e`` itself.

    ``rows=0`` folds over an EMPTY input — the case where a WHERE excludes the
    implicit row (``SELECT count(*) WHERE 1=2``). An ungrouped aggregate still
    produces exactly one output row, with ``COUNT`` 0 and the others NULL.

    Aggregates inside a nested SELECT are left alone: that subquery has its own
    row source, so folding it against the outer implicit row is simply wrong —
    ``SELECT (SELECT count(*) FROM t)`` answered 1 for any ``t``, and
    ``SELECT (SELECT max(a) FROM t)`` raised ``column "a" does not exist``."""
    from secantus.sql import scalar

    node = node.copy()
    nested_aggs = {
        id(agg)
        for sub in node.find_all(exp.Select, exp.Subquery)
        if sub is not node
        for agg in sub.find_all(_SINGLE_ROW_AGGS)
    }

    def fold(n: exp.Expression) -> exp.Expression:
        if not isinstance(n, _SINGLE_ROW_AGGS) or id(n) in nested_aggs:
            return n
        arg = n.this
        if isinstance(arg, exp.Distinct):
            if len(arg.expressions) != 1:
                raise errors.feature_not_supported(f"unsupported aggregate: {n.sql()}")
            arg = arg.expressions[0]
        if isinstance(n, exp.Count):
            if rows == 0:
                return exp.Literal.number(0)
            if arg is None or isinstance(arg, exp.Star):
                return exp.Literal.number(1)
            return exp.Literal.number(0 if scalar.evaluate(arg, _const_scope, ctx) is None else 1)
        if rows == 0:
            return exp.Null()
        return _value_to_node(scalar.evaluate(arg, _const_scope, ctx))

    return node.transform(fold, copy=False)


def _select_has_aggregate(stmt: exp.Select) -> bool:
    """Whether a FROM-less SELECT's projections contain an aggregate — such a
    statement yields exactly one row even when its WHERE is false."""
    for e in stmt.expressions:
        target = e.this if isinstance(e, exp.Alias) else e
        if target.find(exp.AggFunc) is not None:
            return True
    return False


_SEQUENCE_FUNCS = frozenset({"nextval", "currval", "setval", "lastval"})


def _is_sequence_func(node: exp.Expression) -> bool:
    return (
        isinstance(node, (exp.Anonymous, exp.Func))
        and str(getattr(node, "this", node.sql_name())).lower() in _SEQUENCE_FUNCS
    )


_CAST_TYPNAME_BY_OID = {
    1042: "bpchar",
    1043: "varchar",
    1560: "bit",
    1562: "varbit",
    1700: "numeric",
    1083: "time",
    1114: "timestamp",
    1184: "timestamptz",
    1186: "interval",
    1266: "timetz",
    114: "json",
}


def _cast_output_name(target: exp.Expression) -> str | None:
    """PG names an unaliased top-level cast's output column after the target
    type's ``typname`` — ``SELECT 2::int8`` yields a column named ``int8``,
    ``'x'::varchar`` yields ``varchar`` — and constructor keywords after
    themselves (``ARRAY[…]`` → ``array``, ``ROW(…)`` → ``row``; pgtest float
    corpus). None when the ``?column?`` fallback stands."""
    if isinstance(target, exp.Array):
        return "array"
    if isinstance(target, exp.Anonymous):
        if str(target.this).upper() == "ROW":
            return "row"
        # PG names an unaliased function-call column after the function
        # (``SELECT jsonb_path_query(…)`` → column ``jsonb_path_query``).
        return str(target.this).rsplit(".", 1)[-1].lower() or None
    if not isinstance(target, exp.Cast) or target.to is None:
        return None
    # PG's FigureColname recurses into the cast's OPERAND first: a name the
    # operand supplies wins over the type name, so ``n::int4`` is ``n`` and
    # ``f()::int`` is ``f`` — only a nameless operand (a literal, an
    # expression) falls back to the typname (pgtest parameter_description).
    inner = target.this
    while isinstance(inner, exp.Paren):
        inner = inner.this
    if isinstance(inner, exp.Column):
        return inner.name or None
    if isinstance(inner, (exp.Cast, exp.Array, exp.Anonymous)):
        nested = _cast_output_name(inner)
        if nested is not None:
            return nested
    ident = typemap.cast_type_identity(target.to)
    if ident is not None and ident[0] in _CAST_TYPNAME_BY_OID:
        return _CAST_TYPNAME_BY_OID[ident[0]]
    if target.to.this == exp.DataType.Type.USERDEFINED:
        # PG names an unaliased cast to a user-defined type (enum, composite,
        # domain) after the TYPE name — ``SELECT 'hi'::te`` yields a column
        # named ``te`` (pgtest enum corpus).
        kind = target.to.args.get("kind")
        if kind is not None:
            name = str(getattr(kind, "this", kind)).strip('"').lower()
            # The quoted-"char" cast rewrites to the pg_char_1 sentinel
            # pre-parse; its typname is the bare word (oid 18).
            return "char" if name == "pg_char_1" else name
    tag = typemap.type_tag_for_sql(target.to)
    if tag is None:
        return None
    if tag == "char1":
        return "char"  # pg_type.typname for oid 18 is the bare word
    if tag.endswith("[]"):
        # PG names an array-cast column after the ELEMENT typname:
        # ``'{a}'::text[]`` yields a column named ``text``.
        return tag[:-2]
    return tag


def plan_constant_select(
    stmt: exp.Select,
    session: Any,
    storage: Any = None,
    catalog: Any = None,
    db: str | None = None,
) -> ConstantSelectPlan:
    """Plan a FROM-less ``SELECT <expr>, ... [WHERE <const>]``.

    Literals are read directly; session/info functions (``version()``,
    ``current_database()``, ``current_setting(...)``, ...) resolve against the
    connection ``session``; any other constant expression (arithmetic, ``||``,
    function calls, ``CASE`` …) is evaluated by the scalar evaluator against an
    empty scope. A constant ``WHERE`` that evaluates false yields zero rows.
    """
    from secantus.sql import functions, scalar

    if stmt.args.get("group") or stmt.args.get("joins"):
        raise errors.feature_not_supported("FROM-less SELECT supports only constant projections")
    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    where = stmt.args.get("where")
    passes = where is None or scalar._truthy(scalar.evaluate(where.this, _const_scope, ctx))
    # An ungrouped aggregate always produces exactly one row, even when the
    # WHERE excludes the implicit input row — ``SELECT count(*) WHERE 1=2`` is
    # 0, not "no rows" (and ``SELECT 0/count(*) WHERE 1=2`` therefore divides
    # by zero, which is how pgjdbc's batch tests inject a runtime failure).
    aggregated = _select_has_aggregate(stmt)
    emit = passes or aggregated
    agg_rows = 1 if passes else 0
    columns: list[tuple[str, str, Any]] = []
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        target = e.this if isinstance(e, exp.Alias) else e
        if target.find(exp.AggFunc) is not None:
            if alias is None and isinstance(target, _SINGLE_ROW_AGGS):
                alias = target.key  # Postgres names a bare aggregate output "count" etc.
            target = _fold_single_row_aggregates(target.copy(), ctx, rows=agg_rows)
        if isinstance(target, _LITERAL_NODES) and _is_pure_literal(target):
            value = _literal(target)
            # Tag from the AST, not the Python value — a decimal constant
            # (``SELECT 1.5``) is numeric in Postgres, which the float can't show.
            columns.append(
                (
                    alias or _cast_output_name(target) or "?column?",
                    _infer_scalar_tag(target, _const_scope),
                    value,
                )
            )
        elif _is_sequence_func(target):
            # nextval / currval / setval / lastval need storage + session state,
            # so they go through the scalar evaluator (not the storage-free
            # session-function path).
            fname = str(getattr(target, "this", target.sql_name())).lower()
            value = scalar.evaluate(target, _const_scope, ctx)
            columns.append((alias or fname, "int8", value))
        elif (udf := _udf_lookup(target, catalog, db)) is not None:
            # A user-defined function (CREATE FUNCTION) needs storage/catalog, so
            # it goes through the scalar evaluator, not the session-function path.
            value = scalar.evaluate(target, _const_scope, ctx)
            tag = udf.get("return_tag") or _infer_scalar_tag(target, _const_scope)
            columns.append((alias or _udf_call_name(target), tag, value))
        elif functions.is_scalar_function(target):
            try:
                fname, value, tag = functions.evaluate_scalar(target, session)
                columns.append((alias or fname, tag, value))
            except errors.SQLError as exc:
                if exc.sqlstate != "0A000":
                    raise
                # Not a session/info function after all (e.g. a user-declared
                # range type's constructor) — the full scalar evaluator decides.
                value = scalar.evaluate(target, _const_scope, ctx)
                columns.append(
                    (
                        alias or _cast_output_name(target) or "?column?",
                        _infer_scalar_tag(target, _const_scope),
                        value,
                    )
                )
        else:
            value = scalar.evaluate(target, _const_scope, ctx)
            columns.append(
                (
                    alias or _cast_output_name(target) or "?column?",
                    _infer_scalar_tag(target, _const_scope),
                    value,
                )
            )
    pg_oids: list[int | None] = [None] * len(columns)
    typmods: list[int] = [-1] * len(columns)
    for i, e in enumerate(stmt.expressions):
        override = _constant_enum_override(e, ctx) or _constant_composite_extra_override(e, ctx)
        if override is not None:
            tag, oid = override
            pg_oids[i] = oid
            if tag is not None:
                name, _old_tag, value = columns[i]
                columns[i] = (name, tag, value)
            continue
        # ``null::varchar(42)`` — a modifier-bearing cast target describes with
        # its distinct oid + typmod (varchar/bpchar differ from text even bare).
        target = e.this if isinstance(e, exp.Alias) else e
        if isinstance(target, exp.Cast):
            identity = typemap.cast_type_identity(target.to)
            if identity is not None:
                pg_oids[i], typmods[i] = identity
    return ConstantSelectPlan(columns=columns, emit=emit, pg_oids=pg_oids, typmods=typmods)


def _constant_enum_override(e: exp.Expression, ctx: Any) -> tuple[str | None, int] | None:
    """A ``(tag_override, oid)`` for a constant-select output that casts to a
    declared enum (``'ok'::mood`` → the minted enum oid, tag unchanged) or an
    enum array (``%s::mood[]`` → the paired array oid, tag ``text[]`` so the
    value renders as an array literal)."""
    from secantus.sql import scalar
    from secantus.sql.catalog import USER_TYPE_ARRAY_OID_OFFSET

    target = e.this if isinstance(e, exp.Alias) else e
    if not isinstance(target, exp.Cast):
        return None
    oid = scalar.enum_cast_oid(target.to, ctx)
    if oid is not None:
        return (None, oid)
    if scalar.enum_array_cast_element(target.to, ctx) is not None:
        inner = (target.to.args.get("expressions") or [None])[0]
        elem_oid = scalar.enum_cast_oid(inner, ctx)
        if elem_oid is not None:
            return ("text[]", elem_oid + USER_TYPE_ARRAY_OID_OFFSET)
    if scalar._composite_cast_target(target.to, ctx) is not None:
        # ``'(a,b)'::testcomp`` — describe with the minted composite oid so a
        # registered psycopg loader fires; the value is a record subdoc, tagged
        # composite so it renders as ``(a,b)``.
        from secantus.sql import virtual
        from secantus.sql.catalog import fold_type_name

        name = fold_type_name(target.to.sql(dialect="postgres"))
        oid = virtual._composite_oids(ctx.db, ctx.catalog).get(name)
        if oid is not None:
            return ("composite", oid)
    rng = scalar._range_type_cast_target(target.to, ctx)
    if rng is not None:
        from secantus.sql.catalog import fold_type_name

        doc, _elem = rng
        name = fold_type_name(target.to.sql(dialect="postgres"))
        oid = doc.get("multirange_oid") if name == doc.get("multirange") else doc.get("oid")
        if oid:
            return (None, oid)
    return None


def _constant_composite_extra_override(
    e: exp.Expression, ctx: Any
) -> tuple[str | None, int] | None:
    """Overrides beyond plain casts: an ``array[…::testcomp]`` describes with
    the composite's paired array oid; ``('…'::testcomp).field`` types as the
    field's declared tag."""
    if ctx is None or getattr(ctx, "catalog", None) is None or getattr(ctx, "db", None) is None:
        return None
    from secantus.sql import scalar, virtual
    from secantus.sql.catalog import USER_TYPE_ARRAY_OID_OFFSET, fold_type_name

    target = e.this if isinstance(e, exp.Alias) else e
    if isinstance(target, exp.Cast):
        # A cast to a table's row type (``'(foo)'::mytype`` / ``::mytype[]``)
        # describes with the rowtype's pg_type oid (its array with the paired
        # array oid) so psycopg's registered TypeInfo loaders fire.
        dt = target.to
        is_arr = isinstance(dt, exp.DataType) and dt.this == exp.DataType.Type.ARRAY
        inner_dt = (dt.args.get("expressions") or [dt])[0] if is_arr else dt
        if isinstance(inner_dt, exp.DataType) and inner_dt.this == exp.DataType.Type.USERDEFINED:
            tname = fold_type_name(inner_dt.sql(dialect="postgres"))
            if getattr(ctx.catalog, "get", None) is not None and ctx.catalog.get(ctx.db, tname):
                rowtype = virtual._table_rowtype_oids(ctx.db, ctx.catalog).get(tname)
                if rowtype is not None:
                    if is_arr:
                        return (None, rowtype + virtual._ROWTYPE_ARRAY_OID_OFFSET)
                    return (None, rowtype)
    if isinstance(target, exp.Anonymous):
        # A user-declared range type's constructor (``testrange(lo, hi)``) —
        # describe with the minted oid so a registered loader fires.
        fname = str(target.this).lower()
        getter = getattr(ctx.catalog, "get_range_type", None)
        doc = getter(ctx.db, fname) if getter is not None else None
        if doc is not None:
            oid = doc.get("multirange_oid") if fname == doc.get("multirange") else doc.get("oid")
            if oid:
                return (None, oid)
    if isinstance(target, exp.Array) and target.expressions:
        elems = target.expressions
        if all(isinstance(el, exp.Cast) for el in elems):
            elem_enum_oid = scalar.enum_cast_oid(elems[0].to, ctx)
            if elem_enum_oid is not None and all(
                scalar.enum_cast_oid(el.to, ctx) == elem_enum_oid for el in elems
            ):
                # ``array['sad'::mood, …]`` — the minted array companion oid,
                # tag text[] so the labels render as an array literal.
                return ("text[]", elem_enum_oid + USER_TYPE_ARRAY_OID_OFFSET)
        if all(
            isinstance(el, exp.Cast) and scalar._composite_cast_target(el.to, ctx) is not None
            for el in elems
        ):
            name = fold_type_name(elems[0].to.sql(dialect="postgres"))
            oid = virtual._composite_oids(ctx.db, ctx.catalog).get(name)
            if oid is not None:
                return ("composite[]", oid + USER_TYPE_ARRAY_OID_OFFSET)
    if isinstance(target, exp.Dot) and isinstance(target.this, exp.Paren):
        inner = target.this.this
        if isinstance(inner, exp.Cast):
            fields = scalar._composite_cast_target(inner.to, ctx)
            if fields is not None:
                fname = str(target.expression.name)
                for entry in fields:
                    if entry[0] == fname:
                        sub = entry[2] if len(entry) > 2 else None
                        tag = "composite" if sub is not None else entry[1]
                        return (tag, typemap.PG_OID.get(tag, 25))
    return None


def _where_has_udf(node: exp.Expression, catalog: Any, db: str | None) -> bool:
    """True if the WHERE tree calls a user-defined function — those don't lower to
    a Mongo filter, so the whole predicate is evaluated per-row."""
    return any(_udf_lookup(call, catalog, db) is not None for call in node.find_all(exp.Anonymous))


def _udf_call_name(node: exp.Expression) -> str:
    inner = node.expression if isinstance(node, exp.Dot) else node
    return str(inner.this).lower().rsplit(".", 1)[-1]


def _udf_lookup(node: exp.Expression, catalog: Any, db: str | None) -> dict[str, Any] | None:
    """A stored user-defined function matching ``node`` (an ``Anonymous`` call), or
    None — used to route ``CREATE FUNCTION`` calls through the scalar evaluator."""
    inner = node.expression if isinstance(node, exp.Dot) else node
    if not isinstance(inner, exp.Anonymous) or catalog is None or db is None:
        return None
    name = str(inner.this).lower().rsplit(".", 1)[-1]
    getter = getattr(catalog, "get_function", None)
    if getter is None:
        return None
    try:
        return getter(db, name, len(inner.expressions))
    except Exception:  # noqa: BLE001 — a lookup failure just means "not a UDF"
        return None


def rewrite_expr_index_refs(stmt: exp.Select, table: TableDef) -> None:
    """Rewrite each occurrence of an indexed expression (``ExprIndex.expr_sql``) in
    the WHERE clause into a reference to that index's hidden field, so the query
    plans through the normal single-field-index path (the field holds the
    precomputed value). SELECT / GROUP BY / HAVING / ORDER BY are left untouched:
    the output shape and grouping stay unaffected, and ORDER BY on the expression
    already sorts correctly via per-row evaluation on the pipeline path (the hidden
    field is projected away before a pipeline ``$sort`` could reach it)."""
    eis = getattr(table, "expr_indexes", None)
    if not eis:
        return
    targets = {ei.expr_sql: ei.field for ei in eis}
    node = stmt.args.get("where")
    if node is None:
        return
    for n in list(node.find_all(exp.Func, exp.Binary, exp.Paren)):
        repl = targets.get(n.sql())
        if repl is not None and n.parent is not None:
            n.replace(exp.column(repl))


def plan_select(stmt: exp.Select, table: TableDef, subctx: SubqueryCtx | None = None) -> SelectPlan:
    if stmt.args.get("joins"):
        raise errors.feature_not_supported("JOIN is not supported yet")
    if stmt.args.get("group") or stmt.args.get("having"):
        raise errors.feature_not_supported("GROUP BY / HAVING is not supported yet")
    if stmt.args.get("distinct"):
        raise errors.feature_not_supported("SELECT DISTINCT is not supported yet")

    rewrite_expr_index_refs(stmt, table)
    _rewrite_order_by_aliases(stmt, table)
    filt = _where_filter(stmt, table, subctx)
    order = _order_terms(stmt, table)
    limit, skip = _limit_skip(stmt)

    count_alias = _count_star_alias(stmt)
    if count_alias is not None:
        return SelectPlan(
            table=table,
            filter=filt,
            order=order,
            limit=limit,
            skip=skip,
            count_star=True,
            count_alias=count_alias,
        )
    return SelectPlan(
        table=table,
        filter=filt,
        order=order,
        limit=limit,
        skip=skip,
        out_columns=_select_out_columns(stmt, table),
        enum_orders=_enum_order_map(stmt, table, subctx),
        citext_orders=_citext_order_set(stmt, table),
    )


def _count_star_alias(stmt: exp.Select) -> str | None:
    """The output alias if this SELECT is a sole ``COUNT(*)`` (no GROUP BY), else
    None."""
    exprs = stmt.expressions
    if len(exprs) != 1 or not isinstance(exprs[0], (exp.Count, exp.Alias)):
        return None
    inner = exprs[0].this if isinstance(exprs[0], exp.Alias) else exprs[0]
    if isinstance(inner, exp.Count) and isinstance(inner.this, exp.Star):
        alias = exprs[0].alias if isinstance(exprs[0], exp.Alias) else "count"
        return alias or "count"
    return None


def _out_columns(exprs: list[exp.Expression], table: TableDef) -> list[tuple[str, Column]]:
    """The projected ``(output_name, Column)`` list for a column / ``*`` / jsonb
    projection over ``table`` — shared by the SELECT pushdown, correlated-WHERE,
    and ``RETURNING`` plans."""
    out_columns: list[tuple[str, Column]] = []
    for e in exprs:
        if isinstance(e, exp.Star):
            for col in table.columns:
                out_columns.append((col.name, col))
            continue
        alias = e.alias if isinstance(e, exp.Alias) else None
        target = e.this if isinstance(e, exp.Alias) else e
        if isinstance(target, _JSONB_CLASSES):
            # jsonb navigation (doc->>'k', doc->'a'->>'b', doc #> '{a,b}') reads a
            # dotted path; surface it as a synthetic column.
            path, tag = _field(target, table_resolver(table))
            out_name = alias or "?column?"
            out_columns.append((out_name, Column(out_name, tag, path, pk=False, nullable=True)))
            continue
        cname = _column_name(target)
        col = table.column(cname)
        if col is None:
            if table.reflected:
                # Schema-on-read: any selected field is valid on a reflected table.
                col = Column(cname, "any", cname, pk=False, nullable=True)
            else:
                raise errors.undefined_column(cname)
        out_columns.append((alias or cname, col))
    return out_columns


def _select_out_columns(stmt: exp.Select, table: TableDef) -> list[tuple[str, Column]]:
    """The projected columns for a plain (non-aggregate) SELECT."""
    return _out_columns(stmt.expressions, table)


def _returning_columns(
    stmt: exp.Expression, table: TableDef
) -> list[tuple[str, Column, Any]] | None:
    """The projected items for a write statement's ``RETURNING`` clause, or None
    when there's no ``RETURNING``. Each item is ``(name, Column, expr)``: ``expr``
    is None for a plain column / ``*`` / jsonb path (read straight from the doc);
    for a computed expression (arithmetic, ``||``, function calls, ``CASE`` …) it
    is the raw node, evaluated per returned row by the executor."""
    returning = stmt.args.get("returning")
    if returning is None:
        return None
    items: list[tuple[str, Column, Any]] = []
    resolve = table_resolver(table)
    for e in returning.expressions:
        if isinstance(e, exp.Star):
            items.extend((col.name, col, None) for col in table.columns)
            continue
        alias = e.alias if isinstance(e, exp.Alias) else None
        target = e.this if isinstance(e, exp.Alias) else e
        if isinstance(target, exp.Column):
            cname = _column_name(target)
            col = table.column(cname)
            if col is None:
                if table.reflected:
                    col = Column(cname, "any", cname, pk=False, nullable=True)
                else:
                    raise errors.undefined_column(cname)
            items.append((alias or cname, col, None))
        elif isinstance(target, _JSONB_CLASSES):
            path, tag = _field(target, resolve)
            out_name = alias or "?column?"
            items.append((out_name, Column(out_name, tag, path, pk=False, nullable=True), None))
        elif isinstance(target, exp.Anonymous) and str(target.this).lower() == "merge_action":
            # ``merge_action()`` — only valid in a MERGE RETURNING; the executor
            # resolves it to the row's action ('INSERT' / 'UPDATE' / 'DELETE').
            out_name = alias or "merge_action"
            items.append(
                (out_name, Column(out_name, "text", out_name, pk=False, nullable=True), target)
            )
        else:
            # A computed expression — evaluated per returned row (field unused).
            out_name = alias or _cast_output_name(target) or "?column?"
            tag = _infer_scalar_tag(target, resolve)
            items.append(
                (out_name, Column(out_name, tag, out_name, pk=False, nullable=True), target)
            )
    return items


def _where_has_text_cast_comparison(node: exp.Expression, table: TableDef | None = None) -> bool:
    """Whether the WHERE compares a COLUMN cast to text against something.

    The pushdown compares the stored value and does not apply the cast, so
    `WHERE n::text = '2'` lowered to a filter on the raw int and matched
    NOTHING (Postgres returns the row). The scalar evaluator does apply it
    (`scalar._eval_cast` renders numbers, decimals, Decimal128 and booleans with
    Postgres' spellings), so routing these to per-row evaluation is correct; the
    cost is losing index pushdown for a predicate that could not have used it
    correctly anyway.

    Deliberately narrow, because per-row evaluation is a whole different
    execution path:

    * the cast operand must be a COLUMN. A cast on a LITERAL needs nothing — the
      value is already text — and claiming those broke SQLAlchemy's reflection,
      which filters with `relkind = ANY(ARRAY[CAST('v' AS VARCHAR)])`;
    * a column that is ALREADY text is skipped too: casting text to text cannot
      change the comparison, so there is nothing to fix and no reason to pay for
      the slower path.
    """
    for cmp_node in node.find_all(exp.EQ, exp.NEQ, exp.GT, exp.GTE, exp.LT, exp.LTE):
        for side in (cmp_node.this, cmp_node.expression):
            inner = side
            while isinstance(inner, exp.Paren):
                inner = inner.this
            if not isinstance(inner, exp.Cast):
                continue
            if typemap.type_tag_for_sql(inner.to) != "text":
                continue
            operand = inner.this
            while isinstance(operand, exp.Paren):
                operand = operand.this
            if not isinstance(operand, exp.Column):
                continue
            if table is not None:
                col = table.column(_column_name(operand))
                if col is not None and col.type_tag == "text":
                    continue
            return True
    return False


def where_needs_per_row(
    stmt: exp.Select,
    table: TableDef | None = None,
    catalog: Any = None,
    db: str | None = None,
) -> bool:
    """Whether the WHERE clause must be evaluated per-row in Python rather than
    pushed down as a Mongo filter: it contains an ``EXISTS`` predicate, a
    correlated subquery (one that references the outer row), or a range operator
    (``@>`` / ``<@`` / ``&&`` over a range column — the scalar evaluator handles
    range semantics that don't lower to a Mongo filter). Non-correlated ``IN`` /
    scalar ``= (SELECT …)`` subqueries stay on the fast pushdown path."""
    where = stmt.args.get("where")
    if where is None:
        return False
    node = where.this
    if node.find(exp.Exists) is not None:
        return True
    if catalog is not None and _where_has_udf(node, catalog, db):
        return True
    if table is not None and _where_has_range_predicate(node, table):
        return True
    if _where_has_text_cast_comparison(node, table):
        return True
    if table is not None and _where_has_net_predicate(node, table):
        return True
    if table is not None and _where_has_bit_predicate(node, table):
        return True
    if table is not None and _where_has_geo_predicate(node, table):
        return True
    if table is not None and _where_has_hstore_predicate(node, table):
        return True
    if table is not None and _where_has_array_predicate(node, table):
        return True
    if table is not None and _where_has_jsonb_contained_predicate(node, table):
        return True
    # A full-text ``@@`` match (``exp.MatchAgainst``) is evaluated per-row too.
    if getattr(exp, "MatchAgainst", None) is not None and node.find(exp.MatchAgainst) is not None:
        return True
    if any(_subquery_has_outer_ref(sub) for sub in node.find_all(exp.Select)):
        return True
    # Anything the pushdown lowering can't express — column arithmetic inside a
    # comparison (``- b + a > -10``), a constant-LHS ``IN`` (``1 IN (2)``),
    # ``NOT BETWEEN`` over expressions — is evaluated per-row rather than
    # rejected. Dry-run the lowering to find out; the probe is skipped for
    # subquery-bearing WHEREs (they need a live SubqueryCtx and are already
    # routed by the checks above or supported by the pushdown path).
    if table is not None and node.find(exp.Select) is None:
        try:
            _where_filter(stmt, table)
        except errors.SQLError:
            return True
    return False


def _is_net_operand(operand: exp.Expression, table: TableDef) -> bool:
    """Whether ``operand`` resolves to a network-address value — a net-typed
    column or a cast to ``inet`` / ``cidr`` / ``macaddr``."""
    if isinstance(operand, exp.Column):
        col = table.column(_column_name(operand))
        return col is not None and col.type_tag in typemap._NET_TAGS
    if isinstance(operand, exp.Cast) and operand.to is not None:
        return operand.to.sql(dialect="postgres").lower().strip() in typemap._NET_TAGS
    return False


def _where_has_net_predicate(node: exp.Expression, table: TableDef) -> bool:
    """True if ``node`` contains a network operator — ``<<`` / ``>>`` (subnet
    containment) or ``&&`` (overlap) — whose operand is a net-typed column or a
    net cast. Those don't lower to a Mongo filter and need per-row evaluation."""
    net_ops = [exp.BitwiseLeftShift, exp.BitwiseRightShift, exp.ArrayOverlaps]
    for op in node.find_all(*net_ops):
        if _is_net_operand(op.this, table) or _is_net_operand(op.expression, table):
            return True
    return False


def _where_has_bit_predicate(node: exp.Expression, table: TableDef) -> bool:
    """True if ``node`` contains a bit-string literal (``B'…'``) or a bitwise
    operator over a bit-typed column — those don't lower to a Mongo filter and
    need per-row evaluation."""
    if node.find(exp.BitString) is not None:
        return True
    bit_ops = [
        exp.BitwiseAnd,
        exp.BitwiseOr,
        exp.BitwiseXor,
        exp.BitwiseNot,
        exp.BitwiseLeftShift,
        exp.BitwiseRightShift,
    ]
    for op in node.find_all(*bit_ops):
        for operand in (op.this, op.expression):
            if isinstance(operand, exp.Column):
                col = table.column(_column_name(operand))
                if col is not None and col.type_tag in typemap._BIT_TAGS:
                    return True
    return False


def _is_geo_operand(operand: exp.Expression, table: TableDef) -> bool:
    if isinstance(operand, exp.Column):
        col = table.column(_column_name(operand))
        return col is not None and col.type_tag in typemap._GEO_TAGS
    if isinstance(operand, exp.Cast) and operand.to is not None:
        return typemap.type_tag_for_sql(operand.to) in typemap._GEO_TAGS
    return False


def _where_has_geo_predicate(node: exp.Expression, table: TableDef) -> bool:
    """True if ``node`` contains a geometric distance (``<->``) or an ``@>`` / ``<@``
    / ``&&`` whose operand is a geo-typed column or a geo cast — those need per-row
    evaluation."""
    if getattr(exp, "Distance", None) is not None and node.find(exp.Distance) is not None:
        return True
    for op in node.find_all(exp.ArrayContainsAll, exp.ArrayContainedBy, exp.ArrayOverlaps):
        if _is_geo_operand(op.this, table) or _is_geo_operand(op.expression, table):
            return True
    return False


def _is_hstore_operand(operand: exp.Expression, table: TableDef) -> bool:
    """Whether ``operand`` resolves to an hstore — an hstore-typed column or a cast
    to ``hstore``."""
    if isinstance(operand, exp.Column):
        col = table.column(_column_name(operand))
        return col is not None and col.type_tag == "hstore"
    if isinstance(operand, exp.Cast) and operand.to is not None:
        return typemap.type_tag_for_sql(operand.to) == "hstore"
    return False


def _where_has_hstore_predicate(node: exp.Expression, table: TableDef) -> bool:
    """True if ``node`` has an hstore ``@>`` / ``<@`` containment or a ``?`` / ``?&``
    / ``?|`` key-exists op over an hstore column / cast — those don't lower to a
    Mongo filter and need per-row evaluation. (A ``->`` lookup *does* push down: it
    resolves to the dotted ``<col>.hstore.<key>`` field path in ``_field``.)"""
    contain_ops = (exp.ArrayContainsAll, exp.ArrayContainedBy)
    exists_ops = (exp.JSONBContains, exp.JSONBContainsAllTopKeys, exp.JSONBContainsAnyTopKeys)
    for op in node.find_all(*contain_ops):
        if _is_hstore_operand(op.this, table) or _is_hstore_operand(op.expression, table):
            return True
    return any(_is_hstore_operand(op.this, table) for op in node.find_all(*exists_ops))


def _is_array_operand(operand: exp.Expression, table: TableDef) -> bool:
    """Whether ``operand`` resolves to a Postgres array — an array-typed column, an
    ``ARRAY[...]`` constructor, or a cast to an ``<type>[]`` array type."""
    if isinstance(operand, exp.Paren):
        operand = operand.this
    if isinstance(operand, exp.Array):
        return True
    if isinstance(operand, exp.Column):
        col = table.column(_column_name(operand))
        return col is not None and typemap.is_array_tag(col.type_tag)
    if isinstance(operand, exp.Cast) and operand.to is not None:
        return typemap.is_array_tag(typemap.type_tag_for_sql(operand.to))
    return False


def _is_array_field(operand: exp.Expression, table: TableDef) -> bool:
    """Whether ``operand`` is an array-typed stored column."""
    if isinstance(operand, exp.Column):
        col = table.column(_column_name(operand))
        return col is not None and typemap.is_array_tag(col.type_tag)
    return False


def _is_nonempty_array_literal(operand: exp.Expression) -> bool:
    """Whether ``operand`` is a non-empty ``ARRAY[...]`` literal. (An *empty* array
    literal is excluded: ``arr @> '{}'`` is true for every row, which ``$all: []``
    would not express — those stay on the per-row path.)"""
    if isinstance(operand, exp.Paren):
        operand = operand.this
    return isinstance(operand, exp.Array) and len(operand.expressions) > 0


def _array_index_operands(op: exp.Expression, table: TableDef) -> bool:
    """Whether ``op`` is an array ``@>`` / ``&&`` in the index-eligible shape
    ``field @> ARRAY[...]`` / ``field && ARRAY[...]`` (``&&`` is symmetric) — i.e. a
    stored array column against a non-empty array literal, which lowers to a Mongo
    ``$all`` / ``$in`` filter."""
    if isinstance(op, exp.ArrayContainsAll):
        cands = [(op.this, op.expression)]
    elif isinstance(op, exp.ArrayOverlaps):
        cands = [(op.this, op.expression), (op.expression, op.this)]
    else:
        return False
    return any(_is_array_field(f, table) and _is_nonempty_array_literal(lit) for f, lit in cands)


def _array_index_filter(op: exp.Expression, resolve: Resolve) -> dict[str, Any] | None:
    """The Mongo filter for an index-eligible array ``@>`` / ``&&``: ``field &&
    ARRAY[a, b]`` (overlaps) → ``{field: {$in: [a, b]}}`` and ``field @> ARRAY[a,
    b]`` (contains all) → ``{$and: [{field: a}, {field: b}]}``. Each bare-equality
    on a multikey array field is an "array contains element" test, so both forms
    are exact for Postgres array semantics *and* light up a multikey index (a plain
    ``$all`` does not, in this storage planner). Returns None when the operator
    isn't this shape (jsonb / range / field-vs-field / empty literal)."""
    is_overlap = isinstance(op, exp.ArrayOverlaps)
    cands = [(op.this, op.expression)]
    if is_overlap:
        cands.append((op.expression, op.this))
    for fnode, lit in cands:
        if not (_is_field_node(fnode) and _is_nonempty_array_literal(lit)):
            continue
        field, tag = _field(fnode, resolve)
        if not typemap.is_array_tag(tag):
            continue
        elem_tag = typemap.array_element_tag(tag)
        elems = [typemap.coerce(_literal(e), elem_tag) for e in _array_elements(lit)]
        if is_overlap:  # && : share ≥1 element
            return {field: {"$in": elems}}
        # @> : contains every element — an $and of multikey equalities (a lone
        # element collapses to a bare equality).
        if len(elems) == 1:
            return {field: elems[0]}
        return {"$and": [{field: e} for e in elems]}
    return None


def _where_has_array_predicate(node: exp.Expression, table: TableDef) -> bool:
    """True if ``node`` contains an ``@>`` / ``<@`` / ``&&`` whose operand is a
    Postgres array that must be evaluated per-row. The index-eligible shapes
    ``field @> ARRAY[...]`` and ``field && ARRAY[...]`` are excluded — they lower to
    a Mongo ``$all`` / ``$in`` filter (see ``_array_index_filter``); everything else
    (``<@``, field-vs-field, empty literal, ``const @> field``) has different
    semantics from jsonb containment and is handled per-row by the scalar
    evaluator."""
    for op in node.find_all(exp.ArrayContainsAll, exp.ArrayContainedBy, exp.ArrayOverlaps):
        if not (_is_array_operand(op.this, table) or _is_array_operand(op.expression, table)):
            continue
        if _array_index_operands(op, table):
            continue  # lowers to $all / $in
        return True
    return False


def _where_has_jsonb_contained_predicate(node: exp.Expression, table: TableDef) -> bool:
    """True if ``node`` contains an ``@>`` / ``<@`` in a shape that can't lower to a
    Mongo filter and so needs per-row evaluation. Only two shapes push down —
    ``field @> const`` and ``const <@ field`` (both a subset-of-the-stored-value
    lookup handled by ``_jsonb_contains_filter``); the reverse shapes
    (``field <@ const``, ``const @> field``) and field-vs-field comparisons fall
    through to a COLLSCAN + residual predicate evaluated by the scalar pass (which
    handles jsonb / array / range / hstore / geo containment). Typed range / array /
    net / hstore / geo operators are already routed to the residual by their own
    ``_where_has_*`` checks, so this generic shape test is only additive."""
    for op in node.find_all(exp.ArrayContainsAll, exp.ArrayContainedBy):
        if _array_index_operands(op, table):
            continue  # an array @> that lowers to $all — not a jsonb residual
        if isinstance(op, exp.ArrayContainsAll):  # @>
            if _is_field_node(op.this) and _is_literalish(op.expression):
                continue  # field @> const — pushes down
        elif _is_literalish(op.this) and _is_field_node(op.expression):
            continue  # const <@ field — pushes down (== field @> const)
        return True
    return False


_RANGE_LIKE_TAGS = typemap._RANGE_TAGS | typemap._MULTIRANGE_TAGS


def _where_has_range_predicate(node: exp.Expression, table: TableDef) -> bool:
    """True if ``node`` contains an ``@>`` / ``<@`` / ``&&`` whose operand is a
    range- / multirange-typed column or constructor — those need per-row
    evaluation (COLLSCAN + residual), not a jsonb-containment pushdown."""
    for op in node.find_all(exp.ArrayContainsAll, exp.ArrayContainedBy, exp.ArrayOverlaps):
        for operand in (op.this, op.expression):
            if isinstance(operand, exp.Column):
                col = table.column(_column_name(operand))
                if col is not None and col.type_tag in _RANGE_LIKE_TAGS:
                    return True
            if isinstance(operand, exp.Anonymous) and str(operand.this).lower() in _RANGE_LIKE_TAGS:
                return True
    return False


def plan_correlated_select(
    stmt: exp.Select, table: TableDef, subctx: SubqueryCtx | None = None
) -> CorrelatedSelectPlan:
    """Plan a single-table SELECT whose WHERE needs per-row evaluation (EXISTS /
    correlated subquery). The whole WHERE is carried verbatim and evaluated by
    the executor against each candidate row via the scalar evaluator."""
    if stmt.args.get("joins") or stmt.args.get("group") or stmt.args.get("having"):
        raise errors.feature_not_supported(
            "correlated subqueries are supported only in a single-table SELECT"
        )
    if stmt.args.get("distinct"):
        raise errors.feature_not_supported("SELECT DISTINCT is not supported yet")
    order = _order_terms(stmt, table)
    limit, skip = _limit_skip(stmt)
    from_node = stmt.find(exp.From)
    outer_alias = from_node.this.alias or None if from_node is not None else None
    count_alias = _count_star_alias(stmt)
    return CorrelatedSelectPlan(
        table=table,
        where=stmt.args["where"].this,
        out_columns=[] if count_alias is not None else _select_out_columns(stmt, table),
        order=order,
        limit=limit,
        skip=skip,
        count_star=count_alias is not None,
        count_alias=count_alias or "count",
        outer_alias=outer_alias,
        enum_orders=_enum_order_map(stmt, table, subctx),
    )


def _composite_subfield_target(target: exp.Expression, table: TableDef):
    """A composite subfield SET target ``col.field`` parses as ``Column(this=field,
    table=col)``; return ``(composite_column, subfield, field_tag, subfields)`` when
    ``col`` is a composite column, else None. ``subfields`` is the field's nested
    field list when it is itself composite, else None."""
    if not (isinstance(target, exp.Column) and target.table):
        return None
    comp_col = table.column(target.table)
    if comp_col is None or comp_col.composite_fields is None:
        return None
    subfield = target.name
    match = next(
        (_field_parts(f) for f in comp_col.composite_fields if _field_parts(f)[0] == subfield),
        None,
    )
    if match is None:
        raise errors.SQLError(
            "42703", f'column "{subfield}" not found in composite type "{comp_col.composite_type}"'
        )
    return comp_col, subfield, match[1], match[2]


def plan_update(stmt: exp.Update, table: TableDef) -> UpdatePlan:
    set_doc: dict[str, Any] = {}
    # Companion fields to remove — see the invariant in `secantus.sql.subms`.
    unset_fields: list[str] = []
    rekey = False
    computed: list[tuple[str, str, Any]] = []
    for assign in stmt.expressions:
        if not isinstance(assign, exp.EQ):
            raise errors.feature_not_supported(f"unsupported SET item: {assign.sql()}")
        # ``SET col.field = v`` writes into the composite subdocument at ``col.field``.
        subfield_target = _composite_subfield_target(assign.this, table)
        if subfield_target is not None:
            comp_col, subfield, tag, subfields = subfield_target
            if comp_col.pk:
                # Changing a composite-PK subfield re-keys the row (rewrites ``_id``).
                rekey = True
            raw = _try_literal(assign.expression)
            if raw is _LITERAL_SENTINEL:  # a per-row expression, not a literal
                computed.append((f"{comp_col.field}.{subfield}", tag, assign.expression))
                continue
            if subfields is not None and raw is not None:
                value = _build_composite(raw, subfields, tag)
            else:
                value = typemap.coerce(raw, tag)
            set_doc[f"{comp_col.field}.{subfield}"] = value
            continue
        col_name = _column_name(assign.this)
        col = table.column(col_name)
        if col is None:
            if table.reflected:
                # Schema-on-read: any field is a valid SET target (still can't
                # rewrite the PK, which maps to the immutable Mongo ``_id``).
                col = Column(col_name, "any", col_name, pk=(col_name == "_id"), nullable=True)
            else:
                raise errors.undefined_column(col_name)
        if col.pk:
            if table.reflected:
                # A reflected collection's ``_id`` is the real Mongo key — re-keying
                # a schema-on-read doc isn't modelled.
                raise errors.feature_not_supported("updating the primary key is not supported")
            rekey = True  # changing a declared PK column re-keys the row
        if col.generated is not None:
            # A generated column can only be set to DEFAULT (which recomputes it);
            # any other value is rejected. The executor recomputes it either way.
            if not _is_default_cell(assign.expression):
                raise errors.SQLError(
                    "428C9", f'column "{col_name}" can only be updated to DEFAULT'
                )
            continue
        raw = _try_literal(assign.expression)
        if raw is _LITERAL_SENTINEL:  # ``SET col = <expr>`` — evaluated per row
            computed.append((col.field, col.type_tag, assign.expression))
            continue
        if raw is None and not col.nullable:
            raise errors.not_null_violation(col_name, table.name)
        if col.composite_type is not None and raw is not None:
            set_doc[col.field] = _composite_value(raw, col)
        elif col.decl_oid in (typemap.BPCHAR_OID, typemap.VARCHAR_OID):
            set_doc[col.field] = typemap.enforce_declared_length(
                typemap.coerce(raw, col.type_tag), col.decl_oid, col.typmod, col.name
            )
        elif col.type_tag in subms.SUBMS_TAGS:
            stored, companion, remainder = subms.subms_update_ops(
                col.field, typemap.coerce(raw, col.type_tag)
            )
            set_doc[col.field] = stored
            if remainder is not None:
                set_doc[companion] = remainder
            else:
                # No remainder: the companion must GO, or the row keeps the
                # microseconds of whatever it held before this update.
                unset_fields.append(companion)
        else:
            set_doc[col.field] = typemap.coerce(raw, col.type_tag)
    return UpdatePlan(
        table=table,
        filter=_where_filter(stmt, table),
        update=(
            {"$set": set_doc, "$unset": {f: "" for f in unset_fields}}
            if unset_fields
            else {"$set": set_doc}
        ),
        returning=_returning_columns(stmt, table),
        rekey=rekey,
        computed=computed,
    )


def plan_delete(stmt: exp.Delete, table: TableDef) -> DeletePlan:
    return DeletePlan(
        table=table,
        filter=_where_filter(stmt, table),
        returning=_returning_columns(stmt, table),
    )


def _py_elem_tag(value: Any) -> str:
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, (bytes, bytearray)):
        return "bytea"
    if isinstance(value, int):
        return "int8"
    if isinstance(value, float):
        return "float8"
    return "text"


def _value_to_node(value: Any) -> exp.Expression:
    if value is None:
        return exp.Null()
    if isinstance(value, bool):
        return exp.Boolean(this=value)
    if isinstance(value, float) and not _math.isfinite(value):
        # ``repr(inf)`` is not a parseable numeric literal; carry the Postgres
        # spelling through a float8 cast (the cast evaluator converts it back).
        text = "NaN" if _math.isnan(value) else ("-Infinity" if value < 0 else "Infinity")
        return exp.Cast(this=exp.Literal.string(text), to=exp.DataType.build("double"))
    if isinstance(value, (int, float)):
        return exp.Literal.number(repr(value))
    if isinstance(value, _Decimal):
        # A numeric parameter must stay numeric — a bare string literal would
        # compare as text (``'…'::numeric = $1`` false) and lose the declared
        # type. The cast round-trips the exact digits.
        return exp.Cast(this=exp.Literal.string(str(value)), to=exp.DataType.build("decimal"))
    if isinstance(value, _dt.datetime):
        # Same reasoning as Decimal: a datetime param substituted as a bare
        # string would compare as text against a real datetime and be silently
        # false. The cast re-parses the exact ISO text back to a datetime.
        target = "timestamptz" if value.tzinfo is not None else "timestamp"
        return exp.Cast(
            this=exp.Literal.string(value.isoformat(sep=" ")), to=exp.DataType.build(target)
        )
    if isinstance(value, dict) and "interval" in value:
        # An interval subdoc (binary or text interval param) — carried through
        # an ``::interval`` cast so it stays a typed interval value.
        from secantus.sql import intervals as _intervals

        return exp.Cast(
            this=exp.Literal.string(_intervals.render(value)),
            to=exp.DataType.build("interval"),
        )
    if isinstance(value, typemap.TaggedText):
        # A typed-text parameter (range/multirange, or a user composite) — the
        # ``::tag`` cast coerces the text into the structured value so equality
        # against another value compares subdocs, not str-vs-dict.
        try:
            dtype = exp.DataType.build(value.tag, dialect="postgres")
        except Exception:  # noqa: BLE001 — a user-defined type name needs udt
            dtype = exp.DataType.build(value.tag, dialect="postgres", udt=True)
        return exp.Cast(this=exp.Literal.string(str(value)), to=dtype)
    if isinstance(value, typemap.JsonText):
        # A json/jsonb-declared parameter — substitute as a ``::jsonb`` cast so
        # the raw JSON text parses into a real JSON value and types as json (a
        # bare string literal would stay text and double-encode on output).
        return exp.Cast(this=exp.Literal.string(str(value)), to=exp.DataType.build("jsonb"))
    if isinstance(value, typemap.DateText):
        return exp.Cast(this=exp.Literal.string(str(value)), to=exp.DataType.build("date"))
    if isinstance(value, typemap.TimeText):
        return exp.Cast(this=exp.Literal.string(str(value)), to=exp.DataType.build("time"))
    if isinstance(value, typemap.TimeTzText):
        return exp.Cast(this=exp.Literal.string(str(value)), to=exp.DataType.build("timetz"))
    if isinstance(value, _dt.date) and not isinstance(value, _dt.datetime):
        # A binary date parameter decodes to a date object — same ::date cast.
        return exp.Cast(this=exp.Literal.string(value.isoformat()), to=exp.DataType.build("date"))
    if isinstance(value, _dt.time):
        target = "timetz" if value.tzinfo is not None else "time"
        return exp.Cast(this=exp.Literal.string(value.isoformat()), to=exp.DataType.build(target))
    if isinstance(value, (bytes, bytearray, memoryview)):
        # A ``bytea`` parameter — the ``::bytea`` cast re-parses the hex text
        # into bytes so equality against another bytea value compares bytes,
        # not text-vs-bytes (a bare literal broke ``$1 = set_byte(…)``).
        return exp.Cast(
            this=exp.Literal.string("\\x" + bytes(value).hex()),
            to=exp.DataType.build("bytea", dialect="postgres"),
        )
    if isinstance(value, typemap.TypedList):
        # A typed array parameter (a text-format int2[]/inet[]/… decoded at
        # Bind) — the ``::tag[]`` cast re-parses the literal into a typed list
        # so equality against ``array[…]`` values compares element-wise.
        literal = typemap._render_pg_array(value, value.elem_tag)
        try:
            dtype = exp.DataType.build(f"{value.elem_tag}[]", dialect="postgres")
        except Exception:  # noqa: BLE001 — non-keyword element names (cidr) need udt
            dtype = exp.DataType.build(f"{value.elem_tag}[]", dialect="postgres", udt=True)
        return exp.Cast(this=exp.Literal.string(literal), to=dtype)
    if isinstance(value, (list, tuple)):
        # A binary array parameter decodes to a Python list; carry it as the
        # Postgres array text literal (str() would embed the Python repr).
        elem = next((v for v in value if v is not None), None)
        if isinstance(elem, typemap.TaggedText):
            # A binary range[]/multirange[] param — elements decoded to their
            # text literals; rebuild the array literal and cast to elem.tag[].
            literal = typemap._render_pg_array(
                [str(v) if v is not None else None for v in value], "text"
            )
            return exp.Cast(
                this=exp.Literal.string(literal),
                to=exp.DataType.build(f"{elem.tag}[]", dialect="postgres"),
            )
        if isinstance(elem, typemap.JsonText):
            # json[]/jsonb[] param: elements are raw JSON text — parse them so
            # the array literal renders each element as JSON (quoted, escaped)
            # exactly like a client's own text dump of the same array.
            parsed = [None if v is None else json.loads(str(v)) for v in value]
            return exp.Literal.string(typemap._render_pg_array(parsed, "json"))
        if isinstance(elem, (bytes, bytearray, memoryview)):
            # bytea[] param — the ::bytea[] cast re-parses each hex element to
            # bytes (same reasoning as the scalar bytea cast above).
            literal = typemap._render_pg_array(
                ["\\x" + bytes(v).hex() if v is not None else None for v in value], "text"
            )
            return exp.Cast(
                this=exp.Literal.string(literal),
                to=exp.DataType.build("bytea[]", dialect="postgres"),
            )
        return exp.Literal.string(typemap._render_pg_array(value, _py_elem_tag(elem)))
    return exp.Literal.string(str(value))


#: Sentinel for a NULL bound with declared type VOID (oid 2278). pgjdbc's
#: CallableStatement passes a function's OUT placeholder as a real argument
#: bound as ``NULL::void`` (``select * from f($1,$2)`` for ``{?= call f(?)}``)
#: — PostgreSQL's function resolution drops void arguments for exactly this
#: convention, and so do we: the placeholder is removed from the call's
#: argument list at substitution time.
VOID_BIND = object()


def substitute_parameters(stmt: exp.Expression, values: list[Any]) -> exp.Expression:
    """Replace ``$1`` / ``$2`` ... placeholders with bound literal nodes.

    Bound values arrive as Python scalars (text params decode to ``str``); the
    column-type coercion in the planner then converts them to the right BSON
    type, so a text ``"5"`` bound into an ``int8`` column lands as ``Int64(5)``.
    """
    stmt = stmt.copy()
    bound: list[tuple[exp.Parameter, exp.Expression]] = []
    for param in stmt.find_all(exp.Parameter):
        try:
            idx = int(param.name) - 1
        except (TypeError, ValueError) as exc:
            raise errors.syntax_error(f"invalid bind parameter ${param.name}") from exc
        if idx < 0 or idx >= len(values):
            raise errors.syntax_error(f"bind parameter ${param.name} has no value")
        if values[idx] is VOID_BIND:
            parent = param.parent
            if isinstance(parent, (exp.Anonymous, exp.Func)) and param in (
                parent.expressions or []
            ):
                param.pop()  # PG drops void args from the call (JDBC OUT slot)
                continue
            bound.append((param, exp.Null()))
            continue
        bound.append((param, _value_to_node(values[idx])))
    # Swap each placeholder for its bound literal. Replacing them one at a time
    # through ``Expression.replace`` is quadratic — sqlglot re-parents *every*
    # sibling in the argument list on each call — so a statement binding many
    # parameters under one node (pgjdbc's rewritten batch INSERT binds tens of
    # thousands) spends O(N**2) here. Collect the swaps per argument list and
    # apply each list once instead.
    edits: dict[tuple[int, str], tuple[exp.Expression, str, list]] = {}
    for param, node in bound:
        parent = param.parent
        container = parent.args.get(param.arg_key) if parent is not None else None
        if parent is None:
            stmt = node  # the whole statement was a bare ``$1``
            continue
        if not isinstance(container, list) or param.index is None:
            param.replace(node)  # a scalar argument slot — already O(1)
            continue
        key = (id(parent), param.arg_key)
        entry = edits.get(key)
        if entry is None:
            # Not ``setdefault``: it would evaluate the list copy on every
            # parameter, reintroducing the quadratic cost this avoids.
            entry = edits[key] = (parent, param.arg_key, container[:])
        entry[2][param.index] = node
    for parent, arg_key, new_list in edits.values():
        parent.set(arg_key, new_list)
    # A statement sqlglot keeps as a raw Command (``DECLARE c CURSOR FOR
    # SELECT $1::text``) carries its ``$N`` placeholders inside the tail
    # *text*, invisible to find_all — substitute them textually with rendered
    # literals. (A ``$N`` inside a quoted string in the tail would be
    # substituted too; cursor declarations don't hit that in practice.)
    if isinstance(stmt, exp.Command) and values:
        tail = stmt.args.get("expression")
        text = str(tail.this) if isinstance(tail, exp.Literal) else None
        if text is not None and re.search(r"\$\d+", text):

            def _sub(m: re.Match) -> str:
                idx = int(m.group(1)) - 1
                if idx < 0 or idx >= len(values):
                    raise errors.syntax_error(f"bind parameter ${m.group(1)} has no value")
                return _value_to_node(values[idx]).sql(dialect="postgres")

            stmt.set("expression", exp.Literal.string(re.sub(r"\$(\d+)", _sub, text)))
    return stmt


def parameter_count(stmt: exp.Expression) -> int:
    """Highest ``$N`` index referenced by ``stmt`` (0 if none)."""
    indices = []
    for param in stmt.find_all(exp.Parameter):
        try:
            indices.append(int(param.name))
        except (TypeError, ValueError):
            continue
    # ``CALL proc($1)`` is kept as a raw Command; its ``$N`` placeholders live in
    # the tail text, invisible to ``find_all`` — scan them so the extended
    # protocol binds the parameter. Restricted to CALL: other Command tails
    # (``PREPARE … AS SELECT $1``) carry ``$N`` that belong to an EMBEDDED query,
    # not to the command's own bind parameters.
    if isinstance(stmt, exp.Command) and str(stmt.this).upper() == "CALL":
        tail = stmt.args.get("expression")
        if isinstance(tail, exp.Literal):
            indices += [int(n) for n in re.findall(r"\$(\d+)", str(tail.this))]
    return max(indices, default=0)


_COMPARISON_NODES = (exp.EQ, exp.NEQ, exp.GT, exp.GTE, exp.LT, exp.LTE)

# Functions taking VARIADIC "any" — an untyped parameter passed directly can't
# be resolved by parse analysis (PG raises 42P18 indeterminate_datatype).
_VARIADIC_ANY_FUNCS = frozenset({"concat", "concat_ws", "format"})


def parameter_numbering_gap(stmt: exp.Expression | None) -> int | None:
    """The lowest parameter number a statement SKIPS (``SELECT $2 > 0`` never
    mentions ``$1``), or None. Nothing can type the missing one, so PG rejects
    the Parse with 42P18 (pgtest parameter_description).

    Must run on the RAW parsed statement: later rewrites (``pg_typeof($1)``
    folds its argument to a type name) remove parameters from the AST and
    would look like a gap.
    """
    if stmt is None:
        return None
    used: set[int] = set()
    for param in stmt.find_all(exp.Parameter):
        try:
            used.add(int(param.name))
        except (TypeError, ValueError):
            continue
    if not used:
        return None
    missing = [n for n in range(1, max(used)) if n not in used]
    return missing[0] if missing else None


def indeterminate_parameter(stmt: exp.Expression | None, oids: list[int]) -> int | None:
    """The 1-based index of a parameter PG cannot type at Parse, or None.

    Two cases, both 42P18 in real Postgres: an untyped parameter passed
    directly to a VARIADIC "any" function (``concat($1, $2)``), and a GAP in
    the parameter numbering — ``SELECT $2 > 0`` never mentions ``$1``, so
    nothing can type it (pgtest parameter_description)."""
    if stmt is None:
        return None
    # A bare parameter as a CASE result with NO typed sibling branch has no
    # type context at all — PG can't resolve the CASE's type (pgtest
    # parameter_description). A CASE with another concrete branch resolves
    # from that branch, so those are left alone.
    for case in stmt.find_all(exp.Case):
        results = [i.args.get("true") for i in case.args.get("ifs") or []]
        if case.args.get("default") is not None:
            results.append(case.args["default"])
        results = [r for r in results if r is not None]
        params = [r for r in results if isinstance(r, exp.Parameter)]
        if not params or len(params) != len(results):
            continue  # no bare-parameter result, or a typed sibling resolves it
        for r in params:
            try:
                idx = int(r.name)
            except (TypeError, ValueError):
                continue
            if idx >= 1 and (idx > len(oids) or not oids[idx - 1]):
                return idx
    calls: list[exp.Expression] = [
        c for c in stmt.find_all(exp.Anonymous) if str(c.this).lower() in _VARIADIC_ANY_FUNCS
    ]
    calls += list(stmt.find_all(exp.Concat, exp.ConcatWs))
    for call in calls:
        for arg in call.expressions:
            if isinstance(arg, exp.Parameter):
                try:
                    idx = int(arg.name)
                except (TypeError, ValueError):
                    continue
                if idx >= 1 and (idx > len(oids) or not oids[idx - 1]):
                    return idx
    return None


#: Text-only functions: PG has no numeric/date overload, so a parameter whose
#: type another use already pinned to a non-text type makes the call resolve to
#: nothing — 42883 undefined_function.
_TEXT_ONLY_FUNC_NODES = (exp.Lower, exp.Upper, exp.Trim, exp.Length, exp.Initcap)
#: Non-text parameter oids that cannot feed a text-only function.
_NON_TEXT_PARAM_OIDS = frozenset({16, 17, 20, 21, 23, 26, 700, 701, 1082, 1083, 1114, 1184, 1700})
_OID_PG_NAME = {
    16: "boolean",
    17: "bytea",
    20: "bigint",
    21: "smallint",
    23: "integer",
    26: "oid",
    700: "real",
    701: "double precision",
    1082: "date",
    1083: "time",
    1114: "timestamp",
    1184: "timestamp with time zone",
    1700: "numeric",
}


def _column_param_oid(cname: str, stmt: exp.Expression, catalog: Any, db: str) -> int | None:
    """The parameter oid PG assigns from a comparison/assignment against column
    ``cname`` in ``stmt``'s tables, or None. ``"char"`` (oid 18) deliberately
    yields None: the pgtest char corpus pins such a parameter at text."""
    for tbl in stmt.find_all(exp.Table):
        t = catalog.get(db, tbl.name)
        col = t.column(cname) if t is not None else None
        if col is None:
            continue
        tag = col.type_tag
        if tag == "char1":
            return None
        if getattr(col, "json_plain", False):
            return 114
        oid = typemap.PG_OID.get(tag)
        if oid is None and typemap.is_array_tag(tag):
            oid = typemap._ARRAY_PG_OID.get(typemap.array_element_tag(tag))
        return oid
    return None


def conflicting_parameter_use(
    stmt: exp.Expression | None, oids: list[int]
) -> tuple[str, str] | None:
    """``(function, type_name)`` when a parameter whose type is already pinned
    to a non-text type is passed to a text-only function, else None.

    PG gives each parameter ONE type, so ``select lower($1) … $1::int`` can't
    resolve ``lower(integer)`` and fails 42883 (pgtest
    parameter_description). We only flag types something else PINNED — an
    untyped parameter still defaults to text and resolves fine."""
    if stmt is None:
        return None
    for call in stmt.find_all(*_TEXT_ONLY_FUNC_NODES):
        arg = call.this
        while isinstance(arg, exp.Paren):
            arg = arg.this
        if not isinstance(arg, exp.Parameter):
            continue
        try:
            idx = int(arg.name) - 1
        except (TypeError, ValueError):
            continue
        if 0 <= idx < len(oids) and oids[idx] in _NON_TEXT_PARAM_OIDS:
            fname = type(call).__name__.lower()
            return (fname, _OID_PG_NAME.get(oids[idx], str(oids[idx])))
    return None


def infer_parameter_types(
    stmt: exp.Expression | None,
    declared: list[int],
    *,
    catalog: Any = None,
    db: str | None = None,
) -> list[int]:
    """Fill in undeclared (oid 0) parameter types from the statement's AST at
    Parse time, the way real Postgres' parse analysis does. A client that binds
    a value in BINARY format with no declared type (psycopg's ``Range(empty=
    True)`` dump) needs the server to know the type before Bind decodes the
    payload. Two sound contexts: the parameter under an explicit cast
    (``$1::int4range``), and the parameter compared against a cast operand
    (``'empty'::int4range = $1``). Everything else stays 0 (→ text)."""
    if stmt is None:
        return declared
    count = parameter_count(stmt)
    # An explicitly-declared ``unknown`` (oid 705) is treated like an undeclared
    # parameter: PG's parse analysis resolves it from context (a target column,
    # a cast, a compared operand) rather than echoing 705 back in
    # ParameterDescription (pgtest ``unknown`` corpus).
    oids = [0 if o == 705 else o for o in declared] + [0] * (count - len(declared))
    # INSERT: an untyped parameter in a VALUES cell takes the target column's
    # type, like PG's parse analysis (``insert into t (j) values ($1)`` with a
    # jsonb column types $1 jsonb).
    if isinstance(stmt, exp.Insert) and catalog is not None and db is not None:
        schema = stmt.this
        colnames: list[str] | None = None
        tname = None
        if isinstance(schema, exp.Schema):
            tname = schema.this.name
            colnames = [c.name for c in schema.expressions]
        elif isinstance(schema, exp.Table):
            tname = schema.name
        table = catalog.get(db, tname) if tname else None
        values = stmt.expression
        if table is not None and isinstance(values, exp.Values):
            if colnames is None:
                colnames = [c.name for c in table.columns]
            for tup in values.expressions:
                cells = tup.expressions if isinstance(tup, exp.Tuple) else [tup]
                for i, cell in enumerate(cells):
                    while isinstance(cell, exp.Paren):
                        cell = cell.this
                    if not isinstance(cell, exp.Parameter) or i >= len(colnames):
                        continue
                    try:
                        idx = int(cell.name) - 1
                    except (TypeError, ValueError):
                        continue
                    if not 0 <= idx < count or oids[idx]:
                        continue
                    col = table.column(colnames[i])
                    if col is not None:
                        oids[idx] = (
                            114
                            if getattr(col, "json_plain", False)
                            else typemap.PG_OID.get(col.type_tag, 0)
                        )
    # UPDATE ... SET col = $N — the assignment target's column type, like PG.
    if isinstance(stmt, exp.Update) and catalog is not None and db is not None:
        for assign in stmt.args.get("expressions") or []:
            if not isinstance(assign, exp.EQ):
                continue
            target, value = assign.this, assign.expression
            while isinstance(value, exp.Paren):
                value = value.this
            if not (isinstance(target, exp.Column) and isinstance(value, exp.Parameter)):
                continue
            try:
                idx = int(value.name) - 1
            except (TypeError, ValueError):
                continue
            if 0 <= idx < count and not oids[idx]:
                oid = _column_param_oid(target.name, stmt, catalog, db)
                if oid:
                    oids[idx] = oid
    for param in stmt.find_all(exp.Parameter):
        try:
            idx = int(param.name) - 1
        except (TypeError, ValueError):
            continue
        if not 0 <= idx < count or oids[idx]:
            continue
        node: exp.Expression = param
        # Unwrap parens between the parameter and its typing context.
        while isinstance(node.parent, exp.Paren):
            node = node.parent
        parent = node.parent
        target: exp.DataType | None = None
        if isinstance(parent, exp.Cast) and parent.this is node:
            target = parent.to
        elif isinstance(parent, _COMPARISON_NODES):
            other = parent.expression if parent.this is node else parent.this
            if isinstance(other, exp.Paren):
                other = other.this
            if isinstance(other, exp.Cast):
                target = other.to
            elif isinstance(other, exp.Anonymous):
                # A range/multirange constructor operand types the parameter
                # (``int8range($1, $2) = $3``).
                fname = str(other.this).lower()
                if fname in typemap._RANGE_TAGS or fname in typemap._MULTIRANGE_TAGS:
                    oids[idx] = typemap.PG_OID[fname]
                    continue
            elif isinstance(other, exp.Column) and catalog is not None and db is not None:
                # ``col = $N`` types the parameter as the COLUMN's type, like
                # PG's parse analysis (pgtest parameter_description reads uuid
                # 2950 and timestamptz 1184 this way; citext/ltree corpora read
                # their extension oids). ``"char"`` is the one exception the
                # corpus pins at text — PG resolves that comparison through
                # text rather than the one-byte type.
                oid = _column_param_oid(other.name, stmt, catalog, db)
                if oid:
                    oids[idx] = oid
        if target is None:
            continue
        # ``$1::REGCLASS`` and friends parse as ObjectIdentifier, not
        # DataType — map the reg-pseudotype oids directly (the pgtest
        # bind_and_resolve corpus reads ParameterDescription byte-for-byte).
        if isinstance(target, exp.ObjectIdentifier):
            reg_oid = {
                "REGCLASS": 2205,
                "REGTYPE": 2206,
                "REGPROC": 24,
                "REGPROCEDURE": 2202,
                "REGNAMESPACE": 4089,
                "REGROLE": 4096,
                "OID": 26,
            }.get(str(target.this).upper())
            if reg_oid:
                oids[idx] = reg_oid
            continue
        # ``$1::JSON`` / ``$1::JSON[]`` keep the plain-json identities
        # (114 / 199) — the collapsed tag would report jsonb's 3802/3807.
        ident = typemap.cast_type_identity(target)
        if ident is not None and ident[0] in (114, 199):
            oids[idx] = ident[0]
            continue
        tag = typemap.type_tag_for_sql(target)
        oid = typemap.PG_OID.get(tag) if tag is not None else None
        if oid is None and typemap.is_array_tag(tag):
            oid = typemap._ARRAY_PG_OID.get(typemap.array_element_tag(tag))
        if (
            oid is None
            and catalog is not None
            and db is not None
            and isinstance(target, exp.DataType)
            and target.this == exp.DataType.Type.USERDEFINED
        ):
            # ``$1::r`` where ``r`` is a user-declared type (composite / enum /
            # domain / range) — resolve to its minted oid so a BINARY param
            # decodes through the type's record layout, not as raw text.
            from secantus.sql import virtual

            kind = target.args.get("kind")
            uname = str(getattr(kind, "this", kind)).strip('"') if kind is not None else None
            if uname:
                oid = virtual.user_type_oid(db, catalog, uname)
        if oid:
            oids[idx] = oid
    return oids


# ---------------------------------------------------------------------------
# Pipeline path: JOIN / GROUP BY / aggregates -> an aggregation pipeline
# ---------------------------------------------------------------------------


@dataclass
class RawDerived:
    """A derived-table sub-plan carried as a raw statement (a set operation or
    a ``VALUES`` list in FROM) — the executor runs it through the engine and
    optionally renames the output columns positionally (``AS alias(c1, c2)``)."""

    stmt: Any
    names: list[str] | None = None


@dataclass
class DerivedTable:
    """A ``(SELECT ...) AS alias`` join source, materialized before the main
    pipeline runs. ``name`` is the ephemeral collection the executor registers
    the sub-plan's rows under (and the join's ``$lookup`` reads from)."""

    name: str
    plan: Any  # a sub-plan (PipelineSelectPlan / EvaluatedSelectPlan)
    columns: list[tuple[str, str]]  # (output_name, type_tag)


@dataclass
class LateralJoin:
    """A *rich* ``[LEFT] JOIN LATERAL (subquery) alias`` whose subquery has its own
    JOIN / GROUP BY / aggregate / DISTINCT — too much to lower to a correlated
    ``$lookup`` sub-pipeline. It runs nested-loop on the evaluated path: for each
    outer row the executor substitutes the correlated outer columns with that row's
    values and runs ``select`` as a plain (non-correlated) inner query.

    ``inner_aliases`` are the subquery's own FROM/JOIN aliases (a column qualified by
    anything else is an outer correlation to substitute); ``side`` is ``"LEFT"`` for a
    ``LEFT JOIN LATERAL`` (keep an outer row whose subquery is empty, null-padded) or
    ``""`` for INNER/CROSS (drop it)."""

    alias: str
    tdef: TableDef
    select: exp.Select
    side: str
    inner_aliases: set[str | None]


@dataclass
class PipelineSelectPlan:
    base_collection: str
    base_filter: dict[str, Any]
    pipeline: list[dict[str, Any]]
    out_columns: list[tuple[str, str]]  # (output_name, type_tag)
    # Output positions whose value is a user enum: index -> enum type name. The
    # string tag stays "text" (labels are stored as text); the executor/Describe
    # resolve the minted enum OID from this map for RowDescription.
    out_enum_types: dict[int, str] = field(default_factory=dict)
    derived: list[DerivedTable] = field(default_factory=list)
    # A WHERE that references the outer row (EXISTS / correlated subquery) can't
    # lower to a Mongo ``$match``; it's carried here and evaluated in Python by the
    # executor. ``residual_split`` is how many leading pipeline stages run *before*
    # the filter — 0 for a single-table GROUP BY (filter the base docs, then group),
    # or the join-prefix length for a JOIN + GROUP BY (join, filter the joined rows,
    # then group), so the survivors are what gets grouped.
    residual_where: exp.Expression | None = None
    residual_resolve: Resolve | None = None
    residual_split: int = 0
    # Computed GROUP BY keys the aggregation engine can't lower (``GROUP BY
    # col = ascii(x)``): synthetic field name -> the scalar AST, evaluated in
    # Python per base doc before the pipeline runs. ``pre_eval_resolve`` maps
    # its column refs to doc field paths.
    pre_eval_fields: dict[str, exp.Expression] = field(default_factory=dict)
    pre_eval_resolve: Resolve | None = None
    # Ordered-set aggregates (percentile_cont / percentile_disc / mode) collect
    # their ORDER BY values via a ``$push`` accumulator, then the executor computes
    # the scalar in Python (the aggregation engine has no ``$sortArray``). Each
    # entry is ``(output_field, kind, fraction)`` — ``fraction`` None for mode.
    post_aggregates: list[tuple[str, str, float | None]] = field(default_factory=list)
    # Per output position, the ``(TableDef, 1-based attnum)`` the column came
    # from, or None for a computed / unattributable output. A join has no single
    # base table, so this is how RowDescription still carries each output's base
    # column identity (pgtest's row_description asserts it across a JOIN).
    out_sources: list[tuple[Any, int] | None] = field(default_factory=list)


@dataclass
class EvaluatedSelectPlan:
    """A join whose SELECT list / ORDER BY has scalar expressions (functions,
    CASE, correlated subqueries) that can't be lowered to a ``$project``.

    The ``pipeline`` performs the joins + WHERE and yields the full joined docs;
    the executor evaluates each output expression in Python per row (via
    ``secantus.sql.scalar``), then applies DISTINCT / ORDER BY / LIMIT.
    """

    base_collection: str
    base_filter: dict[str, Any]
    pipeline: list[dict[str, Any]]  # join + where; NO final $project
    out_columns: list[tuple[str, str]]  # (output_name, type_tag)
    out_exprs: list[exp.Expression]  # parallel to out_columns; AST per output
    resolve: Resolve  # join resolver: Column node -> (field_path, tag)
    order: list[tuple[exp.Expression, int, bool]]  # (expr, direction, nulls_first)
    distinct: bool
    limit: int
    skip: int
    out_enum_types: dict[int, str] = field(default_factory=dict)  # see PipelineSelectPlan
    derived: list[DerivedTable] = field(default_factory=list)
    # A correlated / EXISTS WHERE that couldn't lower to a ``$match`` — evaluated
    # per joined row (via ``resolve`` as the outer scope) after the pipeline.
    where: exp.Expression | None = None
    # A correlated / EXISTS WHERE that must filter the *base* docs **before** the
    # pipeline's ``$group`` (WHERE happens before grouping) — evaluated per base doc
    # via ``pre_where_resolve``. Distinct from ``where`` (a post-group residual, e.g.
    # a HAVING subquery).
    pre_where: exp.Expression | None = None
    pre_where_resolve: Resolve | None = None
    # How many leading pipeline stages (a JOIN's $lookup/$unwind prefix) run before
    # ``pre_where`` filters — 0 filters the base docs directly.
    pre_where_split: int = 0
    # ``DISTINCT ON (exprs)`` — keep the first row (in ORDER BY order) per distinct
    # value of these expressions. Mutually exclusive with plain ``distinct``.
    distinct_on: list[exp.Expression] = field(default_factory=list)
    # ORDER BY index -> the enum's declared labels, when that ORDER BY term is an
    # enum column, so the executor sorts by declared order not lexically.
    enum_orders: dict[int, list[str]] = field(default_factory=dict)
    # Rich ``JOIN LATERAL`` sources (subquery with its own join/group/aggregate),
    # expanded nested-loop per outer row by the executor after the pipeline runs.
    lateral_joins: list[LateralJoin] = field(default_factory=list)
    # The single base TableDef when the plan came from a one-table SELECT —
    # lets the descriptor builder attribute bare-column outputs to their
    # source table/attnum (RowDescription base-column identity, which JDBC's
    # getBaseColumnName resolves through). None for joins, which carry the
    # same identity per output position in ``out_sources`` instead.
    base_table: Any = None
    # Per output position, the ``(TableDef | ViewSource, 1-based attnum)`` the
    # column came from, or None for a computed / unattributable output.
    out_sources: list[tuple[Any, int] | None] = field(default_factory=list)


def _evaluated_enum_orders(
    order: list[tuple[exp.Expression, int, bool]],
    column_of: Any,
) -> dict[int, list[str]]:
    """Map each ORDER BY term index to its enum labels (if the term is an enum
    column, resolved via ``column_of(node) -> Column | None``)."""
    out: dict[int, list[str]] = {}
    for i, (expr, _d, _n) in enumerate(order):
        labels = _enum_labels_for_column(column_of(expr))
        if labels is not None:
            out[i] = labels
    return out


_AGG_CLASSES: dict[type, str] = {
    exp.Count: "count",
    exp.Sum: "sum",
    exp.Avg: "avg",
    exp.Min: "min",
    exp.Max: "max",
    exp.LogicalAnd: "bool_and",
    exp.LogicalOr: "bool_or",
}
# Statistical / bitwise aggregates parse to dedicated nodes whose availability
# varies across sqlglot versions; look them up by attribute. sqlglot maps both
# ``variance`` and ``var_samp`` onto ``Variance`` (sample variance).
for _agg_cls_name, _agg_func in (
    ("Stddev", "stddev"),
    ("StddevSamp", "stddev_samp"),
    ("StddevPop", "stddev_pop"),
    ("Variance", "variance"),
    ("VariancePop", "var_pop"),
    ("BitwiseAndAgg", "bit_and"),
    ("BitwiseOrAgg", "bit_or"),
    ("BitwiseXorAgg", "bit_xor"),
):
    _agg_cls = getattr(exp, _agg_cls_name, None)
    if _agg_cls is not None:
        _AGG_CLASSES[_agg_cls] = _agg_func

# Aggregates that need a Python finish after the $group: sample/pop variance
# (square of the corresponding stdDev) and the bitwise reductions (a $push then
# a fold). ``variance`` -> $stdDevSamp, ``var_pop`` -> $stdDevPop.
_POST_STAT_FUNCS = {"variance": "$stdDevSamp", "var_pop": "$stdDevPop"}
_BIT_AGG_FUNCS = {"bit_and", "bit_or", "bit_xor"}

_HAVING_CMP: dict[type, tuple[str, str]] = {
    exp.GT: ("$gt", "$lt"),
    exp.GTE: ("$gte", "$lte"),
    exp.LT: ("$lt", "$gt"),
    exp.LTE: ("$lte", "$gte"),
}


def _strip_identity_wrappers(arg: exp.Expression | None) -> exp.Expression | None:
    """Peel decorations that don't change an aggregate argument's value:
    parentheses and *pairs* of unary minus (``- - ( col0 )`` is ``col0``; a
    single ``-`` is a real negation and stays)."""
    while arg is not None:
        if isinstance(arg, exp.Paren):
            arg = arg.this
            continue
        if isinstance(arg, exp.Neg) and isinstance(arg.this, (exp.Neg, exp.Paren)):
            inner = arg.this
            while isinstance(inner, exp.Paren):
                inner = inner.this
            if isinstance(inner, exp.Neg):
                arg = inner.this
                continue
        break
    return arg


def _agg_expr_arg(node: exp.Expression) -> exp.Expression | None:
    """The (identity-stripped) argument node of an aggregate call — for lowering
    expression arguments that aren't a bare column. None for ``COUNT(*)`` or a
    non-aggregate node."""
    inner = node.this if isinstance(node, exp.Alias) else node
    if isinstance(inner, exp.Filter):
        inner = inner.this
    for cls in _AGG_CLASSES:
        if isinstance(inner, cls):
            arg = inner.this
            if isinstance(arg, exp.Distinct):
                arg = arg.expressions[0] if arg.expressions else None
            return _strip_identity_wrappers(arg)
    return None


def _single_agg_key(node: exp.Expression, agg: tuple[str, str | None, bool]) -> tuple:
    """Accumulator-dedup identity for a single-table aggregate. ``_aggregate_of``
    reports ``col=None`` for both ``COUNT(*)`` and any *expression* argument, so
    two different expression aggregates (``MAX(3)`` / ``MAX(a - b)``) would
    collide on ``(func, None, distinct)`` and share one accumulator; identify
    expression args by their SQL text instead (the join paths' ``_agg_key``
    already does)."""
    func, col, distinct = agg
    if col is not None:
        return (func, col, distinct)
    arg = _agg_expr_arg(node)
    ident = arg.sql() if arg is not None and not isinstance(arg, exp.Star) else None
    return (func, ident, distinct)


def _aggregate_of(node: exp.Expression) -> tuple[str, str | None, bool] | None:
    """If ``node`` (or its alias target) is an aggregate, return
    ``(func, column, distinct)``. ``column`` is None for ``COUNT(*)``; the
    argument of a ``COUNT(DISTINCT x)`` is unwrapped from its ``exp.Distinct``."""
    inner = node.this if isinstance(node, exp.Alias) else node
    if isinstance(inner, exp.Filter):  # agg(...) FILTER (WHERE ...)
        inner = inner.this
    for cls, name in _AGG_CLASSES.items():
        if isinstance(inner, cls):
            arg = inner.this
            distinct = isinstance(arg, exp.Distinct)
            if distinct:
                exprs = arg.expressions
                arg = exprs[0] if exprs else None
            arg = _strip_identity_wrappers(arg)
            col = _column_name(arg) if isinstance(arg, exp.Column) else None
            return name, col, distinct
    # ``every(x)`` is the standard-SQL spelling of ``bool_and(x)`` (parses as an
    # Anonymous call rather than a dedicated node).
    if isinstance(inner, exp.Anonymous):
        fname = (inner.this if isinstance(inner.this, str) else inner.name).lower()
        if fname == "every" and inner.expressions:
            arg = inner.expressions[0]
            return "bool_and", (_column_name(arg) if isinstance(arg, exp.Column) else None), False
    return None


# ``agg(...) FILTER (WHERE cond)`` parses as ``exp.Filter(this=<agg>,
# expression=Where(cond))`` (possibly under an Alias). These helpers peel the
# Filter so the aggregate detectors still see the underlying func, and lower the
# FILTER condition to a Mongo aggregation expression for use inside a ``$cond``.


def _agg_filter_where(node: exp.Expression) -> exp.Where | None:
    """The ``WHERE`` node of an ``agg(...) FILTER (WHERE cond)``, else None."""
    inner = node.this if isinstance(node, exp.Alias) else node
    if isinstance(inner, exp.Filter):
        where = inner.args.get("expression")
        return where if isinstance(where, exp.Where) else None
    return None


def _filter_cond_to_agg(cond: exp.Expression, resolve: Resolve) -> Any:
    """Lower a FILTER predicate (a boolean WHERE condition) to a Mongo aggregation
    expression suitable for ``$cond``. Supports comparisons, AND/OR/NOT, IS [NOT]
    NULL, and a bare boolean column; anything else raises (unsupported)."""
    if isinstance(cond, exp.Where):
        cond = cond.this
    if isinstance(cond, exp.Paren):
        return _filter_cond_to_agg(cond.this, resolve)
    if isinstance(cond, exp.And):
        return {
            "$and": [
                _filter_cond_to_agg(cond.this, resolve),
                _filter_cond_to_agg(cond.expression, resolve),
            ]
        }
    if isinstance(cond, exp.Or):
        return {
            "$or": [
                _filter_cond_to_agg(cond.this, resolve),
                _filter_cond_to_agg(cond.expression, resolve),
            ]
        }
    if isinstance(cond, exp.Not):
        return {"$not": [_filter_cond_to_agg(cond.this, resolve)]}
    if isinstance(cond, exp.Is):
        left = _to_agg_expr(cond.this, resolve)
        if isinstance(cond.expression, exp.Null):
            return {"$eq": [left, None]}
        return {"$eq": [left, _to_agg_expr(cond.expression, resolve)]}
    if type(cond) in _EXPR_CMP:
        return {
            _EXPR_CMP[type(cond)]: [
                _to_agg_expr(cond.this, resolve),
                _to_agg_expr(cond.expression, resolve),
            ]
        }
    # A bare boolean column / expression is truthy.
    return {"$eq": [_to_agg_expr(cond, resolve), True]}


def _table_resolve(table: TableDef) -> Resolve:
    """A single-table ``Resolve``: a column node -> (field path, type tag)."""

    def resolve(node: exp.Expression) -> tuple[str, str]:
        name = node.name
        return table.field_for(name), table.type_for(name)

    resolve.table = table  # type: ignore[attr-defined]
    return resolve


def _string_agg_arg(node: exp.Expression) -> tuple[exp.Expression, str] | None:
    """If ``node`` is ``string_agg(expr, sep)`` (sqlglot ``GroupConcat``), return
    ``(value_expr, separator)``. The separator is a string literal."""
    inner = node.this if isinstance(node, exp.Alias) else node
    if isinstance(inner, exp.Filter):
        inner = inner.this
    if not isinstance(inner, exp.GroupConcat):
        return None
    sep_node = inner.args.get("separator")
    sep = sep_node.name if isinstance(sep_node, exp.Literal) else ""
    return inner.this, sep


def _agg_order_spec(
    value_node: exp.Expression,
) -> tuple[exp.Expression, list[tuple[exp.Expression, int, bool]] | None]:
    """Unwrap an in-call ``ORDER BY`` from an aggregate argument. ``array_agg(x
    ORDER BY y DESC)`` / ``string_agg(x, sep ORDER BY y)`` parse the argument as an
    ``exp.Order`` whose ``this`` is the value and ``expressions`` are the sort
    keys. Returns ``(value_expr, [(key_expr, direction, nulls_first), …])`` — or
    ``(value_node, None)`` when there is no in-call ORDER BY."""
    if not isinstance(value_node, exp.Order):
        return value_node, None
    terms: list[tuple[exp.Expression, int, bool]] = []
    for o in value_node.expressions:  # exp.Ordered
        direction = -1 if o.args.get("desc") else 1
        nulls_first = bool(o.args.get("nulls_first"))
        terms.append((o.this, direction, nulls_first))
    return value_node.this, terms


def _sorted_agg_push(
    value_node: exp.Expression,
    terms: list[tuple[exp.Expression, int, bool]],
    table: TableDef,
) -> dict[str, Any]:
    """The ``$push`` expression for an ordered aggregate: a ``{v, k}`` pair per row
    (``v`` the value, ``k`` the list of sort-key values) that the executor sorts."""
    return {
        "$push": {
            "v": _agg_arg_to_expr(value_node, table),
            "k": [_agg_arg_to_expr(key, table) for key, _dir, _nf in terms],
        }
    }


def _sorted_agg_push_resolve(
    value_node: exp.Expression,
    terms: list[tuple[exp.Expression, int, bool]],
    resolve: Resolve,
) -> dict[str, Any]:
    """``_sorted_agg_push`` for the join path — the value / sort-key expressions
    lower through the join ``resolve`` (via ``_to_agg_expr``) instead of a table."""
    return {
        "$push": {
            "v": _to_agg_expr(value_node, resolve),
            "k": [_to_agg_expr(key, resolve) for key, _dir, _nf in terms],
        }
    }


def _string_agg_project(fname: str, sep: str) -> dict[str, Any]:
    """The ``$project`` expression that turns a ``string_agg`` field's pushed
    array (``[v1, v2, …]``) into the delimited string, skipping NULL elements and
    yielding NULL when every element was NULL (Postgres ``string_agg`` semantics)."""
    return {
        "$reduce": {
            "input": f"${fname}",
            "initialValue": None,
            "in": {
                "$cond": [
                    {"$eq": ["$$this", None]},
                    "$$value",  # skip NULL elements
                    {
                        "$cond": [
                            {"$eq": ["$$value", None]},
                            {"$toString": "$$this"},
                            {"$concat": ["$$value", {"$literal": sep}, {"$toString": "$$this"}]},
                        ]
                    },
                ]
            },
        }
    }


def _push_filtered(value_expr: Any, fcond: Any, *, wrap: bool = False) -> Any:
    """A ``$push`` element honouring ``FILTER (WHERE cond)``: a non-matching row
    pushes ``None`` (dropped by the paired projection / reduce). ``wrap=True`` boxes
    the value as ``{"v": …}`` so a *matching* NULL survives the drop (Postgres
    ``array_agg`` keeps NULLs; ``string_agg`` / ``jsonb_object_agg`` skip them, so
    they push the bare value and let ``None`` double as "absent")."""
    if fcond is None:
        return value_expr
    return {"$cond": [fcond, ({"v": value_expr} if wrap else value_expr), None]}


def _array_agg_project(fname: str, fcond: Any) -> Any:
    """The projection for an ``array_agg`` field. Without a FILTER the pushed array
    is emitted as-is; with one, drop the ``None`` sentinels and unbox the ``{v}``
    wrappers (so matching NULLs are preserved)."""
    if fcond is None:
        return f"${fname}"
    return {
        "$map": {
            "input": {"$filter": {"input": f"${fname}", "as": "e", "cond": {"$ne": ["$$e", None]}}},
            "as": "e",
            "in": "$$e.v",
        }
    }


def _jsonb_object_agg_project(fname: str, fcond: Any) -> Any:
    """``$arrayToObject`` over a ``jsonb_object_agg`` field, dropping the ``None``
    sentinels a FILTER leaves behind."""
    src: Any = f"${fname}"
    if fcond is not None:
        src = {"$filter": {"input": f"${fname}", "as": "e", "cond": {"$ne": ["$$e", None]}}}
    return {"$arrayToObject": src}


def _array_agg_out_tag(arr_arg: exp.Expression, resolve: Resolve) -> str:
    """The output tag of ``array_agg(x)`` — the element's array type when the
    element tag is known (``text`` → ``text[]``/1009, so psycopg loads a real
    list and ``coalesce(array_agg(…), '{}')`` over an empty group parses as an
    empty ARRAY, not JSON text — CompositeInfo.fetch of a zero-field type
    depends on it). ``json`` element (or unknown) keeps the jsonb rendering."""
    value_node, _terms = _agg_order_spec(arr_arg)
    try:
        elem = _infer_scalar_tag(value_node, resolve)
    except Exception:  # noqa: BLE001 — inference failure keeps the old shape
        return "json"
    if elem and not typemap.is_array_tag(elem) and f"{elem}[]" in typemap.PG_OID:
        return f"{elem}[]"
    return "json"


def _is_true_array_agg(e: exp.Expression) -> bool:
    """True for ``array_agg`` proper — ``jsonb_agg``/``json_agg`` share the
    ``$push`` machinery but must keep the json output type."""
    inner = e.this if isinstance(e, exp.Alias) else e
    if isinstance(inner, exp.Filter):
        inner = inner.this
    return isinstance(inner, exp.ArrayAgg)


def _array_agg_arg(node: exp.Expression) -> exp.Expression | None:
    """If ``node`` is ``array_agg(<arg>)`` — or ``jsonb_agg`` / ``json_agg``, which
    build the same ``$push`` array and are likewise typed ``json`` here — return its
    argument expression (an in-call ``ORDER BY`` stays wrapped as ``exp.Order``)."""
    inner = node.this if isinstance(node, exp.Alias) else node
    if isinstance(inner, exp.Filter):
        inner = inner.this
    if isinstance(inner, exp.ArrayAgg):
        return inner.this
    # json_agg has a dedicated sqlglot node; jsonb_agg parses as an Anonymous call.
    if isinstance(inner, exp.JSONArrayAgg):
        return inner.this
    if isinstance(inner, exp.Anonymous) and str(inner.this).lower() in ("jsonb_agg", "json_agg"):
        return inner.expressions[0] if inner.expressions else None
    return None


def _range_agg_arg(node: exp.Expression) -> exp.Expression | None:
    """If ``node`` is ``range_agg(<range>)``, return its argument expression. The
    aggregate coalesces the group's ranges into a multirange."""
    inner = node.this if isinstance(node, exp.Alias) else node
    if isinstance(inner, exp.Filter):
        inner = inner.this
    if (
        isinstance(inner, exp.Anonymous)
        and str(inner.this).lower() == "range_agg"
        and inner.expressions
    ):
        return inner.expressions[0]
    return None


def _multirange_tag_for_arg(arg: exp.Expression, table: TableDef) -> str:
    """The multirange output tag for ``range_agg(arg)`` — mapped from the argument's
    range type (defaults to int4multirange when the range type can't be resolved)."""
    if isinstance(arg, exp.Column):
        rtag = table.type_for(arg.name)
    elif isinstance(arg, exp.Anonymous) and str(arg.this).lower() in typemap._RANGE_TAGS:
        rtag = str(arg.this).lower()
    else:
        rtag = None
    return ranges.RANGE_TO_MULTIRANGE.get(rtag or "", "int4multirange")


def _jsonb_object_agg_args(
    node: exp.Expression,
) -> tuple[exp.Expression, exp.Expression] | None:
    """If ``node`` is ``jsonb_object_agg(k, v)`` / ``json_object_agg(k, v)``, return
    the ``(key_expr, value_expr)`` pair. Both a dedicated sqlglot node and an
    Anonymous two-argument call are accepted."""
    inner = node.this if isinstance(node, exp.Alias) else node
    if isinstance(inner, exp.Filter):
        inner = inner.this
    # jsonb_object_agg → JSONBObjectAgg (this / expression); json_object_agg →
    # JSONObjectAgg (a two-element expressions list). Attribute lookup for version
    # tolerance across sqlglot releases.
    jsonb_cls = getattr(exp, "JSONBObjectAgg", None)
    if jsonb_cls is not None and isinstance(inner, jsonb_cls):
        return inner.this, inner.expression
    json_cls = getattr(exp, "JSONObjectAgg", None)
    if json_cls is not None and isinstance(inner, json_cls) and len(inner.expressions) == 2:
        return inner.expressions[0], inner.expressions[1]
    if (
        isinstance(inner, exp.Anonymous)
        and str(inner.this).lower() in ("jsonb_object_agg", "json_object_agg")
        and len(inner.expressions) == 2
    ):
        return inner.expressions[0], inner.expressions[1]
    return None


def _jsonb_object_agg_push(
    key: exp.Expression, val: exp.Expression, table: TableDef, fcond: Any = None
) -> dict:
    """The ``$push`` accumulator for ``jsonb_object_agg`` — a ``{k, v}`` pair per row
    (key coerced to a string, per Postgres' text object keys). With a ``FILTER
    (WHERE cond)`` (``fcond``), a non-matching row pushes ``None``, dropped by the
    paired ``_jsonb_object_agg_project`` before ``$arrayToObject``."""
    pair = {
        "k": {"$toString": _agg_arg_to_expr(key, table)},
        "v": _agg_arg_to_expr(val, table),
    }
    return {"$push": _push_filtered(pair, fcond)}


# Ordered-set aggregates: ``<func>(...) WITHIN GROUP (ORDER BY expr)``.
_ORDERED_SET_KINDS: dict[type, str] = {
    exp.PercentileCont: "percentile_cont",
    exp.PercentileDisc: "percentile_disc",
    exp.Mode: "mode",
}


def _ordered_set_agg(node: exp.Expression) -> tuple[str, float | None, exp.Expression] | None:
    """If ``node`` is an ordered-set aggregate — ``percentile_cont(f)`` /
    ``percentile_disc(f)`` / ``mode() WITHIN GROUP (ORDER BY expr)`` — return
    ``(kind, fraction, order_value_expr)``. ``fraction`` is None for ``mode``.
    Raises for a fraction outside [0, 1] (``2202E``) or a non-single ORDER BY."""
    inner = node.this if isinstance(node, exp.Alias) else node
    if not isinstance(inner, exp.WithinGroup):
        return None
    kind = _ORDERED_SET_KINDS.get(type(inner.this))
    if kind is None:
        raise errors.feature_not_supported(
            f"unsupported WITHIN GROUP aggregate: {inner.this.sql()}"
        )
    order = inner.expression
    ordered = order.expressions if isinstance(order, exp.Order) else []
    if len(ordered) != 1:
        raise errors.feature_not_supported("WITHIN GROUP requires exactly one ORDER BY expression")
    order_val = ordered[0].this
    fraction: float | None = None
    if kind != "mode":
        fraction = float(typemap.unwrap_numeric(_literal(inner.this.this)))
        if not 0.0 <= fraction <= 1.0:
            raise errors.SQLError("2202E", f"percentile value {fraction} is not between 0 and 1")
    return kind, fraction, order_val


#: SRF kinds that yield a composite record, so ``(srf(...)).field`` is valid on
#: them. ``_srf_of`` tags those as ``"<kind>.<field>"``.
_RECORD_SRF_KINDS = frozenset({"_pg_expandarray"})


def _srf_of(node: exp.Expression) -> tuple[str, exp.Expression] | None:
    """If ``node`` is a set-returning function, return (kind, array_expr).

    ``unnest(arr)`` (sqlglot ``Explode``) → ('unnest', arr); ``generate_subscripts
    (arr, dim)`` (``Anonymous``) → ('generate_subscripts', arr). The dimension
    argument is ignored (our arrays are one-dimensional)."""
    inner = node.this if isinstance(node, exp.Alias) else node
    if isinstance(inner, exp.Explode):
        return ("unnest", inner.this)
    # ``(schema.srf(arr)).field`` — a composite field selected off a
    # record-returning SRF, which is how pgjdbc's DatabaseMetaData asks for
    # ``(information_schema._pg_expandarray(i.indkey)).n``. Recurse on the
    # parenthesised call and tag the kind with the field being taken.
    if isinstance(inner, exp.Dot) and isinstance(inner.this, exp.Paren):
        field = inner.expression
        field_name = field.name if isinstance(field, (exp.Identifier, exp.Column)) else None
        if field_name:
            base = _srf_of(inner.this.this)
            if base is not None and base[0] in _RECORD_SRF_KINDS:
                return (f"{base[0]}.{field_name.lower()}", base[1])
        return None
    if isinstance(inner, exp.Dot) and isinstance(inner.expression, exp.Anonymous):
        inner = inner.expression
    if isinstance(inner, exp.Anonymous):
        name = (
            (inner.this if isinstance(inner.this, str) else inner.name).rsplit(".", 1)[-1].lower()
        )
        if name == "unnest" and inner.expressions:
            return ("unnest", inner.expressions[0])
        if name == "generate_subscripts" and inner.expressions:
            return ("generate_subscripts", inner.expressions[0])
        # jsonb set-returning functions: one row per array element / object key.
        if name in ("jsonb_array_elements", "json_array_elements") and inner.expressions:
            return ("jsonb_array_elements", inner.expressions[0])
        if name in ("jsonb_object_keys", "json_object_keys") and inner.expressions:
            return ("jsonb_object_keys", inner.expressions[0])
        # information_schema._pg_expandarray(arr) -> one (x, n) record per
        # element: the value and its 1-based subscript.
        if name == "_pg_expandarray" and inner.expressions:
            return ("_pg_expandarray", inner.expressions[0])
    return None


def _agg_arg_to_expr(node: exp.Expression, table: TableDef) -> Any:
    """Lower an aggregate argument to a Mongo aggregation expression.

    Used by ``array_agg`` (``$push``). Catalog functions with no Mongo analogue
    that are always NULL in our model (``pg_get_constraintdef`` / ``pg_get_expr``)
    lower to a literal NULL — sound because we store no constraints/defaults.
    """
    if isinstance(node, exp.Paren):
        return _agg_arg_to_expr(node.this, table)
    if isinstance(node, exp.Order):
        # ``array_agg(x ORDER BY y)`` — the intra-aggregate ordering isn't modeled
        # (our only use is over empty catalogs); aggregate the bare expression.
        return _agg_arg_to_expr(node.this, table)
    if isinstance(node, exp.Cast):
        return _agg_arg_to_expr(node.this, table)
    if isinstance(node, exp.Column):
        return f"${table.field_for(node.name)}"
    if isinstance(node, exp.Neg) and not isinstance(node.this, (exp.Literal, exp.Null)):
        return {"$multiply": [-1, _agg_arg_to_expr(node.this, table)]}
    if isinstance(node, (exp.Literal, exp.Boolean, exp.Null, exp.Neg)):
        return {"$literal": _literal(node)}
    _arith_ops = {
        exp.Add: "$add",
        exp.Sub: "$subtract",
        exp.Mul: "$multiply",
        exp.Div: "$divide",
        exp.Mod: "$mod",
    }
    op = _arith_ops.get(type(node))
    if op is not None and node.expression is not None:
        lowered = {
            op: [_agg_arg_to_expr(node.this, table), _agg_arg_to_expr(node.expression, table)]
        }
        if isinstance(node, exp.Div) and _int_division_operands(node, table_resolver(table)):
            # PG integer ``/`` truncates toward zero; Mongo's $divide is real.
            return {"$trunc": [lowered, 0]}
        return lowered
    fname = None
    if isinstance(node, exp.Dot) and isinstance(node.expression, exp.Anonymous):
        fname = node.expression.name
    elif isinstance(node, exp.Anonymous):
        fname = node.this if isinstance(node.this, str) else node.name
    if fname is not None and str(fname).rsplit(".", 1)[-1].lower() in (
        "pg_get_constraintdef",
        "pg_get_expr",
    ):
        return {"$literal": None}
    raise errors.feature_not_supported(f"unsupported array_agg argument: {node.sql()}")


def select_needs_pipeline(stmt: exp.Select) -> bool:
    """Whether a SELECT must be compiled to an aggregation pipeline."""
    if (
        stmt.args.get("joins")
        or stmt.args.get("group")
        or stmt.args.get("having")
        or stmt.args.get("distinct")
    ):
        return True
    # A ``(SELECT ...) AS alias`` derived table in FROM — e.g. an expanded view —
    # is materialized by the pipeline path's ``_resolve_source``.
    from_node = next((v for v in stmt.args.values() if isinstance(v, exp.From)), None)
    if from_node is not None and isinstance(from_node.this, (exp.Subquery, exp.Values)):
        return True
    # A SELECT list / ORDER BY with set-returning or scalar functions, CASE, or
    # subqueries needs per-row evaluation (the pipeline path), not a plain find.
    if _stmt_needs_evaluation(stmt):
        return True
    aggs = [
        e
        for e in stmt.expressions
        if _aggregate_of(e) is not None
        or _array_agg_arg(e) is not None
        or _jsonb_object_agg_args(e) is not None
        or _range_agg_arg(e) is not None
        or _string_agg_arg(e) is not None
        or _ordered_set_agg(e) is not None
    ]
    if not aggs:
        return False
    # A lone COUNT(*) (no GROUP BY, no FILTER) is served by the simpler find path.
    # ``COUNT(<literal>)`` also reports ``col=None`` — check the arg really is
    # ``*`` so it stays on the pipeline path.
    if len(stmt.expressions) == 1:
        e = stmt.expressions[0]
        only = _aggregate_of(e)
        inner = e.this if isinstance(e, exp.Alias) else e
        if (
            only is not None
            and only == ("count", None, False)
            and isinstance(inner, exp.Count)
            and (inner.this is None or isinstance(inner.this, exp.Star))
            and _agg_filter_where(e) is None
        ):
            return False
    return True


def qualified_table_name(table_node: exp.Table) -> str:
    """The catalog key for a (possibly schema-qualified) table reference: the
    bare name for ``public`` and unqualified references, else
    ``"<schema>.<name>"`` — the same dotted-key mapping user types take. The
    backing Mongo collection uses the same composed string, so the
    dual-protocol view addresses it as ``db["schema.table"]``."""
    schema = table_node.args.get("db")
    sname = schema.name if schema is not None else None
    if not sname or sname == "public":
        return table_node.name
    return f"{sname}.{table_node.name}"


def _join_source_alias(node: exp.Expression | None) -> str | None:
    """The name a join source is referenced by: its alias if it has one, else
    the table name. Returns None for a source we cannot name (and therefore
    cannot build a qualified ON against)."""
    if node is None:
        return None
    if isinstance(node, exp.From):
        node = node.this
    alias = node.args.get("alias") if isinstance(node.args.get("alias"), exp.TableAlias) else None
    if alias is not None and alias.name:
        return alias.name
    if isinstance(node, exp.Table):
        return node.alias_or_name or None
    return None


def desugar_join_using(stmt: exp.Expression) -> None:
    """Rewrite ``JOIN b USING (c, …)`` into the equivalent qualified ON.

    Nothing in join planning read ``args["using"]``, so a USING join lost its
    condition entirely and degraded to a CROSS JOIN — ``SELECT v, w FROM a
    JOIN b USING (k)`` returned every pair instead of the matching ones. That
    is a silent wrong answer, so USING is normalised to ON here and the
    existing ON machinery does the rest.

    The left side of each equality is the nearest preceding source. In a chain
    (``a JOIN b USING (k) JOIN c USING (k)``) Postgres joins against the merged
    column, which equals the nearest preceding one by construction, so the
    result set is the same.
    """
    for select in stmt.find_all(exp.Select):
        joins = select.args.get("joins") or []
        if not joins:
            continue
        # sqlglot spells the FROM arg "from_", not "from".
        prev = _join_source_alias(select.args.get("from_"))
        for jn in joins:
            right = _join_source_alias(jn.this)
            columns = jn.args.get("using") or []
            if columns and prev and right:
                conds = [
                    exp.EQ(
                        this=exp.column(col.name, table=prev),
                        expression=exp.column(col.name, table=right),
                    )
                    for col in columns
                ]
                condition = conds[0]
                for extra in conds[1:]:
                    condition = exp.And(this=condition, expression=extra)
                jn.set("on", condition)
                jn.set("using", None)
            prev = right or prev


def _create_target(stmt: exp.Expression) -> exp.Table | None:
    """The relation a CREATE statement *defines*, which search_path resolution
    must leave alone: Postgres creates into the path's first schema and never
    binds a create target to an existing relation elsewhere on the path. The
    body of a CREATE TABLE AS / CREATE VIEW still resolves normally, as does a
    CREATE INDEX's target table (that one names an existing relation)."""
    if not isinstance(stmt, exp.Create):
        return None
    if (stmt.args.get("kind") or "TABLE").upper() not in ("TABLE", "VIEW"):
        return None
    target = stmt.this
    if isinstance(target, exp.Schema):
        target = target.this
    return target if isinstance(target, exp.Table) else None


def qualify_from_search_path(stmt: exp.Expression, catalog: Any, db: str, session: Any) -> None:
    """Qualify bare table references against the session's ``search_path``.

    Postgres resolves an unqualified relation by walking ``search_path`` in
    order and taking the first schema that holds it. We only consult the path
    when the bare name is *not* itself a catalog entry, so this can turn a
    "relation does not exist" into a hit but can never redirect a name that
    already resolves. The node is rewritten in place, which keeps the write
    path honest: ``qualified_table_name`` composes the storage key from the
    same node the resolver matched, so a read and a write of one unqualified
    name cannot land in different schemas.

    Names bound by a CTE in scope are left alone — they shadow real relations.

    The session's private temp namespace (``pg_temp_<n>``) participates the way
    real PG's does: an explicit ``pg_temp.<name>`` qualifier is rewritten to the
    session's own namespace, and — unless the user placed ``pg_temp`` explicitly
    on the path — an unqualified name is tried against the temp namespace FIRST,
    so a session's temp table shadows a permanent one of the same name.
    """
    path = [s for s in session.search_path if s != "public"]
    temp_ns = getattr(session, "temp_schema", None)
    cte_names = {cte.alias_or_name.lower() for cte in stmt.find_all(exp.CTE) if cte.alias_or_name}
    skip = _create_target(stmt)
    temp_first = temp_ns is not None and "pg_temp" not in path
    for table in stmt.find_all(exp.Table):
        if not table.name:
            continue
        schema_arg = table.args.get("db")
        if schema_arg is not None:
            # ``pg_temp.<name>`` means *this session's* temp namespace. A create
            # target resolves here too — CREATE TABLE pg_temp.t IS a temp table
            # (the engine's qualify_temp_create_target handles the temp flag).
            if schema_arg.name == "pg_temp":
                table.set("db", exp.to_identifier(session.ensure_temp_schema()))
            continue
        if table.name.lower() in cte_names or table is skip:
            continue
        if temp_first and catalog.get(db, f"{temp_ns}.{table.name}") is not None:
            table.set("db", exp.to_identifier(temp_ns))
            continue
        if catalog.get(db, table.name) is not None:
            continue
        for schema in path:
            resolved = temp_ns if schema == "pg_temp" and temp_ns is not None else schema
            if catalog.get(db, f"{resolved}.{table.name}") is not None:
                table.set("db", exp.to_identifier(resolved))
                break


def qualify_temp_create_target(stmt: exp.Create, session: Any) -> None:
    """Home a ``CREATE TEMP TABLE`` target in the session's private temp
    namespace (``pg_temp_<n>``) by qualifying the target node in place, so
    concurrent sessions' same-named temp tables land on distinct catalog keys
    — real PG gives every backend its own temp schema. An explicit ``pg_temp``
    qualifier was already rewritten by ``qualify_from_search_path``; a TEMP
    keyword aimed at any other schema is rejected like real PG."""
    target = _create_target(stmt)
    if target is None:
        return
    props = stmt.args.get("properties")
    is_temp_kw = bool(props) and any(
        isinstance(p, exp.TemporaryProperty) for p in props.expressions
    )
    if not is_temp_kw:
        return
    schema = target.args.get("db")
    sname = schema.name if schema is not None else None
    if sname is None:
        target.set("db", exp.to_identifier(session.ensure_temp_schema()))
    elif sname != getattr(session, "temp_schema", None):
        raise errors.SQLError("42P16", "cannot create temporary relation in non-temporary schema")


def _lookup_table_def(
    catalog: Any, db: str, table_node: exp.Table, storage: Any = None
) -> TableDef | None:
    """Resolve a (possibly schema-qualified) table to a TableDef.

    Tries the user catalog first, then the ``pg_catalog`` / ``information_schema``
    virtual tables, then — when ``storage`` is supplied and the name is not
    schema-qualified — a reflected (schema-on-read) view of an existing Mongo
    collection. This is what lets joins / aggregates span user tables, the system
    catalogs, *and* un-declared collections written via ``pymongo``.
    """
    from secantus.sql import reflect, virtual

    table = catalog.get(db, qualified_table_name(table_node))
    if table is not None:
        return table
    schema = table_node.args.get("db")
    schema_name = schema.name if schema is not None else None
    vtable = virtual.lookup(schema_name, table_node.name)
    if vtable is not None:
        return vtable.table_def()
    # A reflected collection only makes sense for an unqualified name (a schema
    # qualifier means the caller asked for a specific catalog relation).
    if storage is not None and schema_name is None:
        return reflect.reflect(storage, db, table_node.name)
    return None


def expand_using_star(stmt: exp.Select, catalog: Any, db: str) -> None:
    """Expand a lone ``SELECT *`` over USING joins into Postgres' merged list.

    ``SELECT * FROM a JOIN b USING (k)`` returns the join column ONCE (from
    the left side; the right side for RIGHT joins; ``COALESCE`` for FULL),
    then each source's remaining columns in FROM order. Our star expansion
    emitted ``k`` once per side. Rewriting the AST here — one site, before
    planning — beats teaching every star-expansion path about join shapes.

    Sound-not-complete: anything unusual (mixed ON/USING chains, non-table
    sources, unknown tables, ``tbl.*``, extra select items, outer sides in a
    multi-join chain) bails and keeps the old expansion.
    """
    if len(stmt.expressions) != 1 or not isinstance(stmt.expressions[0], exp.Star):
        return
    from_node = stmt.args.get("from_")
    joins = stmt.args.get("joins") or []
    if from_node is None or not joins or not all(j.args.get("using") for j in joins):
        return
    if len(joins) > 1 and any(j.side for j in joins):
        return  # outer sides in a chain: merge-source rules get positional; bail

    def resolve(node: exp.Expression) -> tuple[str, Any] | None:
        if not isinstance(node, exp.Table) or not isinstance(node.this, exp.Identifier):
            return None
        td = catalog.get(db, node.name) if catalog is not None else None
        if td is None:
            return None
        return (node.alias or node.name, td)

    base = resolve(from_node.this)
    if base is None:
        return
    sources = [base]
    for j in joins:
        r = resolve(j.this)
        if r is None:
            return
        sources.append(r)
    cols_of = {alias: [c.name for c in td.columns] for alias, td in sources}

    # Merged USING columns, in first-use order; each must exist in the left
    # accumulation and the join's right side, or we bail.
    merged: list[str] = []
    for i, j in enumerate(joins):
        right_alias = sources[i + 1][0]
        left_aliases = [a for a, _ in sources[: i + 1]]
        for u in j.args["using"]:
            name = u.name
            if name not in cols_of[right_alias] or not any(
                name in cols_of[a] for a in left_aliases
            ):
                return
            if name not in merged:
                merged.append(name)

    def qcol(alias: str, name: str) -> exp.Column:
        return exp.column(name, table=alias)

    out: list[exp.Expression] = []
    for name in merged:
        holders = [a for a, _ in sources if name in cols_of[a]]
        side = joins[0].side if len(joins) == 1 else None
        if side == "FULL":
            out.append(
                exp.alias_(
                    exp.Coalesce(
                        this=qcol(holders[0], name),
                        expressions=[qcol(h, name) for h in holders[1:]],
                    ),
                    name,
                )
            )
        elif side == "RIGHT":
            out.append(exp.alias_(qcol(holders[-1], name), name))
        else:
            out.append(exp.alias_(qcol(holders[0], name), name))
    for alias, _td in sources:
        for name in cols_of[alias]:
            if name not in merged:
                out.append(qcol(alias, name))
    stmt.set("expressions", out)


def expand_table_stars(stmt: exp.Select, catalog: Any, db: str) -> None:
    """Expand ``tbl.*`` select items over a JOIN into explicit columns.

    The join planner resolves select items column-by-column and crashed on a
    table-qualified star (``column "*" does not exist``). Postgres expands it
    to the table's columns in order — and does NOT merge USING columns for
    ``tbl.*`` (only the bare ``*`` merges). Bails per-item when the source
    isn't a resolvable plain table."""
    joins = stmt.args.get("joins") or []
    from_node = stmt.args.get("from_")
    if from_node is None or not joins:
        return
    if not any(
        isinstance(e, exp.Column) and isinstance(e.this, exp.Star) for e in stmt.expressions
    ):
        return
    defs: dict[str, Any] = {}
    for node in [from_node.this] + [j.this for j in joins]:
        if isinstance(node, exp.Table) and isinstance(node.this, exp.Identifier):
            td = catalog.get(db, node.name) if catalog is not None else None
            if td is not None:
                defs[node.alias or node.name] = td
    out: list[exp.Expression] = []
    for e in stmt.expressions:
        if isinstance(e, exp.Column) and isinstance(e.this, exp.Star) and e.table in defs:
            out.extend(exp.column(c.name, table=e.table) for c in defs[e.table].columns)
        else:
            out.append(e)
    stmt.set("expressions", out)


def unwrap_paren_join_from(stmt: exp.Select) -> None:
    """Hoist a parenthesized join out of FROM, in place.

    ``FROM (a CROSS JOIN b)`` parses as an alias-less ``Subquery`` wrapping the
    first table with the joins attached to it — not a derived table. Postgres
    treats the parens as pure grouping, so rewrite to ``FROM a CROSS JOIN b``
    (inner joins precede any outer-level joins). An *aliased* subquery is a real
    derived table and is left alone."""
    frm = stmt.args.get("from_") or stmt.args.get("from")
    if frm is None:
        return
    node = frm.this
    while (
        isinstance(node, exp.Subquery)
        and not node.alias
        and isinstance(node.this, (exp.Table, exp.Subquery))
    ):
        inner = node.this
        # Joins can sit on the INNER node (``FROM (a JOIN b)``) or on the
        # grouping Subquery ITSELF (``FROM ((a JOIN b) JOIN c)`` attaches the
        # c-join to the outer parens; extra grouping layers — CrystalReports'
        # {oj (((…))) } shape — nest join-less wrappers that still must peel).
        # Hoist both, inner-first (their join order in the original text).
        # A wrapper whose inner is a SELECT is a derived table missing its
        # alias and is left for the error path (the isinstance gate above).
        joins = (inner.args.pop("joins", None) or []) + (node.args.pop("joins", None) or [])
        if joins:
            stmt.set("joins", joins + (stmt.args.get("joins") or []))
        frm.set("this", inner)
        node = inner


def plan_pipeline_select(
    stmt: exp.Select, db: str, catalog: Any, storage: Any = None, session: Any = None
) -> PipelineSelectPlan | EvaluatedSelectPlan:
    # Publish the subquery context so any WHERE `$match` in the pipeline planners
    # can evaluate a scalar / IN subquery (the same as the single-table pushdown).
    # The session rides along for session-aware predicates (pg_table_is_visible's
    # own-temp-table branch); a nested planning call inherits the outer one's.
    if session is None:
        session = getattr(_pipeline_subctx.get(), "session", None)
    token = _pipeline_subctx.set(
        SubqueryCtx(storage=storage, db=db, catalog=catalog, session=session)
    )
    # Resolved BEFORE planning: planning flattens ``FROM (subquery) AS v`` into
    # the subquery itself, so the view reference is gone by the time the plan
    # comes back.
    view_positions = _view_source_positions(stmt, db, catalog)
    try:
        plan = _plan_pipeline_select(stmt, db, catalog, storage)
    finally:
        _pipeline_subctx.reset(token)
    if view_positions is not None:
        _attribute_view_source(plan, *view_positions)
    return plan


@dataclass(frozen=True)
class ViewSource:
    """A view relation standing as an output column's provenance. Carries only
    the name — the pg_class oid is minted in ``virtual``, which the descriptor
    builder resolves (a view has no ``TableDef``)."""

    name: str


def _view_source_positions(
    stmt: exp.Select, db: str, catalog: Any
) -> tuple[str, dict[str, int]] | None:
    """``(view_name, {column_name: 1-based position})`` when ``stmt`` selects from
    exactly one expanded view, else None.

    A view is expanded into an inline subquery before planning, which loses the
    relation identity Postgres reports in RowDescription — a view's columns carry
    the view's own oid and its own 1-based positions, not the underlying tables'.
    The expansion keeps the view's name as the subquery alias, and the stored
    definition round-trips to exactly the subquery body, so an exact-SQL match
    identifies the source without mistaking a user subquery that happens to be
    aliased like a view.
    """
    # sqlglot spells the arg ``from_``; older versions used ``from``.
    from_node = stmt.args.get("from_") or stmt.args.get("from")
    if from_node is None or stmt.args.get("joins"):
        return None
    src = from_node.this
    if not isinstance(src, exp.Subquery) or not src.alias:
        return None
    getter = getattr(catalog, "get_view", None)
    vdef = getter(db, src.alias) if getter is not None else None
    if vdef is None or src.this.sql(dialect="postgres") != vdef:
        return None
    try:
        view_select = sqlglot.parse_one(vdef, read="postgres")
        names = view_select.named_selects
    except Exception:  # pragma: no cover - a stored definition that won't reparse
        return None
    return src.alias, {n: i + 1 for i, n in enumerate(names)}


def _attribute_view_source(plan: Any, view_name: str, positions: dict[str, int]) -> None:
    """Point ``plan``'s outputs at the view relation they were selected from.

    This OVERRIDES any table-level attribution already on the plan: planning a
    view over a join can flatten down to the view body's own plan, whose columns
    were attributed to the underlying tables. Postgres reports the view.
    """
    if not hasattr(plan, "out_sources"):
        return
    plan.out_sources = [
        (ViewSource(view_name), positions[name]) if name in positions else None
        for name, _tag in plan.out_columns
    ]


def _plan_pipeline_select(
    stmt: exp.Select, db: str, catalog: Any, storage: Any = None
) -> PipelineSelectPlan | EvaluatedSelectPlan:
    unwrap_paren_join_from(stmt)
    if stmt.args.get("joins"):
        if _has_grouping_sets(stmt):
            if _select_has_window(stmt):
                return _plan_join_grouping_sets_window_select(stmt, db, catalog, storage)
            return _plan_join_grouping_sets_select(stmt, db, catalog, storage)
        has_agg = any(
            _aggregate_of(e) is not None
            or _array_agg_arg(e) is not None
            or _jsonb_object_agg_args(e) is not None
            or _range_agg_arg(e) is not None
            or _string_agg_arg(e) is not None
            for e in stmt.expressions
        )
        grouped = bool(stmt.args.get("group") or stmt.args.get("having") or has_agg)
        if (
            _select_has_window(stmt)
            or _select_has_computed_aggregate(stmt)
            or (grouped and _group_projection_needs_evaluation(stmt))
        ) and (grouped or _group_agg_nodes(stmt)):
            # Window functions — or an expression *wrapping* an aggregate
            # (``COUNT(*) * COUNT(*)``) — over a JOIN + GROUP BY (or implicit
            # aggregation) run the wrapping phase via the evaluated executor.
            return _plan_join_group_window_select(stmt, db, catalog, storage)
        if grouped:
            return _plan_join_group_select(stmt, db, catalog, storage)
        if _group_agg_nodes(stmt):
            # A computed-over-aggregate output without GROUP BY (``COUNT(*) * 32``
            # over a join) — the group-then-evaluate builder handles it with an
            # empty window list.
            return _plan_join_group_window_select(stmt, db, catalog, storage)
        return _plan_join_select(stmt, db, catalog, storage)
    from_node = next((v for v in stmt.args.values() if isinstance(v, exp.From)), None)
    if from_node is None:
        raise errors.feature_not_supported("aggregate without FROM is not supported")
    # The FROM may be a real table or a ``(SELECT ...) AS alias`` derived table
    # (materialized into an ephemeral collection by the executor).
    derived: list[DerivedTable] = []
    _alias, table = _resolve_source(from_node.this, db, catalog, storage, derived)

    # Route a WHERE / ORDER BY on an indexed expression onto its hidden field so the
    # leading ``$match`` / sort can use the storage index.
    rewrite_expr_index_refs(stmt, table)

    # Computed GROUP BY keys (``GROUP BY lower(name)`` / ``x + 1`` / ``ROLLUP(lower(x))``)
    # — rewrite each into a synthetic column materialised by a pre-``$group``
    # ``$addFields`` so the bare-column group machinery handles SELECT / HAVING /
    # ORDER BY. Works for plain GROUP BY and GROUPING SETS / ROLLUP / CUBE alike.
    group_addfields: dict[str, Any] | None = None
    group_pyfields: dict[str, exp.Expression] = {}
    group_pyresolve: Resolve | None = None
    rewrite = _rewrite_computed_group_keys(stmt, table)
    if rewrite is not None:
        stmt, table, group_addfields, group_pyfields, group_pyresolve = rewrite

    has_aggregate = any(
        _aggregate_of(e) is not None
        or _array_agg_arg(e) is not None
        or _jsonb_object_agg_args(e) is not None
        or _range_agg_arg(e) is not None
        or _string_agg_arg(e) is not None
        or _ordered_set_agg(e) is not None
        for e in stmt.expressions
    )
    grouped = bool(stmt.args.get("group") or stmt.args.get("having") or has_aggregate)
    if grouped:
        _expand_grouped_star(stmt, table)
    if _has_grouping_sets(stmt):
        if _select_has_window(stmt):
            plan: PipelineSelectPlan | EvaluatedSelectPlan = _plan_grouping_sets_window_select(
                stmt, table, group_addfields
            )
        else:
            plan = _plan_grouping_sets_select(stmt, table, group_addfields)
        group_addfields = None  # injected per union-branch by the grouping-sets planner
    elif (
        _select_has_window(stmt)
        or _select_has_computed_aggregate(stmt)
        or _having_has_subquery(stmt)
        or (grouped and _select_projects_subquery(stmt))
        or (grouped and _group_projection_needs_evaluation(stmt))
    ) and (grouped or _group_agg_nodes(stmt)):
        # Window functions computed over GROUP BY aggregates, an expression that
        # *wraps* an aggregate (``sum(x) + 1``), or a subquery in HAVING — all run the
        # wrapping / window / residual-HAVING phase over the grouped rows via the
        # evaluated executor.
        plan = _plan_group_window_select(stmt, table)
    elif grouped:
        # A HAVING shape the `$match` lowerer can't express is not a hard
        # 0A000: re-plan through the evaluated path, which carries HAVING as a
        # per-grouped-row residual (the route the HAVING-subquery case already
        # takes). The copy is taken first because planning mutates the tree
        # (aggregates are replaced by their computed-field references), so the
        # re-plan needs a pristine statement.
        having_backup = stmt.copy() if stmt.args.get("having") is not None else None
        try:
            plan = _plan_group_select(stmt, table)
        except errors.SQLError as exc:
            if exc.sqlstate != "0A000" or having_backup is None:
                raise
            plan = _plan_group_window_select(having_backup, table)
    elif _stmt_needs_evaluation(stmt) or _distinct_on(stmt) or where_needs_per_row(stmt, table):
        # DISTINCT ON needs the evaluated path's sort-then-keep-first-per-key;
        # a WHERE the pushdown can't lower (column arithmetic in a comparison,
        # ``expr IS NOT NULL``) rides the same path as a per-row residual.
        plan = _build_evaluated_single(stmt, table)
    elif stmt.args.get("distinct"):
        plan = _plan_distinct_select(stmt, table)
    else:
        plan = _plan_plain_select(stmt, table)
    if group_pyfields:
        # Keys the aggregation engine can't lower are evaluated in Python per
        # base doc — only the pipeline plan's executor supports that hook
        # (grouping-sets $unionWith branches re-read the collection and the
        # evaluated planners never see base docs).
        if not isinstance(plan, PipelineSelectPlan):
            raise errors.feature_not_supported(
                "unsupported computed GROUP BY key: "
                + ", ".join(k.sql() for k in group_pyfields.values())
            )
        plan.pre_eval_fields = group_pyfields
        plan.pre_eval_resolve = group_pyresolve
    if group_addfields:
        # Compute the synthetic GROUP BY key fields before the $group stage reads them.
        plan.pipeline.insert(0, {"$addFields": group_addfields})
    plan.derived = derived
    return plan


def _plan_plain_select(stmt: exp.Select, table: TableDef) -> PipelineSelectPlan:
    """A plain projection over a (derived) table — ``$project`` the columns."""
    base_filter = _where_filter(stmt, table)
    resolve = table_resolver(table)
    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    names = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Star):
            for col in table.columns:
                nm = names.fresh(col.name)
                project[nm] = f"${col.field}"
                out_columns.append((nm, col.type_tag))
            continue
        path, tag = _field(inner, resolve)
        nm = names.fresh(alias or _column_name(inner))
        project[nm] = f"${path}"
        out_columns.append((nm, tag))
    pipeline: list[dict[str, Any]] = [{"$project": project}]
    _append_sort_limit(pipeline, stmt, out_columns, table)
    return PipelineSelectPlan(table.collection, base_filter, pipeline, out_columns)


def _build_evaluated_single(stmt: exp.Select, table: TableDef) -> EvaluatedSelectPlan:
    """A single-table SELECT needing per-row evaluation (SRFs / scalar funcs).

    The base collection is read with the WHERE filter; the executor evaluates
    each output expression per row (expanding set-returning functions)."""
    resolve = table_resolver(table)
    # A WHERE that can't lower to a ``$match`` (a full-text ``@@`` / range operator)
    # is carried as a per-row residual and evaluated by the executor's scalar pass.
    residual_where = None
    if where_needs_per_row(stmt, table):
        where_node = stmt.args.get("where")
        residual_where = where_node.this if where_node is not None else None
        base_filter: dict[str, Any] = {}
    else:
        base_filter = _where_filter(stmt, table)
    out_columns: list[tuple[str, str]] = []
    out_enum_types: dict[int, str] = {}
    out_exprs: list[exp.Expression] = []
    alias_exprs: dict[str, exp.Expression] = {}
    # No name uniquifying here: the evaluated executor extracts row values
    # POSITIONALLY (zip with out_exprs), so duplicate output names are pure
    # display — and real PG repeats them verbatim (``select 'a', 'b'`` is
    # ``?column?, ?column?``, never ``?column?_2``; pgx's NetworkUsage test
    # byte-counts the RowDescription).
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Star):
            for col in table.columns:
                if col.enum_type is not None:
                    out_enum_types[len(out_columns)] = col.enum_type
                out_columns.append((col.name, col.type_tag))
                out_exprs.append(exp.column(col.name))
            continue
        name = alias or (
            _column_name(inner)
            if isinstance(inner, exp.Column)
            else _cast_output_name(inner) or "?column?"
        )
        enum_name = _projected_enum_type(inner, table)
        if enum_name is not None:
            out_enum_types[len(out_columns)] = enum_name
        out_columns.append((name, _infer_scalar_tag(inner, resolve)))
        out_exprs.append(inner)
        if alias is not None:
            alias_exprs[alias] = inner
    order: list[tuple[exp.Expression, int, bool]] = []
    order_node = stmt.args.get("order")
    if order_node is not None:
        for o in order_node.expressions:
            # ORDER BY may name a SELECT output alias (``ORDER BY rank``) or a
            # 1-based output ordinal (``ORDER BY 1``) — Postgres resolves both
            # to that output expression, so sorting a computed column works.
            term = o.this
            if isinstance(term, exp.Column) and not term.table and term.name in alias_exprs:
                term = alias_exprs[term.name]
            elif (
                isinstance(term, exp.Literal)
                and not term.is_string
                and str(term.this).isdigit()
                and 1 <= int(term.this) <= len(out_exprs)
                # An SRF output can't be the sort key pre-expansion (one source
                # row fans out to many); leave the ordinal to the executor.
                and _srf_of(out_exprs[int(term.this) - 1]) is None
            ):
                term = out_exprs[int(term.this) - 1]
            order.append((term, -1 if o.args.get("desc") else 1, _nulls_first(o)))
    limit, skip = _limit_skip(stmt)
    don = _distinct_on(stmt)
    enum_orders = _evaluated_enum_orders(
        order,
        lambda node: table.column(_column_name(node)) if isinstance(node, exp.Column) else None,
    )
    return EvaluatedSelectPlan(
        base_collection=table.collection,
        base_table=table,
        base_filter=base_filter,
        pipeline=[],
        out_columns=out_columns,
        out_enum_types=out_enum_types,
        out_exprs=out_exprs,
        resolve=resolve,
        where=residual_where,
        order=order,
        distinct=bool(stmt.args.get("distinct")) and not don,
        limit=limit,
        skip=skip,
        distinct_on=don,
        enum_orders=enum_orders,
    )


def _distinct_on(stmt: exp.Select) -> list[exp.Expression]:
    """The expressions of a ``SELECT DISTINCT ON (…)``, or ``[]`` for plain / no
    DISTINCT. Postgres keeps the first row per distinct value of these, in the
    query's ORDER BY order."""
    d = stmt.args.get("distinct")
    if isinstance(d, exp.Distinct) and d.args.get("on") is not None:
        on = d.args["on"]
        return list(on.expressions) if isinstance(on, exp.Tuple) else [on]
    return []


def _projected_enum_type(inner: exp.Expression, table: TableDef | None) -> str | None:
    """The enum type name when ``inner`` is a plain projection of an enum column.

    Feeds ``out_enum_types`` on the pipeline/evaluated plans: the string type
    tag stays ``text`` (labels are stored as text), so the enum identity must
    travel separately for RowDescription to report the minted OID.
    """
    if table is None or not isinstance(inner, exp.Column):
        return None
    col = table.column(inner.name)
    return col.enum_type if col is not None else None


def _plan_distinct_select(stmt: exp.Select, table: TableDef) -> PipelineSelectPlan:
    """A single-table ``SELECT DISTINCT`` → project the columns, then dedup."""
    base_filter = _where_filter(stmt, table)
    resolve = table_resolver(table)
    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    out_enum_types: dict[int, str] = {}
    names = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Star):
            for col in table.columns:
                nm = names.fresh(col.name)
                project[nm] = f"${col.field}"
                if col.enum_type is not None:
                    out_enum_types[len(out_columns)] = col.enum_type
                out_columns.append((nm, col.type_tag))
            continue
        path, tag = _field(inner, resolve)
        nm = names.fresh(alias or _column_name(inner))
        project[nm] = f"${path}"
        enum_name = _projected_enum_type(inner, table)
        if enum_name is not None:
            out_enum_types[len(out_columns)] = enum_name
        out_columns.append((nm, tag))
    pipeline: list[dict[str, Any]] = [{"$project": project}]
    _append_distinct(pipeline, out_columns)
    _append_sort_limit(pipeline, stmt, out_columns, table)
    return PipelineSelectPlan(
        table.collection, base_filter, pipeline, out_columns, out_enum_types=out_enum_types
    )


def _accumulator_for(
    func: str, field: str | None, tag: str | None, filter_cond: Any = None
) -> tuple[dict[str, Any], str]:
    """Build a ``$group`` accumulator from an already-resolved field path + tag.

    ``field`` is the pipeline field path (e.g. ``amt`` or ``b.amt`` after a join)
    and is None only for ``COUNT(*)``. This is the field-resolved core shared by
    the single-table (`_accumulator`) and join (`_join_accumulator`) paths.

    ``filter_cond`` (a Mongo aggregation boolean expression) implements
    ``agg(...) FILTER (WHERE cond)``: only rows satisfying it contribute. A
    non-matching row donates the accumulator's neutral element (0 for sum/count,
    NULL for avg/min/max — the aggregate engine skips NULL there)."""
    val = f"${field}" if field is not None else None
    if func == "count":
        if field is None:
            body = {"$cond": [filter_cond, 1, 0]} if filter_cond is not None else 1
            return {"$sum": body}, "int8"
        # COUNT(col) counts non-null values.
        matched = {"$ne": [val, None]}
        cond = {"$and": [filter_cond, matched]} if filter_cond is not None else matched
        return {"$sum": {"$cond": [cond, 1, 0]}}, "int8"
    if func == "sum":
        body = {"$cond": [filter_cond, val, 0]} if filter_cond is not None else val
        return {"$sum": body}, _sum_tag(tag)
    if func == "avg":
        body = {"$cond": [filter_cond, val, None]} if filter_cond is not None else val
        return {"$avg": body}, _avg_tag(tag)
    if func in ("min", "bool_and"):
        body = {"$cond": [filter_cond, val, None]} if filter_cond is not None else val
        return {"$min": body}, ("bool" if func == "bool_and" else (tag or "text"))
    if func in ("max", "bool_or"):
        body = {"$cond": [filter_cond, val, None]} if filter_cond is not None else val
        return {"$max": body}, ("bool" if func == "bool_or" else (tag or "text"))
    if func in ("stddev", "stddev_samp"):
        # Native Mongo accumulators; a lone value yields NULL (Mongo returns null
        # for a single sample), matching Postgres' sample stddev.
        return {"$stdDevSamp": val}, "float8"
    if func == "stddev_pop":
        return {"$stdDevPop": val}, "float8"
    raise errors.feature_not_supported(f"aggregate {func} is not supported")


def _accumulator(
    func: str,
    col: str | None,
    table: TableDef,
    filter_cond: Any = None,
    arg_node: exp.Expression | None = None,
) -> tuple[dict[str, Any], str]:
    if (
        col is None
        and arg_node is not None
        and not isinstance(arg_node, exp.Star)
        and func in ("count", "sum", "avg", "min", "max")
    ):
        # An expression argument (``SUM(- 83)``, ``MAX(col0 + 1)``) lowers to a
        # Mongo aggregation expression (_agg_arg_to_expr raises 0A000 for
        # shapes it can't lower).
        body = _agg_arg_to_expr(arg_node, table)
        if func == "count":  # COUNT(<expr>) counts non-null values (COUNT(NULL) is 0)
            matched = {"$ne": [body, None]}
            cond = {"$and": [filter_cond, matched]} if filter_cond is not None else matched
            return {"$sum": {"$cond": [cond, 1, 0]}}, "int8"
        if filter_cond is not None:
            body = {"$cond": [filter_cond, body, 0 if func == "sum" else None]}
        tag = _agg_out_tag(func, _infer_scalar_tag(arg_node, table_resolver(table)))
        return {f"${func}": body}, tag
    if col is None:
        return _accumulator_for(func, None, None, filter_cond)
    return _accumulator_for(func, table.field_for(col), table.type_for(col), filter_cond)


# DISTINCT changes the result only for these — MIN/MAX of a set equal MIN/MAX of
# the raw values, so a DISTINCT min/max just runs the ordinary accumulator.
_DISTINCT_FUNCS = {"count", "sum", "avg"}


def _sum_tag(tag: str | None) -> str:
    """Postgres' sum() output type: int2/int4 -> int8, int8 -> numeric, floats
    and numeric keep their type."""
    if tag in ("int2", "int4"):
        return "int8"
    if tag in ("int8", "numeric"):
        return "numeric"
    if tag in ("float4", "float8"):
        return tag
    return "float8"


def _avg_tag(tag: str | None) -> str:
    """Postgres' avg() output type: numeric for integer/numeric inputs, float8
    for floats."""
    return "float8" if tag in ("float4", "float8") else "numeric"


def _agg_out_tag(func: str, tag: str | None) -> str:
    if func == "count":
        return "int8"
    if func == "sum":
        return _sum_tag(tag)
    if func == "avg":
        return _avg_tag(tag)
    return tag or "text"


def _distinct_reduction(func: str, set_field: str) -> dict[str, Any]:
    """Reduce a ``$addToSet`` result (at ``set_field``, e.g. ``$tmp``) to the
    DISTINCT aggregate value, dropping NULLs (SQL aggregates ignore NULL)."""
    nonnull = {"$filter": {"input": set_field, "as": "v", "cond": {"$ne": ["$$v", None]}}}
    if func == "count":
        return {"$size": nonnull}
    total = {
        "$reduce": {"input": nonnull, "initialValue": 0, "in": {"$add": ["$$value", "$$this"]}}
    }
    if func == "sum":
        # PG: SUM over zero non-null values is NULL, not 0.
        return {"$cond": [{"$eq": [{"$size": nonnull}, 0]}, None, total]}
    if func == "avg":
        cnt = {"$size": nonnull}
        return {"$cond": [{"$eq": [cnt, 0]}, None, {"$divide": [total, cnt]}]}
    raise errors.feature_not_supported(f"DISTINCT is not supported for {func}()")


def _guard_sum_null(
    fname: str,
    value: Any,
    filter_cond: Any,
    names: _NameAllocator,
    accumulators: dict[str, Any],
    reductions: dict[str, Any],
) -> None:
    """Postgres' SUM over zero non-null contributing values is NULL; Mongo's
    ``$sum`` yields 0. Pair the sum with a non-null contribution counter and
    rewrite the output to NULL when nothing contributed (``value`` is the raw
    aggregation expression the sum reads, before any FILTER folding)."""
    nn = names.fresh(f"{fname}__nn")
    matched = {"$ne": [value, None]}
    cond = {"$and": [filter_cond, matched]} if filter_cond is not None else matched
    accumulators[nn] = {"$sum": {"$cond": [cond, 1, 0]}}
    reductions[fname] = {"$cond": [{"$gt": [f"${nn}", 0]}, f"${fname}", None]}


def _register_distinct_agg(
    func: str,
    field: str | None,
    tag: str | None,
    alias: str | None,
    names: _NameAllocator,
    accumulators: dict[str, Any],
    reductions: dict[str, Any],
    fcond: Any = None,
    value: Any = None,
) -> tuple[str, str]:
    """Wire a DISTINCT count/sum/avg: a ``$addToSet`` accumulator collects the
    distinct values; a post-``$group`` ``$addFields`` reduces the set. Returns
    the output field name and its type tag. The distinct value is the ``field``
    path, or the pre-lowered aggregation expression ``value`` when the argument
    isn't a bare column (``SUM(DISTINCT 77)``). With a ``FILTER (WHERE cond)``
    (``fcond``), a non-matching row contributes ``None`` to the set, which the
    reduction's NULL filter drops — so only matching rows' distinct values count
    (SQL ``agg(DISTINCT x) FILTER (WHERE cond)`` semantics)."""
    set_name = names.fresh(f"{alias or func}__distinct")
    accumulators[set_name] = {
        "$addToSet": _push_filtered(value if field is None else f"${field}", fcond)
    }
    fname = names.fresh(alias or func)
    reductions[fname] = _distinct_reduction(func, f"${set_name}")
    return fname, _agg_out_tag(func, tag)


class _NameAllocator:
    def __init__(self) -> None:
        self._used: set[str] = set()

    def fresh(self, name: str) -> str:
        base, i = name, 1
        while name in self._used:
            i += 1
            name = f"{base}_{i}"
        self._used.add(name)
        return name


def _has_grouping_sets(stmt: exp.Select) -> bool:
    g = stmt.args.get("group")
    return bool(g and (g.args.get("rollup") or g.args.get("cube") or g.args.get("grouping_sets")))


def _lower_computed_group_keys(
    computed: list[exp.Expression],
    resolve: Resolve,
    taken: set[str],
    *,
    allow_python: bool = False,
) -> tuple[dict[str, str], dict[str, Any], dict[str, exp.Expression]]:
    """Lower each computed GROUP BY key to a Mongo aggregation expression and mint a
    synthetic ``__gkeyN`` field name for it (avoiding names in ``taken``).

    Returns ``(targets, addfields)`` — ``targets`` maps each key's normalised SQL to
    its synthetic name, ``addfields`` maps each synthetic name to its Mongo expr (the
    body of a pre-``$group`` ``$addFields``). Raises ``feature_not_supported`` when a
    key uses a function the aggregation engine can't evaluate (→ ``0A000``)."""
    targets: dict[str, str] = {}
    addfields: dict[str, Any] = {}
    pyfields: dict[str, exp.Expression] = {}
    counter = 0
    for key in computed:
        srepr = key.sql()
        if srepr in targets:
            continue
        mongo: Any = None
        lowered = True
        try:
            mongo = _to_agg_expr(key, resolve)
        except Exception as exc:  # noqa: BLE001 — any lowering failure
            if not allow_python:
                raise errors.feature_not_supported(
                    f"unsupported computed GROUP BY key: {srepr}"
                ) from exc
            lowered = False
        while (fname := f"__gkey{counter}") in taken:
            counter += 1
        counter += 1
        taken.add(fname)
        if lowered:
            addfields[fname] = mongo
        else:
            # The scalar evaluator computes it per doc before the pipeline.
            pyfields[fname] = key.copy()
        targets[srepr] = fname
    return targets, addfields, pyfields


def _apply_group_key_rewrite(stmt: exp.Select, targets: dict[str, str]) -> exp.Select:
    """Rewrite every SQL-equal occurrence of a computed GROUP BY key (``targets``
    maps normalised key SQL → synthetic column name) into a bare column reference,
    across GROUP BY / SELECT / HAVING / ORDER BY. An ORDER BY term that names a key
    projected under an output alias is redirected to that alias (the synthetic
    ``__gkeyN`` field is gone by the post-``$group`` ``$project``)."""
    key_alias: dict[str, str] = {}
    for e in stmt.expressions:
        if isinstance(e, exp.Alias) and (repl := targets.get(e.this.sql())) is not None:
            key_alias[repl] = e.alias

    def xf(node: exp.Expression) -> exp.Expression:
        if not isinstance(node, exp.Column):
            repl = targets.get(node.sql())
            if repl is not None:
                return exp.column(repl)
        return node

    new_stmt = stmt.transform(xf)
    order = new_stmt.args.get("order")
    if order is not None and key_alias:
        for o in order.expressions:
            term = o.this
            if isinstance(term, exp.Column) and not term.table and term.name in key_alias:
                o.set("this", exp.column(key_alias[term.name]))
    return new_stmt


def _flatten_group_set_element(node: exp.Expression) -> list[exp.Expression]:
    """The leaf key expressions of one grouping-set element — a ``(a, b)`` Tuple, a
    ``(a)`` Paren, or a bare key — flattened to a list (empty ``()`` → ``[]``)."""
    if isinstance(node, exp.Tuple):
        out: list[exp.Expression] = []
        for x in node.expressions:
            out.extend(_flatten_group_set_element(x))
        return out
    if isinstance(node, exp.Paren):
        return _flatten_group_set_element(node.this)
    return [node]


def _all_group_key_nodes(group_node: exp.Group) -> list[exp.Expression]:
    """Every leaf key expression across the whole GROUP BY clause — the leading
    plain keys plus the elements of each ``ROLLUP`` / ``CUBE`` / ``GROUPING SETS``
    wrapper. Used to find computed keys anywhere in the clause."""
    nodes = list(group_node.expressions)
    for arg in ("rollup", "cube", "grouping_sets"):
        for wrapper in group_node.args.get(arg) or []:
            for e in wrapper.expressions:
                nodes.extend(_flatten_group_set_element(e))
    return nodes


def _computed_group_keys(group_node: exp.Group | None) -> list[exp.Expression]:
    """The non-column, non-ordinal GROUP BY keys (``lower(name)``, ``x + 1``) — the
    ones that need lowering into a synthetic column, collected across the whole
    clause (leading keys + ROLLUP / CUBE / GROUPING SETS elements). Bare columns and
    positional ordinals (``GROUP BY 1``) are left to their existing handling."""
    if group_node is None:
        return []
    return [
        k for k in _all_group_key_nodes(group_node) if not isinstance(k, (exp.Column, exp.Literal))
    ]


def _rewrite_computed_group_keys(
    stmt: exp.Select, table: TableDef
) -> tuple[exp.Select, TableDef, dict[str, Any], dict[str, exp.Expression], Resolve] | None:
    """Rewrite non-column GROUP BY keys (``GROUP BY lower(name)``, ``GROUP BY x + 1``)
    into synthetic bare columns.

    Each computed key is lowered to a Mongo aggregation expression, materialised
    into a fresh ``__gkeyN`` field by a pre-``$group`` ``$addFields``, and every
    SQL-equal occurrence of the key (in GROUP BY / SELECT / HAVING / ORDER BY) is
    replaced with a reference to that synthetic column. The existing bare-column
    group machinery then handles the rest transparently.

    Returns ``(rewritten_stmt, augmented_table, addfields)`` or ``None`` when no
    GROUP BY key is computed. Raises ``feature_not_supported`` when a key uses a
    function the aggregation engine can't evaluate (→ ``0A000``)."""
    computed = _computed_group_keys(stmt.args.get("group"))
    if not computed:
        return None
    resolve = table_resolver(table)
    existing = {c.name for c in table.columns}
    targets, addfields, pyfields = _lower_computed_group_keys(
        computed, resolve, existing, allow_python=True
    )
    # Type each synthetic key column from its source expression so the grouped
    # output describes (and renders) as bool/int/…, not raw text.
    key_tag_by_name = {
        targets[key.sql()]: _infer_scalar_tag(key, resolve)
        for key in computed
        if key.sql() in targets
    }
    synth = [
        Column(
            name=fname,
            type_tag=key_tag_by_name.get(fname, "any"),
            field=fname,
            pk=False,
            nullable=True,
        )
        for fname in [*addfields, *pyfields]
    ]
    new_stmt = _apply_group_key_rewrite(stmt, targets)
    new_table = replace(table, columns=[*table.columns, *synth])
    return new_stmt, new_table, addfields, pyfields, resolve


def _grouping_args(e: exp.Expression) -> list[str] | None:
    """If ``e`` is a ``GROUPING(col, …)`` call (possibly aliased), the list of its
    column-name arguments; else None. ``GROUPING`` returns a bitmask that is 1 for
    each argument rolled up (absent from the row's grouping set), 0 otherwise, most
    significant bit first."""
    inner = e.this if isinstance(e, exp.Alias) else e
    if isinstance(inner, exp.Grouping):
        return [_column_name(a) for a in inner.expressions]
    return None


def _grouping_bitmask(cols: list[str], in_set: set[str]) -> int:
    bits = 0
    for c in cols:
        bits = (bits << 1) | (0 if c in in_set else 1)
    return bits


def _grouping_set_cols(node: exp.Expression) -> list[str]:
    """Column names in one grouping-set element — a ``(a, b)`` Tuple, a ``(a)``
    Paren, a bare column, or the empty set ``()`` (→ ``[]``)."""
    if isinstance(node, exp.Tuple):
        cols: list[str] = []
        for x in node.expressions:
            cols.extend(_grouping_set_cols(x))
        return cols
    if isinstance(node, exp.Paren):
        return _grouping_set_cols(node.this)
    if isinstance(node, exp.Column):
        return [_column_name(node)]
    return []  # empty () or a stray literal → no columns


def _grouping_sets(group_node: exp.Group) -> list[list[str]]:
    """Enumerate the grouping sets (each a list of column names) for a GROUP BY
    that uses ROLLUP / CUBE / GROUPING SETS. A plain leading ``GROUP BY a, …`` is
    a prefix present in every set; ROLLUP / CUBE / explicit GROUPING SETS each
    contribute a list of alternatives, cross-producted together (Postgres
    semantics)."""
    base = [_column_name(c) for c in group_node.expressions]
    factors: list[list[list[str]]] = []
    for r in group_node.args.get("rollup") or []:
        cols = [_column_name(c) for c in r.expressions]
        factors.append([cols[:i] for i in range(len(cols), -1, -1)])
    for cnode in group_node.args.get("cube") or []:
        cols = [_column_name(x) for x in cnode.expressions]
        subsets = [
            [cols[i] for i in range(len(cols)) if mask & (1 << i)] for mask in range(2 ** len(cols))
        ]
        factors.append(subsets)
    for gs in group_node.args.get("grouping_sets") or []:
        factors.append([_grouping_set_cols(n) for n in gs.expressions])
    result: list[list[str]] = [list(base)]
    for factor in factors:
        result = [r + s for r in result for s in factor]
    seen: set[tuple[str, ...]] = set()
    deduped: list[list[str]] = []
    for s in result:
        key = tuple(s)
        if key not in seen:
            seen.add(key)
            deduped.append(s)
    return deduped


def _group_col_nodes(group_node: exp.Group) -> dict[str, exp.Column]:
    """Map each grouping column's bare name to its ``exp.Column`` node, walking the
    base ``GROUP BY`` list plus every ROLLUP / CUBE / GROUPING SETS factor. The join
    grouping-sets planner needs the qualified node (``d.region``) so the join
    resolver can map it to its post-unwind path — ``_grouping_sets`` only yields the
    bare names."""
    nodes: dict[str, exp.Column] = {}

    def collect(node: exp.Expression) -> None:
        if isinstance(node, exp.Column):
            nodes.setdefault(node.name, node)
        elif isinstance(node, exp.Paren):
            collect(node.this)
        elif isinstance(node, exp.Tuple):
            for x in node.expressions:
                collect(x)

    for c in group_node.expressions:
        collect(c)
    for r in group_node.args.get("rollup") or []:
        for c in r.expressions:
            collect(c)
    for cnode in group_node.args.get("cube") or []:
        for c in cnode.expressions:
            collect(c)
    for gs in group_node.args.get("grouping_sets") or []:
        for n in gs.expressions:
            collect(n)
    return nodes


def _grouping_set_branch(
    stmt: exp.Select, table: TableDef, gset: list[str], group_cols: list[str]
) -> tuple[list[dict[str, Any]], list[tuple[str, str]], list[tuple[str, str, float | None]]]:
    """One grouping set's ``[$group, $project]`` sub-pipeline. Group columns not in
    this set project as literal NULL (Postgres' grouping-set semantics), so every
    branch has the same output shape (required for the ``$unionWith``). A ``HAVING``
    filters this branch's grouped rows via a ``$match`` on the ``$group`` output
    (resolved before the ``$group`` is built so any hidden aggregate accumulator it
    needs lands in the group stage); every branch registers HAVING the same way, so
    the shapes stay aligned. Returns ``(stages, out_columns, post_aggregates)`` —
    ``post_aggregates`` finishes statistical / bitwise aggregates in Python after the
    union (identical across branches, so the planner keeps one copy)."""
    in_set = set(gset)
    group_id = {c: f"${table.field_for(c)}" for c in gset} or None
    accumulators: dict[str, Any] = {}
    reductions: dict[str, Any] = {}
    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    post_aggregates: list[tuple[str, str, float | None]] = []
    names = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        grp = _grouping_args(e)
        arr_arg = _array_agg_arg(e)
        oagg = _jsonb_object_agg_args(e)
        sagg = _string_agg_arg(e)
        agg = _aggregate_of(e)
        where = _agg_filter_where(e)
        fcond = _filter_cond_to_agg(where, _table_resolve(table)) if where is not None else None
        if grp is not None:
            for c in grp:
                table.field_for(c)  # validate
            fname = names.fresh(alias or "grouping")
            project[fname] = {"$literal": _grouping_bitmask(grp, in_set)}
            out_columns.append((fname, "int4"))
        elif arr_arg is not None:
            fname = names.fresh(alias or "array_agg")
            value_node, terms = _agg_order_spec(arr_arg)
            if terms:  # array_agg(x ORDER BY …): push {v, k}, executor sorts over the union
                if fcond is not None:
                    raise errors.feature_not_supported(
                        "FILTER (WHERE ...) with an in-aggregate ORDER BY is not supported"
                    )
                accumulators[fname] = _sorted_agg_push(value_node, terms, table)
                project[fname] = f"${fname}"
                post_aggregates.append((fname, "sorted_array", [(d, nf) for _k, d, nf in terms]))
            else:
                accumulators[fname] = {
                    "$push": _push_filtered(_agg_arg_to_expr(arr_arg, table), fcond, wrap=True)
                }
                project[fname] = _array_agg_project(fname, fcond)
            out_columns.append(
                (
                    fname,
                    _array_agg_out_tag(arr_arg, table_resolver(table))
                    if _is_true_array_agg(e)
                    else "json",
                )
            )
        elif oagg is not None:
            fname = names.fresh(alias or "jsonb_object_agg")
            accumulators[fname] = _jsonb_object_agg_push(oagg[0], oagg[1], table, fcond)
            project[fname] = _jsonb_object_agg_project(fname, fcond)
            out_columns.append((fname, "json"))
        elif sagg is not None:
            fname = names.fresh(alias or "string_agg")
            value_node, terms = _agg_order_spec(sagg[0])
            if terms:  # string_agg(x, sep ORDER BY …): push {v, k}, executor sorts+joins
                if fcond is not None:
                    raise errors.feature_not_supported(
                        "FILTER (WHERE ...) with an in-aggregate ORDER BY is not supported"
                    )
                accumulators[fname] = _sorted_agg_push(value_node, terms, table)
                project[fname] = f"${fname}"
                post_aggregates.append(
                    (fname, "sorted_string", ([(d, nf) for _k, d, nf in terms], sagg[1]))
                )
            else:
                accumulators[fname] = {
                    "$push": _push_filtered(_agg_arg_to_expr(sagg[0], table), fcond)
                }
                project[fname] = _string_agg_project(fname, sagg[1])
            out_columns.append((fname, "text"))
        elif agg is not None and agg[0] in (set(_POST_STAT_FUNCS) | _BIT_AGG_FUNCS):
            # variance / var_pop (square of stdDev) and bit_and/or/xor (push + Python
            # fold) — a $group accumulator plus a post-aggregate finish that runs
            # over the unioned rows (same field name in every branch).
            func, col, _distinct = agg
            if col is None:
                raise errors.feature_not_supported(f"{func}(*) is not supported")
            if fcond is not None:
                raise errors.feature_not_supported(
                    f"FILTER (WHERE ...) on {func}() is not supported"
                )
            fname = names.fresh(alias or func)
            val = f"${table.field_for(col)}"
            if func in _BIT_AGG_FUNCS:
                accumulators[fname] = {"$push": val}
                tag = table.type_for(col) if table.type_for(col) in ("int4", "int8") else "int8"
                post_aggregates.append((fname, func, None))
            else:
                accumulators[fname] = {_POST_STAT_FUNCS[func]: val}
                tag = "numeric"
                post_aggregates.append((fname, "variance", None))
            project[fname] = f"${fname}"
            out_columns.append((fname, tag))
        elif agg is not None:
            func, col, distinct = agg
            # DISTINCT count/sum/avg → a $addToSet accumulator + a post-$group
            # reduction (mirrors the plain-GROUP-BY path); min/max ignore DISTINCT
            # (a distinct extremum equals the raw extremum) so they take the plain
            # accumulator, which threads any FILTER condition.
            if distinct and func in _DISTINCT_FUNCS:
                if col is None:
                    raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
                fname, tag = _register_distinct_agg(
                    func,
                    table.field_for(col),
                    table.type_for(col),
                    alias,
                    names,
                    accumulators,
                    reductions,
                    fcond,
                )
            else:
                acc, tag = _accumulator(func, col, table, fcond, arg_node=_agg_expr_arg(e))
                fname = names.fresh(alias or func)
                accumulators[fname] = acc
            project[fname] = f"${fname}"
            out_columns.append((fname, tag))
        else:
            inner = e.this if isinstance(e, exp.Alias) else e
            if isinstance(inner, exp.Star):
                raise errors.feature_not_supported("SELECT * with GROUP BY is not supported")
            col = _column_name(inner)
            out_name = names.fresh(alias or col)
            if col in in_set:
                project[out_name] = f"$_id.{col}"
            else:
                # A group column absent from this set (or an ungrouped column) reads
                # NULL for these rows.
                table.field_for(col)  # validate it's a real column
                project[out_name] = {"$literal": None}
            out_columns.append((out_name, table.type_for(col)))
    # Resolve HAVING before building the $group so any hidden accumulator it needs
    # is included; the resulting $match runs on the grouped doc (which carries
    # `_id.<col>` for this set's group columns and the accumulator fields).
    having = stmt.args.get("having")
    having_match = (
        _having_to_match(having.this, table, accumulators, {}, group_cols, names, reductions)
        if having is not None
        else None
    )
    stages: list[dict[str, Any]] = [{"$group": {"_id": group_id, **accumulators}}]
    if reductions:
        # Reduce each DISTINCT set to its scalar value before HAVING / the projection.
        stages.append({"$addFields": reductions})
    if having_match is not None:
        stages.append({"$match": having_match})
    stages.append({"$project": project})
    return stages, out_columns, post_aggregates


def _plan_grouping_sets_select(
    stmt: exp.Select, table: TableDef, gkey_addfields: dict[str, Any] | None = None
) -> PipelineSelectPlan:
    """GROUP BY ROLLUP / CUBE / GROUPING SETS → the UNION (via ``$unionWith``) of a
    plain GROUP BY per enumerated grouping set.

    ``gkey_addfields`` materialises any computed GROUP BY keys (``ROLLUP(lower(x))``)
    into synthetic ``__gkeyN`` fields; it runs before each branch's ``$group`` — in
    the base pipeline *and* every ``$unionWith`` sub-pipeline (which reads the
    collection fresh, so the fields must be recomputed there too)."""
    if _residual_where(stmt, table) is not None:
        raise errors.feature_not_supported("a correlated WHERE with GROUPING SETS is not supported")
    base_filter = _where_filter(stmt, table)
    add_stage = [{"$addFields": gkey_addfields}] if gkey_addfields else []
    sets = _grouping_sets(stmt.args["group"])
    # HAVING may reference any column that appears in some grouping set.
    group_cols = sorted({c for gs in sets for c in gs})
    branches = [_grouping_set_branch(stmt, table, gs, group_cols) for gs in sets]
    pipeline = add_stage + list(branches[0][0])
    out_columns = branches[0][1]
    # The statistical / bitwise post-aggregate finish is identical across branches
    # (same aggregates → same field names), so one copy applies to the whole union.
    post_aggregates = branches[0][2]
    prefix = [{"$match": base_filter}] if base_filter else []
    for sub, _cols, _post in branches[1:]:
        pipeline.append(
            {"$unionWith": {"coll": table.collection, "pipeline": prefix + add_stage + sub}}
        )
    _append_sort_limit(pipeline, stmt, out_columns, table)
    return PipelineSelectPlan(
        table.collection, base_filter, pipeline, out_columns, post_aggregates=post_aggregates
    )


def _plan_grouping_sets_window_select(
    stmt: exp.Select, table: TableDef, gkey_addfields: dict[str, Any] | None = None
) -> EvaluatedSelectPlan:
    """Window function(s) over a GROUP BY ROLLUP / CUBE / GROUPING SETS query — e.g.
    ``SELECT region, SUM(amt), row_number() OVER (ORDER BY SUM(amt) DESC)
    FROM t GROUP BY ROLLUP(region)``.

    Phase 1: the grouping-sets ``$unionWith`` pipeline produces the grouped rows,
    but each branch projects *flat* group-column + aggregate fields (not the final
    SELECT expressions, which contain the window). Phase 2: the evaluated executor
    computes each window over the union's rows, and every group aggregate /
    ``GROUPING()`` reference resolves to its precomputed field — including the
    rolled-up rows (a group column absent from a set reads NULL, so a window
    ``ORDER BY SUM(amt)`` still orders the grand-total row)."""
    stmt = stmt.copy()  # we mutate the tree, replacing aggregates / GROUPING with columns
    if _residual_where(stmt, table) is not None:
        raise errors.feature_not_supported(
            "a correlated WHERE with a window over GROUPING SETS is not supported"
        )
    having = stmt.args.get("having")
    if having is not None and having.this.find(exp.Select) is not None:
        raise errors.feature_not_supported(
            "a subquery in HAVING with a window over GROUPING SETS is not supported"
        )
    base_filter = _where_filter(stmt, table)
    add_stage = [{"$addFields": gkey_addfields}] if gkey_addfields else []
    sets = _grouping_sets(stmt.args["group"])
    group_cols = sorted({c for gs in sets for c in gs})

    names = _NameAllocator()
    for c in group_cols:  # reserve group names so synthetic fields never collide
        names.fresh(c)
    field_tags: dict[str, str] = {c: table.type_for(c) for c in group_cols}
    accumulators: dict[str, Any] = {}
    reductions: dict[str, Any] = {}
    agg_fields: dict[tuple[str, str | None, bool], str] = {}
    agg_field_names: list[str] = []

    def register_agg(node: exp.AggFunc) -> str:
        arr_arg = _array_agg_arg(node)
        if arr_arg is not None:
            fname = names.fresh("array_agg")
            accumulators[fname] = {"$push": _agg_arg_to_expr(arr_arg, table)}
            field_tags[fname] = "json"
            agg_field_names.append(fname)
            return fname
        sa = _string_agg_arg(node)
        if sa is not None:
            # A function-wrapped ``string_agg`` (``decode(string_agg(…),
            # 'hex')`` — RefCursorFetchTest's seeding INSERT) reaches the
            # computed-projection registrar; push the values and join with the
            # separator in the reduction, like the plain string_agg path does.
            sa_expr, sep = sa
            fname = names.fresh("string_agg")
            accumulators[fname] = {"$push": _agg_arg_to_expr(sa_expr, table)}
            reductions[fname] = _string_agg_project(fname, sep)
            field_tags[fname] = "text"
            agg_field_names.append(fname)
            return fname
        agg = _aggregate_of(node)
        if agg is None:
            raise errors.feature_not_supported(f"unsupported aggregate: {node.sql()}")
        agg = _single_agg_key(node, agg)
        if agg in agg_fields:
            return agg_fields[agg]
        func, col, distinct = _aggregate_of(node)
        where = _agg_filter_where(node)
        fcond = _filter_cond_to_agg(where, _table_resolve(table)) if where is not None else None
        if distinct and func in _DISTINCT_FUNCS:
            if col is None:
                arg_node = _agg_expr_arg(node)
                if arg_node is None or isinstance(arg_node, exp.Star):
                    raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
                fname, tag = _register_distinct_agg(
                    func,
                    None,
                    _infer_scalar_tag(arg_node, table_resolver(table)),
                    None,
                    names,
                    accumulators,
                    reductions,
                    fcond,
                    value=_agg_arg_to_expr(arg_node, table),
                )
            else:
                fname, tag = _register_distinct_agg(
                    func,
                    table.field_for(col),
                    table.type_for(col),
                    None,
                    names,
                    accumulators,
                    reductions,
                    fcond,
                )
        else:
            acc, tag = _accumulator(func, col, table, fcond, arg_node=_agg_expr_arg(node))
            fname = names.fresh(func)
            accumulators[fname] = acc
            if func == "sum":
                value = (
                    f"${table.field_for(col)}"
                    if col is not None
                    else _agg_arg_to_expr(_agg_expr_arg(node), table)
                )
                _guard_sum_null(fname, value, fcond, names, accumulators, reductions)
        agg_fields[agg] = fname
        field_tags[fname] = tag
        agg_field_names.append(fname)
        return fname

    # ``GROUPING(col, …)`` is a per-branch literal bitmask, not an aggregate —
    # assign each a synthetic field, materialise it per branch, and rewrite the node
    # to a column reference so the window phase resolves it like any grouped field.
    grouping_specs: list[tuple[str, list[str]]] = []
    for gnode in list(stmt.find_all(exp.Grouping)):
        gcols = [_column_name(a) for a in gnode.expressions]
        for c in gcols:
            table.field_for(c)  # validate
        gfname = names.fresh("grouping")
        field_tags[gfname] = "int4"
        grouping_specs.append((gfname, gcols))
        gnode.replace(exp.column(gfname))

    # Replace each group aggregate with a reference to its computed field.
    for node in _group_agg_nodes(stmt):
        node.replace(exp.column(register_agg(node)))

    # HAVING filters the grouped rows before the window; register it once (it may add
    # accumulators to the shared dict) and apply the same ``$match`` in every branch.
    having_match = (
        _having_to_match(
            having.this, table, accumulators, agg_fields, group_cols, names, reductions
        )
        if having is not None
        else None
    )

    def branch(gset: list[str]) -> list[dict[str, Any]]:
        in_set = set(gset)
        group_id = {c: f"${table.field_for(c)}" for c in gset} or None
        project: dict[str, Any] = {"_id": 0}
        for c in group_cols:
            project[c] = f"$_id.{c}" if c in in_set else {"$literal": None}
        for fname in agg_field_names:
            project[fname] = f"${fname}"
        for gfname, gcols in grouping_specs:
            project[gfname] = {"$literal": _grouping_bitmask(gcols, in_set)}
        stages: list[dict[str, Any]] = [{"$group": {"_id": group_id, **accumulators}}]
        if reductions:
            stages.append({"$addFields": reductions})
        if having_match is not None:
            stages.append({"$match": having_match})
        stages.append({"$project": project})
        return stages

    pipeline = add_stage + branch(sets[0])
    prefix = [{"$match": base_filter}] if base_filter else []
    for gset in sets[1:]:
        pipeline.append(
            {
                "$unionWith": {
                    "coll": table.collection,
                    "pipeline": prefix + add_stage + branch(gset),
                }
            }
        )
    return _finish_group_window(stmt, table.collection, base_filter, pipeline, field_tags)


def _plan_group_select(stmt: exp.Select, table: TableDef) -> PipelineSelectPlan:
    # A correlated / EXISTS WHERE can't push to a Mongo filter — carry it for
    # per-base-doc evaluation before the $group (the executor filters, then groups).
    residual = _residual_where(stmt, table)
    base_filter = {} if residual is not None else _where_filter(stmt, table)
    group_node = stmt.args.get("group")
    group_cols = [_column_name(c) for c in group_node.expressions] if group_node else []
    for c in group_cols:
        table.field_for(c)  # validate
    group_id = {c: f"${table.field_for(c)}" for c in group_cols} or None

    accumulators: dict[str, Any] = {}
    reductions: dict[str, Any] = {}
    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    out_enum_types: dict[int, str] = {}
    post_aggregates: list[tuple[str, str, float | None]] = []
    names = _NameAllocator()
    agg_fields: dict[tuple[str, str | None, bool], str] = {}

    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        grp = _grouping_args(e)
        arr_arg = _array_agg_arg(e)
        oagg = _jsonb_object_agg_args(e)
        ragg = _range_agg_arg(e)
        sagg = _string_agg_arg(e)
        agg = _aggregate_of(e)
        osa = _ordered_set_agg(e)
        where = _agg_filter_where(e)
        fcond = _filter_cond_to_agg(where, _table_resolve(table)) if where is not None else None
        if grp is not None:
            # Plain GROUP BY: every argument is grouped, so GROUPING() is always 0.
            for c in grp:
                table.field_for(c)  # validate
            fname = names.fresh(alias or "grouping")
            project[fname] = {"$literal": _grouping_bitmask(grp, set(group_cols))}
            out_columns.append((fname, "int4"))
        elif oagg is not None:
            fname = names.fresh(alias or "jsonb_object_agg")
            accumulators[fname] = _jsonb_object_agg_push(oagg[0], oagg[1], table, fcond)
            project[fname] = _jsonb_object_agg_project(fname, fcond)
            out_columns.append((fname, "json"))
        elif ragg is not None:
            fname = names.fresh(alias or "range_agg")
            accumulators[fname] = {"$push": _agg_arg_to_expr(ragg, table)}
            project[fname] = f"${fname}"
            post_aggregates.append((fname, "range_agg", None))
            out_columns.append((fname, _multirange_tag_for_arg(ragg, table)))
        elif osa is not None:
            kind, fraction, order_val = osa
            fname = names.fresh(alias or kind)
            # Collect the ORDER BY values; the executor sorts + computes in Python.
            accumulators[fname] = {"$push": _agg_arg_to_expr(order_val, table)}
            project[fname] = f"${fname}"
            if kind == "percentile_cont":
                tag = "float8"
            elif isinstance(order_val, exp.Column):
                tag = table.type_for(order_val.name)
            else:
                tag = "float8"
            post_aggregates.append((fname, kind, fraction))
            out_columns.append((fname, tag))
        elif arr_arg is not None:
            fname = names.fresh(alias or "array_agg")
            value_node, terms = _agg_order_spec(arr_arg)
            if terms:  # array_agg(x ORDER BY …): push {v, k}, executor sorts
                if fcond is not None:
                    raise errors.feature_not_supported(
                        "FILTER (WHERE ...) with an in-aggregate ORDER BY is not supported"
                    )
                accumulators[fname] = _sorted_agg_push(value_node, terms, table)
                post_aggregates.append((fname, "sorted_array", [(d, nf) for _k, d, nf in terms]))
                project[fname] = f"${fname}"
            else:
                accumulators[fname] = {
                    "$push": _push_filtered(_agg_arg_to_expr(arr_arg, table), fcond, wrap=True)
                }
                project[fname] = _array_agg_project(fname, fcond)
            out_columns.append(
                (
                    fname,
                    _array_agg_out_tag(arr_arg, table_resolver(table))
                    if _is_true_array_agg(e)
                    else "json",
                )
            )
        elif sagg is not None:
            fname = names.fresh(alias or "string_agg")
            value_node, terms = _agg_order_spec(sagg[0])
            if terms:  # string_agg(x, sep ORDER BY …): push {v, k}, executor sorts+joins
                if fcond is not None:
                    raise errors.feature_not_supported(
                        "FILTER (WHERE ...) with an in-aggregate ORDER BY is not supported"
                    )
                accumulators[fname] = _sorted_agg_push(value_node, terms, table)
                project[fname] = f"${fname}"
                post_aggregates.append(
                    (fname, "sorted_string", ([(d, nf) for _k, d, nf in terms], sagg[1]))
                )
            else:
                accumulators[fname] = {
                    "$push": _push_filtered(_agg_arg_to_expr(sagg[0], table), fcond)
                }
                project[fname] = _string_agg_project(fname, sagg[1])
            out_columns.append((fname, "text"))
        elif agg is not None and agg[0] in (set(_POST_STAT_FUNCS) | _BIT_AGG_FUNCS):
            # variance / var_pop (square of stdDev) and bit_and/or/xor (push +
            # Python fold) — a $group accumulator plus a post-aggregate finish.
            func, col, _distinct = agg
            if col is None:
                raise errors.feature_not_supported(f"{func}(*) is not supported")
            if fcond is not None:
                raise errors.feature_not_supported(
                    f"FILTER (WHERE ...) on {func}() is not supported"
                )
            fname = names.fresh(alias or func)
            val = f"${table.field_for(col)}"
            if func in _BIT_AGG_FUNCS:
                accumulators[fname] = {"$push": val}
                tag = table.type_for(col) if table.type_for(col) in ("int4", "int8") else "int8"
                post_aggregates.append((fname, func, None))
            else:
                accumulators[fname] = {_POST_STAT_FUNCS[func]: val}
                tag = "numeric"
                post_aggregates.append((fname, "variance", None))
            project[fname] = f"${fname}"
            out_columns.append((fname, tag))
        elif agg is not None:
            func, col, distinct = agg
            if distinct and func in _DISTINCT_FUNCS:
                if col is None:
                    arg_node = _agg_expr_arg(e)
                    if arg_node is None or isinstance(arg_node, exp.Star):
                        raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
                    # An expression DISTINCT argument (``SUM(DISTINCT 77)``)
                    # pushes the lowered expression into the distinct set.
                    fname, tag = _register_distinct_agg(
                        func,
                        None,
                        _infer_scalar_tag(arg_node, table_resolver(table)),
                        alias,
                        names,
                        accumulators,
                        reductions,
                        fcond,
                        value=_agg_arg_to_expr(arg_node, table),
                    )
                else:
                    fname, tag = _register_distinct_agg(
                        func,
                        table.field_for(col),
                        table.type_for(col),
                        alias,
                        names,
                        accumulators,
                        reductions,
                        fcond,
                    )
            else:
                acc, tag = _accumulator(func, col, table, fcond, arg_node=_agg_expr_arg(e))
                fname = names.fresh(alias or func)
                accumulators[fname] = acc
                if func == "sum":
                    value = (
                        f"${table.field_for(col)}"
                        if col is not None
                        else _agg_arg_to_expr(_agg_expr_arg(e), table)
                    )
                    _guard_sum_null(fname, value, fcond, names, accumulators, reductions)
            agg_fields[_single_agg_key(e, (func, col, distinct))] = fname
            project[fname] = f"${fname}"
            out_columns.append((fname, tag))
        else:
            inner = e.this if isinstance(e, exp.Alias) else e
            if isinstance(inner, exp.Star):
                raise errors.feature_not_supported("SELECT * with GROUP BY is not supported")
            col = _column_name(inner)
            if col not in group_cols:
                raise errors.SQLError(
                    "42803",
                    f'column "{col}" must appear in the GROUP BY clause '
                    "or be used in an aggregate function",
                )
            out_name = names.fresh(alias or col)
            project[out_name] = f"$_id.{col}"
            enum_name = _projected_enum_type(inner, table)
            if enum_name is not None:
                out_enum_types[len(out_columns)] = enum_name
            out_columns.append((out_name, table.type_for(col)))

    # Resolve HAVING first — it may register hidden accumulators that must be
    # present in the $group stage built below.
    having = stmt.args.get("having")
    having_match = (
        _having_to_match(
            having.this, table, accumulators, agg_fields, group_cols, names, reductions
        )
        if having is not None
        else None
    )
    # An ORDER BY aggregate that isn't in the select list gets a hidden accumulator,
    # projected so the $sort can reach it but kept out of out_columns.
    order_aggs = _register_orderby_aggs_single(
        stmt, table, accumulators, reductions, project, names
    )
    pipeline: list[dict[str, Any]] = [{"$group": {"_id": group_id, **accumulators}}]
    # Reduce any DISTINCT sets to their scalar value before HAVING / projection.
    if reductions:
        pipeline.append({"$addFields": reductions})
    if having_match is not None:
        pipeline.append({"$match": having_match})
    pipeline.append({"$project": project})
    if stmt.args.get("distinct") and not post_aggregates and not order_aggs:
        # SELECT DISTINCT over grouped output (``SELECT DISTINCT col1 … GROUP BY
        # col1, col0``) dedups the projected rows — a second $group over every
        # output column. Skipped when the executor still has to finish
        # post-aggregates (their values aren't final in the pipeline) or when a
        # hidden ORDER BY aggregate must survive to the $sort.
        dedup_id = {name: f"${name}" for name, _tag in out_columns}
        pipeline.append({"$group": {"_id": dedup_id}})
        pipeline.append({"$project": {"_id": 0, **{n: f"$_id.{n}" for n in dedup_id}}})
    _append_sort_limit(pipeline, stmt, out_columns, table, order_aggs=order_aggs)
    return PipelineSelectPlan(
        table.collection,
        base_filter,
        pipeline,
        out_columns,
        out_enum_types=out_enum_types,
        residual_where=residual,
        residual_resolve=table_resolver(table) if residual is not None else None,
        post_aggregates=post_aggregates,
    )


def _residual_where(stmt: exp.Select, table: TableDef) -> exp.Expression | None:
    """The WHERE predicate to evaluate per-row (rather than push down) when it
    references the outer row — an ``EXISTS`` or correlated subquery. ``None`` when
    the WHERE (if any) lowers cleanly to a Mongo filter."""
    where = stmt.args.get("where")
    if where is None or not where_needs_per_row(stmt, table):
        return None
    return where.this


def _select_has_window(stmt: exp.Select) -> bool:
    """Whether any SELECT item or ORDER BY term contains a window (``OVER``)."""
    roots: list[exp.Expression] = list(stmt.expressions)
    order = stmt.args.get("order")
    if order is not None:
        roots.extend(o.this for o in order.expressions)
    return any(next(r.find_all(exp.Window), None) is not None for r in roots)


def _is_bare_aggregate(e: exp.Expression) -> bool:
    """Whether ``e`` *is* an aggregate call (any recognised family), rather than an
    expression that merely contains one."""
    return (
        _aggregate_of(e) is not None
        or _array_agg_arg(e) is not None
        or _jsonb_object_agg_args(e) is not None
        or _range_agg_arg(e) is not None
        or _string_agg_arg(e) is not None
        or _ordered_set_agg(e) is not None
    )


def _expand_grouped_star(stmt: exp.Select, table: TableDef) -> None:
    """Expand ``SELECT *`` under GROUP BY into explicit columns, in place, when
    every table column is a group key (Postgres allows the star there since
    each output is a grouping column). Left untouched otherwise — the group
    planners' own 42803 check reports the offending column."""
    if not any(
        isinstance(e.this if isinstance(e, exp.Alias) else e, exp.Star) for e in stmt.expressions
    ):
        return
    group_node = stmt.args.get("group")
    if group_node is None:
        return
    try:
        group_cols = {_column_name(g) for g in group_node.expressions}
    except errors.SQLError:
        return
    if not all(c.name in group_cols for c in table.columns):
        return
    new_exprs: list[exp.Expression] = []
    for e in stmt.expressions:
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Star):
            new_exprs.extend(exp.column(c.name) for c in table.columns)
        else:
            new_exprs.append(e)
    stmt.set("expressions", new_exprs)


def _group_projection_needs_evaluation(stmt: exp.Select) -> bool:
    """Whether a grouped SELECT projects an expression the ``$group`` planner's
    projection loop can't shape — not a bare column / ``*`` and not one of the
    recognized aggregate forms (``-col0 * 84 + 38`` over a group key, a bare
    constant under GROUP BY, a CASE over group keys, …). Those route to the
    group-then-evaluate path, which computes arbitrary expressions over the
    grouped rows. A projection that *is* a computed GROUP BY key is excluded —
    the group planners rewrite those to synthetic key columns themselves."""
    group_node = stmt.args.get("group")
    group_sqls = {g.sql() for g in group_node.expressions} if group_node is not None else set()
    for e in stmt.expressions:
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, (exp.Star, exp.Column)):
            continue
        if inner.sql() in group_sqls:
            continue
        if (
            _grouping_args(e) is not None
            or _array_agg_arg(e) is not None
            or _jsonb_object_agg_args(e) is not None
            or _range_agg_arg(e) is not None
            or _string_agg_arg(e) is not None
            or _ordered_set_agg(e) is not None
            or _aggregate_of(e) is not None
        ):
            continue
        return True
    return False


def _select_has_computed_aggregate(stmt: exp.Select) -> bool:
    """Whether any SELECT item or ORDER BY term is an *expression over* an aggregate
    (e.g. ``sum(x) + 1``, ``round(avg(x), 2)``) rather than a bare aggregate — those
    are projected by the evaluated group path (which runs the wrapping expression
    over the grouped rows). A bare aggregate stays on the fast ``$group`` path."""
    roots: list[exp.Expression] = list(stmt.expressions)
    order = stmt.args.get("order")
    if order is not None:
        roots.extend(o.this for o in order.expressions)
    for e in roots:
        inner = e.this if isinstance(e, exp.Alias) else e
        if _is_bare_aggregate(inner) or _grouping_args(e) is not None:
            continue
        # ``GROUPING()`` subclasses AggFunc but is a super-aggregate helper handled
        # by the group / grouping-sets planner, not a real aggregate. An aggregate
        # inside a scalar subquery belongs to that inner query, not this one.
        agg = next(
            (
                a
                for a in inner.find_all(exp.AggFunc)
                if not isinstance(a, exp.Grouping) and not _nested_in_subquery(a, inner)
            ),
            None,
        )
        if agg is not None:
            return True
    return False


def _select_projects_subquery(stmt: exp.Select) -> bool:
    """Whether any SELECT item projects a subquery (``SELECT g, (SELECT max(v) FROM u)
    FROM t``). Such a query — when it is also grouped — routes to the evaluated group
    path, which runs each projection (including the subquery) per grouped row rather
    than through the fast ``$group``/``$project`` path (which can't project a
    subquery)."""
    for e in stmt.expressions:
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Subquery) or inner.find(exp.Subquery) is not None:
            return True
    return False


def _having_has_subquery(stmt: exp.Select) -> bool:
    """Whether the HAVING clause contains a subquery (``HAVING sum(x) > (SELECT …)``).
    Such a predicate can't lower to a post-``$group`` ``$match`` — it is carried as a
    per-grouped-row residual and evaluated by the executor (which can run a
    correlated inner query against the group key)."""
    having = stmt.args.get("having")
    return having is not None and having.this.find(exp.Select) is not None


def _nested_in_subquery(n: exp.Expression, root: exp.Expression) -> bool:
    """Whether ``n`` (found under ``root`` via ``find_all``) is nested inside a
    subquery *below* ``root`` — i.e. it belongs to an inner ``(SELECT …)`` rather
    than this query level. An aggregate in a scalar subquery in the SELECT list
    (``SELECT g, (SELECT max(v) FROM u) FROM t``) is the inner query's aggregate,
    not an outer GROUP BY aggregate."""
    if n is root:
        return False
    p = n.parent
    while p is not None:
        if p is root:
            return False
        if isinstance(p, (exp.Subquery, exp.Select)):
            return True
        p = p.parent
    return False


def _outer_agg_nodes(node: exp.Expression) -> list[exp.AggFunc]:
    """The aggregates directly in ``node`` (e.g. a HAVING predicate) that are *not*
    nested inside a subquery — those belong to this query's GROUP BY, whereas an
    aggregate inside a ``(SELECT …)`` belongs to that inner query."""
    return [n for n in node.find_all(exp.AggFunc) if not _nested_in_subquery(n, node)]


def _group_agg_nodes(stmt: exp.Select) -> list[exp.AggFunc]:
    """Every aggregate (``SUM``/``COUNT``/… / ``array_agg``) in the SELECT list and
    ORDER BY that is *not* itself a window function — i.e. not the direct operand
    of an ``OVER`` clause. These are the GROUP BY aggregates; a window aggregate
    like ``SUM(...) OVER (...)`` is computed later, over the grouped rows. An
    aggregate nested inside a window aggregate (``SUM(SUM(x)) OVER ()``) is still
    a group aggregate — only the outermost, window-owned one is excluded."""
    roots: list[exp.Expression] = list(stmt.expressions)
    order = stmt.args.get("order")
    if order is not None:
        roots.extend(o.this for o in order.expressions)
    found: list[exp.AggFunc] = []
    for root in roots:
        for n in root.find_all(exp.AggFunc):
            parent = n.parent
            if isinstance(parent, exp.Window) and parent.this is n:
                continue  # a window aggregate — resolved over the grouped rows
            if _nested_in_subquery(n, root):
                continue  # an aggregate inside a scalar subquery — the inner query's
            found.append(n)
    return found


def _synthetic_resolver(field_tags: dict[str, str]) -> Resolve:
    """A column resolver over the flat, post-``$group`` document — group columns
    and synthetic aggregate fields resolve to themselves; anything else is a
    non-grouped column reference, which Postgres rejects with 42803."""

    def resolve(node: exp.Expression) -> tuple[str, str]:
        col = _column_name(node)
        if col in field_tags:
            return col, field_tags[col]
        raise errors.SQLError(
            "42803",
            f'column "{col}" must appear in the GROUP BY clause '
            "or be used in an aggregate function",
        )

    return resolve


def _plan_group_window_select(stmt: exp.Select, table: TableDef) -> EvaluatedSelectPlan:
    """GROUP BY (or an implicit whole-table aggregation) combined with window
    functions in the same SELECT — e.g. ``SELECT dept, SUM(sal),
    RANK() OVER (ORDER BY SUM(sal)) FROM emp GROUP BY dept``.

    Phase 1 (aggregation pipeline): a ``$group`` computes the grouping columns and
    every group aggregate into flat fields. Phase 2 (the evaluated executor): the
    window functions run over those grouped rows, and each aggregate reference —
    inside the window's args / PARTITION BY / ORDER BY, or standing alone in the
    SELECT list — resolves to its precomputed field."""
    stmt = stmt.copy()  # we mutate the tree, replacing aggregates with columns
    # A correlated / EXISTS WHERE can't push to a Mongo filter — carry it for
    # per-base-doc evaluation before the $group (WHERE precedes grouping).
    residual_pre = _residual_where(stmt, table)
    base_filter = {} if residual_pre is not None else _where_filter(stmt, table)
    group_node = stmt.args.get("group")
    group_cols = [_column_name(c) for c in group_node.expressions] if group_node else []
    for c in group_cols:
        table.field_for(c)  # validate
    group_id = {c: f"${table.field_for(c)}" for c in group_cols} or None

    accumulators: dict[str, Any] = {}
    reductions: dict[str, Any] = {}
    names = _NameAllocator()
    for c in group_cols:  # reserve group names so synthetic fields never collide
        names.fresh(c)
    field_tags: dict[str, str] = {c: table.type_for(c) for c in group_cols}
    agg_fields: dict[tuple[str, str | None, bool], str] = {}
    agg_field_names: list[str] = []

    def register_agg(node: exp.AggFunc) -> str:
        arr_arg = _array_agg_arg(node)
        if arr_arg is not None:
            fname = names.fresh("array_agg")
            accumulators[fname] = {"$push": _agg_arg_to_expr(arr_arg, table)}
            field_tags[fname] = "json"
            agg_field_names.append(fname)
            return fname
        sa = _string_agg_arg(node)
        if sa is not None:
            # A function-wrapped ``string_agg`` (``decode(string_agg(…),
            # 'hex')`` — RefCursorFetchTest's seeding INSERT) reaches the
            # computed-projection registrar; push the values and join with the
            # separator in the reduction, like the plain string_agg path does.
            sa_expr, sep = sa
            fname = names.fresh("string_agg")
            accumulators[fname] = {"$push": _agg_arg_to_expr(sa_expr, table)}
            reductions[fname] = _string_agg_project(fname, sep)
            field_tags[fname] = "text"
            agg_field_names.append(fname)
            return fname
        agg = _aggregate_of(node)
        if agg is None:
            raise errors.feature_not_supported(f"unsupported aggregate: {node.sql()}")
        agg = _single_agg_key(node, agg)
        if agg in agg_fields:
            return agg_fields[agg]
        func, col, distinct = _aggregate_of(node)
        where = _agg_filter_where(node)
        fcond = _filter_cond_to_agg(where, _table_resolve(table)) if where is not None else None
        if distinct and func in _DISTINCT_FUNCS:
            if col is None:
                arg_node = _agg_expr_arg(node)
                if arg_node is None or isinstance(arg_node, exp.Star):
                    raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
                fname, tag = _register_distinct_agg(
                    func,
                    None,
                    _infer_scalar_tag(arg_node, table_resolver(table)),
                    None,
                    names,
                    accumulators,
                    reductions,
                    fcond,
                    value=_agg_arg_to_expr(arg_node, table),
                )
            else:
                fname, tag = _register_distinct_agg(
                    func,
                    table.field_for(col),
                    table.type_for(col),
                    None,
                    names,
                    accumulators,
                    reductions,
                    fcond,
                )
        else:
            acc, tag = _accumulator(func, col, table, fcond, arg_node=_agg_expr_arg(node))
            fname = names.fresh(func)
            accumulators[fname] = acc
            if func == "sum":
                value = (
                    f"${table.field_for(col)}"
                    if col is not None
                    else _agg_arg_to_expr(_agg_expr_arg(node), table)
                )
                _guard_sum_null(fname, value, fcond, names, accumulators, reductions)
        agg_fields[agg] = fname
        field_tags[fname] = tag
        agg_field_names.append(fname)
        return fname

    # Replace each group aggregate with a reference to its computed field. The
    # nodes were collected from the original tree (parents intact); group
    # aggregates never nest without a window between them, so replacement order
    # is immaterial.
    for node in _group_agg_nodes(stmt):
        node.replace(exp.column(register_agg(node)))

    having = stmt.args.get("having")
    having_match = None
    residual_having: exp.Expression | None = None
    if having is not None:
        if having.this.find(exp.Select) is not None:
            # A subquery in HAVING (`HAVING sum(x) > (SELECT … WHERE t.k = g.k)`) can't
            # lower to a `$match` — rewrite its aggregates to their computed fields
            # and carry it as a per-grouped-row residual the evaluated executor runs
            # (the correlated inner query resolves the group key through the scope).
            for node in _outer_agg_nodes(having.this):
                node.replace(exp.column(register_agg(node)))
            residual_having = having.this
        else:
            try:
                having_match = _having_to_match(
                    having.this, table, accumulators, agg_fields, group_cols, names, reductions
                )
            except errors.SQLError as exc:
                # Only "we can't lower this shape" (0A000) falls back — a real
                # user error (42803 "must appear in the GROUP BY clause", say)
                # has to surface, not be silently deferred to a residual that
                # would then evaluate it as a plain expression.
                if exc.sqlstate != "0A000":
                    raise
                # The same route the HAVING-subquery case takes: rewrite the
                # aggregates to their computed fields and evaluate the predicate
                # per grouped row. This is what keeps a HAVING shape the $match
                # lowerer doesn't cover from being a hard 0A000.
                for node in _outer_agg_nodes(having.this):
                    node.replace(exp.column(register_agg(node)))
                residual_having = having.this
                having_match = None

    pipeline: list[dict[str, Any]] = [{"$group": {"_id": group_id, **accumulators}}]
    if reductions:
        pipeline.append({"$addFields": reductions})
    if having_match is not None:
        pipeline.append({"$match": having_match})
    project: dict[str, Any] = {"_id": 0}
    for c in group_cols:
        project[c] = f"$_id.{c}"
    for fname in agg_field_names:
        project[fname] = f"${fname}"
    pipeline.append({"$project": project})
    return _finish_group_window(
        stmt,
        table.collection,
        base_filter,
        pipeline,
        field_tags,
        where=residual_having,
        pre_where=residual_pre,
        pre_where_resolve=table_resolver(table) if residual_pre is not None else None,
    )


def _finish_group_window(
    stmt: exp.Select,
    base_collection: str,
    base_filter: dict[str, Any],
    pipeline: list[dict[str, Any]],
    field_tags: dict[str, str],
    derived: list[DerivedTable] | None = None,
    where: exp.Expression | None = None,
    pre_where: exp.Expression | None = None,
    pre_where_resolve: Resolve | None = None,
    pre_where_split: int = 0,
) -> EvaluatedSelectPlan:
    """Shared tail of the group-then-window planners: with the grouped rows'
    field→tag map in hand, build the per-row output expressions, the window-alias
    aware ORDER BY, and the ``EvaluatedSelectPlan`` that runs the window phase."""
    resolve = _synthetic_resolver(field_tags)
    out_columns: list[tuple[str, str]] = []
    out_exprs: list[exp.Expression] = []
    alias_exprs: dict[str, exp.Expression] = {}
    onames = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Star):
            raise errors.feature_not_supported("SELECT * with GROUP BY is not supported")
        name = alias or (
            _column_name(inner)
            if isinstance(inner, exp.Column)
            else _cast_output_name(inner) or "?column?"
        )
        out_columns.append((onames.fresh(name), _infer_scalar_tag(inner, resolve)))
        out_exprs.append(inner)
        if alias is not None:
            alias_exprs[alias] = inner

    # ORDER BY may reference a SELECT output alias (``ORDER BY rk``) or an
    # ordinal (``ORDER BY 1``) — Postgres resolves both to the output
    # expression, so sorting on a window/aggregate/computed output works even
    # though the grouped rows carry no such field.
    order: list[tuple[exp.Expression, int, bool]] = []
    order_node = stmt.args.get("order")
    if order_node is not None:
        for o in order_node.expressions:
            term = o.this
            if isinstance(term, exp.Literal) and not term.is_string and str(term.name).isdigit():
                idx = int(term.name) - 1
                if 0 <= idx < len(out_exprs):
                    term = out_exprs[idx]
            elif isinstance(term, exp.Column) and not term.table and term.name in alias_exprs:
                term = alias_exprs[term.name]
            order.append((term, -1 if o.args.get("desc") else 1, _nulls_first(o)))

    limit, skip = _limit_skip(stmt)
    return EvaluatedSelectPlan(
        base_collection=base_collection,
        base_filter=base_filter,
        pipeline=pipeline,
        out_columns=out_columns,
        out_exprs=out_exprs,
        resolve=resolve,
        order=order,
        distinct=bool(stmt.args.get("distinct")),
        limit=limit,
        skip=skip,
        derived=derived or [],
        where=where,
        pre_where=pre_where,
        pre_where_resolve=pre_where_resolve,
        pre_where_split=pre_where_split,
    )


def _constant_predicate_filter(node: exp.Expression) -> dict[str, Any] | None:
    """Fold a constant predicate (no columns, aggregates, or subqueries — e.g.
    ``HAVING NOT NULL IS NULL``) to a match-all / match-nothing filter with
    three-valued semantics, or None when it isn't constant (or doesn't fold)."""
    if (
        next(node.find_all(exp.Column), None) is not None
        or next(node.find_all(exp.AggFunc), None) is not None
        or next(node.find_all(exp.Select), None) is not None
        # An Anonymous call (to_regtype, random, …) needs a live catalog /
        # session context — a context-free evaluation would fold wrongly.
        or next(node.find_all(exp.Anonymous), None) is not None
    ):
        return None
    from secantus.sql import scalar

    try:
        v = scalar.evaluate(node, _const_scope, scalar.ScalarContext(None, None, "", None))
    except errors.SQLError:
        return None
    return {} if v else {"$nor": [{}]}  # unknown (None) never satisfies


def _having_to_match(
    node: exp.Expression,
    table: TableDef,
    accumulators: dict[str, Any],
    agg_fields: dict[tuple[str, str | None], str],
    group_cols: list[str],
    names: Any = None,
    reductions: Any = None,
) -> dict[str, Any]:
    def rec(n: exp.Expression) -> dict[str, Any]:
        return _having_to_match(n, table, accumulators, agg_fields, group_cols, names, reductions)

    const = _constant_predicate_filter(node)
    if const is not None:
        return const
    if isinstance(node, exp.Paren):
        return rec(node.this)
    if isinstance(node, exp.And):
        return _merge_and([rec(node.this), rec(node.expression)])
    if isinstance(node, exp.Or):
        return {"$or": [rec(node.this), rec(node.expression)]}

    def field_tag(term: exp.Expression) -> tuple[str, str]:
        if isinstance(term, exp.Column):
            col = _column_name(term)
            if col not in group_cols:
                raise errors.SQLError(
                    "42803",
                    f'column "{col}" must appear in the GROUP BY clause '
                    "or be used in an aggregate function",
                )
            return f"_id.{col}", table.type_for(col)
        agg = _aggregate_of(term)
        if agg is None:
            raise errors.feature_not_supported(f"unsupported HAVING term: {term.sql()}")
        func, col, distinct = agg
        agg = _single_agg_key(term, agg)
        where = _agg_filter_where(term)
        fcond = _filter_cond_to_agg(where, _table_resolve(table)) if where is not None else None
        if distinct and func in _DISTINCT_FUNCS:
            if agg in agg_fields:  # already registered by the SELECT list — reuse
                return agg_fields[agg], _agg_out_tag(func, table.type_for(col) if col else None)
            if names is None or reductions is None or col is None:
                raise errors.feature_not_supported(
                    f"DISTINCT inside {func}() is not supported in HAVING"
                )
            fname, tag = _register_distinct_agg(
                func,
                table.field_for(col),
                table.type_for(col),
                None,
                names,
                accumulators,
                reductions,
                fcond,
            )
            agg_fields[agg] = fname
            return fname, tag
        acc, tag = _accumulator(func, col, table, fcond, arg_node=_agg_expr_arg(term))
        if agg not in agg_fields:
            fname = f"__having_{len(agg_fields)}"
            accumulators[fname] = acc
            agg_fields[agg] = fname
        return agg_fields[agg], tag

    if isinstance(node, (exp.EQ, exp.NEQ)) or type(node) in _HAVING_CMP:
        left, right = node.this, node.expression
        term, lit, on_left = (left, right, True)
        if not isinstance(left, (exp.Column, exp.Filter, *(_AGG_CLASSES.keys()))):
            term, lit, on_left = right, left, False
        field, tag = field_tag(term)
        value = typemap.coerce(_literal(lit), tag)
        if isinstance(node, exp.EQ):
            return {field: value}
        if isinstance(node, exp.NEQ):
            return {field: {"$ne": value}}
        op, flipped = _HAVING_CMP[type(node)]
        return {field: {(op if on_left else flipped): value}}

    # A NULL-literal operand that makes the predicate *always unknown* — a
    # NULL side of a comparison, a NULL IN a non-empty list, a NULL BETWEEN
    # subject (or both bounds NULL) — excludes the group, and NOT preserves
    # unknown, so the fold holds through any NOT / paren nesting.
    core = node
    while isinstance(core, (exp.Not, exp.Paren)):
        core = core.this
    if _always_unknown_predicate(core):
        return {"$nor": [{}]}

    # ``HAVING [NOT …] <operand> IS [NOT] NULL`` — IS NOT NULL parses as
    # Not(Is(…)); IS NULL is two-valued, so each NOT just flips the filter and
    # any nesting depth stays exact.
    is_node = node
    negate = False
    while isinstance(is_node, (exp.Not, exp.Paren)):
        if isinstance(is_node, exp.Not):
            negate = not negate
        is_node = is_node.this
    if isinstance(is_node, exp.Is) and isinstance(is_node.expression, exp.Null):

        def gk_resolve(col_node: exp.Expression) -> tuple[str, str]:
            if not isinstance(col_node, exp.Column):
                raise errors.feature_not_supported(f"expected a column: {col_node.sql()}")
            name = _column_name(col_node)
            if name not in group_cols:
                raise errors.SQLError(
                    "42803",
                    f'column "{name}" must appear in the GROUP BY clause '
                    "or be used in an aggregate function",
                )
            return f"_id.{name}", table.type_for(name)

        operand = is_node.this
        while isinstance(operand, exp.Paren):
            operand = operand.this
        if isinstance(operand, (exp.Column, exp.Filter, *(_AGG_CLASSES.keys()))):
            field, _tag = field_tag(operand)
            return {field: {"$ne": None}} if negate else {field: None}
        if next(operand.find_all(exp.AggFunc), None) is None:
            # A computed operand over group keys (``(- col2) IS NOT NULL``).
            value = _to_agg_expr(operand, gk_resolve)
            return {"$expr": {("$ne" if negate else "$eq"): [value, None]}}

    if (
        isinstance(is_node, exp.In)
        and is_node.args.get("query") is None
        and is_node.expressions
        and next(is_node.find_all(exp.AggFunc), None) is None
        and next(is_node.find_all(exp.Select), None) is None
    ):
        # ``HAVING [NOT] <expr> IN (<exprs over group keys>)`` — three-valued
        # membership over the grouped fields.
        def gk2_resolve(col_node: exp.Expression) -> tuple[str, str]:
            if not isinstance(col_node, exp.Column):
                raise errors.feature_not_supported(f"expected a column: {col_node.sql()}")
            name = _column_name(col_node)
            if name not in group_cols:
                raise errors.SQLError(
                    "42803",
                    f'column "{name}" must appear in the GROUP BY clause '
                    "or be used in an aggregate function",
                )
            return f"_id.{name}", table.type_for(name)

        return _having_in_filter(is_node, negate, gk2_resolve)

    raise errors.feature_not_supported(f"unsupported HAVING clause: {node.sql()}")


def _having_in_filter(in_node: exp.In, negate: bool, gk_resolve: Resolve) -> dict[str, Any]:
    """Lower ``[NOT] <expr> IN (<exprs>)`` over grouped fields with SQL
    three-valued semantics: IN is true when some candidate definitively equals
    the (non-null) left side; NOT IN is true only when the left side and every
    candidate are non-null and all differ (a NULL anywhere makes it unknown —
    the group is excluded either way)."""
    lhs = _to_agg_expr(in_node.this, gk_resolve)
    cands = [_to_agg_expr(e, gk_resolve) for e in in_node.expressions]
    if negate:
        cond: dict[str, Any] = {
            "$and": [{"$ne": [lhs, None]}]
            + [{"$and": [{"$ne": [c, None]}, {"$ne": [lhs, c]}]} for c in cands]
        }
    else:
        cond = {
            "$or": [
                {"$and": [{"$ne": [lhs, None]}, {"$ne": [c, None]}, {"$eq": [lhs, c]}]}
                for c in cands
            ]
        }
    return {"$expr": cond}


def _join_where_lowerable(stmt: exp.Select, resolve: Resolve) -> bool:
    """Whether a join's WHERE lowers to a ``$match`` — dry-run of the same
    ``_expr_to_filter`` the builders push (the join twin of the probe in
    ``where_needs_per_row``). Subquery-bearing WHEREs keep their existing
    routing (they need a live SubqueryCtx)."""
    where = stmt.args.get("where")
    if where is None or where.this.find(exp.Select) is not None:
        return True
    try:
        _expr_to_filter(where.this, resolve, None)
    except errors.SQLError:
        return False
    return True


def _join_resolver(amap: dict[str, tuple[str, TableDef]]) -> Resolve:
    def resolve(node: exp.Expression) -> tuple[str, str]:
        if not isinstance(node, exp.Column):
            raise errors.feature_not_supported(f"expected a column: {node.sql()}")
        alias = node.table or None
        name = node.name
        if alias:
            if alias not in amap:
                raise errors.SQLError("42P01", f'missing FROM-clause entry for table "{alias}"')
            role, tdef = amap[alias]
        else:
            cands = [(a, v) for a, v in amap.items() if v[1].column(name) is not None]
            if not cands:
                raise errors.undefined_column(name)
            if len(cands) > 1:
                raise errors.SQLError("42702", f'column reference "{name}" is ambiguous')
            alias, (role, tdef) = cands[0][0], cands[0][1]
        path = tdef.field_for(name)
        if role != "base":
            path = f"{alias}.{path}"
        return path, tdef.type_for(name)

    return resolve


def _alias_col(node: exp.Expression) -> tuple[str | None, str]:
    if not isinstance(node, exp.Column):
        raise errors.feature_not_supported(f"ON must compare columns: {node.sql()}")
    return (node.table or None), node.name


def _alias_field_path(amap: dict[str, tuple[str, TableDef]], alias: str, col: str) -> str:
    """Resolve ``alias.col`` to its pipeline field path (bare for the base)."""
    role, tdef = amap[alias]
    field = tdef.field_for(col)
    return field if role == "base" else f"{alias}.{field}"


def _and_conjuncts(node: exp.Expression) -> list[exp.Expression]:
    if isinstance(node, exp.Paren):
        return _and_conjuncts(node.this)
    if isinstance(node, exp.And):
        return _and_conjuncts(node.this) + _and_conjuncts(node.expression)
    return [node]


class _OnTranslator:
    """Translate a (possibly compound) JOIN ON into an aggregation ``$expr`` for
    a ``$lookup`` ``let``/``pipeline`` stage.

    References to the *new* (being-joined) table become ``$field`` paths inside
    the lookup sub-pipeline; references to already-known tables become ``$$vN``
    let variables bound to the outer document's field paths.

    When ``new_amap`` is supplied the "new/current-doc" side is a *multi-table
    composite* rather than a single table: any column of a table in ``new_amap``
    resolves to its composite field path (bare for the base, ``alias.field``
    otherwise), and only the ``amap`` tables (the outer/known side) become ``$$``
    let vars. This is what the trailing-composite anti-branch uses to translate the
    outer ON against a forward-built ``A⋈B⋈D`` (composite = current doc, C = let).
    """

    _OPS = {
        exp.EQ: "$eq",
        exp.NEQ: "$ne",
        exp.GT: "$gt",
        exp.GTE: "$gte",
        exp.LT: "$lt",
        exp.LTE: "$lte",
    }

    def __init__(
        self,
        new_alias: str,
        new_table: TableDef,
        amap: dict[str, tuple[str, TableDef]],
        new_amap: dict[str, tuple[str, TableDef]] | None = None,
    ):
        self.new_alias = new_alias
        self.new_table = new_table
        self.amap = amap
        self.new_amap = new_amap
        self.lets: dict[str, str] = {}  # outer field path -> let var name

    def _let_for(self, path: str) -> str:
        if path not in self.lets:
            self.lets[path] = f"v{len(self.lets)}"
        return self.lets[path]

    def _is_new(self, alias: str | None, name: str) -> bool:
        if self.new_amap is not None:
            if alias is not None:
                return alias in self.new_amap
            return any(t.column(name) is not None for _r, t in self.new_amap.values())
        if alias is not None:
            return alias == self.new_alias
        return self.new_table.column(name) is not None

    def _new_column(self, alias: str | None, name: str) -> str:
        # The current-doc side: a single new table (bare ``$field``) or, when
        # ``new_amap`` is set, a table inside the composite (its composite path).
        if self.new_amap is None:
            return f"${self.new_table.field_for(name)}"
        if alias is None:
            cands = [a for a, (_r, t) in self.new_amap.items() if t.column(name) is not None]
            if len(cands) != 1:
                raise errors.SQLError("42702", f'column reference "{name}" is ambiguous')
            alias = cands[0]
        return f"${_alias_field_path(self.new_amap, alias, name)}"

    def _column(self, node: exp.Column) -> str:
        alias, name = node.table or None, node.name
        if self._is_new(alias, name):
            return self._new_column(alias, name)
        # A known (outer) table reference -> a let-bound variable.
        if alias is None:
            cands = [a for a, (_r, t) in self.amap.items() if t.column(name) is not None]
            if len(cands) != 1:
                raise errors.SQLError("42702", f'column reference "{name}" is ambiguous')
            alias = cands[0]
        if alias not in self.amap:
            raise errors.SQLError("42P01", f'missing FROM-clause entry for table "{alias}"')
        return f"$${self._let_for(_alias_field_path(self.amap, alias, name))}"

    def expr(self, node: exp.Expression) -> Any:
        if isinstance(node, exp.Paren):
            return self.expr(node.this)
        if isinstance(node, exp.Column):
            return self._column(node)
        if isinstance(node, (exp.Literal, exp.Boolean, exp.Null, exp.Neg)):
            return _literal(node)
        if isinstance(node, exp.Cast):
            return _coerce_cast(self.expr(node.this), node.to)
        if isinstance(node, exp.And):
            return {"$and": [self.expr(node.this), self.expr(node.expression)]}
        if isinstance(node, exp.Or):
            return {"$or": [self.expr(node.this), self.expr(node.expression)]}
        if isinstance(node, exp.Not):
            return {"$not": [self.expr(node.this)]}
        if isinstance(node, exp.Is) and isinstance(node.expression, exp.Null):
            return {"$eq": [self.expr(node.this), None]}
        # ``col = ANY(ARRAY[...])`` → ``$in`` (Postgres IN, as in SQLAlchemy's
        # ``contype = ANY(ARRAY['p','u','x'])`` index-reflection join condition).
        if isinstance(node, exp.EQ) and isinstance(node.expression, exp.Any):
            elems = [self.expr(e) for e in _array_elements(node.expression.this)]
            return {"$in": [self.expr(node.this), elems]}
        # ``col IN ('p', 'u', 'x')`` — same $in lowering (psql's ``\\d`` index
        # listing joins pg_constraint with a literal IN list).
        if isinstance(node, exp.In) and node.args.get("expressions"):
            elems = [self.expr(e) for e in node.args["expressions"]]
            return {"$in": [self.expr(node.this), elems]}
        # ``(col).field`` — composite-value access on a record column (the
        # ``(i.keys).x`` term in pgjdbc's index-metadata join). A record cell
        # is a subdocument, so the access is just the dotted field path; only
        # the NEW (being-joined) side lowers this way — an outer composite
        # would need its full path let-bound.
        if (
            isinstance(node, exp.Dot)
            and isinstance(node.this, exp.Paren)
            and isinstance(node.this.this, exp.Column)
        ):
            base = self.expr(node.this.this)
            if isinstance(base, str) and base.startswith("$") and not base.startswith("$$"):
                return f"{base}.{node.expression.name}"
        for cls, op in self._OPS.items():
            if isinstance(node, cls):
                return {op: [self.expr(node.this), self.expr(node.expression)]}
        raise errors.feature_not_supported(f"unsupported JOIN ON term: {node.sql()}")


def _on_is_simple_equality(
    on: exp.Expression, join_alias: str, amap: dict[str, tuple[str, TableDef]]
) -> tuple[str, str, str] | None:
    """If ``on`` is a single equality relating the new table to a known one,
    return (new_col, known_alias, known_col); else None (→ pipeline form)."""
    conjuncts = _and_conjuncts(on)
    if len(conjuncts) != 1 or not isinstance(conjuncts[0], exp.EQ):
        return None
    eq = conjuncts[0]
    la, lc = _alias_col(eq.this)
    ra, rc = _alias_col(eq.expression)
    if la is None or ra is None:
        return None
    if join_alias == la and ra != join_alias and ra in amap:
        return lc, ra, rc
    if join_alias == ra and la != join_alias and la in amap:
        return rc, la, lc
    return None


def _lookup_stage(
    on: exp.Expression, join_alias: str, join_table: TableDef, amap: dict[str, tuple[str, TableDef]]
) -> dict[str, Any]:
    """Build the ``$lookup`` stage for one JOIN.

    A single equality uses the simple ``localField``/``foreignField`` form (so a
    user-table join keeps index acceleration). A compound ON (multi-key join or
    residual predicates on the joined table) uses the ``let``/``pipeline`` form.
    A *constant* ON folds three-valued: TRUE joins every foreign row (cartesian),
    FALSE/unknown joins none (INNER drops the row; LEFT null-pads at the
    ``$unwind``).
    """
    const = _constant_predicate_filter(on)
    if const is None:
        # An always-unknown ON (a NULL-literal comparison operand, under any
        # NOT/paren nesting) never joins either — same as a constant FALSE.
        core = on
        while isinstance(core, (exp.Not, exp.Paren)):
            core = core.this
        if _always_unknown_predicate(core):
            const = {"$nor": [{}]}
    if const is not None:
        pipeline = [] if const == {} else [{"$match": const}]
        return {"$lookup": {"from": join_table.collection, "pipeline": pipeline, "as": join_alias}}
    simple = _on_is_simple_equality(on, join_alias, amap)
    if simple is not None:
        new_col, known_alias, known_col = simple
        return {
            "$lookup": {
                "from": join_table.collection,
                "localField": _alias_field_path(amap, known_alias, known_col),
                "foreignField": join_table.field_for(new_col),
                "as": join_alias,
            }
        }
    tr = _OnTranslator(join_alias, join_table, amap)
    cond = tr.expr(on)
    return {
        "$lookup": {
            "from": join_table.collection,
            "let": {var: f"${path}" for path, var in tr.lets.items()},
            "pipeline": [{"$match": {"$expr": cond}}],
            "as": join_alias,
        }
    }


# Collector for *rich* LATERAL joins encountered while building a join pipeline.
# ``_plan_join_select`` sets a fresh list before building and reads it after; the
# forward-join builder appends to it. Left unset (None) on the GROUP BY / window
# paths, where a rich LATERAL stays ``0A000``.
_lateral_collect: contextvars.ContextVar[list[LateralJoin] | None] = contextvars.ContextVar(
    "_lateral_collect", default=None
)


def _lateral_inner_aliases(select: exp.Select) -> set[str | None]:
    """The FROM/JOIN aliases defined *inside* a LATERAL subquery — a column qualified
    by anything else is an outer correlation. (Mirrors ``_subquery_has_outer_ref``.)"""
    inner: set[str | None] = set()
    from_node = select.find(exp.From)
    if from_node is not None:
        src = from_node.this
        inner.add(src.alias or getattr(src, "name", None))
    for jn in select.args.get("joins") or []:
        src = jn.this
        inner.add(src.alias or getattr(src, "name", None))
    return inner


def _lateral_is_rich(sub: exp.Select) -> bool:
    """Whether a LATERAL subquery is too rich for the correlated-``$lookup`` lowering
    — it has its own JOIN / GROUP BY / HAVING / DISTINCT / aggregate, or a projection
    the pipeline can't build. Such a subquery runs nested-loop on the evaluated path."""
    has_agg = any(
        _aggregate_of(e) is not None
        or _array_agg_arg(e) is not None
        or _jsonb_object_agg_args(e) is not None
        or _range_agg_arg(e) is not None
        or _string_agg_arg(e) is not None
        for e in sub.expressions
    )
    return bool(
        sub.args.get("joins")
        or sub.args.get("group")
        or sub.args.get("having")
        or sub.args.get("distinct")
        or has_agg
        or _stmt_needs_evaluation(sub)
    )


def _lateral_literal(value: Any) -> exp.Expression:
    """Wrap an outer row's Python value as a SQL literal node to splice into a
    correlated LATERAL subquery in place of the outer column reference."""
    import datetime as _dt
    from decimal import Decimal

    if value is None:
        return exp.Null()
    if isinstance(value, bool):
        return exp.Boolean(this=value)
    if isinstance(value, int):
        return exp.Literal.number(str(value))
    if isinstance(value, (float, Decimal)):
        return exp.Literal.number(repr(value) if isinstance(value, float) else str(value))
    if isinstance(value, _dt.datetime):
        return exp.cast(exp.Literal.string(value.isoformat(sep=" ")), "timestamp")
    if isinstance(value, _dt.date):
        return exp.cast(exp.Literal.string(value.isoformat()), "date")
    return exp.Literal.string(str(value))


def _substitute_outer_columns(
    select: exp.Select, inner_aliases: set[str | None], value_of: Any
) -> exp.Select:
    """A copy of ``select`` with every *outer* column reference (qualified by a table
    not in ``inner_aliases``) replaced by ``_lateral_literal(value_of(col))`` — turning
    the correlated subquery into a plain, non-correlated one for this outer row."""
    out = select.copy()
    for col in list(out.find_all(exp.Column)):
        if col.table and col.table not in inner_aliases:
            col.replace(_lateral_literal(value_of(col)))
    return out


def _plan_rich_lateral(
    lateral: exp.Lateral, side: str, db: str, catalog: Any, storage: Any
) -> LateralJoin:
    """Plan a rich ``JOIN LATERAL (subquery) alias`` into a ``LateralJoin``. The
    subquery's output shape (column names + tags) is obtained once by planning it with
    the outer correlations replaced by ``NULL`` (a non-correlated query); the executor
    re-substitutes real outer values per row at run time."""
    alias = lateral.alias
    if not alias:
        raise errors.feature_not_supported("a LATERAL subquery requires an alias")
    sub = lateral.this
    if isinstance(sub, exp.Subquery):
        sub = sub.this
    if not isinstance(sub, exp.Select):
        raise errors.feature_not_supported(f"unsupported LATERAL source: {lateral.sql()}")
    inner_aliases = _lateral_inner_aliases(sub)
    shape = _substitute_outer_columns(sub, inner_aliases, lambda _col: None)
    shape_plan = plan_pipeline_select(shape, db, catalog, storage)
    tdef = TableDef(
        name=alias,
        collection=alias,
        columns=[Column(n, t, n, pk=False, nullable=True) for n, t in shape_plan.out_columns],
    )
    return LateralJoin(
        alias=alias,
        tdef=tdef,
        select=sub,
        side=str(side or "").upper(),
        inner_aliases=inner_aliases,
    )


def _lateral_stage(
    lateral: exp.Lateral,
    side: str,
    amap: dict[str, tuple[str, TableDef]],
    db: str,
    catalog: Any,
    storage: Any,
    derived: list[DerivedTable],
) -> tuple[str, TableDef, list[dict[str, Any]]]:
    """Lower a ``LATERAL (SELECT … FROM inner [WHERE …] [ORDER BY …] [LIMIT n])``
    to a correlated ``$lookup`` + ``$unwind``.

    The subquery may reference columns from the preceding FROM items (that's what
    makes it lateral); those become ``let``-bound ``$$vars`` in the lookup's
    sub-pipeline via ``_OnTranslator`` (the same inner-``$field`` / outer-``$$var``
    split a compound JOIN ON uses). Scope is a single-table subquery with an
    optional WHERE / ORDER BY / LIMIT — a join / GROUP BY / scalar-fn subquery is
    rejected rather than mis-lowered."""
    alias = lateral.alias
    if not alias:
        raise errors.feature_not_supported("a LATERAL subquery requires an alias")
    sub = lateral.this
    if isinstance(sub, exp.Subquery):
        sub = sub.this
    if not isinstance(sub, exp.Select):
        raise errors.feature_not_supported(f"unsupported LATERAL source: {lateral.sql()}")
    has_agg = any(
        _aggregate_of(e) is not None
        or _array_agg_arg(e) is not None
        or _jsonb_object_agg_args(e) is not None
        or _range_agg_arg(e) is not None
        or _string_agg_arg(e) is not None
        for e in sub.expressions
    )
    if (
        sub.args.get("joins")
        or sub.args.get("group")
        or sub.args.get("having")
        or sub.args.get("distinct")
        or has_agg
        or _stmt_needs_evaluation(sub)
    ):
        raise errors.feature_not_supported(
            "only a single-table LATERAL subquery (projection + WHERE + ORDER BY / LIMIT) "
            "is supported"
        )
    from_node = sub.find(exp.From)
    if from_node is None:
        raise errors.feature_not_supported("a LATERAL subquery requires a FROM clause")
    inner_alias, inner = _resolve_source(from_node.this, db, catalog, storage, derived)
    inner_resolve = table_resolver(inner)

    sub_pipeline: list[dict[str, Any]] = []
    lets: dict[str, str] = {}
    where = sub.args.get("where")
    if where is not None:
        tr = _OnTranslator(inner_alias, inner, amap)
        cond = tr.expr(where.this)
        lets = tr.lets
        sub_pipeline.append({"$match": {"$expr": cond}})
    order = sub.args.get("order")
    if order is not None:
        sort_spec: dict[str, int] = {}
        for o in order.expressions:
            path, _ = inner_resolve(o.this)
            sort_spec[path] = -1 if o.args.get("desc") else 1
        sub_pipeline.append({"$sort": sort_spec})
    limit, skip = _limit_skip(sub)
    if skip:
        sub_pipeline.append({"$skip": skip})
    if limit:
        sub_pipeline.append({"$limit": limit})

    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    names = _NameAllocator()
    for e in sub.expressions:
        col_alias = e.alias if isinstance(e, exp.Alias) else None
        target = e.this if isinstance(e, exp.Alias) else e
        if isinstance(target, exp.Star):
            for c in inner.columns:
                nm = names.fresh(c.name)
                project[nm] = f"${c.field}"
                out_columns.append((nm, c.type_tag))
            continue
        path, tag = _field(target, inner_resolve)
        nm = names.fresh(col_alias or _column_name(target))
        project[nm] = f"${path}"
        out_columns.append((nm, tag))
    sub_pipeline.append({"$project": project})

    tdef = TableDef(
        name=alias,
        collection=alias,
        columns=[Column(n, t, n, pk=False, nullable=True) for n, t in out_columns],
    )
    stages: list[dict[str, Any]] = [
        {
            "$lookup": {
                "from": inner.collection,
                "let": {var: f"${path}" for path, var in lets.items()},
                "pipeline": sub_pipeline,
                "as": alias,
            }
        },
        {"$unwind": {"path": f"${alias}", "preserveNullAndEmptyArrays": side == "LEFT"}},
    ]
    return alias, tdef, stages


def _srf_body(stmt: exp.Expression) -> Any:
    """The ``SrfSource`` when ``stmt`` is a SELECT whose FROM is a base-less
    set-returning function, else None."""
    from secantus.sql import srf

    if not isinstance(stmt, exp.Select):
        return None
    return srf.from_source(stmt)


def _is_srf_table_source(node: exp.Expression) -> bool:
    """True for a table function sitting directly in FROM / JOIN position.

    sqlglot models it as a ``Table`` whose ``this`` is the function node rather
    than an identifier, which is why such a source used to fall through to the
    catalog lookup and report ``relation "" does not exist``."""
    from secantus.sql import srf

    if isinstance(node, exp.Unnest):
        return True
    return isinstance(node, exp.Table) and srf._is_srf_node(node.this)


def _srf_out_columns(
    stmt: exp.Select, db: str, catalog: Any, storage: Any
) -> list[tuple[str, str]]:
    """Output (name, tag) shape of a base-less SRF SELECT.

    Built by asking ``srf`` to materialize the source, which is how the engine
    derives the same shape at run time — so the planner's column types agree
    with the rows the executor will produce. The evaluation is best-effort:
    an SRF whose arguments need session state (``generate_series(1,
    array_upper(current_schemas(false), 1))``) cannot be resolved from here,
    and those columns fall back to the untyped tag rather than a guess.
    """
    from secantus.sql import scalar, srf

    source = srf.from_source(stmt)
    names: list[str] = []
    if source is not None:
        try:
            _rows, tdef = srf.build(source, scalar.ScalarContext(storage, catalog, db, None))
            by_name = {c.name: c.type_tag for c in tdef.columns}
            names = list(by_name)
        except Exception:
            by_name = {}
            names = list(source.column_aliases) or [source.table_alias or "?column?"]
    else:  # pragma: no cover - guarded by the caller
        by_name = {}

    projected = _srf_projected_names(stmt, names)
    return [(n, by_name.get(n, "any")) for n in projected]


def _srf_projected_names(stmt: exp.Select, source_names: list[str]) -> list[str]:
    """The SELECT's output names over an SRF source: the source's own columns
    for ``SELECT *``, else the projection's aliases / column names."""
    out: list[str] = []
    for e in stmt.expressions:
        target = e.this if isinstance(e, exp.Alias) else e
        if isinstance(target, exp.Star):
            out.extend(source_names)
            continue
        alias = e.alias if isinstance(e, exp.Alias) else None
        out.append(alias or (target.name if isinstance(target, exp.Column) else "?column?"))
    return out or source_names


def _fromless_out_columns(stmt: exp.Select) -> list[tuple[str, str]]:
    """Output (name, tag) shape of a FROM-less SELECT — used for derived tables
    whose inner query has no row source (``FROM (SELECT 1 AS x) AS a``)."""
    cols: list[tuple[str, str]] = []
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        target = e.this if isinstance(e, exp.Alias) else e
        name = alias or (
            target.name
            if isinstance(target, exp.Column)
            else _cast_output_name(target) or "?column?"
        )
        try:
            tag = _infer_scalar_tag(target, _const_scope)
        except errors.SQLError:
            tag = "text"
        cols.append((name, tag))
    return cols


def _resolve_source(
    node: exp.Expression, db: str, catalog: Any, storage: Any, derived: list[DerivedTable]
) -> tuple[str, TableDef]:
    """Resolve a FROM / JOIN source to (alias, TableDef).

    A plain table resolves through the catalog / virtual / reflection lookup. A
    ``(SELECT ...) AS alias`` derived table is planned as a sub-plan and recorded
    in ``derived`` (the executor materializes it into an ephemeral collection
    named by the alias before running the main pipeline)."""
    if isinstance(node, exp.Lateral):
        raise errors.feature_not_supported("LATERAL cannot be the first FROM item")
    if isinstance(node, exp.Table) and isinstance(node.this, exp.Values):
        # Extra grouping parens make sqlglot parse ``((VALUES …) AS t (…))``
        # as a Table WRAPPING the Values (CrystalReports' {oj (((…)))} shape)
        # — move the alias onto the Values node and take the normal branch.
        inner = node.this
        inner.set("alias", node.args.get("alias"))
        node = inner
    if isinstance(node, exp.Values):
        # ``FROM (VALUES (…), …) AS alias(c1, c2)`` — a constant derived table.
        # Column names from the alias list, tags inferred from the first row.
        alias_node = node.args.get("alias")
        alias = alias_node.name if alias_node is not None else ""
        if not alias:
            raise errors.feature_not_supported("VALUES in FROM requires an alias")
        first = node.expressions[0].expressions if node.expressions else []
        acols = [c.name for c in (alias_node.args.get("columns") or [])]
        if len(acols) < len(first):
            # PG allows an alias with fewer columns — the rest keep the
            # default column1..columnN names.
            acols = acols + [f"column{i + 1}" for i in range(len(acols), len(first))]
        tags = []
        for cell in first:
            try:
                tags.append(_infer_value_tag(_literal(cell)))
            except errors.SQLError:
                tags.append("text")
        cols = list(zip(acols, tags, strict=False))
        tdef = TableDef(
            name=alias,
            collection=alias,
            columns=[Column(n, t, n, pk=False, nullable=True) for n, t in cols],
        )
        derived.append(DerivedTable(name=alias, plan=RawDerived(node, acols), columns=cols))
        return alias, tdef
    if isinstance(node, exp.Subquery):
        alias = node.alias
        if not alias:
            raise errors.feature_not_supported("a derived table requires an alias")
        sub = node.this
        if isinstance(sub, exp.SetOperation):
            # ``FROM (SELECT … UNION SELECT …) AS alias`` — column shape from
            # planning the first arm; the executor materializes the whole set
            # operation through the engine (dedup / ALL semantics included).
            arm: exp.Expression = sub
            while isinstance(arm, exp.SetOperation):
                arm = arm.left
            if isinstance(arm, exp.Subquery):
                arm = arm.this
            if not isinstance(arm, exp.Select):
                raise errors.feature_not_supported(f"unsupported derived table: {node.sql()}")
            if arm.args.get("from_") is None:
                cols = _fromless_out_columns(arm)
            else:
                cols = plan_pipeline_select(arm, db, catalog, storage).out_columns
            tdef = TableDef(
                name=alias,
                collection=alias,
                columns=[Column(n, t, n, pk=False, nullable=True) for n, t in cols],
            )
            derived.append(DerivedTable(name=alias, plan=sub, columns=cols))
            return alias, tdef
        if not isinstance(sub, exp.Select):
            raise errors.feature_not_supported(f"unsupported derived table: {node.sql()}")
        if _srf_body(sub) is not None:
            # ``FROM (SELECT … FROM generate_series(…) AS s(r)) AS d`` — the
            # engine's base-less SRF path already materializes this shape, so
            # hand it over as a raw sub-plan rather than pipelining it (there
            # is no collection to pipeline over).
            cols = _srf_out_columns(sub, db, catalog, storage)
            tdef = TableDef(
                name=alias,
                collection=alias,
                columns=[Column(n, t, n, pk=False, nullable=True) for n, t in cols],
            )
            derived.append(DerivedTable(name=alias, plan=RawDerived(sub), columns=cols))
            return alias, tdef
        if sub.args.get("from_") is None:
            # ``FROM (SELECT 1 AS x) AS a`` — no row source to pipeline; the
            # executor runs the constant SELECT through the engine.
            cols = _fromless_out_columns(sub)
            tdef = TableDef(
                name=alias,
                collection=alias,
                columns=[Column(n, t, n, pk=False, nullable=True) for n, t in cols],
            )
            derived.append(DerivedTable(name=alias, plan=RawDerived(sub), columns=cols))
            return alias, tdef
        sub_plan = plan_pipeline_select(sub, db, catalog, storage)
        cols = sub_plan.out_columns
        tdef = TableDef(
            name=alias,
            collection=alias,
            columns=[Column(n, t, n, pk=False, nullable=True) for n, t in cols],
        )
        derived.append(DerivedTable(name=alias, plan=sub_plan, columns=cols))
        return alias, tdef
    if _is_srf_table_source(node):
        # A table function in JOIN position (``JOIN generate_series(…) AS s(r)``).
        # Wrapping it in a SELECT reduces it to the base-less SRF shape the
        # engine already materializes, so it goes through the same raw sub-plan
        # path as a derived table over one.
        alias_node = node.args.get("alias")
        alias = alias_node.name if alias_node is not None else ""
        if not alias:
            raise errors.feature_not_supported("a set-returning function in FROM requires an alias")
        sub = exp.Select(expressions=[exp.Star()], from_=exp.From(this=node.copy()))
        cols = _srf_out_columns(sub, db, catalog, storage)
        tdef = TableDef(
            name=alias,
            collection=alias,
            columns=[Column(n, t, n, pk=False, nullable=True) for n, t in cols],
        )
        derived.append(DerivedTable(name=alias, plan=RawDerived(sub), columns=cols))
        return alias, tdef
    if isinstance(node, exp.Table) and not isinstance(node.this, exp.Identifier):
        raise errors.feature_not_supported(f"unsupported FROM item: {node.sql()}")
    tdef = _lookup_table_def(catalog, db, node, storage)
    if tdef is None:
        raise errors.undefined_table(node.name)
    return (node.alias or node.name), tdef


def _unnest_join_stage(
    unnest: exp.Unnest, side: str, amap: dict[str, tuple[str, TableDef]]
) -> tuple[list[dict[str, Any]], str, TableDef]:
    """A ``FROM … , unnest(<array>) AS alias`` table-function source. Returns the
    ``$addFields`` + ``$unwind`` stages, the alias, and a synthetic one-column
    ``TableDef`` (the unwound element, exposed at the top level).

    The array source may be a column of an already-joined table (``t.tags``) or an
    ``ARRAY[…]`` literal. ``WITH ORDINALITY`` and multi-array unnest aren't
    supported."""
    alias_node = unnest.args.get("alias")
    if alias_node is None or not alias_node.name:
        raise errors.feature_not_supported("unnest() in FROM requires an alias")
    alias = alias_node.name
    if unnest.args.get("offset"):
        raise errors.feature_not_supported("unnest … WITH ORDINALITY is not supported")
    exprs = unnest.expressions
    if len(exprs) != 1:
        raise errors.feature_not_supported("unnest() of multiple arrays is not supported")
    # An ``AS x(v)`` column-alias names the element column; else it's the alias.
    col_idents = alias_node.args.get("columns") or []
    col_name = col_idents[0].name if col_idents else alias

    arr = exprs[0]
    while isinstance(arr, (exp.Paren, exp.Cast)):  # unwrap ``ARRAY[…]::text[]`` etc.
        arr = arr.this
    if _is_field_node(arr):
        path, tag = _field(arr, _join_resolver(amap))
        elem_tag = typemap.array_element_tag(tag) if typemap.is_array_tag(tag) else "any"
        arr_value: Any = f"${path}"
    elif isinstance(arr, exp.Array):
        items = [_literal(e) for e in arr.expressions]
        elem_tag = _infer_value_tag(items[0]) if items else "any"
        arr_value = items
    else:
        raise errors.feature_not_supported(f"unsupported unnest() source: {arr.sql()}")

    stages = [
        {"$addFields": {col_name: arr_value}},
        {"$unwind": {"path": f"${col_name}", "preserveNullAndEmptyArrays": side == "LEFT"}},
    ]
    tdef = TableDef(
        name=alias,
        collection=alias,
        columns=[Column(col_name, elem_tag, col_name, pk=False, nullable=True)],
    )
    return stages, alias, tdef


def _is_jsonb_each_join(jt: exp.Expression) -> bool:
    """Whether a join source is ``jsonb_each(...)`` / ``json_each(...)`` (the
    json-valued record SRFs; the ``_text`` variants aren't supported in the lateral
    form because the value would need per-row text rendering in the pipeline)."""
    return (
        isinstance(jt, exp.Table)
        and isinstance(jt.this, exp.Anonymous)
        and str(jt.this.this).rsplit(".", 1)[-1].lower() in ("jsonb_each", "json_each")
    )


def _jsonb_each_join_stage(
    table_node: exp.Table, side: str, amap: dict[str, tuple[str, TableDef]]
) -> tuple[list[dict[str, Any]], str, TableDef]:
    """A ``FROM t, jsonb_each(t.doc) AS e(k, v)`` source: expand each outer row's
    object into ``(key, value)`` pairs via ``$objectToArray`` + ``$unwind``. Returns
    the stages, the alias, and a synthetic two-column ``TableDef`` (key ``text`` /
    value ``json``, resolved from the unwound ``{k, v}`` subdocument)."""
    fn = table_node.this
    alias_node = table_node.args.get("alias")
    # ``FROM t, jsonb_each(doc)`` needs no explicit alias — the columns default to
    # ``key`` / ``value`` (Postgres); ``AS e(k, v)`` renames them.
    if alias_node is not None and alias_node.name:
        alias = alias_node.name
        col_idents = alias_node.args.get("columns") or []
    else:
        alias = "jsonb_each"
        col_idents = []
    key_col = col_idents[0].name if col_idents else "key"
    val_col = col_idents[1].name if len(col_idents) > 1 else "value"

    arg = fn.expressions[0] if fn.expressions else None
    if arg is not None and _is_field_node(arg):
        path, _ = _field(arg, _join_resolver(amap))
        src_expr: Any = f"${path}"
    elif arg is not None and _is_literalish(arg):
        src_expr = {"$literal": _json_value(arg)}
    else:
        raise errors.feature_not_supported("unsupported jsonb_each() source in FROM")

    kv = f"__{alias}_kv"
    # ``$objectToArray`` of a missing / non-object value yields null; ``$unwind``
    # then drops the row (or keeps it as null for a LEFT / comma join).
    stages = [
        {"$addFields": {kv: {"$objectToArray": src_expr}}},
        {"$unwind": {"path": f"${kv}", "preserveNullAndEmptyArrays": side == "LEFT"}},
    ]
    tdef = TableDef(
        name=alias,
        collection=alias,
        columns=[
            Column(key_col, "text", f"{kv}.k", pk=False, nullable=True),
            Column(val_col, "json", f"{kv}.v", pk=False, nullable=True),
        ],
    )
    return stages, alias, tdef


def _append_forward_join(
    jn: exp.Expression,
    amap: dict[str, tuple[str, TableDef]],
    pipeline: list[dict[str, Any]],
    db: str,
    catalog: Any,
    storage: Any,
    derived: list[DerivedTable],
) -> None:
    """Append one left-driven join's stages to ``pipeline`` and register its alias
    in ``amap``. Handles INNER / LEFT / CROSS table joins plus the LATERAL / unnest /
    jsonb_each table-function sources — every case whose driving side is whatever the
    pipeline already produced (so the ``$lookup``'s ``from`` is always a real
    collection)."""
    jt = jn.this
    side = str(jn.args.get("side") or "").upper()
    on = jn.args.get("on")
    if isinstance(jt, exp.Lateral):
        # A LATERAL subquery correlates *inside* itself (its WHERE references outer
        # columns), so the join ON is only ever TRUE (or absent for the comma / CROSS
        # form). A real ON predicate here isn't supported.
        if on is not None and not (isinstance(on, exp.Boolean) and bool(on.this)):
            raise errors.feature_not_supported(
                "LATERAL join ON must be TRUE — correlate inside the subquery's WHERE"
            )
        lat_sub = jt.this
        if isinstance(lat_sub, exp.Subquery):
            lat_sub = lat_sub.this
        if isinstance(lat_sub, exp.Select) and _lateral_is_rich(lat_sub):
            # A subquery with its own join/group/aggregate/DISTINCT — collected for the
            # evaluated nested-loop path. Only available in a plain SELECT (the
            # collector is set); a rich LATERAL under an outer GROUP BY / window stays
            # 0A000.
            collector = _lateral_collect.get()
            if collector is None:
                raise errors.feature_not_supported(
                    "a LATERAL subquery with a join / GROUP BY / aggregate is only supported "
                    "in a plain SELECT (not combined with an outer GROUP BY or window)"
                )
            lat = _plan_rich_lateral(jt, side, db, catalog, storage)
            collector.append(lat)
            amap[lat.alias] = ("join", lat.tdef)
            return
        lat_alias, lat_table, stages = _lateral_stage(jt, side, amap, db, catalog, storage, derived)
        pipeline.extend(stages)
        amap[lat_alias] = ("join", lat_table)
        return
    if isinstance(jt, exp.Unnest):
        # ``FROM t, unnest(t.tags) AS tag`` — a table-function source. Expose the
        # array under the alias column, then $unwind it (one row per element, paired
        # with the outer row). LEFT/comma keeps empty arrays.
        stages, un_alias, un_tdef = _unnest_join_stage(jt, side, amap)
        pipeline.extend(stages)
        amap[un_alias] = ("base", un_tdef)  # element field lives at top level
        return
    if _is_jsonb_each_join(jt):
        # ``FROM t, jsonb_each(t.doc) AS e(k, v)`` — expand each row's object into
        # (key, value) pairs.
        stages, je_alias, je_tdef = _jsonb_each_join_stage(jt, side, amap)
        pipeline.extend(stages)
        amap[je_alias] = ("base", je_tdef)  # k / v resolve via dotted fields
        return
    join_alias, join_table = _resolve_source(jt, db, catalog, storage, derived)
    if on is None:
        # No ON: a CROSS JOIN or an implicit comma-join — the cartesian product (an
        # empty `$lookup` pipeline returns every foreign doc, then `$unwind` pairs
        # each with the outer row). An outer join without ON is not valid SQL.
        if side in ("LEFT", "RIGHT", "FULL"):
            raise errors.syntax_error(f"{side} JOIN requires an ON clause")
        pipeline.append(
            {"$lookup": {"from": join_table.collection, "pipeline": [], "as": join_alias}}
        )
        pipeline.append(
            {"$unwind": {"path": f"${join_alias}", "preserveNullAndEmptyArrays": False}}
        )
        amap[join_alias] = ("join", join_table)
        return
    pipeline.append(_lookup_stage(on, join_alias, join_table, amap))
    pipeline.append(
        {"$unwind": {"path": f"${join_alias}", "preserveNullAndEmptyArrays": side == "LEFT"}}
    )
    amap[join_alias] = ("join", join_table)


def _key_comma_joins_from_where(
    stmt: exp.Select,
    base_alias: str,
    base: TableDef | None = None,
    db: str | None = None,
    catalog: Any = None,
    storage: Any = None,
) -> None:
    """Push simple cross-table equalities from WHERE onto comma-join ON clauses.

    A pure comma-join (``FROM a, b, …`` — no ON, no outer side) compiles to an
    UNKEYED ``$lookup`` that returns the entire foreign collection per outer row
    (a cartesian product), with the join predicates applied only by the terminal
    WHERE ``$match``. Over several catalog tables that intermediate is
    astronomical (pgjdbc's getImportedKeys for multi-column FKs reached 183GB).

    For each comma-join, move the WHERE equalities of the form ``joined.col =
    available.col`` (both plain columns, the other side an ALREADY-available
    alias) onto that join's ON, so the ``$lookup`` is keyed. Comma-joins are
    INNER, so relocating an equality from the terminal ``$match`` onto the
    inner-join ON is result-preserving. Residual predicates (single-table
    filters, array-subscript / expression joins) stay in WHERE. Idempotent — a
    re-plan finds the joins already carry an ON and does nothing."""
    joins = stmt.args.get("joins") or []
    where = stmt.args.get("where")
    if where is None or not any(j.args.get("on") is None and not j.args.get("side") for j in joins):
        return
    conjuncts = _and_conjuncts(where.this)
    available = {base_alias}
    consumed: set[int] = set()
    # Which alias owns each column NAME. The sqllogictest corpus writes its join
    # equalities unqualified (``WHERE a3=b9``, not ``t3.a3=t9.b9``), so without
    # this every comma join stayed unkeyed and the plan degenerated into the
    # cartesian product this function exists to prevent. Only UNAMBIGUOUS names
    # are usable: a name declared by two joined tables can't be attributed, and
    # guessing would key the join on the wrong table.
    owner_of: dict[str, str | None] = {}
    if catalog is not None and db is not None:
        defs: list[tuple[str, TableDef]] = []
        if base is not None:
            defs.append((base_alias, base))
        for jn in joins:
            src = jn.this
            a = _join_source_alias(src)
            if a is None or not isinstance(src, exp.Table):
                continue
            tdef = _lookup_table_def(catalog, db, src, storage)
            if tdef is not None:
                defs.append((a, tdef))
        for alias, tdef in defs:
            for col in tdef.columns:
                # None marks "ambiguous" — seen under more than one alias.
                owner_of[col.name] = None if col.name in owner_of else alias

    def alias_of(node: exp.Column) -> str | None:
        return node.table or owner_of.get(node.name)

    for jn in joins:
        alias = _join_source_alias(jn.this)
        if jn.args.get("on") is not None or jn.args.get("side") or alias is None:
            if alias is not None:
                available.add(alias)
            continue
        keys: list[exp.Expression] = []
        for i, c in enumerate(conjuncts):
            if (
                i in consumed
                or not isinstance(c, exp.EQ)
                or not isinstance(c.this, exp.Column)
                or not isinstance(c.expression, exp.Column)
            ):
                continue
            la, ra = alias_of(c.this), alias_of(c.expression)
            if (la == alias and ra in available and ra != alias) or (
                ra == alias and la in available and la != alias
            ):
                keys.append(c)
                consumed.add(i)
        if keys:
            # QUALIFY the relocated columns. The equality may have arrived
            # unqualified (`a3=b9`), and an ON clause has to say which side is
            # local and which is foreign for the `$lookup` to be keyed at all.
            def qualified(node: exp.Column) -> exp.Column:
                owner = alias_of(node)
                return exp.column(node.name, table=owner) if owner else node.copy()

            on: exp.Expression | None = None
            for k in keys:
                eq = exp.EQ(this=qualified(k.this), expression=qualified(k.expression))
                on = eq if on is None else exp.And(this=on, expression=eq)
            jn.set("on", on)
        available.add(alias)
    if not consumed:
        return
    remaining = [c for i, c in enumerate(conjuncts) if i not in consumed]
    if remaining:
        new_where = remaining[0].copy()
        for c in remaining[1:]:
            new_where = exp.And(this=new_where, expression=c.copy())
        stmt.set("where", exp.Where(this=new_where))
    else:
        stmt.set("where", None)


def _build_join_pipeline(
    stmt: exp.Select, db: str, catalog: Any, storage: Any
) -> tuple[
    TableDef, dict[str, tuple[str, TableDef]], Resolve, list[dict[str, Any]], list[DerivedTable]
]:
    """Build the $lookup/$unwind (+ WHERE $match) prefix shared by the join
    builders. Returns (base, amap, resolve, pipeline, derived)."""
    derived: list[DerivedTable] = []
    fr = stmt.find(exp.From).this
    base_alias, base = _resolve_source(fr, db, catalog, storage, derived)
    joins = stmt.args["joins"]

    # ``$lookup`` is inherently left-driven (for each base doc, fetch matching
    # foreign docs), so RIGHT / FULL OUTER need the base swapped and (for FULL) an
    # anti-join union. A single two-table outer join composes directly; a *pure*
    # RIGHT chain of 3+ tables reverses into a LEFT chain driven from the last table
    # (approach a). Mixed LEFT/RIGHT chains and any multi-table FULL stay 0A000.
    sides = [str(jn.args.get("side") or "").upper() for jn in joins]
    if any(s in ("RIGHT", "FULL") for s in sides):
        if len(joins) == 1:
            return _build_outer_join_pipeline(
                stmt, base_alias, base, joins[0], db, catalog, storage, derived
            )
        if set(sides) == {"RIGHT"}:
            return _build_right_chain_pipeline(
                stmt, base_alias, base, joins, db, catalog, storage, derived
            )
        if sides[0] in ("RIGHT", "FULL") and all(s in ("", "LEFT") for s in sides[1:]):
            # A *leading* RIGHT/FULL join, then a tail of only INNER/LEFT joins. The
            # leading outer join builds the composite (A⋈B) as the driving stream;
            # each tail join $lookups its (real-collection) table over that stream, so
            # the composite is never a $lookup source and the plan stays sound.
            return _build_leading_outer_join_pipeline(
                stmt, base_alias, base, joins, db, catalog, storage, derived
            )
        if all(s in ("", "LEFT") for s in sides[:-1]) and sides[-1] in ("RIGHT", "FULL"):
            # A *trailing* RIGHT/FULL over an N-table INNER/LEFT composite —
            # ``A [INNER|LEFT] JOIN B ON o1 [[INNER|LEFT] JOIN … ] RIGHT|FULL JOIN C ON
            # o2`` (``joins[:-1]`` build the composite, ``joins[-1]`` is the trailing
            # outer join). One builder covers the 2-, 3-, and 4+-table composites via
            # the main ∪ anti decomposition: the main branch builds the composite
            # forward (natural root A) then joins C INNER (RIGHT) / LEFT (FULL); the
            # anti branch `$unionWith`s the C rows whose forward composite is empty.
            # Sound for INNER *and* LEFT composites (never re-rooted), with ``o2`` free
            # to reference C plus *any* subset of composite tables. Re-raises ``0A000``
            # for shapes it can't prove sound (unqualified/non-adjacent ON,
            # non-plain-table source). ``len == 1`` is the two-table base case handled
            # above by ``_build_outer_join_pipeline``.
            return _build_trailing_composite_pipeline(
                stmt, base_alias, base, joins, db, catalog, storage, derived, kind=sides[-1]
            )
        raise errors.feature_not_supported(
            "RIGHT / FULL OUTER JOIN in a 3+ table chain is only supported for an all-RIGHT "
            "chain, a leading RIGHT/FULL join followed by INNER/LEFT joins, or a trailing "
            "RIGHT/FULL join over an INNER/LEFT composite"
        )

    amap: dict[str, tuple[str, TableDef]] = {base_alias: ("base", base)}
    pipeline: list[dict[str, Any]] = []

    # Key comma-joins from WHERE before building stages: an unkeyed comma-join
    # $lookup returns the WHOLE foreign collection (a cartesian product), so a
    # multi-table comma-join over the catalogs explodes (getImportedKeys 183GB).
    _key_comma_joins_from_where(stmt, base_alias, base, db, catalog, storage)

    # Each JOIN compiles to a $lookup + $unwind. The lookup's localField may point
    # into an already-joined alias (a chain like a⋈b⋈c where c joins on b), which
    # Mongo's dotted localField handles since b was unwound into the doc.
    for jn in joins:
        _append_forward_join(jn, amap, pipeline, db, catalog, storage, derived)

    resolve = _join_resolver(amap)
    where = stmt.args.get("where")
    # A correlated / EXISTS WHERE is left for per-row evaluation (see
    # ``_build_evaluated_join``); only a pushdown-able WHERE becomes a ``$match``.
    if where is not None and not where_needs_per_row(stmt) and _join_where_lowerable(stmt, resolve):
        filt = _expr_to_filter(where.this, resolve, _pipeline_subctx.get())
        residual = _push_single_table_predicates(filt, pipeline, amap, base_alias)
        if residual:
            pipeline.append({"$match": residual})
    return base, amap, resolve, pipeline, derived


def _filter_field_keys(value: Any) -> list[str]:
    """Every field key a (possibly nested) filter fragment touches.

    Operator keys are structural, not fields, so they are walked through rather
    than collected — what comes back is the set of document paths the fragment
    constrains.
    """
    out: list[str] = []
    if isinstance(value, dict):
        for k, v in value.items():
            if k.startswith("$"):
                out.extend(_filter_field_keys(v))
            else:
                out.append(k)
    elif isinstance(value, list):
        for item in value:
            out.extend(_filter_field_keys(item))
    return out


def _sole_filter_owner(
    value: Any, amap: dict[str, tuple[str, TableDef]], base_alias: str | None
) -> str | None:
    """The one alias every field in ``value`` belongs to, or None if it spans
    tables (or touches nothing attributable)."""
    owners: set[str] = set()
    keys = _filter_field_keys(value)
    if not keys:
        return None
    for key in keys:
        prefix, _, rest = key.partition(".")
        if rest and prefix in amap:
            owners.add(prefix)
        elif not rest and prefix not in amap:
            owners.add(base_alias or "")
        else:
            return None
        if len(owners) > 1:
            return None
    return next(iter(owners)) if owners else None


def _strip_alias_prefix(value: Any, alias: str) -> Any:
    """``value`` with a leading ``alias.`` removed from every field key — the
    lookup sub-pipeline runs against that collection, so its own paths apply."""
    if isinstance(value, dict):
        out: dict[str, Any] = {}
        for k, v in value.items():
            if k.startswith("$"):
                out[k] = _strip_alias_prefix(v, alias)
            else:
                out[k[len(alias) + 1 :] if k.startswith(f"{alias}.") else k] = v
        return out
    if isinstance(value, list):
        return [_strip_alias_prefix(item, alias) for item in value]
    return value


def _flatten_and(filt: dict[str, Any]) -> list[dict[str, Any]]:
    """``filt`` as a list of single-key conjuncts, flattening nested ``$and``.

    A filter carrying an OR arrives as one ``{"$and": [...]}`` key, which would
    otherwise be judged as a single (table-spanning) conjunct.
    """
    out: list[dict[str, Any]] = []
    for key, value in filt.items():
        if key == "$and" and isinstance(value, list):
            for item in value:
                if isinstance(item, dict):
                    out.extend(_flatten_and(item))
        else:
            out.append({key: value})
    return out


def _merge_frags(frags: list[dict[str, Any]]) -> dict[str, Any]:
    """Recombine conjunct fragments, using ``$and`` only when a key repeats (two
    predicates on one field can't share a dict key)."""
    out: dict[str, Any] = {}
    extra: list[dict[str, Any]] = []
    for frag in frags:
        for key, value in frag.items():
            if key in out:
                extra.append({key: value})
            else:
                out[key] = value
    if extra:
        first = dict(out)
        out = (
            {"$and": [first, *extra]}
            if first
            else ({"$and": extra} if len(extra) > 1 else extra[0])
        )
    return out


def _push_single_table_predicates(
    filt: dict[str, Any],
    pipeline: list[dict[str, Any]],
    amap: dict[str, tuple[str, TableDef]],
    base_alias: str | None,
) -> dict[str, Any]:
    """Move each single-table WHERE conjunct to the stage that produces its rows,
    returning whatever must still be matched after the join.

    A comma join whose WHERE conjuncts each constrain ONE table is a cross
    product in disguise — sqllogictest's ``select4`` is full of them::

        SELECT b7, d5+18+d5, c2 FROM t7, t2, t5
         WHERE c5=733 AND a2 IN (...) AND 460=e7

    There is no join condition at all. Matching after the ``$lookup``s means
    materialising |t7| x |t2| x |t5| rows to return a handful: growth measured
    cubic in table size (27k rows 0.09s, 216k 0.56s, 1M 2.53s), so the corpus's
    ~700-row tables are ~343M rows — the >300s in the backlog. Filtering each
    table as it enters collapses that to the product of the SURVIVING rows.

    Only a conjunct attributable to exactly one table moves:

    * a bare (unprefixed) key belongs to the base table and becomes a ``$match``
      ahead of the first ``$lookup``;
    * an ``<alias>.<path>`` key moves into that alias's ``$lookup``
      sub-pipeline, prefix stripped (the sub-pipeline runs against the foreign
      collection, so the remainder is that collection's own path);
    * an operator subtree (``$or`` / ``$nor`` / ``$expr``) moves when EVERY field
      it touches belongs to one table — sqllogictest constrains a table with
      nothing but ``(e9=245 OR 35=e9 OR 799=e9)``, and leaving that behind left
      the table both unfiltered and unjoined;
    * anything else stays behind — a subtree spanning tables cannot be decided by
      either table alone, and a prefix that is not a joined alias is just a
      dotted field of the base table.

    A top-level ``$and`` is FLATTENED first. Whenever the WHERE contains an OR,
    the whole filter arrives as a single ``{"$and": [...]}`` key; treating that as
    one conjunct made it span tables, so nothing moved — not even the plain
    single-table equalities beside it.

    **A left join's lookup is never pushed into.** WHERE runs after the join, so
    a predicate on the right table of a LEFT JOIN must delete the outer row;
    filtering inside the lookup would leave that row with nulls and KEEP it.
    Only an alias whose ``$unwind`` is non-preserving (an inner join) qualifies.
    """
    inner_aliases = {
        str(st["$unwind"]["path"]).lstrip("$")
        for st in pipeline
        if isinstance(st.get("$unwind"), dict)
        and not st["$unwind"].get("preserveNullAndEmptyArrays", False)
    }

    per_frags: dict[str, list[dict[str, Any]]] = {}
    base_frags: list[dict[str, Any]] = []
    residual_frags: list[dict[str, Any]] = []

    for frag in _flatten_and(filt):
        ((key, value),) = frag.items()
        if key.startswith("$"):
            owner = _sole_filter_owner(value, amap, base_alias)
            if owner is not None and owner != base_alias and owner in inner_aliases:
                per_frags.setdefault(owner, []).append({key: _strip_alias_prefix(value, owner)})
            elif owner is not None and owner == base_alias:
                base_frags.append(frag)
            else:
                residual_frags.append(frag)
            continue
        prefix, _, rest = key.partition(".")
        if rest and prefix in amap and prefix != base_alias and prefix in inner_aliases:
            per_frags.setdefault(prefix, []).append({rest: value})
        elif not rest and prefix not in amap:
            base_frags.append(frag)
        else:
            residual_frags.append(frag)

    for stage in pipeline:
        lookup = stage.get("$lookup")
        if not isinstance(lookup, dict):
            continue
        pushed = per_frags.pop(str(lookup.get("as")), None)
        if pushed:
            lookup.setdefault("pipeline", []).insert(0, {"$match": _merge_frags(pushed)})
    # An alias whose $lookup wasn't found keeps its predicate rather than losing it.
    for alias, frags in per_frags.items():
        for frag in frags:
            for path, value in frag.items():
                residual_frags.append({f"{alias}.{path}": value})

    if base_frags:
        pipeline.insert(0, {"$match": _merge_frags(base_frags)})
    return _merge_frags(residual_frags)


def _build_outer_join_pipeline(
    stmt: exp.Select,
    a_alias: str,
    a_table: TableDef,
    jn: exp.Expression,
    db: str,
    catalog: Any,
    storage: Any,
    derived: list[DerivedTable],
) -> tuple[
    TableDef, dict[str, tuple[str, TableDef]], Resolve, list[dict[str, Any]], list[DerivedTable]
]:
    """Build the prefix for a single ``A <RIGHT|FULL> JOIN B ON …``.

    ``A RIGHT JOIN B`` is ``B LEFT JOIN A``: drive the pipeline from B, look A up,
    and preserve unmatched B rows. ``A FULL JOIN B`` is the LEFT join from A
    (preserving unmatched A) unioned with the B rows that found no A match
    (reshaped so B's columns sit under its alias and A's columns read as NULL).
    The ``amap`` is always inserted in FROM order (A then B) so ``SELECT *`` keeps
    Postgres's left-to-right column order regardless of which side drives."""
    base, amap, pipeline = _outer_join_stages(a_alias, a_table, jn, db, catalog, storage, derived)
    resolve = _join_resolver(amap)
    where = stmt.args.get("where")
    if where is not None and not where_needs_per_row(stmt) and _join_where_lowerable(stmt, resolve):
        pipeline.append({"$match": _expr_to_filter(where.this, resolve, _pipeline_subctx.get())})
    return base, amap, resolve, pipeline, derived


def _outer_join_stages(
    a_alias: str,
    a_table: TableDef,
    jn: exp.Expression,
    db: str,
    catalog: Any,
    storage: Any,
    derived: list[DerivedTable],
) -> tuple[TableDef, dict[str, tuple[str, TableDef]], list[dict[str, Any]]]:
    """The ``$lookup``/``$unwind`` (+ FULL anti-join ``$unionWith``) stages for one
    ``A <RIGHT|FULL> JOIN B``, plus the driving ``base`` and the FROM-ordered
    ``amap``. No WHERE is appended — the caller places it after any trailing joins."""
    side = str(jn.args.get("side") or "").upper()
    b_alias, b_table = _resolve_source(jn.this, db, catalog, storage, derived)
    on = jn.args.get("on")
    if on is None:
        raise errors.feature_not_supported("JOIN without ON is not supported")
    if side == "RIGHT":
        amap = {a_alias: ("join", a_table), b_alias: ("base", b_table)}
        pipeline = [
            _lookup_stage(on, a_alias, a_table, amap),
            {"$unwind": {"path": f"${a_alias}", "preserveNullAndEmptyArrays": True}},
        ]
        base = b_table
    else:  # FULL
        amap = {a_alias: ("base", a_table), b_alias: ("join", b_table)}
        pipeline = [
            _lookup_stage(on, b_alias, b_table, amap),
            {"$unwind": {"path": f"${b_alias}", "preserveNullAndEmptyArrays": True}},
            {
                "$unionWith": {
                    "coll": b_table.collection,
                    "pipeline": _full_join_anti_branch(on, a_alias, a_table, b_alias, b_table),
                }
            },
        ]
        base = a_table
    return base, amap, pipeline


def _build_leading_outer_join_pipeline(
    stmt: exp.Select,
    base_alias: str,
    base: TableDef,
    joins: list[exp.Expression],
    db: str,
    catalog: Any,
    storage: Any,
    derived: list[DerivedTable],
) -> tuple[
    TableDef, dict[str, tuple[str, TableDef]], Resolve, list[dict[str, Any]], list[DerivedTable]
]:
    """A *leading* ``RIGHT``/``FULL`` join followed by a tail of only INNER/LEFT
    joins — ``A RIGHT|FULL JOIN B ON p1 [INNER|LEFT] JOIN C ON p2 …``.

    The leading outer join (`_outer_join_stages`) builds the composite ``(A⋈B)`` as
    the driving stream (FROM-ordered ``amap``: A then B). Each tail join then runs
    the ordinary forward ``$lookup``/``$unwind`` over that stream — the composite is
    only ever the *driving* side, never a ``$lookup.from`` / ``$unionWith.coll``, so
    the "composite is not a real collection" obstruction never arises. For a leading
    FULL, the tail joins run after the anti-join ``$unionWith`` and so apply to both
    branches (the anti-branch already carries ``b.<field>`` under B's alias, so the
    tail ON resolves identically and A's columns read NULL there)."""
    driving, amap, pipeline = _outer_join_stages(
        base_alias, base, joins[0], db, catalog, storage, derived
    )
    for jn in joins[1:]:
        _append_forward_join(jn, amap, pipeline, db, catalog, storage, derived)
    resolve = _join_resolver(amap)
    where = stmt.args.get("where")
    if where is not None and not where_needs_per_row(stmt):
        pipeline.append({"$match": _expr_to_filter(where.this, resolve, _pipeline_subctx.get())})
    return driving, amap, resolve, pipeline, derived


def _full_join_anti_branch(
    on: exp.Expression, a_alias: str, a_table: TableDef, b_alias: str, b_table: TableDef
) -> list[dict[str, Any]]:
    """The FULL-join's right anti-join arm: B rows with no A match, reshaped to the
    main branch's layout. Driving from B, look A up; keep only the B rows whose
    lookup came back empty; then nest the whole B doc under its alias so ``b.col``
    paths resolve and A's (base) bare-field paths are absent (→ NULL)."""
    amap_b = {b_alias: ("base", b_table)}
    return [
        _lookup_stage(on, a_alias, a_table, amap_b),
        {"$match": {a_alias: {"$size": 0}}},
        {"$replaceWith": {b_alias: "$$ROOT"}},
    ]


def _on_referenced_aliases(on: exp.Expression) -> set[str] | None:
    """The set of table aliases an ON predicate references. Returns None if any
    column is unqualified — then adjacency can't be proven and the caller bails."""
    refs: set[str] = set()
    for col in on.find_all(exp.Column):
        if not col.table:
            return None
        refs.add(col.table)
    return refs


def _build_right_chain_pipeline(
    stmt: exp.Select,
    base_alias: str,
    base: TableDef,
    joins: list[exp.Expression],
    db: str,
    catalog: Any,
    storage: Any,
    derived: list[DerivedTable],
) -> tuple[
    TableDef, dict[str, tuple[str, TableDef]], Resolve, list[dict[str, Any]], list[DerivedTable]
]:
    """A *pure* RIGHT chain of 3+ tables, via approach (a): reverse into a LEFT chain
    driven from the last table.

    ``A RIGHT JOIN B ON o1 RIGHT JOIN C ON o2`` binds left-associatively as
    ``(A RJ B) RJ C``. Since ``X RJ Y == Y LJ X``, that is
    ``C LEFT JOIN B ON o2 LEFT JOIN A ON o1`` — the reversed FROM order driven from
    C. The re-association is sound only when each ON is *adjacent* (joins its table
    to the immediately-prior FROM table), so no predicate reaches across the chain;
    a non-adjacent ON falls back to ``0A000``. The ``amap`` is rebuilt in original
    FROM order so ``SELECT *`` keeps Postgres's left-to-right column order even
    though the pipeline drives from the far end."""
    # Resolve every source in FROM order: T0 = base, T1..Tn from the join list.
    tables: list[tuple[str, TableDef]] = [(base_alias, base)]
    ons: list[exp.Expression] = []
    for jn in joins:
        jt = jn.this
        on = jn.args.get("on")
        if on is None or isinstance(jt, (exp.Lateral, exp.Unnest)) or _is_jsonb_each_join(jt):
            raise errors.feature_not_supported(
                "a RIGHT JOIN chain supports only plain-table joins with an ON clause"
            )
        j_alias, j_table = _resolve_source(jt, db, catalog, storage, derived)
        tables.append((j_alias, j_table))
        ons.append(on)

    # Adjacency guard: join k (bringing table T_{k+1}) may reference only that table
    # and its predecessor T_k in FROM order.
    aliases = [a for a, _ in tables]
    for k, on in enumerate(ons):
        refs = _on_referenced_aliases(on)
        if refs is None or not refs <= {aliases[k + 1], aliases[k]}:
            raise errors.feature_not_supported(
                "a RIGHT JOIN chain is only supported when each ON joins adjacent tables"
            )

    # Reversed LEFT chain: base = the last table; then walk the joins back-to-front,
    # each bringing its predecessor table in with the same ON (which now relates an
    # already-placed table to the newly-joined one).
    n = len(joins)
    new_base_alias, new_base = tables[n]
    amap: dict[str, tuple[str, TableDef]] = {new_base_alias: ("base", new_base)}
    pipeline: list[dict[str, Any]] = []
    for k in range(n - 1, -1, -1):
        j_alias, j_table = tables[k]
        pipeline.append(_lookup_stage(ons[k], j_alias, j_table, amap))
        pipeline.append({"$unwind": {"path": f"${j_alias}", "preserveNullAndEmptyArrays": True}})
        amap[j_alias] = ("join", j_table)

    # Rebuild amap in original FROM order for SELECT * (roles unchanged: the reversed
    # base drives, every other table is nested under its alias).
    ordered_amap: dict[str, tuple[str, TableDef]] = {}
    for a, t in tables:
        ordered_amap[a] = ("base", t) if a == new_base_alias else ("join", t)
    resolve = _join_resolver(ordered_amap)

    where = stmt.args.get("where")
    if where is not None and not where_needs_per_row(stmt):
        pipeline.append({"$match": _expr_to_filter(where.this, resolve, _pipeline_subctx.get())})
    return new_base, ordered_amap, resolve, pipeline, derived


def _build_trailing_composite_pipeline(
    stmt: exp.Select,
    a_alias: str,
    a_table: TableDef,
    joins: list[exp.Expression],
    db: str,
    catalog: Any,
    storage: Any,
    derived: list[DerivedTable],
    *,
    kind: str,
) -> tuple[
    TableDef, dict[str, tuple[str, TableDef]], Resolve, list[dict[str, Any]], list[DerivedTable]
]:
    """An N-table INNER/LEFT composite followed by a single trailing RIGHT/FULL join
    — ``A [INNER|LEFT] JOIN B ON o1 [[INNER|LEFT] JOIN … ON …] RIGHT|FULL JOIN C ON
    o2`` (``joins[:-1]`` build the composite, ``joins[-1]`` is the trailing outer
    join). One construction covers the 2-table, 3-table, and 4+-table composites.

    Lowered by the **main ∪ anti** decomposition (proven exact):
    ``(A⋈…) RIGHT JOIN C`` = ``[(A⋈…) INNER JOIN C]  ∪  [C with no composite match]``
    and ``… FULL JOIN C`` = ``[(A⋈…) LEFT JOIN C]  ∪  [C with no composite match]`` —
    RIGHT and FULL differ only in whether C's ``$unwind`` in the main branch preserves
    unmatched composite rows (``kind == "FULL"``).

    - The **main branch** is the ordinary forward pipeline: drive from A, forward
      ``$lookup`` each composite table (honoring its INNER/LEFT side), then ``$lookup``
      C. No reversal → the composite is built at its natural root A, so there is no
      half-match leak (a C matching a table row that isn't in the composite pads the
      whole composite side NULL rather than leaking that row's columns).
    - The **anti branch** ``$unionWith``s the C collection and, for each C, rebuilds
      the *same forward composite* inside a ``$lookup`` from A, filters it by ``o2``
      (composite columns as ``$field`` paths, C columns as ``$$`` let vars via
      ``_OnTranslator(new_amap=…)``), and keeps only the C rows whose composite came
      back empty (``$size: 0``); C is then nested under its alias so the composite
      columns read NULL.

    Because both branches build the composite forward from A (never re-rooted at a
    pivot), this is sound for INNER *and* LEFT composites, and ``o2`` may reference C
    together with *any* subset of the composite tables (not just a single pivot).
    ``0A000`` for a non-plain-table source, a missing/unqualified ON, a non-adjacent
    composite ON, or an ``o2`` that doesn't relate C to the composite."""
    *lead_joins, jn_c = joins
    o2 = jn_c.args.get("on")
    if o2 is None:
        raise errors.feature_not_supported(
            f"a trailing {kind} join over a composite requires an ON clause on every join"
        )
    for jn in joins:
        if isinstance(jn.this, (exp.Lateral, exp.Unnest)) or _is_jsonb_each_join(jn.this):
            raise errors.feature_not_supported(
                f"a trailing {kind} join over a composite supports plain-table joins only"
            )
    c_alias, c_table = _resolve_source(jn_c.this, db, catalog, storage, derived)

    # Resolve the composite tables (FROM order) and prove adjacency: each leading ON
    # must be fully qualified and join its own table to an already-known one, so the
    # composite builds forward from A.
    lead: list[tuple[str, TableDef, exp.Expression, str]] = []
    known: set[str] = {a_alias}
    for jn in lead_joins:
        j_alias, j_table = _resolve_source(jn.this, db, catalog, storage, derived)
        on = jn.args.get("on")
        if on is None:
            raise errors.feature_not_supported(
                f"a trailing {kind} join over a composite requires an ON clause on every join"
            )
        refs = _on_referenced_aliases(on)
        if refs is None:
            raise errors.feature_not_supported(
                f"a trailing {kind} join over a composite requires fully-qualified ON columns"
            )
        if j_alias not in refs or not refs <= (known | {j_alias}):
            raise errors.feature_not_supported(
                "each composite ON must join its table to an already-joined one"
            )
        known.add(j_alias)
        lead.append((j_alias, j_table, on, str(jn.args.get("side") or "").upper()))

    o2_refs = _on_referenced_aliases(o2)
    if o2_refs is None:
        raise errors.feature_not_supported(
            f"a trailing {kind} join requires fully-qualified ON columns"
        )
    composite_refs = o2_refs - {c_alias}
    if c_alias not in o2_refs or not composite_refs or not composite_refs <= known:
        raise errors.feature_not_supported(
            f"a trailing {kind} join's ON must relate C to the composite tables"
        )

    def _emit_composite(amap: dict[str, tuple[str, TableDef]], pipe: list[dict[str, Any]]) -> None:
        # Forward A⋈B⋈… over A's docs: A stays base (bare), each table nests under its
        # alias, preserving unmatched rows only for a LEFT join.
        for j_alias, j_table, on, side in lead:
            pipe.append(_lookup_stage(on, j_alias, j_table, amap))
            pipe.append(
                {"$unwind": {"path": f"${j_alias}", "preserveNullAndEmptyArrays": side == "LEFT"}}
            )
            amap[j_alias] = ("join", j_table)

    # Main branch: forward composite, then C joined INNER (RIGHT) / LEFT (FULL).
    amap: dict[str, tuple[str, TableDef]] = {a_alias: ("base", a_table)}
    pipeline: list[dict[str, Any]] = []
    _emit_composite(amap, pipeline)
    pipeline.append(_lookup_stage(o2, c_alias, c_table, amap))
    pipeline.append(
        {"$unwind": {"path": f"${c_alias}", "preserveNullAndEmptyArrays": kind == "FULL"}}
    )
    amap[c_alias] = ("join", c_table)

    # Anti branch: C rows whose forward composite (filtered by o2) is empty.
    comp_amap: dict[str, tuple[str, TableDef]] = {a_alias: ("base", a_table)}
    comp_pipe: list[dict[str, Any]] = []
    _emit_composite(comp_amap, comp_pipe)
    tr = _OnTranslator(c_alias, c_table, {c_alias: ("base", c_table)}, new_amap=comp_amap)
    comp_pipe.append({"$match": {"$expr": tr.expr(o2)}})
    anti_field = "__tc"
    anti_pipeline: list[dict[str, Any]] = [
        {
            "$lookup": {
                "from": a_table.collection,
                "let": {var: f"${path}" for path, var in tr.lets.items()},
                "pipeline": comp_pipe,
                "as": anti_field,
            }
        },
        {"$match": {anti_field: {"$size": 0}}},
        {"$project": {anti_field: 0}},
        {"$replaceWith": {c_alias: "$$ROOT"}},
    ]
    pipeline.append({"$unionWith": {"coll": c_table.collection, "pipeline": anti_pipeline}})

    resolve = _join_resolver(amap)
    where = stmt.args.get("where")
    if where is not None and not where_needs_per_row(stmt):
        pipeline.append({"$match": _expr_to_filter(where.this, resolve, _pipeline_subctx.get())})
    return a_table, amap, resolve, pipeline, derived


def _plan_join_select(
    stmt: exp.Select, db: str, catalog: Any, storage: Any = None
) -> PipelineSelectPlan | EvaluatedSelectPlan:
    # Collect any rich LATERAL joins encountered while building the pipeline (they're
    # registered in ``amap`` but produce no pipeline stages — the executor expands
    # them nested-loop). A fresh collector scopes this to the plain join-select path.
    token = _lateral_collect.set([])
    try:
        base, amap, resolve, pipeline, derived = _build_join_pipeline(stmt, db, catalog, storage)
        laterals = _lateral_collect.get() or []
    finally:
        _lateral_collect.reset(token)

    # A scalar SELECT list / ORDER BY, a correlated / EXISTS WHERE (which the
    # pipeline builder deliberately left un-pushed), a WHERE the join $match
    # couldn't lower (it was skipped in _build_join_pipeline — dropping it here
    # would return unfiltered rows), DISTINCT ON (keep-first per key), or a rich
    # LATERAL all need the per-row evaluator.
    if (
        _stmt_needs_evaluation(stmt)
        or where_needs_per_row(stmt)
        or not _join_where_lowerable(stmt, resolve)
        or _distinct_on(stmt)
        or laterals
    ):
        plan = _build_evaluated_join(stmt, base, amap, resolve, pipeline, derived)
        plan.lateral_joins = laterals
        return plan

    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    out_enum_types: dict[int, str] = {}
    out_sources: list[tuple[Any, int] | None] = []
    names = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Star):
            for a, (role, tdef) in amap.items():
                for i, c in enumerate(tdef.columns, start=1):
                    name = names.fresh(c.name)
                    project[name] = f"${c.field if role == 'base' else f'{a}.{c.field}'}"
                    if c.enum_type is not None:
                        out_enum_types[len(out_columns)] = c.enum_type
                    out_columns.append((name, c.type_tag))
                    out_sources.append((tdef, i))
            continue
        path, tag = resolve(inner)
        name = names.fresh(alias or _column_name(inner))
        project[name] = f"${path}"
        src_col = _column_for_order_node(inner, amap)
        if src_col is not None and src_col.enum_type is not None:
            out_enum_types[len(out_columns)] = src_col.enum_type
        out_columns.append((name, tag))
        out_sources.append(_source_table_attnum(inner, amap))
    _append_join_tail(pipeline, stmt, resolve, project, out_columns, amap)
    return PipelineSelectPlan(
        base.collection,
        {},
        pipeline,
        out_columns,
        out_enum_types=out_enum_types,
        derived=derived,
        out_sources=out_sources,
    )


def _join_aggregate_of(
    node: exp.Expression,
) -> tuple[str, exp.Expression | None, bool] | None:
    """Like ``_aggregate_of`` but keeps the argument NODE so the join resolver can
    map a qualified column (``b.amt``). Returns ``(func, arg_node, distinct)``; a
    None argument means ``COUNT(*)`` and a ``DISTINCT`` argument is unwrapped."""
    inner = node.this if isinstance(node, exp.Alias) else node
    if isinstance(inner, exp.Filter):  # agg(...) FILTER (WHERE ...)
        inner = inner.this
    for cls, name in _AGG_CLASSES.items():
        if isinstance(inner, cls):
            arg = inner.this
            distinct = isinstance(arg, exp.Distinct)
            if distinct:
                exprs = arg.expressions
                arg = exprs[0] if exprs else None
            return name, (None if arg is None or isinstance(arg, exp.Star) else arg), distinct
    # ``every(x)`` is the standard-SQL spelling of ``bool_and(x)`` (parses as an
    # Anonymous call rather than a dedicated node).
    if isinstance(inner, exp.Anonymous):
        fname = (inner.this if isinstance(inner.this, str) else inner.name).lower()
        if fname == "every" and inner.expressions:
            return "bool_and", inner.expressions[0], False
    return None


def _join_accumulator(
    func: str, arg: exp.Expression | None, resolve: Resolve, filter_cond: Any = None
) -> tuple[dict[str, Any], str]:
    if arg is None:
        return _accumulator_for(func, None, None, filter_cond)
    arg = _strip_identity_wrappers(arg)
    if not _is_field_node(arg) and func in ("count", "sum", "avg", "min", "max"):
        # An expression argument (``SUM(- 83)``, ``MAX(o.qty + 1)``) — lower it
        # the way the single-table accumulator does.
        body = _to_agg_expr(arg, resolve)
        if func == "count":  # COUNT(<expr>) counts non-null values (COUNT(NULL) is 0)
            matched = {"$ne": [body, None]}
            cond = {"$and": [filter_cond, matched]} if filter_cond is not None else matched
            return {"$sum": {"$cond": [cond, 1, 0]}}, "int8"
        if filter_cond is not None:
            body = {"$cond": [filter_cond, body, 0 if func == "sum" else None]}
        return {f"${func}": body}, _agg_out_tag(func, _infer_scalar_tag(arg, resolve))
    path, tag = resolve(arg)
    return _accumulator_for(func, path, tag, filter_cond)


def _agg_key(
    func: str, arg: exp.Expression | None, resolve: Resolve, distinct: bool = False
) -> str:
    """A hashable identity for an aggregate (for HAVING accumulator dedup)."""
    if arg is None:
        ident = "*"
    elif _is_field_node(arg):
        ident = resolve(arg)[0]
    else:
        ident = arg.sql()  # expression argument — identity by SQL text
    return f"{func}:{'d' if distinct else ''}:{ident}"


def _plan_join_group_select(
    stmt: exp.Select, db: str, catalog: Any, storage: Any = None
) -> PipelineSelectPlan:
    """JOIN combined with GROUP BY / aggregates: build the $lookup/$unwind/$match
    prefix, then a $group whose keys and accumulators resolve through the join
    resolver (so ``a.region`` / ``SUM(b.amt)`` map to the post-unwind paths)."""
    base, amap, resolve, pipeline, derived = _build_join_pipeline(stmt, db, catalog, storage)
    # A correlated / EXISTS WHERE wasn't pushed into a ``$match`` (see
    # ``_build_join_pipeline``); it's filtered per joined row after the join prefix
    # and before the ``$group`` below. ``residual_split`` marks that boundary.
    where_node = stmt.args.get("where")
    residual = (
        where_node.this
        if where_node is not None
        and (where_needs_per_row(stmt) or not _join_where_lowerable(stmt, resolve))
        else None
    )
    residual_split = len(pipeline)

    # Computed GROUP BY keys (``GROUP BY lower(c.name)`` / ``o.qty + 1``) — lower each
    # through the join resolver into a synthetic ``__gkeyN`` field materialised by a
    # post-join ``$addFields``, then rewrite SELECT / HAVING / ORDER references to it.
    gkey_fields: dict[str, Any] = {}
    computed = _computed_group_keys(stmt.args.get("group"))
    if computed:
        targets, gkey_fields, _pyfields = _lower_computed_group_keys(computed, resolve, set())
        stmt = _apply_group_key_rewrite(stmt, targets)
        pipeline.append({"$addFields": gkey_fields})

    group_node = stmt.args.get("group")
    group_keys: dict[str, str] = {}  # _id key name -> resolved "$path"
    key_tag: dict[str, str] = {}
    qualified_key: dict[tuple[str | None, str], str] = {}
    if group_node is not None:
        for c in group_node.expressions:
            if not isinstance(c, exp.Column):
                raise errors.feature_not_supported(f"GROUP BY expression not supported: {c.sql()}")
            keyname = _column_name(c)
            if keyname in gkey_fields:  # synthetic computed key (materialised above)
                group_keys[keyname] = f"${keyname}"
                key_tag[keyname] = "any"
                continue
            path, tag = resolve(c)
            if keyname in group_keys and group_keys[keyname] != f"${path}":
                # The same bare column name grouped from two aliases
                # (``GROUP BY cor1.col1, cor0.col1``) — mint a distinct grouped
                # field; qualified references find it via ``qualified_key``.
                keyname = f"{c.table or 'key'}__{keyname}"
            qualified_key[(c.table or None, _column_name(c))] = keyname
            group_keys[keyname] = f"${path}"
            key_tag[keyname] = tag
    group_id = group_keys or None

    accumulators: dict[str, Any] = {}
    reductions: dict[str, Any] = {}
    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    out_enum_types: dict[int, str] = {}
    post_aggregates: list[tuple[str, str, float | None]] = []
    names = _NameAllocator()
    agg_fields: dict[str, str] = {}

    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        arr_arg = _array_agg_arg(e)
        oagg = _jsonb_object_agg_args(e)
        sagg = _string_agg_arg(e)
        agg = _join_aggregate_of(e)
        where = _agg_filter_where(e)
        fcond = _filter_cond_to_agg(where, resolve) if where is not None else None
        if arr_arg is not None:
            fname = names.fresh(alias or "array_agg")
            value_node, terms = _agg_order_spec(arr_arg)
            if terms:  # array_agg(x ORDER BY …): push {v, k}, executor sorts
                if fcond is not None:
                    raise errors.feature_not_supported(
                        "FILTER (WHERE ...) with an in-aggregate ORDER BY is not supported"
                    )
                accumulators[fname] = _sorted_agg_push_resolve(value_node, terms, resolve)
                post_aggregates.append((fname, "sorted_array", [(d, nf) for _k, d, nf in terms]))
                project[fname] = f"${fname}"
            else:
                path, _ = resolve(arr_arg)
                accumulators[fname] = {"$push": _push_filtered(f"${path}", fcond, wrap=True)}
                project[fname] = _array_agg_project(fname, fcond)
            out_columns.append((fname, "json"))
        elif oagg is not None:
            fname = names.fresh(alias or "jsonb_object_agg")
            kpath, _ = resolve(oagg[0])
            vpath, _ = resolve(oagg[1])
            pair = {"k": {"$toString": f"${kpath}"}, "v": f"${vpath}"}
            accumulators[fname] = {"$push": _push_filtered(pair, fcond)}
            project[fname] = _jsonb_object_agg_project(fname, fcond)
            out_columns.append((fname, "json"))
        elif sagg is not None:
            fname = names.fresh(alias or "string_agg")
            value_node, terms = _agg_order_spec(sagg[0])
            if terms:  # string_agg(x, sep ORDER BY …): push {v, k}, executor sorts+joins
                if fcond is not None:
                    raise errors.feature_not_supported(
                        "FILTER (WHERE ...) with an in-aggregate ORDER BY is not supported"
                    )
                accumulators[fname] = _sorted_agg_push_resolve(value_node, terms, resolve)
                project[fname] = f"${fname}"
                post_aggregates.append(
                    (fname, "sorted_string", ([(d, nf) for _k, d, nf in terms], sagg[1]))
                )
            else:
                path, _ = resolve(sagg[0])
                accumulators[fname] = {"$push": _push_filtered(f"${path}", fcond)}
                project[fname] = _string_agg_project(fname, sagg[1])
            out_columns.append((fname, "text"))
        elif agg is not None and agg[0] in (set(_POST_STAT_FUNCS) | _BIT_AGG_FUNCS):
            # variance / var_pop (square of stdDev) and bit_and/or/xor (push +
            # Python fold) — same post-aggregate finish as the single-table path,
            # resolved through the join resolver.
            func, arg_node, _distinct = agg
            if arg_node is None:
                raise errors.feature_not_supported(f"{func}(*) is not supported")
            if fcond is not None:
                raise errors.feature_not_supported(
                    f"FILTER (WHERE ...) on {func}() is not supported"
                )
            fname = names.fresh(alias or func)
            path, coltag = resolve(arg_node)
            val = f"${path}"
            if func in _BIT_AGG_FUNCS:
                accumulators[fname] = {"$push": val}
                tag = coltag if coltag in ("int4", "int8") else "int8"
                post_aggregates.append((fname, func, None))
            else:
                accumulators[fname] = {_POST_STAT_FUNCS[func]: val}
                tag = "numeric"
                post_aggregates.append((fname, "variance", None))
            project[fname] = f"${fname}"
            out_columns.append((fname, tag))
        elif agg is not None:
            func, arg, distinct = agg
            if distinct and func in _DISTINCT_FUNCS:
                if arg is None:
                    raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
                if _is_field_node(_strip_identity_wrappers(arg)):
                    path, tag = resolve(_strip_identity_wrappers(arg))
                    fname, tag = _register_distinct_agg(
                        func, path, tag, alias, names, accumulators, reductions, fcond
                    )
                else:
                    # Expression DISTINCT argument (``COUNT(DISTINCT 74)``).
                    fname, tag = _register_distinct_agg(
                        func,
                        None,
                        _infer_scalar_tag(arg, resolve),
                        alias,
                        names,
                        accumulators,
                        reductions,
                        fcond,
                        value=_to_agg_expr(arg, resolve),
                    )
            else:
                acc, tag = _join_accumulator(func, arg, resolve, fcond)
                fname = names.fresh(alias or func)
                accumulators[fname] = acc
                if func == "sum" and arg is not None:
                    stripped = _strip_identity_wrappers(arg)
                    value = (
                        f"${resolve(stripped)[0]}"
                        if _is_field_node(stripped)
                        else _to_agg_expr(stripped, resolve)
                    )
                    _guard_sum_null(fname, value, fcond, names, accumulators, reductions)
            agg_fields[_agg_key(func, arg, resolve, distinct)] = fname
            project[fname] = f"${fname}"
            out_columns.append((fname, tag))
        else:
            inner = e.this if isinstance(e, exp.Alias) else e
            if isinstance(inner, exp.Star):
                raise errors.feature_not_supported("SELECT * with GROUP BY is not supported")
            if not isinstance(inner, exp.Column):
                raise errors.feature_not_supported(
                    f"non-aggregate SELECT expression not supported with GROUP BY: {inner.sql()}"
                )
            colname = _column_name(inner)
            keyname = qualified_key.get((inner.table or None, colname), colname)
            if keyname not in group_keys:
                raise errors.SQLError(
                    "42803",
                    f'column "{colname}" must appear in the GROUP BY clause '
                    "or be used in an aggregate function",
                )
            out_name = names.fresh(alias or colname)
            project[out_name] = f"$_id.{keyname}"
            src_col = _column_for_order_node(inner, amap)
            if src_col is not None and src_col.enum_type is not None:
                out_enum_types[len(out_columns)] = src_col.enum_type
            out_columns.append((out_name, key_tag[keyname]))

    having = stmt.args.get("having")
    having_match = (
        _join_having_to_match(
            having.this, resolve, accumulators, agg_fields, group_keys, key_tag, names, reductions
        )
        if having is not None
        else None
    )
    order_aggs = _register_orderby_aggs_join(
        stmt, resolve, accumulators, reductions, project, names
    )
    pipeline.append({"$group": {"_id": group_id, **accumulators}})
    if reductions:
        pipeline.append({"$addFields": reductions})
    if having_match is not None:
        pipeline.append({"$match": having_match})
    pipeline.append({"$project": project})
    if stmt.args.get("distinct") and not post_aggregates and not order_aggs:
        # SELECT DISTINCT over the grouped join output — same dedup $group as
        # the single-table group planner (skipped when the executor still has
        # post-aggregates to finish or a hidden ORDER BY aggregate must survive).
        dedup_id = {name: f"${name}" for name, _tag in out_columns}
        pipeline.append({"$group": {"_id": dedup_id}})
        pipeline.append({"$project": {"_id": 0, **{n: f"$_id.{n}" for n in dedup_id}}})
    _append_sort_limit(pipeline, stmt, out_columns, amap=amap, order_aggs=order_aggs)
    return PipelineSelectPlan(
        base.collection,
        {},
        pipeline,
        out_columns,
        out_enum_types=out_enum_types,
        derived=derived,
        residual_where=residual,
        residual_resolve=resolve if residual is not None else None,
        residual_split=residual_split,
        post_aggregates=post_aggregates,
    )


def _join_grouping_set_branch(
    stmt: exp.Select,
    resolve: Resolve,
    col_node_for: dict[str, exp.Column],
    group_keys: dict[str, str],
    key_tag: dict[str, str],
    key_path: dict[str, str],
    gset: list[str],
) -> tuple[list[dict[str, Any]], list[tuple[str, str]], list[tuple[str, str, float | None]]]:
    """One grouping set's ``[$group, $project]`` sub-pipeline for the JOIN path —
    the join analogue of ``_grouping_set_branch``. Group columns / aggregate args
    resolve through the join ``resolve`` (so ``d.region`` / ``SUM(e.amt)`` map to
    the post-unwind paths); columns absent from this set project as literal NULL so
    every branch shares the same output shape (required for the ``$unionWith``).
    ``group_keys`` / ``key_tag`` cover every grouping column across all sets so a
    ``HAVING`` may reference any of them. ``key_path`` maps each grouping column
    (bare or synthetic ``__gkeyN``) to its resolved ``$group`` key path. Returns
    ``(stages, out_columns, post_aggregates)`` — statistical / bitwise finishes run
    in Python over the union (identical across branches)."""
    in_set = set(gset)
    group_id = {c: f"${key_path[c]}" for c in gset} or None
    accumulators: dict[str, Any] = {}
    reductions: dict[str, Any] = {}
    project: dict[str, Any] = {"_id": 0}
    out_columns: list[tuple[str, str]] = []
    post_aggregates: list[tuple[str, str, float | None]] = []
    names = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        grp = _grouping_args(e)
        arr_arg = _array_agg_arg(e)
        oagg = _jsonb_object_agg_args(e)
        sagg = _string_agg_arg(e)
        agg = _join_aggregate_of(e)
        where = _agg_filter_where(e)
        fcond = _filter_cond_to_agg(where, resolve) if where is not None else None
        if grp is not None:
            for c in grp:
                if c not in col_node_for:
                    raise errors.feature_not_supported(
                        f"GROUPING() argument must be a grouping column: {c}"
                    )
            fname = names.fresh(alias or "grouping")
            project[fname] = {"$literal": _grouping_bitmask(grp, in_set)}
            out_columns.append((fname, "int4"))
        elif arr_arg is not None:
            fname = names.fresh(alias or "array_agg")
            value_node, terms = _agg_order_spec(arr_arg)
            if terms:  # array_agg(x ORDER BY …) over the join: push {v, k}, sort over union
                if fcond is not None:
                    raise errors.feature_not_supported(
                        "FILTER (WHERE ...) with an in-aggregate ORDER BY is not supported"
                    )
                accumulators[fname] = _sorted_agg_push_resolve(value_node, terms, resolve)
                project[fname] = f"${fname}"
                post_aggregates.append((fname, "sorted_array", [(d, nf) for _k, d, nf in terms]))
            else:
                path, _ = resolve(arr_arg)
                accumulators[fname] = {"$push": _push_filtered(f"${path}", fcond, wrap=True)}
                project[fname] = _array_agg_project(fname, fcond)
            out_columns.append((fname, "json"))
        elif oagg is not None:
            fname = names.fresh(alias or "jsonb_object_agg")
            kpath, _ = resolve(oagg[0])
            vpath, _ = resolve(oagg[1])
            pair = {"k": {"$toString": f"${kpath}"}, "v": f"${vpath}"}
            accumulators[fname] = {"$push": _push_filtered(pair, fcond)}
            project[fname] = _jsonb_object_agg_project(fname, fcond)
            out_columns.append((fname, "json"))
        elif sagg is not None:
            fname = names.fresh(alias or "string_agg")
            value_node, terms = _agg_order_spec(sagg[0])
            if terms:  # string_agg(x, sep ORDER BY …) over the join: push {v, k}, sort+join
                if fcond is not None:
                    raise errors.feature_not_supported(
                        "FILTER (WHERE ...) with an in-aggregate ORDER BY is not supported"
                    )
                accumulators[fname] = _sorted_agg_push_resolve(value_node, terms, resolve)
                project[fname] = f"${fname}"
                post_aggregates.append(
                    (fname, "sorted_string", ([(d, nf) for _k, d, nf in terms], sagg[1]))
                )
            else:
                path, _ = resolve(sagg[0])
                accumulators[fname] = {"$push": _push_filtered(f"${path}", fcond)}
                project[fname] = _string_agg_project(fname, sagg[1])
            out_columns.append((fname, "text"))
        elif agg is not None and agg[0] in (set(_POST_STAT_FUNCS) | _BIT_AGG_FUNCS):
            # variance / var_pop and bit_and/or/xor over the join — a $group
            # accumulator plus a post-aggregate finish (resolved through the join
            # resolver), run over the unioned rows.
            func, arg_node, _distinct = agg
            if arg_node is None:
                raise errors.feature_not_supported(f"{func}(*) is not supported")
            if fcond is not None:
                raise errors.feature_not_supported(
                    f"FILTER (WHERE ...) on {func}() is not supported"
                )
            fname = names.fresh(alias or func)
            path, coltag = resolve(arg_node)
            val = f"${path}"
            if func in _BIT_AGG_FUNCS:
                accumulators[fname] = {"$push": val}
                tag = coltag if coltag in ("int4", "int8") else "int8"
                post_aggregates.append((fname, func, None))
            else:
                accumulators[fname] = {_POST_STAT_FUNCS[func]: val}
                tag = "numeric"
                post_aggregates.append((fname, "variance", None))
            project[fname] = f"${fname}"
            out_columns.append((fname, tag))
        elif agg is not None:
            func, arg, distinct = agg
            if distinct and func in _DISTINCT_FUNCS:
                if arg is None:
                    raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
                path, tag = resolve(arg)
                fname, tag = _register_distinct_agg(
                    func, path, tag, alias, names, accumulators, reductions, fcond
                )
            else:
                acc, tag = _join_accumulator(func, arg, resolve, fcond)
                fname = names.fresh(alias or func)
                accumulators[fname] = acc
            project[fname] = f"${fname}"
            out_columns.append((fname, tag))
        else:
            inner = e.this if isinstance(e, exp.Alias) else e
            if isinstance(inner, exp.Star):
                raise errors.feature_not_supported("SELECT * with GROUP BY is not supported")
            if not isinstance(inner, exp.Column):
                raise errors.feature_not_supported(
                    f"non-aggregate SELECT expression not supported with GROUP BY: {inner.sql()}"
                )
            col = _column_name(inner)
            out_name = names.fresh(alias or col)
            # A grouping column (bare or synthetic ``__gkeyN``) takes its precomputed
            # tag; any other column resolves + types through the join resolver.
            tag = key_tag[col] if col in key_tag else resolve(inner)[1]
            if col in in_set:
                project[out_name] = f"$_id.{col}"
            else:
                project[out_name] = {"$literal": None}
            out_columns.append((out_name, tag))
    having = stmt.args.get("having")
    having_match = (
        _join_having_to_match(
            having.this, resolve, accumulators, {}, group_keys, key_tag, names, reductions
        )
        if having is not None
        else None
    )
    stages: list[dict[str, Any]] = [{"$group": {"_id": group_id, **accumulators}}]
    if reductions:
        stages.append({"$addFields": reductions})
    if having_match is not None:
        stages.append({"$match": having_match})
    stages.append({"$project": project})
    return stages, out_columns, post_aggregates


def _lower_join_group_keys(
    stmt: exp.Select, resolve: Resolve, join_prefix: list[dict[str, Any]]
) -> tuple[exp.Select, dict[str, Any]]:
    """Lower any *computed* GROUP BY keys over a JOIN (``ROLLUP(lower(d.label))``)
    into synthetic ``__gkeyN`` fields: a ``$addFields`` materialising them is appended
    to the shared ``join_prefix`` (so they exist in the base pipeline *and* every
    replayed ``$unionWith`` branch), and SELECT / GROUP BY / HAVING / ORDER references
    are rewritten to the bare ``__gkeyN`` columns. Returns ``(rewritten_stmt,
    gkey_fields)``; ``gkey_fields`` is empty when there are no computed keys."""
    computed = _computed_group_keys(stmt.args.get("group"))
    if not computed:
        return stmt, {}
    targets, gkey_fields, _pyfields = _lower_computed_group_keys(computed, resolve, set())
    stmt = _apply_group_key_rewrite(stmt, targets)
    join_prefix.append({"$addFields": gkey_fields})
    return stmt, gkey_fields


def _plan_join_grouping_sets_select(
    stmt: exp.Select, db: str, catalog: Any, storage: Any = None
) -> PipelineSelectPlan:
    """GROUP BY ROLLUP / CUBE / GROUPING SETS *over a JOIN* → the UNION (via
    ``$unionWith``) of a plain JOIN+GROUP BY per enumerated grouping set. Each
    ``$unionWith`` branch replays the whole ``$lookup``/``$unwind``/``$match`` join
    prefix (it re-reads the base collection) before its own ``$group``/``$project``,
    mirroring the single-table ``_plan_grouping_sets_select``."""
    base, amap, resolve, join_prefix, derived = _build_join_pipeline(stmt, db, catalog, storage)
    # A correlated / EXISTS WHERE couldn't push into the join's ``$match`` — it would
    # need per-row evaluation, which the replayed union branches can't do.
    if stmt.args.get("where") is not None and where_needs_per_row(stmt):
        raise errors.feature_not_supported(
            "a correlated WHERE with GROUPING SETS over a JOIN is not supported"
        )
    # Computed GROUP BY keys (``ROLLUP(lower(d.label))``) lower through the join
    # resolver into synthetic ``__gkeyN`` fields materialised by a ``$addFields``
    # appended to the join prefix — so they exist in the base pipeline *and* every
    # replayed ``$unionWith`` branch — then SELECT / HAVING / ORDER references rewrite
    # to those fields.
    stmt, gkey_fields = _lower_join_group_keys(stmt, resolve, join_prefix)
    group_node = stmt.args["group"]
    col_node_for = _group_col_nodes(group_node)
    sets = _grouping_sets(group_node)
    group_cols = sorted({c for gs in sets for c in gs})
    # HAVING / branch group-key resolution needs every grouping column resolved once;
    # a synthetic ``__gkeyN`` key resolves to its own top-level field (tag ``any``).
    group_keys: dict[str, str] = {}
    key_tag: dict[str, str] = {}
    key_path: dict[str, str] = {}
    for c in group_cols:
        path, tag = (c, "any") if c in gkey_fields else resolve(col_node_for[c])
        group_keys[c] = f"${path}"
        key_tag[c] = tag
        key_path[c] = path

    branches = [
        _join_grouping_set_branch(stmt, resolve, col_node_for, group_keys, key_tag, key_path, gs)
        for gs in sets
    ]
    pipeline = list(join_prefix) + list(branches[0][0])
    out_columns = branches[0][1]
    # Statistical / bitwise post-aggregate finish — identical across branches.
    post_aggregates = branches[0][2]
    for sub, _cols, _post in branches[1:]:
        pipeline.append(
            {"$unionWith": {"coll": base.collection, "pipeline": list(join_prefix) + sub}}
        )
    _append_sort_limit(pipeline, stmt, out_columns, amap=amap)
    return PipelineSelectPlan(
        base.collection, {}, pipeline, out_columns, derived=derived, post_aggregates=post_aggregates
    )


def _plan_join_grouping_sets_window_select(
    stmt: exp.Select, db: str, catalog: Any, storage: Any = None
) -> EvaluatedSelectPlan:
    """Window function(s) over a GROUPING SETS / ROLLUP / CUBE query that *also* sits
    over a JOIN — the join analogue of ``_plan_grouping_sets_window_select`` (b219),
    combined with the join grouping-sets union (b218).

    Each grouping set's branch replays the ``$lookup``/``$unwind``/``$match`` join
    prefix and projects *flat* group-column + aggregate fields (aggregate args and
    group keys resolved through the join resolver); the branches are unioned via
    ``$unionWith``; then the union is handed to the evaluated executor, which
    computes each window over the grouped rows. A rolled-up row (a group column
    reads NULL) still participates, and ``GROUPING()`` is available inside the
    window's ORDER BY / PARTITION BY."""
    stmt = stmt.copy()  # we mutate the tree, replacing aggregates / GROUPING with columns
    base, _amap, resolve, join_prefix, derived = _build_join_pipeline(stmt, db, catalog, storage)
    if stmt.args.get("where") is not None and where_needs_per_row(stmt):
        raise errors.feature_not_supported(
            "a correlated WHERE with a window over GROUPING SETS over a JOIN is not supported"
        )
    having = stmt.args.get("having")
    if having is not None and having.this.find(exp.Select) is not None:
        raise errors.feature_not_supported(
            "a subquery in HAVING with a window over GROUPING SETS over a JOIN is not supported"
        )
    # Computed grouping keys over a JOIN (``ROLLUP(lower(d.label))``) lower into
    # synthetic ``__gkeyN`` fields materialised by a ``$addFields`` on the join prefix.
    stmt, gkey_fields = _lower_join_group_keys(stmt, resolve, join_prefix)
    group_node = stmt.args["group"]
    col_node_for = _group_col_nodes(group_node)
    sets = _grouping_sets(group_node)
    group_cols = sorted({c for gs in sets for c in gs})

    names = _NameAllocator()
    field_tags: dict[str, str] = {}
    group_keys: dict[str, str] = {}  # bare name -> resolved "$path" (for HAVING)
    key_tag: dict[str, str] = {}
    key_path: dict[str, str] = {}  # bare/synthetic name -> resolved $group key path
    for c in group_cols:
        path, tag = (c, "any") if c in gkey_fields else resolve(col_node_for[c])
        group_keys[c] = f"${path}"
        key_tag[c] = tag
        key_path[c] = path
        field_tags[c] = tag
        names.fresh(c)  # reserve group names so synthetic agg fields never collide

    accumulators: dict[str, Any] = {}
    reductions: dict[str, Any] = {}
    agg_fields: dict[str, str] = {}  # _agg_key -> field name
    agg_field_names: list[str] = []

    def register_agg(node: exp.AggFunc) -> str:
        arr_arg = _array_agg_arg(node)
        if arr_arg is not None:
            fname = names.fresh("array_agg")
            path, _ = resolve(arr_arg)
            accumulators[fname] = {"$push": f"${path}"}
            field_tags[fname] = "json"
            agg_field_names.append(fname)
            return fname
        agg = _join_aggregate_of(node)
        if agg is None:
            raise errors.feature_not_supported(f"unsupported aggregate: {node.sql()}")
        func, arg, distinct = agg
        key = _agg_key(func, arg, resolve, distinct)
        if key in agg_fields:
            return agg_fields[key]
        where = _agg_filter_where(node)
        fcond = _filter_cond_to_agg(where, resolve) if where is not None else None
        if distinct and func in _DISTINCT_FUNCS:
            if arg is None:
                raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
            stripped = _strip_identity_wrappers(arg)
            if _is_field_node(stripped):
                path, tag = resolve(stripped)
                fname, tag = _register_distinct_agg(
                    func, path, tag, None, names, accumulators, reductions, fcond
                )
            else:
                # Expression DISTINCT argument (``COUNT(DISTINCT 74)``).
                fname, tag = _register_distinct_agg(
                    func,
                    None,
                    _infer_scalar_tag(stripped, resolve),
                    None,
                    names,
                    accumulators,
                    reductions,
                    fcond,
                    value=_to_agg_expr(stripped, resolve),
                )
        else:
            acc, tag = _join_accumulator(func, arg, resolve, fcond)
            fname = names.fresh(func)
            accumulators[fname] = acc
            if func == "sum" and arg is not None:
                stripped = _strip_identity_wrappers(arg)
                value = (
                    f"${resolve(stripped)[0]}"
                    if _is_field_node(stripped)
                    else _to_agg_expr(stripped, resolve)
                )
                _guard_sum_null(fname, value, fcond, names, accumulators, reductions)
        agg_fields[key] = fname
        field_tags[fname] = tag
        agg_field_names.append(fname)
        return fname

    # ``GROUPING(col, …)`` → a per-branch literal bitmask field, rewritten to a
    # column reference so the window phase resolves it like any grouped field.
    grouping_specs: list[tuple[str, list[str]]] = []
    for gnode in list(stmt.find_all(exp.Grouping)):
        gcols = [_column_name(a) for a in gnode.expressions]
        for c in gcols:
            if c not in col_node_for:
                raise errors.feature_not_supported(
                    f"GROUPING() argument must be a grouping column: {c}"
                )
        gfname = names.fresh("grouping")
        field_tags[gfname] = "int4"
        grouping_specs.append((gfname, gcols))
        gnode.replace(exp.column(gfname))

    for node in _group_agg_nodes(stmt):
        node.replace(exp.column(register_agg(node)))

    having_match = (
        _join_having_to_match(
            having.this, resolve, accumulators, agg_fields, group_keys, key_tag, names, reductions
        )
        if having is not None
        else None
    )

    def branch(gset: list[str]) -> list[dict[str, Any]]:
        in_set = set(gset)
        group_id = {c: f"${key_path[c]}" for c in gset} or None
        project: dict[str, Any] = {"_id": 0}
        for c in group_cols:
            project[c] = f"$_id.{c}" if c in in_set else {"$literal": None}
        for fname in agg_field_names:
            project[fname] = f"${fname}"
        for gfname, gcols in grouping_specs:
            project[gfname] = {"$literal": _grouping_bitmask(gcols, in_set)}
        stages: list[dict[str, Any]] = [{"$group": {"_id": group_id, **accumulators}}]
        if reductions:
            stages.append({"$addFields": reductions})
        if having_match is not None:
            stages.append({"$match": having_match})
        stages.append({"$project": project})
        return stages

    pipeline = list(join_prefix) + branch(sets[0])
    for gset in sets[1:]:
        pipeline.append(
            {"$unionWith": {"coll": base.collection, "pipeline": list(join_prefix) + branch(gset)}}
        )
    return _finish_group_window(stmt, base.collection, {}, pipeline, field_tags, derived)


def _plan_join_group_window_select(
    stmt: exp.Select, db: str, catalog: Any, storage: Any = None
) -> EvaluatedSelectPlan:
    """JOIN + GROUP BY combined with window functions — the join analogue of
    ``_plan_group_window_select``. The $lookup/$unwind/$match/$group/$project
    pipeline produces the grouped rows (aggregates resolved through the join
    resolver), then the evaluated executor runs the windows over them."""
    stmt = stmt.copy()  # we mutate the tree, replacing aggregates with columns
    base, _amap, resolve, pipeline, derived = _build_join_pipeline(stmt, db, catalog, storage)
    # A correlated / EXISTS WHERE — or one the join ``$match`` couldn't lower
    # (``_build_join_pipeline`` skipped the push; dropping it here would group
    # unfiltered rows) — is carried as a residual that filters the joined rows
    # before the ``$group`` (the split is the current length of the join prefix).
    where_node = stmt.args.get("where")
    residual_pre = (
        where_node.this
        if (
            where_node is not None
            and (where_needs_per_row(stmt) or not _join_where_lowerable(stmt, resolve))
        )
        else None
    )
    pre_split = len(pipeline)

    group_node = stmt.args.get("group")
    group_keys: dict[str, str] = {}
    key_tag: dict[str, str] = {}
    field_tags: dict[str, str] = {}
    names = _NameAllocator()
    key_rewrites: list[tuple[str | None, str, str]] = []
    if group_node is not None:
        for c in group_node.expressions:
            if not isinstance(c, exp.Column):
                raise errors.feature_not_supported(f"GROUP BY expression not supported: {c.sql()}")
            keyname = _column_name(c)
            path, tag = resolve(c)
            if keyname in group_keys and group_keys[keyname] != f"${path}":
                # The same bare column name grouped from two aliases
                # (``GROUP BY cor1.col1, cor0.col1``) — mint a distinct grouped
                # field and rewrite this alias's references after the aggregate
                # pass (collapsing them grouped by only one of the columns).
                keyname = names.fresh(f"{c.table or 'key'}__{keyname}")
                key_rewrites.append((c.table or None, _column_name(c), keyname))
            else:
                names.fresh(keyname)  # reserve so synthetic agg fields never collide
            group_keys[keyname] = f"${path}"
            key_tag[keyname] = tag
            field_tags[keyname] = tag
    group_id = group_keys or None

    accumulators: dict[str, Any] = {}
    reductions: dict[str, Any] = {}
    agg_fields: dict[str, str] = {}  # _agg_key -> field name
    agg_field_names: list[str] = []

    def register_agg(node: exp.AggFunc) -> str:
        arr_arg = _array_agg_arg(node)
        if arr_arg is not None:
            fname = names.fresh("array_agg")
            path, _ = resolve(arr_arg)
            accumulators[fname] = {"$push": f"${path}"}
            field_tags[fname] = "json"
            agg_field_names.append(fname)
            return fname
        agg = _join_aggregate_of(node)
        if agg is None:
            raise errors.feature_not_supported(f"unsupported aggregate: {node.sql()}")
        func, arg, distinct = agg
        key = _agg_key(func, arg, resolve, distinct)
        if key in agg_fields:
            return agg_fields[key]
        where = _agg_filter_where(node)
        fcond = _filter_cond_to_agg(where, resolve) if where is not None else None
        if distinct and func in _DISTINCT_FUNCS:
            if arg is None:
                raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
            stripped = _strip_identity_wrappers(arg)
            if _is_field_node(stripped):
                path, tag = resolve(stripped)
                fname, tag = _register_distinct_agg(
                    func, path, tag, None, names, accumulators, reductions, fcond
                )
            else:
                # Expression DISTINCT argument (``COUNT(DISTINCT 74)``).
                fname, tag = _register_distinct_agg(
                    func,
                    None,
                    _infer_scalar_tag(stripped, resolve),
                    None,
                    names,
                    accumulators,
                    reductions,
                    fcond,
                    value=_to_agg_expr(stripped, resolve),
                )
        else:
            acc, tag = _join_accumulator(func, arg, resolve, fcond)
            fname = names.fresh(func)
            accumulators[fname] = acc
            if func == "sum" and arg is not None:
                stripped = _strip_identity_wrappers(arg)
                value = (
                    f"${resolve(stripped)[0]}"
                    if _is_field_node(stripped)
                    else _to_agg_expr(stripped, resolve)
                )
                _guard_sum_null(fname, value, fcond, names, accumulators, reductions)
        agg_fields[key] = fname
        field_tags[fname] = tag
        agg_field_names.append(fname)
        return fname

    for node in _group_agg_nodes(stmt):
        node.replace(exp.column(register_agg(node)))

    if key_rewrites:
        # Rewrite the SELECT / ORDER BY / HAVING references of the renamed
        # duplicate keys onto their minted grouped fields. Aggregate arguments
        # were already replaced above (they resolve pre-group paths); the WHERE
        # is deliberately untouched (it filters the pre-group joined rows).
        roots: list[exp.Expression] = list(stmt.expressions)
        order_node = stmt.args.get("order")
        if order_node is not None:
            roots.extend(o.this for o in order_node.expressions)
        having_node = stmt.args.get("having")
        if having_node is not None:
            roots.append(having_node.this)
        for alias, colname, keyname in key_rewrites:
            for r in roots:
                for col in list(r.find_all(exp.Column)):
                    if col.name == colname and (col.table or None) == alias:
                        col.replace(exp.column(keyname))

    having = stmt.args.get("having")
    having_match = (
        _join_having_to_match(
            having.this, resolve, accumulators, agg_fields, group_keys, key_tag, names, reductions
        )
        if having is not None
        else None
    )
    pipeline.append({"$group": {"_id": group_id, **accumulators}})
    if reductions:
        pipeline.append({"$addFields": reductions})
    if having_match is not None:
        pipeline.append({"$match": having_match})
    project: dict[str, Any] = {"_id": 0}
    for keyname in group_keys:
        project[keyname] = f"$_id.{keyname}"
    for fname in agg_field_names:
        project[fname] = f"${fname}"
    pipeline.append({"$project": project})
    return _finish_group_window(
        stmt,
        base.collection,
        {},
        pipeline,
        field_tags,
        derived,
        pre_where=residual_pre,
        pre_where_resolve=resolve if residual_pre is not None else None,
        pre_where_split=pre_split,
    )


def _join_having_to_match(
    node: exp.Expression,
    resolve: Resolve,
    accumulators: dict[str, Any],
    agg_fields: dict[str, str],
    group_keys: dict[str, str],
    key_tag: dict[str, str],
    names: Any = None,
    reductions: Any = None,
) -> dict[str, Any]:
    """HAVING for the JOIN+GROUP path — mirrors ``_having_to_match`` but resolves
    columns / aggregate args through the join resolver."""

    def rec(n: exp.Expression) -> dict[str, Any]:
        return _join_having_to_match(
            n, resolve, accumulators, agg_fields, group_keys, key_tag, names, reductions
        )

    const = _constant_predicate_filter(node)
    if const is not None:
        return const
    if isinstance(node, exp.Paren):
        return rec(node.this)
    if isinstance(node, exp.And):
        return _merge_and([rec(node.this), rec(node.expression)])
    if isinstance(node, exp.Or):
        return {"$or": [rec(node.this), rec(node.expression)]}

    def field_tag(term: exp.Expression) -> tuple[str, str]:
        if isinstance(term, exp.Column):
            keyname = _column_name(term)
            if keyname not in group_keys:
                raise errors.SQLError(
                    "42803",
                    f'column "{keyname}" must appear in the GROUP BY clause '
                    "or be used in an aggregate function",
                )
            return f"_id.{keyname}", key_tag[keyname]
        agg = _join_aggregate_of(term)
        if agg is None:
            raise errors.feature_not_supported(f"unsupported HAVING term: {term.sql()}")
        func, arg, distinct = agg
        key = _agg_key(func, arg, resolve, distinct)
        where = _agg_filter_where(term)
        fcond = _filter_cond_to_agg(where, resolve) if where is not None else None
        if distinct and func in _DISTINCT_FUNCS:
            if key in agg_fields:  # already registered by the SELECT list — reuse
                path, tag = resolve(arg)
                return agg_fields[key], _agg_out_tag(func, tag)
            if names is None or reductions is None or arg is None:
                raise errors.feature_not_supported(
                    f"DISTINCT inside {func}() is not supported in HAVING"
                )
            path, tag = resolve(arg)
            fname, tag = _register_distinct_agg(
                func, path, tag, None, names, accumulators, reductions, fcond
            )
            agg_fields[key] = fname
            return fname, tag
        acc, tag = _join_accumulator(func, arg, resolve, fcond)
        if key not in agg_fields:
            fname = f"__having_{len(agg_fields)}"
            accumulators[fname] = acc
            agg_fields[key] = fname
        return agg_fields[key], tag

    if isinstance(node, (exp.EQ, exp.NEQ)) or type(node) in _HAVING_CMP:
        left, right = node.this, node.expression
        term, lit, on_left = left, right, True
        if not isinstance(left, (exp.Column, exp.Filter, *_AGG_CLASSES.keys())):
            term, lit, on_left = right, left, False
        field, tag = field_tag(term)
        value = typemap.coerce(_literal(lit), tag)
        if isinstance(node, exp.EQ):
            return {field: value}
        if isinstance(node, exp.NEQ):
            return {field: {"$ne": value}}
        op, flipped = _HAVING_CMP[type(node)]
        return {field: {(op if on_left else flipped): value}}

    # A NULL-literal operand that makes the predicate *always unknown* — a
    # NULL side of a comparison, a NULL IN a non-empty list, a NULL BETWEEN
    # subject (or both bounds NULL) — excludes the group, and NOT preserves
    # unknown, so the fold holds through any NOT / paren nesting.
    core = node
    while isinstance(core, (exp.Not, exp.Paren)):
        core = core.this
    if _always_unknown_predicate(core):
        return {"$nor": [{}]}

    # ``HAVING [NOT …] <operand> IS [NOT] NULL`` — IS NOT NULL parses as
    # Not(Is(…)); IS NULL is two-valued, so each NOT just flips the filter and
    # any nesting depth stays exact.
    is_node = node
    negate = False
    while isinstance(is_node, (exp.Not, exp.Paren)):
        if isinstance(is_node, exp.Not):
            negate = not negate
        is_node = is_node.this
    if isinstance(is_node, exp.Is) and isinstance(is_node.expression, exp.Null):

        def gk_resolve(col_node: exp.Expression) -> tuple[str, str]:
            if not isinstance(col_node, exp.Column):
                raise errors.feature_not_supported(f"expected a column: {col_node.sql()}")
            keyname = _column_name(col_node)
            if keyname not in group_keys:
                raise errors.SQLError(
                    "42803",
                    f'column "{keyname}" must appear in the GROUP BY clause '
                    "or be used in an aggregate function",
                )
            return f"_id.{keyname}", key_tag[keyname]

        operand = is_node.this
        while isinstance(operand, exp.Paren):
            operand = operand.this
        if isinstance(operand, (exp.Column, exp.Filter, *_AGG_CLASSES.keys())):
            field, _tag = field_tag(operand)
            return {field: {"$ne": None}} if negate else {field: None}
        if next(operand.find_all(exp.AggFunc), None) is None:
            # A computed operand over group keys (``(- col2) IS NOT NULL``).
            value = _to_agg_expr(operand, gk_resolve)
            return {"$expr": {("$ne" if negate else "$eq"): [value, None]}}

    if (
        isinstance(is_node, exp.In)
        and is_node.args.get("query") is None
        and is_node.expressions
        and next(is_node.find_all(exp.AggFunc), None) is None
        and next(is_node.find_all(exp.Select), None) is None
    ):
        # ``HAVING [NOT] <expr> IN (<exprs over group keys>)`` — three-valued
        # membership over the grouped join fields.
        def gk2_resolve(col_node: exp.Expression) -> tuple[str, str]:
            if not isinstance(col_node, exp.Column):
                raise errors.feature_not_supported(f"expected a column: {col_node.sql()}")
            keyname = _column_name(col_node)
            if keyname not in group_keys:
                raise errors.SQLError(
                    "42803",
                    f'column "{keyname}" must appear in the GROUP BY clause '
                    "or be used in an aggregate function",
                )
            return f"_id.{keyname}", key_tag[keyname]

        return _having_in_filter(is_node, negate, gk2_resolve)

    raise errors.feature_not_supported(f"unsupported HAVING clause: {node.sql()}")


def _is_simple_projection(node: exp.Expression) -> bool:
    """A SELECT item that lowers to a plain ``$project`` field (no per-row eval)."""
    inner = node.this if isinstance(node, exp.Alias) else node
    return isinstance(inner, (exp.Column, exp.Star, *_JSONB_CLASSES))


def _stmt_needs_evaluation(stmt: exp.Select) -> bool:
    """Whether a SELECT list / ORDER BY needs Python per-row evaluation
    (set-returning or scalar functions, CASE, scalar subqueries) rather than a
    plain ``$project`` / ``$group``. Aggregates and ``array_agg`` are handled by
    the group/find paths, not per-row eval, so they don't count here."""
    for e in stmt.expressions:
        if (
            _is_simple_projection(e)
            or _aggregate_of(e) is not None
            or _array_agg_arg(e) is not None
            or _jsonb_object_agg_args(e) is not None
            or _range_agg_arg(e) is not None
        ):
            continue
        return True
    order = stmt.args.get("order")
    if order is not None:
        return any(not isinstance(o.this, exp.Column) for o in order.expressions)
    return False


_BOOL_EXPR_TYPES = (
    exp.Is,
    exp.Not,
    exp.And,
    exp.Or,
    exp.In,
    exp.Boolean,
    exp.EQ,
    exp.NEQ,
    exp.GT,
    exp.GTE,
    exp.LT,
    exp.LTE,
    exp.Like,
    exp.ILike,
    exp.RegexpLike,
    exp.RegexpILike,
)


def _has_range_operand(node: exp.Expression, resolve: Resolve) -> bool:
    """Does an operand of ``node`` (an @> / <@ / && operator) resolve to a range —
    a range-typed column or a range constructor?"""
    for operand in (node.this, node.expression):
        if isinstance(operand, exp.Anonymous) and str(operand.this).lower() in typemap._RANGE_TAGS:
            return True
        if isinstance(operand, exp.Column):
            try:
                if resolve(operand)[1] in typemap._RANGE_TAGS:
                    return True
            except errors.SQLError:
                pass
    return False


def _has_net_operand(node: exp.Expression, resolve: Resolve) -> bool:
    """Does an operand of ``node`` (a ``<<`` / ``>>`` / ``&&`` operator) resolve to
    a network value — a net-typed column or a cast to ``inet`` / ``cidr`` /
    ``macaddr``?"""
    for operand in (node.this, node.expression):
        if (
            isinstance(operand, exp.Cast)
            and operand.to is not None
            and operand.to.sql(dialect="postgres").lower().strip() in typemap._NET_TAGS
        ):
            return True
        if isinstance(operand, exp.Column):
            try:
                if resolve(operand)[1] in typemap._NET_TAGS:
                    return True
            except errors.SQLError:
                pass
    return False


def _has_geo_operand(node: exp.Expression, resolve: Resolve) -> bool:
    """Does an operand of ``node`` resolve to a geometry — a geo-typed column or a
    cast to a geometric type?"""
    for operand in (node.this, node.expression):
        if (
            isinstance(operand, exp.Cast)
            and operand.to is not None
            and typemap.type_tag_for_sql(operand.to) in typemap._GEO_TAGS
        ):
            return True
        if isinstance(operand, exp.Column):
            try:
                if resolve(operand)[1] in typemap._GEO_TAGS:
                    return True
            except errors.SQLError:
                pass
    return False


def _has_bit_operand(node: exp.Expression, resolve: Resolve) -> bool:
    """Does an operand of ``node`` (a bitwise operator) resolve to a bit string —
    a ``B'…'`` literal, a bit/varbit-typed column, or a cast to ``bit`` /
    ``varbit``?"""
    for operand in (node.this, node.expression):
        if isinstance(operand, exp.BitString):
            return True
        if isinstance(operand, exp.Paren):
            operand = operand.this
        if isinstance(operand, exp.BitString):
            return True
        if (
            isinstance(operand, exp.Cast)
            and operand.to is not None
            and typemap.type_tag_for_sql(operand.to) in typemap._BIT_TAGS
        ):
            return True
        if isinstance(operand, exp.Column):
            try:
                if resolve(operand)[1] in typemap._BIT_TAGS:
                    return True
            except errors.SQLError:
                pass
    return False


def _has_bytea_operand(node: exp.Expression, resolve: Resolve) -> bool:
    """Does an operand of ``node`` resolve to a ``bytea`` — a bytea-typed column, a
    cast to ``bytea``, or a ``decode(...)`` call?"""
    for operand in (node.this, node.expression):
        if isinstance(operand, exp.Paren):
            operand = operand.this
        if getattr(exp, "Decode", None) is not None and isinstance(operand, exp.Decode):
            return True
        if (
            isinstance(operand, exp.Cast)
            and operand.to is not None
            and typemap.type_tag_for_sql(operand.to) == "bytea"
        ):
            return True
        if isinstance(operand, exp.Column):
            try:
                if resolve(operand)[1] == "bytea":
                    return True
            except errors.SQLError:
                pass
    return False


def _has_hstore_operand(node: exp.Expression, resolve: Resolve) -> bool:
    """Does an operand of ``node`` resolve to an hstore — an hstore-typed column, a
    cast to ``hstore``, or an hstore-returning function (``hstore`` / ``delete``)?"""
    for operand in (node.this, node.expression):
        if isinstance(operand, exp.Paren):
            operand = operand.this
        if (
            isinstance(operand, exp.Cast)
            and operand.to is not None
            and typemap.type_tag_for_sql(operand.to) == "hstore"
        ):
            return True
        if isinstance(operand, exp.Anonymous) and str(operand.this).lower() in ("hstore", "delete"):
            return True
        if isinstance(operand, exp.Column):
            try:
                if resolve(operand)[1] == "hstore":
                    return True
            except errors.SQLError:
                pass
    return False


def _has_array_operand(node: exp.Expression, resolve: Resolve) -> bool:
    """Does an operand of ``node`` resolve to a Postgres array — an array-typed
    column, an ``ARRAY[...]`` constructor, or a cast to an ``<type>[]`` array?"""
    for operand in (node.this, node.expression):
        if isinstance(operand, exp.Paren):
            operand = operand.this
        if isinstance(operand, exp.Array):
            return True
        if (
            isinstance(operand, exp.Cast)
            and operand.to is not None
            and typemap.is_array_tag(typemap.type_tag_for_sql(operand.to))
        ):
            return True
        if isinstance(operand, exp.Column):
            try:
                if typemap.is_array_tag(resolve(operand)[1]):
                    return True
            except errors.SQLError:
                pass
    return False


def _date_arith_tag(node: exp.Expression, resolve: Resolve) -> str | None:
    """Result tag of a date / time ``Add`` / ``Sub``: ``date - date -> int4``,
    ``date ± int -> date``, ``date ± interval -> timestamp`` (naive, per Postgres),
    and ``time - time -> interval``. None when it's not a date/time operation."""
    lt = _infer_scalar_tag(node.this, resolve)
    rt = _infer_scalar_tag(node.expression, resolve)
    ints = ("int4", "int8")
    if isinstance(node, exp.Sub):
        if lt == "date" and rt == "date":
            return "int4"
        if lt == "date" and rt in ints:
            return "date"
        if lt == "date" and rt == "interval":
            return "timestamp"
        if lt == "time" and rt == "time":
            return "interval"
    elif isinstance(node, exp.Add):
        if lt == "date" and rt in ints:
            return "date"
        if rt == "date" and lt in ints:
            return "date"
        if (lt == "date" and rt == "interval") or (rt == "date" and lt == "interval"):
            return "timestamp"
    return None


_TS_TAGS = ("timestamp", "timestamptz")


def _interval_arith_tag(node: exp.Expression, resolve: Resolve) -> str | None:
    """Result tag of an ``Add`` / ``Sub`` / ``Mul`` / ``Div`` involving an interval
    or a timestamp difference, or None when neither applies.

    ``timestamp(tz) - timestamp(tz) -> interval``; ``timestamp ± interval`` keeps
    the timestamp's tz-ness (``timestamptz ± interval -> timestamptz``,
    ``timestamp ± interval -> timestamp``)."""
    lt = _infer_scalar_tag(node.this, resolve)
    rt = _infer_scalar_tag(node.expression, resolve)
    if isinstance(node, exp.Sub) and lt in _TS_TAGS and rt in _TS_TAGS:
        return "interval"
    if "interval" in (lt, rt):
        if isinstance(node, (exp.Add, exp.Sub)):
            if lt == "timestamptz" or rt == "timestamptz":
                return "timestamptz"
            if lt == "timestamp" or rt == "timestamp":
                return "timestamp"
        return "interval"
    return None


def _range_tag_of(operands: Any, resolve: Resolve) -> str | None:
    """The range type tag among ``operands`` — a range constructor / range-typed
    column — or None if none is a range."""
    for operand in operands:
        if operand is None:
            continue
        if isinstance(operand, exp.Anonymous) and str(operand.this).lower() in typemap._RANGE_TAGS:
            return str(operand.this).lower()
        if isinstance(operand, exp.Column):
            try:
                tag = resolve(operand)[1]
            except errors.SQLError:
                continue
            if tag in typemap._RANGE_TAGS:
                return tag
    return None


_INT_TAG_ORDER = {"int2": 0, "int4": 1, "int8": 2}
_NUMERIC_FAMILY = frozenset({"int2", "int4", "int8", "float4", "float8", "numeric"})


def _tag_to_regtype(tag: str) -> str:
    if typemap.is_array_tag(tag):
        elem = typemap.array_element_tag(tag)
        return f"{typemap.SQL_TYPE_NAME.get(elem, elem)}[]"
    return typemap.SQL_TYPE_NAME.get(tag, tag)


def _pg_typeof_name(
    arg: exp.Expression,
    resolve: Resolve,
    param_oids: tuple[int, ...] | list[int],
    user_type_name: Any = None,
) -> str:
    """The regtype text ``pg_typeof(arg)`` prints for ``arg``'s static type."""
    # An untyped string literal (and a bare NULL) is the ``unknown`` pseudo-type
    # in Postgres until context types it.
    if isinstance(arg, exp.Null):
        return "unknown"
    if isinstance(arg, exp.Literal) and arg.is_string:
        return "unknown"
    # A cast to a user-declared type (a substituted composite/enum parameter
    # arrives as ``'…'::testcomp``) prints the type's own name.
    if (
        isinstance(arg, exp.Cast)
        and isinstance(arg.to, exp.DataType)
        and arg.to.this
        and getattr(arg.to.this, "name", None) == "USERDEFINED"
    ):
        from secantus.sql.catalog import fold_type_name

        return fold_type_name(arg.to.sql(dialect="postgres"))
    # A bare ``$N`` types as the OID the client declared in Parse (psycopg's
    # ``select pg_typeof(%s)`` sends the value's type there); an undeclared
    # parameter (OID 0) falls to text, the type Postgres assumes when the call
    # gives the parameter no other context.
    if isinstance(arg, exp.Parameter):
        try:
            idx = int(arg.name) - 1
        except (TypeError, ValueError):
            return "text"
        oid = param_oids[idx] if 0 <= idx < len(param_oids) else 0
        tag = typemap.OID_TO_TAG.get(oid)
        if tag is None and oid and user_type_name is not None:
            # A minted user-type oid (registered composite/enum dumpers declare
            # them) — resolve through the caller's catalog.
            uname = user_type_name(oid)
            if uname:
                return str(uname)
        return _tag_to_regtype(tag or "text")
    return _tag_to_regtype(_infer_scalar_tag(arg, resolve))


def rewrite_pg_typeof(
    stmt: exp.Expression,
    table: TableDef | None,
    param_oids: tuple[int, ...] | list[int] = (),
    user_type_name: Any = None,
) -> None:
    """Replace ``pg_typeof(x)`` calls with their regtype text, in place.

    ``pg_typeof`` is *static* — it reports the argument's type without needing
    its value — so it resolves at plan time via the same inference that types
    RowDescription. A call directly in the SELECT list keeps Postgres' output
    column name (``pg_typeof``). For prepared statements the rewrite runs at
    Parse time so ``$N`` arguments can type from the client's declared OIDs."""
    resolve = table_resolver(table) if table is not None else _const_scope
    for node in list(stmt.find_all(exp.Anonymous)):
        if str(node.this).lower() != "pg_typeof" or not node.expressions:
            continue
        try:
            name = _pg_typeof_name(node.expressions[0], resolve, param_oids, user_type_name)
        except errors.SQLError:
            continue  # let the normal path surface the real error
        parent = node.parent
        # ``pg_typeof(x)::oid`` — regtype casts to oid as the type's OID, not its
        # name text. Rewrite the inner call to the OID integer so the surrounding
        # ``::oid`` cast types (OID 26) and coerces it as an oid value; the cast
        # column keeps Postgres' output name (``pg_typeof``).
        if (
            isinstance(parent, exp.Cast)
            and parent.to is not None
            and typemap.type_tag_for_sql(parent.to) == "oid"
        ):
            tag = typemap.builtin_tag_for_name(name)
            type_oid = typemap.PG_OID.get(tag) if tag is not None else None
            if type_oid is not None:
                node.replace(exp.Literal.number(type_oid))
                if isinstance(parent.parent, exp.Select):
                    parent.replace(exp.alias_(parent.copy(), "pg_typeof"))
                continue
        replacement: exp.Expression = exp.Literal.string(name)
        if isinstance(parent, exp.Select):
            replacement = exp.alias_(replacement, "pg_typeof")
        node.replace(replacement)


def _arith_operand_tag(node: exp.Expression, resolve: Resolve) -> str | None:
    """The numeric tag an arithmetic operand contributes, or None if non-numeric.

    An unadorned decimal constant is ``numeric`` in Postgres (``1.5`` in SQL text
    is not a float8), which the Python-value inference can't see."""
    if isinstance(node, (exp.Paren, exp.Neg)) and node.this is not None:
        return _arith_operand_tag(node.this, resolve)
    if isinstance(node, exp.Literal) and not node.is_string:
        text = str(node.this).lower()
        if "." in text or "e" in text:
            return "numeric"
    tag = _infer_scalar_tag(node, resolve)
    return tag if tag in _NUMERIC_FAMILY else None


def _unify_numeric_tags(tags: list[str]) -> str | None:
    """Combine numeric operand tags per Postgres' promotion rules, or None when
    any tag is outside the numeric family (caller decides the fallback)."""
    if not tags or any(t not in _NUMERIC_FAMILY for t in tags):
        return None
    if all(t in _INT_TAG_ORDER for t in tags):
        return max(tags, key=lambda t: _INT_TAG_ORDER[t])
    if all(t == "float4" for t in tags):
        return "float4"
    if any(t in ("float4", "float8") for t in tags):
        return "float8"
    return "numeric"


_tag_memo: contextvars.ContextVar[dict | None] = contextvars.ContextVar("_tag_memo", default=None)


def _infer_scalar_tag(node: exp.Expression, resolve: Resolve) -> str:
    """Best-effort output type tag for a computed SELECT expression.

    Memoized per (node, resolve) for the duration of the outermost call: the
    helpers re-visit operand subtrees (numeric family probes, date/interval
    disambiguation), which is exponential on long arithmetic chains without a
    memo (a 20-term sqllogictest expression took ~0.5s; whole files timed out)."""
    memo = _tag_memo.get()
    if memo is None:
        token = _tag_memo.set({})
        try:
            return _infer_scalar_tag(node, resolve)
        finally:
            _tag_memo.reset(token)
    key = (id(node), id(resolve))
    hit = memo.get(key)
    if hit is not None:
        return hit[0]
    tag = _infer_scalar_tag_impl(node, resolve)
    memo[key] = (tag, node, resolve)  # node/resolve refs pin the ids
    return tag


def _infer_scalar_tag_impl(node: exp.Expression, resolve: Resolve) -> str:
    """The uncached body of ``_infer_scalar_tag``."""
    # Composite field access ``(col).field`` types as the field's declared type.
    composite_tag = _composite_field_tag(node, resolve)
    if composite_tag is not None:
        return composite_tag
    # ``array[x::inet, …]`` — a bare array constructor types as its elements'
    # array type when an element's tag is knowable (a cast or nested literal).
    if isinstance(node, exp.Array) and node.expressions:
        nested = next((e for e in node.expressions if isinstance(e, exp.Array)), None)
        if nested is not None:
            # ``ARRAY[ARRAY[1], …]`` — a multidimensional array keeps its BASE
            # array type: PG has ONE array oid per element type regardless of
            # dimensionality (the pgtest corpus reads the binary element oid).
            inner = _infer_scalar_tag(nested, resolve)
            if typemap.is_array_tag(inner):
                return inner
        first = next((e for e in node.expressions if isinstance(e, exp.Cast)), None)
        elem_tag = typemap.type_tag_for_sql(first.to) if first is not None else None
        if elem_tag and not typemap.is_array_tag(elem_tag) and f"{elem_tag}[]" in typemap.PG_OID:
            return f"{elem_tag}[]"
    # A user-defined function call (CREATE FUNCTION) types as its RETURNS type; the
    # catalog rides the planning ``_pipeline_subctx`` in the evaluated-select path.
    if isinstance(node, (exp.Anonymous, exp.Dot)):
        _sub = _pipeline_subctx.get()
        if _sub is not None and getattr(_sub, "catalog", None) is not None:
            _udf = _udf_lookup(node, _sub.catalog, _sub.db)
            if _udf is not None and _udf.get("return_tag"):
                return _udf["return_tag"]
    # A scalar subquery types as its single projected column — a plain inner
    # column reference adopts the inner table's declared tag (PG types scalar
    # subqueries statically; without this a datetime subquery wires as text).
    if isinstance(node, exp.Subquery) and isinstance(node.this, exp.Select):
        inner = node.this
        exprs = inner.expressions
        _sub = _pipeline_subctx.get()
        if len(exprs) == 1 and _sub is not None and getattr(_sub, "catalog", None) is not None:
            target = exprs[0].this if isinstance(exprs[0], exp.Alias) else exprs[0]
            tbl_node = inner.find(exp.Table)
            if isinstance(target, exp.Column) and tbl_node is not None:
                try:
                    tdef = _lookup_table_def(
                        _sub.catalog, _sub.db, tbl_node, getattr(_sub, "storage", None)
                    )
                except errors.SQLError:
                    tdef = None
                col = tdef.column(target.name) if tdef is not None else None
                if col is not None:
                    return col.type_tag
    # Range operators (@> / <@ / &&) over a range operand are boolean; a range
    # constructor / cast is the range type. (Non-range @> / <@ fall through to the
    # jsonb typing below.)
    if isinstance(
        node, (exp.ArrayContainsAll, exp.ArrayContainedBy, exp.ArrayOverlaps)
    ) and _has_range_operand(node, resolve):
        return "bool"
    if isinstance(node, exp.Anonymous) and str(node.this).lower() in typemap._RANGE_TAGS:
        return str(node.this).lower()
    # Multirange constructor (``int4multirange(...)``) -> the multirange type.
    if isinstance(node, exp.Anonymous) and str(node.this).lower() in typemap._MULTIRANGE_TAGS:
        return str(node.this).lower()
    # ``row(...)`` -> an anonymous record.
    if isinstance(node, exp.Anonymous) and str(node.this).lower() == "row":
        return "composite"
    # ``(a, b, …)`` parenthesized tuple — an anonymous record constructor.
    if isinstance(node, exp.Tuple):
        return "composite"
    # ``range_merge(a, b)`` -> the operands' range type.
    if isinstance(node, exp.Anonymous) and str(node.this).lower() == "range_merge":
        rtag = _range_tag_of(node.expressions, resolve)
        if rtag is not None:
            return rtag
    # ``-|-`` adjacency -> bool.
    if getattr(exp, "Adjacent", None) is not None and isinstance(node, exp.Adjacent):
        return "bool"
    # ``*`` / ``+`` / ``-`` over range operands -> the range type (intersection /
    # union / difference).
    if isinstance(node, (exp.Mul, exp.Add, exp.Sub)):
        rtag = _range_tag_of((node.this, node.expression), resolve)
        if rtag is not None:
            return rtag
    # ``lower(range)`` / ``upper(range)`` yield the range's element type.
    if isinstance(node, (exp.Lower, exp.Upper)) and node.this is not None:
        operand_tag = _infer_scalar_tag(node.this, resolve)
        if operand_tag in typemap._RANGE_TAGS:
            return ranges.RANGE_TYPES[operand_tag][0]
    # Interval literal / negation / arithmetic. ``interval ± interval`` and
    # ``interval * n`` -> interval; ``date ± interval`` -> the date type; and
    # ``timestamp - timestamp`` -> interval.
    if isinstance(node, exp.Interval):
        return "interval"
    if isinstance(node, exp.Neg) and isinstance(node.this, exp.Interval):
        return "interval"
    _iv_nodes = tuple(
        c
        for c in (
            getattr(exp, n, None)
            for n in ("MakeInterval", "JustifyDays", "JustifyHours", "JustifyInterval")
        )
        if c is not None
    )
    if _iv_nodes and isinstance(node, _iv_nodes):
        return "interval"
    # Date / time arithmetic (checked before the interval / numeric fallbacks).
    if isinstance(node, (exp.Add, exp.Sub)):
        _dt_tag = _date_arith_tag(node, resolve)
        if _dt_tag is not None:
            return _dt_tag
    # Money arithmetic: money ± money / money * number -> money; money / money ->
    # float8 (a ratio).
    if isinstance(node, (exp.Add, exp.Sub, exp.Mul, exp.Div)):
        lt_m = _infer_scalar_tag(node.this, resolve)
        rt_m = _infer_scalar_tag(node.expression, resolve)
        if "money" in (lt_m, rt_m):
            if isinstance(node, exp.Div) and lt_m == "money" and rt_m == "money":
                return "float8"
            return "money"
    if isinstance(node, (exp.Add, exp.Sub, exp.Mul, exp.Div)):
        _it = _interval_arith_tag(node, resolve)
        if _it is not None:
            return _it
    # Geometric operators: ``<->`` distance -> float8; ``@>`` / ``<@`` / ``&&``
    # over a geometry operand -> bool.
    if getattr(exp, "Distance", None) is not None and isinstance(node, exp.Distance):
        return "float8"
    if isinstance(
        node, (exp.ArrayContainsAll, exp.ArrayContainedBy, exp.ArrayOverlaps)
    ) and _has_geo_operand(node, resolve):
        return "bool"
    # hstore operators: ``@>`` / ``<@`` containment and ``?`` / ``?&`` / ``?|``
    # key-exists over an hstore operand -> bool; ``->`` lookup -> text; ``||``
    # merge -> hstore.
    if isinstance(node, (exp.ArrayContainsAll, exp.ArrayContainedBy)) and _has_hstore_operand(
        node, resolve
    ):
        return "bool"
    if isinstance(
        node, (exp.JSONBContains, exp.JSONBContainsAllTopKeys, exp.JSONBContainsAnyTopKeys)
    ) and _has_hstore_operand(node, resolve):
        return "bool"
    if isinstance(node, (exp.JSONExtract, exp.JSONExtractScalar)) and _has_hstore_operand(
        node, resolve
    ):
        return "text"
    # ``->`` / ``#>`` keep jsonb; ``->>`` / ``#>>`` return text.
    if isinstance(node, exp.JSONExtractScalar):
        return "text"
    if isinstance(node, exp.JSONExtract):
        return "json"
    if isinstance(node, exp.DPipe) and _has_hstore_operand(node, resolve):
        return "hstore"
    # Postgres array operators: ``@>`` / ``<@`` / ``&&`` over an array operand -> bool.
    if isinstance(
        node, (exp.ArrayContainsAll, exp.ArrayContainedBy, exp.ArrayOverlaps)
    ) and _has_array_operand(node, resolve):
        return "bool"
    # Network operators: ``<<`` / ``>>`` (subnet containment) and ``&&`` (overlap)
    # over a net operand -> bool. ``exp.Host`` (the ``host()`` function) -> text.
    if isinstance(
        node, (exp.BitwiseLeftShift, exp.BitwiseRightShift, exp.ArrayOverlaps)
    ) and _has_net_operand(node, resolve):
        return "bool"
    if getattr(exp, "Host", None) is not None and isinstance(node, exp.Host):
        return "text"
    # ``gen_random_uuid()`` (the dedicated ``exp.Uuid`` node) -> uuid.
    if getattr(exp, "Uuid", None) is not None and isinstance(node, exp.Uuid):
        return "uuid"
    # bytea: ``encode(bytea, fmt)`` -> text; ``decode(text, fmt)`` -> bytea; and
    # ``bytea || bytea`` -> bytea (the dedicated Encode/Decode nodes).
    if getattr(exp, "Encode", None) is not None and isinstance(node, exp.Encode):
        return "text"
    if getattr(exp, "Decode", None) is not None and isinstance(node, exp.Decode):
        return "bytea"
    # xml: ``xmlelement(...)`` (dedicated node) -> xml.
    if getattr(exp, "XMLElement", None) is not None and isinstance(node, exp.XMLElement):
        return "xml"
    if isinstance(node, exp.DPipe) and _has_bytea_operand(node, resolve):
        return "bytea"
    # A bit-string literal (``B'1010'``) types as varbit.
    if isinstance(node, exp.BitString):
        return "varbit"
    # Bit-string operators: ``&`` / ``|`` / ``#`` / ``~`` / ``<<`` / ``>>`` and
    # ``||`` over a bit operand -> varbit. (``<<`` / ``>>`` fall through here only
    # when the net check above didn't match.)
    if isinstance(
        node,
        (
            exp.BitwiseAnd,
            exp.BitwiseOr,
            exp.BitwiseXor,
            exp.BitwiseNot,
            exp.BitwiseLeftShift,
            exp.BitwiseRightShift,
            exp.DPipe,
        ),
    ) and _has_bit_operand(node, resolve):
        return "varbit"
    # ``array || array`` (or array || element) concatenation types as the
    # array operand's tag — text ``||`` stays below.
    if isinstance(node, exp.DPipe):
        for side in (node.this, node.expression):
            side_tag = _infer_scalar_tag(side, resolve)
            if typemap.is_array_tag(side_tag):
                return side_tag
    # Integer bitwise ``&`` / ``|`` / ``#`` / ``~`` (not the bit-string, net, or
    # concat forms handled above) -> int4.
    if isinstance(node, (exp.BitwiseAnd, exp.BitwiseOr, exp.BitwiseXor, exp.BitwiseNot)):
        return "int4"
    if getattr(exp, "Getbit", None) is not None and isinstance(node, exp.Getbit):
        return "int4"
    if getattr(exp, "BitLength", None) is not None and isinstance(node, exp.BitLength):
        return "int4"
    if isinstance(node, exp.Cast) and node.to is not None:
        _to = node.to.sql(dialect="postgres").lower().strip()
        if (
            _to in typemap._RANGE_TAGS
            or _to in typemap._MULTIRANGE_TAGS
            or _to in typemap._FTS_TAGS
            or _to in typemap._NET_TAGS
        ):
            return _to
        _mapped = typemap.type_tag_for_sql(node.to)
        if typemap.is_array_tag(_mapped) and _mapped in typemap.PG_OID:
            return _mapped
        if (
            _mapped in typemap._BIT_TAGS
            or _mapped
            in (
                "int2",
                "int4",
                "int8",
                "oid",
                "float4",
                "float8",
                "numeric",
                "bool",
                "interval",
                "timestamptz",
                "timestamp",
                "uuid",
                "date",
                "time",
                "timetz",
                "money",
                "bytea",
                "hstore",
                "citext",
                "xml",
                "json",
                "aclitem",
                "name",
                "char1",
                "jsonpath",
            )
            or _mapped in typemap._GEO_TAGS
        ):
            return _mapped
    if isinstance(node, exp.Paren):
        return _infer_scalar_tag(node.this, resolve)
    if isinstance(node, (exp.Literal, exp.Boolean, exp.Null, exp.Neg)):
        # A bare literal in the SELECT list (``SELECT 0 AS lvl``) must type from its
        # value, else an int rides the wire as text. An unadorned decimal constant
        # (``1.5``) is numeric in Postgres, not float8 — the Python float can't
        # carry that distinction, so check the literal text first.
        _lit_inner = node.this if isinstance(node, exp.Neg) else node
        if isinstance(_lit_inner, exp.Literal) and not _lit_inner.is_string:
            _lit_text = str(_lit_inner.this).lower()
            if "." in _lit_text or "e" in _lit_text:
                return "numeric"
        if isinstance(node, exp.Neg) and not isinstance(_lit_inner, (exp.Literal, exp.Null)):
            # ``- col`` / ``- expr``: numeric negation keeps its operand's tag
            # (_literal only extracts constants — it must not see a column).
            _neg_tag = _infer_scalar_tag(_lit_inner, resolve)
            return _neg_tag if _neg_tag in _NUMERIC_FAMILY else "numeric"
        return _infer_value_tag(_literal(node))
    if isinstance(node, exp.Case):
        # Type from the result branches (THEN values + ELSE), unified with the
        # same numeric rules as arithmetic; NULL branches don't vote.
        _branches = [i.args.get("true") for i in node.args.get("ifs", [])]
        if node.args.get("default") is not None:
            _branches.append(node.args["default"])
        _tags = [
            _infer_scalar_tag(b, resolve)
            for b in _branches
            if b is not None and not isinstance(b, exp.Null)
        ]
        return _unify_numeric_tags(_tags) or (_tags[0] if _tags else "text")
    if isinstance(node, exp.Array):
        # ``array[...]`` types from its first element (Postgres unifies elements;
        # the first drives the array OID here).
        _etag = _infer_scalar_tag(node.expressions[0], resolve) if node.expressions else "text"
        return f"{_etag}[]" if f"{_etag}[]" in typemap.PG_OID else "text[]"
    if isinstance(node, exp.Bracket) and node.expressions:
        # ``arr[i]`` yields the element type; ``arr[lo:hi]`` stays the array type.
        base_tag = _infer_scalar_tag(node.this, resolve)
        if isinstance(node.expressions[0], exp.Slice):
            return base_tag
        return typemap.array_element_tag(base_tag) if typemap.is_array_tag(base_tag) else base_tag
    if isinstance(node, exp.Window):
        func = node.this
        if isinstance(func, (exp.RowNumber, exp.Rank, exp.DenseRank, exp.Count, exp.Ntile)):
            return "int8"
        if isinstance(func, exp.Avg):
            return "float8"
        value_funcs = (
            exp.Sum,
            exp.Min,
            exp.Max,
            exp.Lag,
            exp.Lead,
            exp.FirstValue,
            exp.LastValue,
            exp.NthValue,
        )
        if isinstance(func, value_funcs) and func.this is not None:
            return _infer_scalar_tag(func.this, resolve)
        return "numeric"
    srf = _srf_of(node)
    if srf is not None:
        # jsonb_array_elements → json elements; jsonb_object_keys → text keys;
        # unnest(indkey/indclass) → attnum/opclass oid; generate_subscripts → ord.
        return {"jsonb_array_elements": "json", "jsonb_object_keys": "text"}.get(srf[0], "int4")
    # A boolean-producing expression (IS NOT NULL, comparisons, AND/OR) must type
    # as bool, not text — else its value rides the wire as the string 'f'/'t' and
    # a driver reads ``if row["x"]`` as truthy (SQLAlchemy's duplicates_constraint).
    if isinstance(node, _BOOL_EXPR_TYPES):
        return "bool"
    # jsonpath predicate operators: ``@?`` (JSONBPathExists) and ``@@``
    # (MatchAgainst) -> bool.
    _jp_names = ("JSONBPathExists", "MatchAgainst")
    _jsonpath_bool = tuple(c for c in (getattr(exp, n, None) for n in _jp_names) if c is not None)
    if _jsonpath_bool and isinstance(node, _jsonpath_bool):
        return "bool"
    # Date/time: extract/date_part -> numeric field; now() / current_timestamp ->
    # timestamptz; current_date -> date; current_time -> timetz; to_char -> text.
    # date_trunc preserves its argument's tz-ness (see below). Classes are looked
    # up by attribute because their availability varies across sqlglot.
    if getattr(exp, "CurrentDate", None) is not None and isinstance(node, exp.CurrentDate):
        return "date"
    if getattr(exp, "CurrentTime", None) is not None and isinstance(node, exp.CurrentTime):
        return "timetz"
    # ``date_trunc(unit, src)`` keeps the tz-ness of ``src`` (Postgres:
    # ``date_trunc(text, timestamptz) -> timestamptz``, ``… timestamp) -> timestamp``;
    # a ``date`` argument is cast to naive timestamp). An ``interval`` argument
    # truncates the interval and yields ``interval``. An argument whose type we
    # can't prove naive defaults to ``timestamptz`` (the historical behaviour).
    if getattr(exp, "TimestampTrunc", None) is not None and isinstance(node, exp.TimestampTrunc):
        arg_tag = _infer_scalar_tag(node.this, resolve) if node.this is not None else None
        if arg_tag == "interval":
            return "interval"
        if arg_tag in ("timestamp", "date"):
            return "timestamp"
        return "timestamptz"
    _ct = getattr(exp, "CurrentTimestamp", None)
    if _ct is not None and isinstance(node, _ct):
        return "timestamptz"
    if getattr(exp, "Extract", None) is not None and isinstance(node, exp.Extract):
        return "numeric"
    if getattr(exp, "TimeToStr", None) is not None and isinstance(node, exp.TimeToStr):
        return "text"
    # ``ts ± interval`` keeps the timestamp's type (the non-interval operand's).
    if isinstance(node, (exp.Add, exp.Sub)):
        left, right = node.this, node.expression
        if isinstance(right, exp.Interval):
            return _infer_scalar_tag(left, resolve)
        if isinstance(left, exp.Interval):
            return _infer_scalar_tag(right, resolve)
    if isinstance(node, (exp.Add, exp.Sub, exp.Mul, exp.Div, exp.Mod)):
        # Numeric operand rules: int op int stays integer (Postgres integer
        # division truncates, matching ``_pg_div``), floats widen per Postgres'
        # promotion table, numeric absorbs ints. Anything unrecognised keeps the
        # old blanket ``numeric``.
        _lt = _arith_operand_tag(node.this, resolve)
        _rt = _arith_operand_tag(node.expression, resolve) if node.expression is not None else None
        _unified = _unify_numeric_tags([t for t in (_lt, _rt) if t is not None])
        if _unified is not None:
            return _unified
        return "numeric"
    if isinstance(node, exp.Abs):
        # abs() keeps its operand's numeric type.
        _at = _arith_operand_tag(node.this, resolve) if node.this is not None else None
        return _at if _at in _NUMERIC_FAMILY else "numeric"
    if isinstance(node, (exp.Round, exp.Ceil, exp.Floor, exp.Pow)):
        return "numeric"
    # Transcendental / root functions produce double precision; ``trunc`` / ``sign``
    # / ``factorial`` stay exact numeric. Classes are looked up by attribute because
    # their availability varies across sqlglot versions.
    _float_names = ("Sqrt", "Cbrt", "Ln", "Log", "Exp", "Pi", "Degrees", "Radians")
    _float_math = tuple(c for c in (getattr(exp, n, None) for n in _float_names) if c is not None)
    if _float_math and isinstance(node, _float_math):
        return "float8"
    _num_math = tuple(
        c for c in (getattr(exp, n, None) for n in ("Trunc", "Sign", "Factorial")) if c is not None
    )
    if _num_math and isinstance(node, _num_math):
        return "numeric"
    if isinstance(
        node,
        (
            exp.DPipe,
            exp.Upper,
            exp.Lower,
            exp.Trim,
            exp.Substring,
            exp.Concat,
            exp.RegexpReplace,
            exp.SplitPart,
        ),
    ) or (getattr(exp, "Translate", None) is not None and isinstance(node, exp.Translate)):
        return "text"
    # String round-out functions -> text (lpad/rpad/left/right/repeat/reverse/
    # initcap/chr/overlay); ascii/strpos/position -> int4. Attribute lookup for
    # version tolerance.
    _text_str = tuple(
        c
        for c in (
            getattr(exp, n, None)
            for n in ("Pad", "Left", "Right", "Repeat", "Reverse", "Initcap", "Chr", "Overlay")
        )
        if c is not None
    )
    if _text_str and isinstance(node, _text_str):
        return "text"
    _int_str = tuple(
        c for c in (getattr(exp, n, None) for n in ("Ascii", "StrPosition")) if c is not None
    )
    if _int_str and isinstance(node, _int_str):
        return "int4"
    if isinstance(node, (exp.Length, exp.ArraySize, exp.ArrayPosition)) or (
        getattr(exp, "RegexpCount", None) is not None and isinstance(node, exp.RegexpCount)
    ):
        return "int4"
    if isinstance(node, exp.ArrayToString):
        return "text"
    if isinstance(node, (exp.ArrayAppend, exp.ArrayPrepend, exp.ArrayConcat, exp.ArrayRemove)):
        # Array-valued result — infer the array type from the array operand.
        return _infer_scalar_tag(node.this, resolve)
    if isinstance(node, (exp.Coalesce, exp.Greatest, exp.Least)):
        # Type from the first operand (its own tag, recursively).
        first = (
            node.this
            if node.this is not None
            else (node.expressions[0] if node.expressions else None)
        )
        return _infer_scalar_tag(first, resolve) if first is not None else "text"
    if isinstance(node, exp.Nullif):
        return _infer_scalar_tag(node.this, resolve)
    if isinstance(node, exp.JSONBDeleteAtPath):  # jsonb #- path -> jsonb
        return "json"
    if isinstance(node, (exp.Column, *_JSONB_CLASSES)):
        try:
            return _field(node, resolve)[1]
        except errors.SQLError:
            return "text"
    name = None
    if isinstance(node, exp.Dot) and isinstance(node.expression, exp.Anonymous):
        name = node.expression.name
    elif isinstance(node, exp.Anonymous):
        name = node.this if isinstance(node.this, str) else node.name
    if name is not None:
        fname = str(name).rsplit(".", 1)[-1].lower()
        if fname in (
            "json_build_object",
            "jsonb_build_object",
            "json_build_array",
            "jsonb_build_array",
            "jsonb_set",
            "jsonb_set_lax",
            "jsonb_insert",
            "jsonb_strip_nulls",
            "json_strip_nulls",
            "jsonb_path_query",
            "jsonb_path_query_array",
            "to_jsonb",
            "to_json",
            "row_to_json",
            "jsonb_agg",
            "json_agg",
            "jsonb_object_agg",
            "json_object_agg",
        ):
            return "json"
        if fname in (
            "jsonb_array_length",
            "json_array_length",
            "array_length",
            "cardinality",
            "array_ndims",
            "array_upper",
            "array_lower",
        ):
            return "int4"
        if fname == "array_dims":
            return "text"
        if fname in ("jsonb_path_exists", "jsonb_path_match"):
            return "bool"
        if fname in ("has_table_privilege", "has_column_privilege"):
            return "bool"
        # Advisory locks (#135): pg_try_* / pg_advisory_unlock* -> bool; the
        # void-returning pg_advisory_lock* fall through to the "text" default.
        if fname in (
            "pg_try_advisory_lock",
            "pg_try_advisory_lock_shared",
            "pg_try_advisory_xact_lock",
            "pg_try_advisory_xact_lock_shared",
            "pg_advisory_unlock",
            "pg_advisory_unlock_shared",
        ):
            return "bool"
        if fname == "isempty":
            return "bool"
        if fname == "to_tsvector":
            return "tsvector"
        if fname in (
            "to_tsquery",
            "plainto_tsquery",
            "phraseto_tsquery",
            "websearch_to_tsquery",
        ):
            return "tsquery"
        if fname in ("ts_rank", "ts_rank_cd"):
            return "float8"
        if fname == "ts_headline":
            return "text"
        # Network functions: masklen/family -> int4; network -> cidr; netmask/
        # broadcast/hostmask -> inet; host/abbrev -> text.
        if fname in ("masklen", "family"):
            return "int4"
        if fname == "network":
            return "cidr"
        if fname in ("netmask", "broadcast", "hostmask"):
            return "inet"
        if fname in ("host", "abbrev"):
            return "text"
        # Bit-string functions.
        if fname in ("bit_length", "octet_length", "get_bit"):
            return "int4"
        if fname == "set_bit":
            return "varbit"
        # bytea byte accessors: get_byte -> int4; set_byte -> bytea.
        if fname == "get_byte":
            return "int4"
        if fname == "set_byte":
            return "bytea"
        # hstore functions: akeys/avals -> text[]; hstore_to_json -> json;
        # hstore/delete -> hstore; defined -> bool.
        if fname in ("akeys", "avals"):
            return "text[]"
        if fname in ("hstore_to_json", "hstore_to_jsonb"):
            return "json"
        if fname in ("hstore", "delete"):
            return "hstore"
        if fname == "defined":
            return "bool"
        # xml functions: xmlforest/xmlconcat -> xml; xpath -> text[];
        # xml_is_well_formed -> bool.
        if fname in ("xmlforest", "xmlconcat"):
            return "xml"
        if fname == "xpath":
            return "text[]"
        if fname in ("xml_is_well_formed", "xml_is_well_formed_document"):
            return "bool"
        # Interval functions.
        if fname in ("make_interval", "justify_days", "justify_hours", "justify_interval", "age"):
            return "interval"
        # UUID generators.
        if fname in ("gen_random_uuid", "uuid_generate_v4", "uuid_generate_v1"):
            return "uuid"
        if fname in ("gcd", "lcm"):
            return "int8"
        if fname == "log10":
            return "float8"
    return "text"


def _build_evaluated_join(
    stmt: exp.Select,
    base: TableDef,
    amap: dict[str, tuple[str, TableDef]],
    resolve: Resolve,
    pipeline: list[dict[str, Any]],
    derived: list[DerivedTable],
) -> EvaluatedSelectPlan:
    out_columns: list[tuple[str, str]] = []
    out_enum_types: dict[int, str] = {}
    out_exprs: list[exp.Expression] = []
    alias_exprs: dict[str, exp.Expression] = {}
    names = _NameAllocator()
    for e in stmt.expressions:
        alias = e.alias if isinstance(e, exp.Alias) else None
        inner = e.this if isinstance(e, exp.Alias) else e
        if isinstance(inner, exp.Star):
            for a, (_role, tdef) in amap.items():
                for c in tdef.columns:
                    if c.enum_type is not None:
                        out_enum_types[len(out_columns)] = c.enum_type
                    out_columns.append((names.fresh(c.name), c.type_tag))
                    out_exprs.append(exp.column(c.name, table=a))
            continue
        if isinstance(inner, exp.Column):
            name = alias or _column_name(inner)
        else:
            name = alias or _cast_output_name(inner) or "?column?"
        src_col = _column_for_order_node(inner, amap)
        if src_col is not None and src_col.enum_type is not None:
            out_enum_types[len(out_columns)] = src_col.enum_type
        out_columns.append((names.fresh(name), _infer_scalar_tag(inner, resolve)))
        out_exprs.append(inner)
        if alias is not None:
            alias_exprs[alias] = inner

    # ORDER BY may name a SELECT output alias (``ORDER BY "TABLE_TYPE"`` in
    # pgjdbc's getTables) or an ordinal — Postgres resolves both to the output
    # expression, and ``resolve`` only knows input columns, so a computed
    # output alias must be substituted here or sorting raises 42703.
    order: list[tuple[exp.Expression, int, bool]] = []
    order_node = stmt.args.get("order")
    if order_node is not None:
        for o in order_node.expressions:
            term = o.this
            if isinstance(term, exp.Column) and not term.table and term.name in alias_exprs:
                term = alias_exprs[term.name]
            elif (
                isinstance(term, exp.Literal)
                and not term.is_string
                and str(term.name).isdigit()
                and 1 <= int(term.name) <= len(out_exprs)
            ):
                term = out_exprs[int(term.name) - 1]
            order.append((term, -1 if o.args.get("desc") else 1, _nulls_first(o)))
    limit, skip = _limit_skip(stmt)
    # A correlated / EXISTS WHERE wasn't pushed into the pipeline (see
    # ``_build_join_pipeline``); carry it for per-joined-row evaluation.
    where_node = stmt.args.get("where")
    residual = (
        where_node.this
        if where_node is not None
        and (where_needs_per_row(stmt) or not _join_where_lowerable(stmt, resolve))
        else None
    )
    don = _distinct_on(stmt)
    enum_orders = _evaluated_enum_orders(order, lambda node: _column_for_order_node(node, amap))
    return EvaluatedSelectPlan(
        base_collection=base.collection,
        base_filter={},
        pipeline=pipeline,
        out_columns=out_columns,
        out_enum_types=out_enum_types,
        out_exprs=out_exprs,
        resolve=resolve,
        order=order,
        distinct=bool(stmt.args.get("distinct")) and not don,
        limit=limit,
        skip=skip,
        derived=derived,
        where=residual,
        distinct_on=don,
        enum_orders=enum_orders,
        # A computed output in the list (which is what routed this join to the
        # evaluator) must not strip the base-column identity from its plain
        # siblings — they still name a real column of a real table.
        out_sources=[_source_table_attnum(e, amap) for e in out_exprs],
    )


def _append_join_tail(
    pipeline: list[dict[str, Any]],
    stmt: exp.Select,
    resolve: Resolve,
    project: dict[str, Any],
    out_columns: list[tuple[str, str]],
    amap: dict[str, tuple[str, TableDef]] | None = None,
) -> None:
    """Project, optionally dedup (DISTINCT), then sort/skip/limit for a join.

    ORDER BY may reference a column that isn't in the SELECT list (legal in
    Postgres for a non-DISTINCT query): such a column is carried as a hidden
    projected field, sorted on, then dropped by a final projection. With
    DISTINCT the ordering must be by a selected output column (Postgres' rule).
    """
    out_names = {n for n, _ in out_columns}
    distinct = bool(stmt.args.get("distinct"))
    order = stmt.args.get("order")
    terms: list[tuple[str, int, bool]] = []
    enum_labels: dict[str, list[str]] = {}
    hidden: list[str] = []
    if order is not None:
        for o in order.expressions:
            direction = -1 if o.args.get("desc") else 1
            name = _column_name(o.this)
            if name in out_names:
                key = name
            elif distinct:
                raise errors.undefined_column(name)
            else:
                path, _ = resolve(o.this)
                key = f"__ord_{len(hidden)}"
                project[key] = f"${path}"
                hidden.append(key)
            terms.append((key, direction, _nulls_first(o)))
            if amap is not None:
                labels = _enum_labels_for_column(_column_for_order_node(o.this, amap))
                if labels is not None:
                    enum_labels[key] = labels
    pipeline.append({"$project": project})
    if distinct:
        _append_distinct(pipeline, out_columns)
    _emit_pipeline_sort(pipeline, terms, enum_labels)
    limit, skip = _limit_skip(stmt)
    if skip:
        pipeline.append({"$skip": skip})
    if limit:
        pipeline.append({"$limit": limit})
    if hidden:
        pipeline.append({"$project": {**{n: 1 for n in out_names}, "_id": 0}})


def _append_distinct(pipeline: list[dict[str, Any]], out_columns: list[tuple[str, str]]) -> None:
    """Append a dedup stage: group by every projected column, then re-project.

    Runs after the `$project` that produces the output columns, so it dedups on
    exactly the selected values (SQL ``DISTINCT`` semantics).
    """
    names = [n for n, _ in out_columns]
    group_id = {n: f"${n}" for n in names}
    project: dict[str, Any] = {"_id": 0}
    for n in names:
        project[n] = f"$_id.{n}"
    pipeline.append({"$group": {"_id": group_id}})
    pipeline.append({"$project": project})


def _resolve_order_output(
    node: exp.Expression,
    stmt: exp.Expression,
    out_columns: list[tuple[str, str]],
    order_aggs: dict[str, str] | None = None,
) -> str:
    """Resolve a pipeline ORDER BY term to an output column name. Handles a
    positional reference (``ORDER BY 2`` → the 2nd select item), an expression that
    matches a SELECT-list item (``ORDER BY count(*)`` when ``count(*)`` is selected —
    select items and ``out_columns`` are 1:1 in order on the pipeline paths), an
    aggregate that is *not* selected but registered as a hidden accumulator
    (``order_aggs``, keyed by the term's SQL), and a plain column name. Postgres
    resolves ORDER BY against output columns like this."""
    if isinstance(node, exp.Literal) and node.is_int:
        idx = int(node.this)
        if not 1 <= idx <= len(out_columns):
            raise errors.SQLError("42P10", f"ORDER BY position {idx} is not in select list")
        return out_columns[idx - 1][0]
    if not isinstance(node, exp.Column):
        target = node.sql()
        selects = stmt.expressions
        for i, sel in enumerate(selects):
            inner = sel.this if isinstance(sel, exp.Alias) else sel
            if i < len(out_columns) and inner.sql() == target:
                return out_columns[i][0]
        if order_aggs is not None and target in order_aggs:
            return order_aggs[target]
    return _column_name(node)


def _append_sort_limit(
    pipeline: list[dict[str, Any]],
    stmt: exp.Expression,
    out_columns: list[tuple[str, str]],
    table: TableDef | None = None,
    amap: dict[str, tuple[str, TableDef]] | None = None,
    order_aggs: dict[str, str] | None = None,
) -> None:
    valid_names = {n for n, _ in out_columns}
    if order_aggs:
        valid_names |= set(order_aggs.values())
    order = stmt.args.get("order")
    if order is not None:
        terms: list[tuple[str, int, bool]] = []
        enum_labels: dict[str, list[str]] = {}
        for o in order.expressions:
            col = _resolve_order_output(o.this, stmt, out_columns, order_aggs)
            if col not in valid_names:
                raise errors.undefined_column(col)
            terms.append((col, -1 if o.args.get("desc") else 1, _nulls_first(o)))
            # Resolve the source column (single-table via `table`, or across a
            # join's alias map) so an enum column sorts by its declared order. Only
            # a plain-column ORDER BY term has a source column for enum ordering.
            if isinstance(o.this, exp.Column):
                src = _column_for_order_node(o.this, amap) if amap is not None else None
                if src is None and table is not None:
                    src = table.column(col)
                labels = _enum_labels_for_column(src)
                if labels is not None:
                    enum_labels[col] = labels
        _emit_pipeline_sort(pipeline, terms, enum_labels)
    limit, skip = _limit_skip(stmt)
    if skip:
        pipeline.append({"$skip": skip})
    if limit:
        pipeline.append({"$limit": limit})


def _selected_sqls(stmt: exp.Expression) -> set[str]:
    """The SQL text of each SELECT item's underlying expression (alias stripped) —
    used to tell whether an ORDER BY aggregate is already in the select list."""
    out: set[str] = set()
    for e in stmt.expressions:
        inner = e.this if isinstance(e, exp.Alias) else e
        out.add(inner.sql())
    return out


def _register_orderby_aggs_single(
    stmt: exp.Expression,
    table: TableDef,
    accumulators: dict[str, Any],
    reductions: dict[str, Any],
    project: dict[str, Any],
    names: _NameAllocator,
) -> dict[str, str]:
    """Register a hidden ``$group`` accumulator for each ORDER BY aggregate that is
    *not* in the select list (single-table GROUP BY) — ``SELECT dept … GROUP BY dept
    ORDER BY sum(sal) DESC``. The accumulator is projected so the ``$sort`` can reach
    it, but it stays out of ``out_columns`` so the executor drops it from the output.
    Returns ``{order_by_term_sql: hidden_field_name}``."""
    order = stmt.args.get("order")
    if order is None:
        return {}
    selected = _selected_sqls(stmt)
    hidden: dict[str, str] = {}
    for o in order.expressions:
        node = o.this
        key = node.sql()
        if key in selected or key in hidden:
            continue  # already a select item (b210) or already registered here
        agg = _aggregate_of(node)
        if agg is None:
            continue  # positional / plain column / non-aggregate — resolved elsewhere
        func, col, distinct = agg
        if distinct and func in _DISTINCT_FUNCS:
            if col is None:
                raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
            fname, _ = _register_distinct_agg(
                func,
                table.field_for(col),
                table.type_for(col),
                None,
                names,
                accumulators,
                reductions,
            )
        else:
            acc, _ = _accumulator(func, col, table, None)
            fname = names.fresh("__ob")
            accumulators[fname] = acc
        project[fname] = f"${fname}"
        hidden[key] = fname
    return hidden


def _register_orderby_aggs_join(
    stmt: exp.Expression,
    resolve: Resolve,
    accumulators: dict[str, Any],
    reductions: dict[str, Any],
    project: dict[str, Any],
    names: _NameAllocator,
) -> dict[str, str]:
    """``_register_orderby_aggs_single`` for the JOIN + GROUP BY path — the aggregate
    argument lowers through the join ``resolve`` instead of a ``TableDef``."""
    order = stmt.args.get("order")
    if order is None:
        return {}
    selected = _selected_sqls(stmt)
    hidden: dict[str, str] = {}
    for o in order.expressions:
        node = o.this
        key = node.sql()
        if key in selected or key in hidden:
            continue
        agg = _join_aggregate_of(node)
        if agg is None:
            continue
        func, arg, distinct = agg
        if distinct and func in _DISTINCT_FUNCS:
            if arg is None:
                raise errors.feature_not_supported(f"{func}(DISTINCT *) is not supported")
            path, tag = resolve(arg)
            fname, _ = _register_distinct_agg(
                func, path, tag, None, names, accumulators, reductions
            )
        else:
            acc, _ = _join_accumulator(func, arg, resolve, None)
            fname = names.fresh("__ob")
            accumulators[fname] = acc
        project[fname] = f"${fname}"
        hidden[key] = fname
    return hidden


def _normalize_params(sql: str) -> str:
    """Space-pad ``$N`` placeholders so sqlglot doesn't misread ``$1,$2``.

    sqlglot's Postgres tokenizer treats ``$1,$2`` (adjacent, no spaces — what
    psycopg / pg8000 emit) as the start of a dollar-quoted string. A ``$``
    followed by digits is unambiguously a bind parameter (Postgres dollar-quote
    tags can't begin with a digit), so we append a space after each one. String
    literals are skipped so a ``'$1'`` inside data is left untouched.
    """
    if "$" not in sql:
        return sql
    out: list[str] = []
    i, n, in_str = 0, len(sql), False
    while i < n:
        ch = sql[i]
        if in_str:
            out.append(ch)
            if ch == "'":
                if i + 1 < n and sql[i + 1] == "'":  # '' escape
                    out.append("'")
                    i += 2
                    continue
                in_str = False
            i += 1
            continue
        if ch == "'":
            in_str = True
            out.append(ch)
            i += 1
            continue
        if ch == "$" and i + 1 < n and sql[i + 1].isdigit():
            j = i + 1
            while j < n and sql[j].isdigit():
                j += 1
            out.append(sql[i:j])
            out.append(" ")
            i = j
            continue
        out.append(ch)
        i += 1
    return "".join(out)


_RELEASE_SAVEPOINT_RE = re.compile(r"(?i)\brelease\s+savepoint\b")
# sqlglot's Postgres dialect can't parse an ``oid`` array column type (``oid[]``
# / ``"oid"[]``) — the OID keyword refuses the array suffix. Rewrite the element
# name to a quoted spelling that parses as a user-defined type array and
# resolves back to the ``oid`` tag via ``typemap._REGTYPE_SPELLINGS``. Applied
# only to CREATE TABLE statements (see ``parse``).
_OID_ARRAY_RE = re.compile(r'(?i)("?)\boid\1(\s*\[\s*\])')
_CREATE_TABLE_RE = re.compile(r"(?i)^\s*create\s+(?:\w+\s+)*?table\b")
# sqlglot's Postgres dialect can't parse ``MOVE`` (cursor positioning) at all, so
# a lone MOVE statement is hand-built into the same ``Command`` shape FETCH gets.
_MOVE_RE = re.compile(r"^\s*MOVE\b\s*(?P<tail>.*?)\s*;?\s*$", re.IGNORECASE | re.DOTALL)
#: Placeholder substituted for a ``COMMENT ON … IS NULL`` (comment removal);
#: ``executor.execute_comment`` reads it back as "remove the comment".
UNCOMMENT_SENTINEL = "\x00__secantus_uncomment__"
# Only a whole ``COMMENT ON … IS NULL`` statement — anchored so a query's
# ``WHERE x IS NULL`` is never touched.
_COMMENT_NULL_RE = re.compile(r"(?is)^(\s*COMMENT\s+ON\b.*\bIS\s+)NULL(\s*;?\s*)$")
_CREATE_FUNCTION_RE = re.compile(r"\bcreate\s+(?:or\s+replace\s+)?function\b", re.I)
#: CREATE [OR REPLACE] PROCEDURE — routed to a Command so the engine's regex
#: parser handles it (sqlglot rejects the ``a INOUT int`` argmode syntax).
_CREATE_PROCEDURE_RE = re.compile(r"(?is)^\s*create\s+(?:or\s+replace\s+)?procedure\b")
_DROP_PROCEDURE_RE = re.compile(r"(?is)^\s*drop\s+procedure\b")
_RETURNS_TRIGGER_RE = re.compile(r"(\breturns\s+)trigger\b", re.I)

# sqlglot parses COPY's options only in the ``WITH (…)`` spelling; the bare
# ``COPY … TO STDOUT (FORMAT csv)`` form (what psycopg emits) needs the WITH
# inserted. Anchored on the STDIN/STDOUT target and a known option keyword so a
# parenthesis inside the query part of ``COPY (query) TO STDOUT`` isn't touched.
_COPY_BARE_OPTIONS_RE = re.compile(
    r"(?is)^(\s*copy\b.*?\b(?:to\s+stdout|from\s+stdin))\s*"
    r"\((?=\s*(?:format|header|delimiter|null|quote|escape|encoding|freeze|force))"
)


# A ``::numeric(p,-s)`` cast — the only spot Postgres syntax allows a negative
# scale. Anchored on the ``::`` cast so a matching text inside a string literal
# isn't touched.
def _rewrite_quoted_char_types(sql: str) -> str:
    """Replace the QUOTED ``"char"`` type spelling (PG's internal one-byte
    type, oid 18) with the ``pg_char_1`` sentinel before parse — sqlglot
    collapses the quoted spelling into plain CHAR in both cast and column-def
    positions, losing the identity. Token-context aware so a ``"char"``
    column NAME, alias, or string literal is never touched: rewrites after
    ``::``, after ``AS`` only inside a CAST(...), and after an identifier in a
    CREATE/ALTER statement (a column def's type position)."""
    from sqlglot.tokens import TokenType

    try:
        tokens = sqlglot.tokenize(sql, read="postgres")
    except Exception:
        return sql
    is_ddl = bool(tokens) and tokens[0].token_type in (TokenType.CREATE, TokenType.ALTER)
    spans: list[tuple[int, int]] = []
    for i, tok in enumerate(tokens):
        if tok.text != "char" or sql[tok.start : tok.end + 1] != '"char"':
            continue
        if i == 0:
            continue
        ptt = tokens[i - 1].token_type
        if ptt == TokenType.DCOLON:
            spans.append((tok.start, tok.end + 1))
        elif ptt == TokenType.ALIAS:
            # ``CAST(expr AS "char")`` vs an ``AS "char"`` output alias: walk
            # back to the paren opening this depth and require CAST before it.
            depth = 0
            for j in range(i - 2, -1, -1):
                jtt = tokens[j].token_type
                if jtt == TokenType.R_PAREN:
                    depth += 1
                elif jtt == TokenType.L_PAREN:
                    if depth == 0:
                        if j > 0 and tokens[j - 1].text.upper() in ("CAST", "TRY_CAST"):
                            spans.append((tok.start, tok.end + 1))
                        break
                    depth -= 1
        elif is_ddl and ptt in (TokenType.VAR, TokenType.IDENTIFIER):
            # ``CREATE TABLE t (c "char" ...)`` — the type follows the column
            # name. A quoted "char" COLUMN name follows ``(`` or ``,`` instead.
            spans.append((tok.start, tok.end + 1))
    for start, end in reversed(spans):
        sql = sql[:start] + "pg_char_1" + sql[end:]
    return sql


#: crdb's ``ADD COLUMN ... NOT VISIBLE`` modifier (the pgtest copy corpus
#: uses it in an unmarked stanza) — sqlglot can't parse it; the column is
#: added as a normal visible column.
_NOT_VISIBLE_RE = re.compile(r"(\bADD\s+COLUMN\s+[^,']*?)\s+NOT\s+VISIBLE\b", re.I)

_NEGSCALE_RE = re.compile(r"(::\s*(?:numeric|decimal)\s*\(\s*\d+\s*,\s*)-\s*(\d+)(\s*\))", re.I)

# ``BEGIN`` / ``START TRANSACTION`` with transaction characteristics — sqlglot
# parses some spellings (``BEGIN ISOLATION LEVEL x``) but not others (``BEGIN
# READ ONLY``, the comma-separated ``START TRANSACTION a, b``). The tail is
# pure keywords (letters/commas/whitespace), so a compound statement never
# matches and falls through to sqlglot.
_BEGIN_CHARACTERISTICS_RE = re.compile(
    r"^\s*(?:BEGIN|START\s+TRANSACTION)(?:\s+(?:WORK|TRANSACTION))?"
    r"(?:\s+(?P<tail>(?:ISOLATION|READ|NOT|DEFERRABLE)[A-Za-z,\s]*?))?\s*;?\s*$",
    re.IGNORECASE,
)


def _split_top_level_semicolons(sql: str) -> list[str]:
    """Split a multi-statement string on semicolons OUTSIDE quotes, dollar
    quotes and comments — the batch fallback when sqlglot rejects the string
    as a whole but the individual statements parse (``BEGIN READ ONLY``
    mid-batch takes the regex path only per-statement)."""
    parts: list[str] = []
    buf: list[str] = []
    i, n = 0, len(sql)
    while i < n:
        c = sql[i]
        if c == "'":
            j = i + 1
            while j < n:
                if sql[j] == "'":
                    if j + 1 < n and sql[j + 1] == "'":
                        j += 2
                        continue
                    break
                j += 1
            buf.append(sql[i : j + 1])
            i = j + 1
            continue
        if c == '"':
            j = sql.find('"', i + 1)
            j = n - 1 if j < 0 else j
            buf.append(sql[i : j + 1])
            i = j + 1
            continue
        if c == "$":
            m = re.match(r"\$[A-Za-z_0-9]*\$", sql[i:])
            if m is not None:
                tag = m.group(0)
                end = sql.find(tag, i + len(tag))
                end = n if end < 0 else end + len(tag)
                buf.append(sql[i:end])
                i = end
                continue
        if c == "-" and sql[i : i + 2] == "--":
            j = sql.find("\n", i)
            j = n if j < 0 else j + 1
            buf.append(sql[i:j])
            i = j
            continue
        if c == "/" and sql[i : i + 2] == "/*":
            j = sql.find("*/", i + 2)
            j = n if j < 0 else j + 2
            buf.append(sql[i:j])
            i = j
            continue
        if c == ";":
            part = "".join(buf).strip()
            if part:
                parts.append(part)
            buf = []
            i += 1
            continue
        buf.append(c)
        i += 1
    part = "".join(buf).strip()
    if part:
        parts.append(part)
    return parts


# ``SET TRANSACTION <characteristics>`` — the keyword tail, like BEGIN's.
_SET_TRANSACTION_RE = re.compile(
    r"^\s*SET\s+TRANSACTION\s+(?P<tail>(?:ISOLATION|READ|NOT|DEFERRABLE)[A-Za-z,\s]*?)\s*;?\s*$",
    re.IGNORECASE,
)

# A LISTEN / NOTIFY / UNLISTEN statement head (single statement only).
_MULTI_DROP_TABLE_RE = re.compile(
    r"(?is)^\s*DROP\s+TABLE\s+(?P<if_exists>IF\s+EXISTS\s+)?"
    r"(?P<names>[^;]+?)\s*;?\s*$"
)
_PUBSUB_HEAD_RE = re.compile(r"^\s*(?:LISTEN|UNLISTEN|NOTIFY)\b[^;]*;?\s*$", re.IGNORECASE)

# ``COMMENT ON CONSTRAINT c ON t IS '…'`` — sqlglot's Comment node can't
# express the two-name form; carry the raw text as a Command the engine's
# constraint-comment handler parses with this same regex.
COMMENT_CONSTRAINT_RE = re.compile(
    r"(?is)^\s*COMMENT\s+ON\s+CONSTRAINT\s+(?P<name>\"[^\"]+\"|[\w$]+)\s+ON\s+"
    r"(?P<table>(?:\"[^\"]+\"|[\w$]+)(?:\.(?:\"[^\"]+\"|[\w$]+))?)\s+IS\s+"
    r"(?P<value>'(?:[^']|'')*'|NULL)\s*;?\s*$"
)

# ``DO $tag$ body $tag$ [LANGUAGE plpgsql]`` — the dollar-quoted body.
_DO_BLOCK_RE = re.compile(
    r"(?is)^\s*DO\s+(?:LANGUAGE\s+\w+\s+)?\$(?P<tag>[A-Za-z_]*)\$(?P<body>.*?)\$(?P=tag)\$"
    r"(?:\s+LANGUAGE\s+\w+)?\s*;?\s*$"
)


#: Reject a statement string longer than this before handing it to sqlglot. 1 MB
#: is far larger than any real query yet small enough that a flood of oversized
#: statements can't pin the parser.
MAX_SQL_LENGTH = 16_000_000


def _resolve_group_by_ordinals(root: exp.Expression) -> None:
    """Rewrite ``GROUP BY 1, 2`` positional references to the select-list
    expressions they name, like Postgres' parse analysis (an alias target
    groups by its inner expression). An out-of-range ordinal is 42P10."""
    for sel in root.find_all(exp.Select):
        group = sel.args.get("group")
        if group is None:
            continue
        exprs = sel.expressions
        new: list[exp.Expression] = []
        for g in group.expressions:
            if isinstance(g, exp.Literal) and not g.is_string and str(g.this).isdigit():
                i = int(g.this)
                if not 1 <= i <= len(exprs):
                    raise errors.SQLError("42P10", f"GROUP BY position {i} is not in select list")
                target = exprs[i - 1]
                inner = target.this if isinstance(target, exp.Alias) else target
                new.append(inner.copy())
            else:
                new.append(g)
        group.set("expressions", new)


_ESTRING_SIMPLE = {"b": "\b", "f": "\f", "n": "\n", "r": "\r", "t": "\t"}


#: A dollar-quote tag: ``$$`` or ``$tag$`` where the tag starts with a letter /
#: underscore and may continue with digits (``$A0$``, ``$_0$``) — PG's rule.
_DOLLAR_TAG_RE = re.compile(r"\$(?:[A-Za-z_][A-Za-z0-9_]*)?\$")


def _strip_nested_block_comments(sql: str) -> str:
    """Strip block comments when (and only when) any of them NEST.

    PostgreSQL nests ``/* /* */ */``; sqlglot's tokenizer does not, so a nested
    comment mis-tokenizes into stray operators (``/*/*/*/**/*/*/*/`` became
    ``* *``). Non-nested comments are left for sqlglot (it keeps them as
    trivia). Strings, quoted identifiers, dollar-quoted bodies, and line
    comments are skipped, mirroring ``_decode_estrings``'s scanner."""
    out: list[str] = []
    i, n = 0, len(sql)
    nested = False
    while i < n:
        c = sql[i]
        if c == "-" and sql[i : i + 2] == "--":  # line comment
            j = sql.find("\n", i)
            j = n if j == -1 else j + 1
            out.append(sql[i:j])
            i = j
        elif c == "/" and sql[i : i + 2] == "/*":  # block comment — count depth
            depth, j = 1, i + 2
            while j < n and depth:
                if sql[j : j + 2] == "/*":
                    depth += 1
                    nested = True
                    j += 2
                elif sql[j : j + 2] == "*/":
                    depth -= 1
                    j += 2
                else:
                    j += 1
            out.append(" ")
            i = j
        elif c == '"' or c == "'":  # quoted identifier / plain literal
            q = c
            j = i + 1
            while j < n:
                if sql[j] == q:
                    if sql[j + 1 : j + 2] == q:
                        j += 2
                        continue
                    j += 1
                    break
                j += 1
            out.append(sql[i:j])
            i = j
        elif (
            c == "$"
            and (m := _DOLLAR_TAG_RE.match(sql, i))
            and not (i and (sql[i - 1].isalnum() or sql[i - 1] in "_$"))
        ):  # dollar-quoted body
            tag = m.group(0)
            j = sql.find(tag, i + len(tag))
            j = n if j == -1 else j + len(tag)
            out.append(sql[i:j])
            i = j
        else:
            out.append(c)
            i += 1
    return "".join(out) if nested else sql


def decode_nonstandard_strings(sql: str) -> str:
    """``standard_conforming_strings = off``: every plain ``'…'`` literal
    treats backslash as an escape character, exactly like ``E'…'``. Rewrite
    them all (and E-strings) to standard literals before parsing. Called by the
    engine/wire layers when the session GUC is off — ``parse`` itself is
    session-independent (and cached), so the transform happens on the text."""
    return _decode_estrings(sql, nonstandard=True)


def _decode_estrings(sql: str, *, nonstandard: bool = False) -> str:
    """Rewrite every ``E'…'`` escape-string literal into an equivalent standard
    literal BEFORE sqlglot parses.

    sqlglot's tokenizer half-decodes E-strings (simple escapes and ``\\\\``),
    which loses the distinction between ``E'\\x5c'`` (a backslash byte) and
    ``E'\\\\x5c'`` (the four characters ``\\x5c``) — decoding here with PG's
    full escape grammar (simple controls, 1-3 digit octal, ``\\xHH``,
    ``\\uXXXX``/``\\UXXXXXXXX``, any other escaped char standing for itself)
    keeps the exact value. Comments, quoted identifiers, dollar-quoted bodies,
    and plain literals pass through untouched."""
    out: list[str] = []
    i, n = 0, len(sql)
    while i < n:
        c = sql[i]
        if c == "-" and sql[i : i + 2] == "--":  # line comment
            j = sql.find("\n", i)
            j = n if j == -1 else j + 1
            out.append(sql[i:j])
            i = j
        elif c == "/" and sql[i : i + 2] == "/*":  # block comment
            j = sql.find("*/", i + 2)
            j = n if j == -1 else j + 2
            out.append(sql[i:j])
            i = j
        elif c == '"':  # quoted identifier
            j = i + 1
            while j < n:
                if sql[j] == '"':
                    if sql[j + 1 : j + 2] == '"':
                        j += 2
                        continue
                    j += 1
                    break
                j += 1
            out.append(sql[i:j])
            i = j
        elif (
            c == "$"
            and (m := _DOLLAR_TAG_RE.match(sql, i))
            and not (i and (sql[i - 1].isalnum() or sql[i - 1] in "_$"))
        ):  # dollar quote (tag may carry digits after the first char: $A0$, $_0$)
            tag = m.group(0)
            j = sql.find(tag, i + len(tag))
            j = n if j == -1 else j + len(tag)
            out.append(sql[i:j])
            i = j
        elif c == "'":  # plain literal — skip over '' doubling
            if nonstandard:
                # standard_conforming_strings=off: backslash escapes apply in
                # plain literals too — decode with the E-string grammar.
                value, i = _consume_estring(sql, i + 1)
                out.append("'" + value.replace("'", "''") + "'")
                continue
            j = i + 1
            while j < n:
                if sql[j] == "'":
                    if sql[j + 1 : j + 2] == "'":
                        j += 2
                        continue
                    j += 1
                    break
                j += 1
            out.append(sql[i:j])
            i = j
        elif (
            c in "eE"
            and sql[i + 1 : i + 2] == "'"
            and not (i and (sql[i - 1].isalnum() or sql[i - 1] in "_$\"'"))
        ):
            value, i = _consume_estring(sql, i + 2)
            out.append("'" + value.replace("'", "''") + "'")
        else:
            out.append(c)
            i += 1
    return "".join(out)


def _consume_estring(sql: str, i: int) -> tuple[str, int]:
    """Decode the body of an ``E'…'`` literal starting after the opening quote;
    returns ``(decoded_value, index_after_closing_quote)``."""
    n = len(sql)
    buf: list[str] = []
    while i < n:
        c = sql[i]
        if c == "'":
            if sql[i + 1 : i + 2] == "'":  # doubled quote
                buf.append("'")
                i += 2
                continue
            return "".join(buf), i + 1
        if c != "\\" or i + 1 >= n:
            buf.append(c)
            i += 1
            continue
        nxt = sql[i + 1]
        if nxt in _ESTRING_SIMPLE:
            buf.append(_ESTRING_SIMPLE[nxt])
            i += 2
        elif nxt in "01234567":
            j = i + 1
            while j < min(i + 4, n) and sql[j] in "01234567":
                j += 1
            buf.append(chr(int(sql[i + 1 : j], 8)))
            i = j
        elif nxt in "xX":
            j = i + 2
            while j < min(i + 4, n) and sql[j] in "0123456789abcdefABCDEF":
                j += 1
            if j == i + 2:
                buf.append(nxt)
                i += 2
            else:
                buf.append(chr(int(sql[i + 2 : j], 16)))
                i = j
        elif nxt in "uU":
            width = 4 if nxt == "u" else 8
            digits = sql[i + 2 : i + 2 + width]
            if len(digits) == width and all(d in "0123456789abcdefABCDEF" for d in digits):
                buf.append(chr(int(digits, 16)))
                i += 2 + width
            else:
                raise errors.SQLError("22025", "invalid Unicode escape value")
        else:
            buf.append(nxt)
            i += 2
    return "".join(buf), i  # unterminated — sqlglot will raise the syntax error


def _fold_unquoted_identifiers(stmt: exp.Expression) -> None:
    """Lower-case every UNQUOTED identifier, in place.

    Postgres folds an unquoted identifier to lower case and keeps a quoted one
    exactly as written, so ``AS TABLE_NAME`` and a later ``r.table_name`` name
    the same column while ``"TABLE_NAME"`` is a different one. We compared them
    exactly, so the two spellings were two different names — every generated or
    SQL-standard-uppercase statement broke on it, while code that wrote and
    read one spelling never noticed.

    Doing it here, once, right after the parse means every downstream
    consumer — alias resolution, catalog lookups, the column resolver — sees
    one canonical spelling and needs no case logic of its own. String literals
    are ``Literal`` nodes, not ``Identifier``, so their contents are untouched.
    """
    for ident in stmt.find_all(exp.Identifier):
        if not ident.args.get("quoted") and isinstance(ident.this, str):
            ident.set("this", ident.this.lower())


#: Parse cache: SQL text -> pristine AST statements, handed out as copies
#: (``exp.Expression.copy()`` measures 3-4x cheaper than a parse). Entries are
#: cached on SECOND sight — the first occurrence only leaves a marker — so
#: workloads of mostly-unique statements (sqllogictest's corpus, inline-literal
#: DML) pay nothing beyond a dict probe, while repeated text (per-connection
#: re-Parse of the same prepared statements, fixture DDL repeated across
#: thousands of tests) hits from the second occurrence on. The cached trees
#: never leave the cache uncopied, so downstream mutation cannot poison them.
_PARSE_CACHE_MAX = 4096
_PARSE_CACHE: OrderedDict[str, list[exp.Expression] | None] = OrderedDict()
_PARSE_CACHE_LOCK = threading.Lock()


def parse(sql: str) -> list[exp.Expression]:
    """Parse a (possibly multi-statement) SQL string into AST statements.

    Cached: repeated text returns copies of the cached trees (see
    ``_PARSE_CACHE``); semantics are identical to an uncached parse because
    ``_parse_uncached`` is a pure function of the text."""
    with _PARSE_CACHE_LOCK:
        entry = _PARSE_CACHE.get(sql)
        if entry is not None:
            _PARSE_CACHE.move_to_end(sql)
    if entry is not None:
        return [s.copy() for s in entry]
    stmts = _parse_uncached(sql)
    seen_before = False
    with _PARSE_CACHE_LOCK:
        seen_before = sql in _PARSE_CACHE
        if not seen_before:
            _PARSE_CACHE[sql] = None  # first sight: mark, don't pay the copy
            _PARSE_CACHE.move_to_end(sql)
            while len(_PARSE_CACHE) > _PARSE_CACHE_MAX:
                _PARSE_CACHE.popitem(last=False)
    if seen_before:
        pristine = [s.copy() for s in stmts]  # copy OUTSIDE the lock
        with _PARSE_CACHE_LOCK:
            _PARSE_CACHE[sql] = pristine
            _PARSE_CACHE.move_to_end(sql)
            while len(_PARSE_CACHE) > _PARSE_CACHE_MAX:
                _PARSE_CACHE.popitem(last=False)
    return stmts


def _parse_uncached(sql: str) -> list[exp.Expression]:
    # Cap the statement length before parsing — a cheap upper bound on parse cost
    # so a flood of oversized statements can't pin CPU (the Mongo wire has
    # analogous 16/48 MB size ceilings). 16 MB matches the Mongo document
    # ceiling; the earlier 1 MB cap was falsified by a REAL query shape —
    # pgx's 65535-parameter statements are ~1.04 MB and real PG accepts up
    # to its 1 GB message limit. (#194)
    if len(sql) > MAX_SQL_LENGTH:
        raise errors.program_limit_exceeded(
            f"statement too long: {len(sql)} bytes exceeds the {MAX_SQL_LENGTH}-byte limit"
        )
    move = _MOVE_RE.match(sql)
    if move is not None:
        return [exp.Command(this="MOVE", expression=exp.Literal.string(move.group("tail")))]
    begin = _BEGIN_CHARACTERISTICS_RE.match(sql)
    if begin is not None:
        tail = begin.group("tail")
        return [exp.Transaction(modes=[tail] if tail else [])]
    # LISTEN / NOTIFY / UNLISTEN — sqlglot mis-parses these; carry the raw text
    # as a Command the engine's pubsub handler executes. (run_sql intercepts
    # them pre-parse; this covers the extended-protocol Parse path.)
    if _PUBSUB_HEAD_RE.match(sql):
        return [exp.Command(this="PUBSUB", expression=exp.Literal.string(sql))]
    multi_drop = _MULTI_DROP_TABLE_RE.match(sql)
    if multi_drop is not None and "," in multi_drop.group("names"):
        # ``DROP TABLE [IF EXISTS] a, b, c`` — sqlglot can't parse the
        # multi-name form (pgbench -i emits it). ONE statement in PG: one
        # CommandComplete tag (pgtest errors:9), and without IF EXISTS the
        # whole drop fails before any table goes. The engine executes each
        # parsed Drop inside a single MULTIDROP_TABLE command.
        head = "DROP TABLE " + ("IF EXISTS " if multi_drop.group("if_exists") else "")
        cmd = exp.Command(this="MULTIDROP_TABLE", expression=exp.Literal.string(sql))
        cmd.set(
            "drops",
            [
                sqlglot.parse_one(head + name.strip(), read="postgres")
                for name in multi_drop.group("names").split(",")
                if name.strip()
            ],
        )
        return [cmd]
    if COMMENT_CONSTRAINT_RE.match(sql):
        return [exp.Command(this="COMMENT_CONSTRAINT", expression=exp.Literal.string(sql))]
    # ``DO $$ … $$ [language plpgsql]`` — the body is handled by the engine's
    # minimal plpgsql interpreter (RAISE notices/exceptions).
    do_m = _DO_BLOCK_RE.match(sql)
    if do_m is not None:
        return [exp.Command(this="DO", expression=exp.Literal.string(do_m.group("body")))]
    # CREATE [OR REPLACE] PROCEDURE / DROP PROCEDURE — sqlglot rejects the
    # ``a INOUT int`` argmode syntax, so carry the raw text to the engine's
    # regex-driven handlers.
    if _CREATE_PROCEDURE_RE.match(sql):
        return [exp.Command(this="CREATE_PROCEDURE", expression=exp.Literal.string(sql))]
    if _DROP_PROCEDURE_RE.match(sql):
        return [exp.Command(this="DROP_PROCEDURE", expression=exp.Literal.string(sql))]
    # ``SET TRANSACTION <characteristics>`` — sqlglot rejects some spellings
    # (``SET TRANSACTION DEFERRABLE``); route every form to the Command SET
    # handler, which applies the characteristics to the open transaction.
    set_txn = _SET_TRANSACTION_RE.match(sql)
    if set_txn is not None:
        return [
            exp.Command(
                this="SET", expression=exp.Literal.string(f"TRANSACTION {set_txn.group('tail')}")
            )
        ]
    # sqlglot parses ``RELEASE x`` but not the equivalent ``RELEASE SAVEPOINT x``
    # (the standard form SQLAlchemy / psycopg emit) — drop the redundant keyword.
    # Savepoint commands are standalone, so this can't touch a string literal.
    sql = _RELEASE_SAVEPOINT_RE.sub("RELEASE", sql)
    # ``oid[]`` column types don't parse (sqlglot's OID keyword rejects the array
    # suffix) — rewrite the element name inside CREATE TABLE only.
    if _CREATE_TABLE_RE.match(sql):
        sql = _OID_ARRAY_RE.sub(r'"secantus_oid"\2', sql)
    # sqlglot can't parse ``COMMENT ON … IS NULL`` (it requires a string
    # expression), so a NULL comment (comment removal) is rewritten to a sentinel
    # the executor reads back as "remove". COMMENT statements are standalone.
    sql = _COMMENT_NULL_RE.sub(lambda m: f"{m.group(1)}'{UNCOMMENT_SENTINEL}'{m.group(2)}", sql)
    # ``COPY … TO STDOUT (FORMAT csv)`` — insert the WITH sqlglot requires.
    sql = _COPY_BARE_OPTIONS_RE.sub(r"\1 WITH (", sql)
    # ``CREATE FUNCTION … RETURNS trigger`` — sqlglot rejects the bare
    # pseudo-type; quoting it parses as a user-defined type whose identity
    # ``_create_function`` recognizes.
    if _CREATE_FUNCTION_RE.search(sql):
        sql = _RETURNS_TRIGGER_RE.sub(r'\1"trigger"', sql)
    # sqlglot can't parse a negative numeric scale (``::numeric(2,-3)``) —
    # rewrite it to a sentinel value the typmod encoder undoes (the cast
    # evaluator ignores precision/scale, so only the descriptor sees it).
    sql = _NEGSCALE_RE.sub(
        lambda m: f"{m.group(1)}{typemap.NEGSCALE_SENTINEL + int(m.group(2))}{m.group(3)}", sql
    )
    if '"char"' in sql:
        sql = _rewrite_quoted_char_types(sql)
    if "visible" in sql.lower():
        sql = _NOT_VISIBLE_RE.sub(r"\1", sql)
    # Decode E'…' escape strings ourselves — sqlglot's half-decoding is lossy.
    if "e'" in sql or "E'" in sql:
        sql = _decode_estrings(sql)
    # PG nests block comments; sqlglot doesn't — strip them when they nest.
    if "/*" in sql:
        sql = _strip_nested_block_comments(sql)
    try:
        try:
            stmts = [
                s for s in sqlglot.parse(_normalize_params(sql), read="postgres") if s is not None
            ]
        except sqlglot.errors.ParseError:
            # A batch whose INDIVIDUAL statements we can parse (some only via
            # the regex fallbacks above — ``BEGIN READ ONLY`` mid-batch is the
            # pgtest shape) still fails as one string. Split on top-level
            # semicolons and parse each segment through the full entry point.
            segments = _split_top_level_semicolons(sql)
            if len(segments) <= 1:
                raise
            out: list[exp.Expression] = []
            for seg in segments:
                out.extend(_parse_uncached(seg))
            return out
        for s in stmts:
            _fold_unquoted_identifiers(s)
            _resolve_group_by_ordinals(s)
            # Dollar-quoted strings tokenize as RawString — downstream code
            # (scalar, typemap, every literal path) only knows Literal, so
            # normalize in place: the value is identical, only the quoting
            # style differed.
            for raw in list(s.find_all(exp.RawString)):
                raw.replace(exp.Literal.string(raw.this))
        return stmts
    except (sqlglot.errors.ParseError, sqlglot.errors.TokenError) as exc:
        raise errors.syntax_error(str(exc).splitlines()[0]) from exc
    except RecursionError as exc:
        # A deeply-nested statement (e.g. hundreds of parentheses) blows Python's
        # recursion limit inside sqlglot. Convert it to a clean error rather than
        # relying on the connection loop's broad catch. (#194)
        raise errors.program_limit_exceeded("statement too deeply nested to parse") from exc
