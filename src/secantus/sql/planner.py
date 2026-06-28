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

import re
from dataclasses import dataclass, field
from typing import Any

import sqlglot
from sqlglot import exp

from secantus.sql import errors, typemap
from secantus.sql.catalog import Column, TableDef

# ---------------------------------------------------------------------------
# Plan objects — ready-to-execute structures over Storage.
# ---------------------------------------------------------------------------


@dataclass
class CreateTablePlan:
    table: TableDef
    if_not_exists: bool


@dataclass
class DropTablePlan:
    name: str
    if_exists: bool


@dataclass
class InsertPlan:
    table: TableDef
    docs: list[dict[str, Any]]


@dataclass
class ConstantSelectPlan:
    # A FROM-less ``SELECT <literals>`` — one row, no storage access. The
    # headline P1 case (``SELECT 1``) and the seed for ``SELECT version()`` etc.
    columns: list[tuple[str, str, Any]]  # (out_name, type_tag, python_value)


@dataclass
class SelectPlan:
    table: TableDef
    filter: dict[str, Any]
    sort: dict[str, int] | None
    limit: int
    skip: int
    out_columns: list[tuple[str, Column]] = field(default_factory=list)
    count_star: bool = False
    count_alias: str = "count"


@dataclass
class UpdatePlan:
    table: TableDef
    filter: dict[str, Any]
    update: dict[str, Any]


@dataclass
class DeletePlan:
    table: TableDef
    filter: dict[str, Any]


Plan = (
    CreateTablePlan | DropTablePlan | InsertPlan | SelectPlan | UpdatePlan | DeletePlan
)


# ---------------------------------------------------------------------------
# Literal / column extraction
# ---------------------------------------------------------------------------


def _literal(node: exp.Expression) -> Any:
    """Extract a Python value from a literal-ish AST node."""
    if isinstance(node, exp.Paren):
        return _literal(node.this)
    if isinstance(node, exp.Neg):
        return -_literal(node.this)
    if isinstance(node, exp.Null):
        return None
    if isinstance(node, exp.Boolean):
        return bool(node.this)
    if isinstance(node, exp.Literal):
        if node.is_string:
            return node.this
        text = node.this
        return float(text) if ("." in text or "e" in text.lower()) else int(text)
    raise errors.feature_not_supported(f"unsupported value expression: {node.sql()}")


def _column_name(node: exp.Expression) -> str:
    if isinstance(node, exp.Column):
        return node.name
    if isinstance(node, exp.Identifier):
        return node.name
    raise errors.feature_not_supported(f"expected a column, got: {node.sql()}")


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


def _like_to_regex(pattern: str) -> str:
    """Translate a SQL LIKE pattern to an anchored regex.

    ``%`` -> ``.*`` and ``_`` -> ``.``; every other character is escaped so it
    matches literally.
    """
    out = ["^"]
    for ch in pattern:
        if ch == "%":
            out.append(".*")
        elif ch == "_":
            out.append(".")
        else:
            out.append(re.escape(ch))
    out.append("$")
    return "".join(out)


def _coerce_for(table: TableDef, column: str, value: Any) -> Any:
    return typemap.coerce(value, table.type_for(column))


def _comparison(node: exp.Expression, table: TableDef) -> tuple[str, Any]:
    """Return (column_name, raw_other_value), normalising column-on-the-right."""
    left, right = node.this, node.expression
    if isinstance(left, exp.Column):
        return _column_name(left), right
    if isinstance(right, exp.Column):
        return _column_name(right), left
    raise errors.feature_not_supported(f"comparison needs a column: {node.sql()}")


def _expr_to_filter(node: exp.Expression, table: TableDef) -> dict[str, Any]:
    if isinstance(node, exp.Paren):
        return _expr_to_filter(node.this, table)

    if isinstance(node, exp.And):
        parts = [_expr_to_filter(node.this, table), _expr_to_filter(node.expression, table)]
        return _merge_and(parts)

    if isinstance(node, exp.Or):
        return {"$or": [_expr_to_filter(node.this, table), _expr_to_filter(node.expression, table)]}

    if isinstance(node, exp.Not):
        inner = node.this
        # IS NOT NULL parses as Not(Is(col, Null)).
        if isinstance(inner, exp.Is) and isinstance(inner.expression, exp.Null):
            field = table.field_for(_column_name(inner.this))
            return {field: {"$ne": None}}
        return {"$nor": [_expr_to_filter(inner, table)]}

    if isinstance(node, exp.Is):
        field = table.field_for(_column_name(node.this))
        if isinstance(node.expression, exp.Null):
            return {field: None}
        raise errors.feature_not_supported(f"unsupported IS predicate: {node.sql()}")

    if isinstance(node, exp.EQ):
        col, other = _comparison(node, table)
        return {table.field_for(col): _coerce_for(table, col, _literal(other))}

    if isinstance(node, exp.NEQ):
        col, other = _comparison(node, table)
        return {table.field_for(col): {"$ne": _coerce_for(table, col, _literal(other))}}

    for cls, (op, flipped) in _CMP_OPS.items():
        if isinstance(node, cls):
            col, other = _comparison(node, table)
            use = op if isinstance(node.this, exp.Column) else flipped
            return {table.field_for(col): {use: _coerce_for(table, col, _literal(other))}}

    if isinstance(node, exp.In):
        if node.args.get("query") is not None:
            raise errors.feature_not_supported("IN (subquery) is not supported")
        col = _column_name(node.this)
        values = [_coerce_for(table, col, _literal(e)) for e in node.expressions]
        return {table.field_for(col): {"$in": values}}

    if isinstance(node, exp.Between):
        col = _column_name(node.this)
        low = _coerce_for(table, col, _literal(node.args["low"]))
        high = _coerce_for(table, col, _literal(node.args["high"]))
        return {table.field_for(col): {"$gte": low, "$lte": high}}

    if isinstance(node, (exp.Like, exp.ILike)):
        col = _column_name(node.this)
        pattern = _literal(node.expression)
        spec: dict[str, Any] = {"$regex": _like_to_regex(str(pattern))}
        if isinstance(node, exp.ILike):
            spec["$options"] = "i"
        return {table.field_for(col): spec}

    raise errors.feature_not_supported(f"unsupported WHERE clause: {node.sql()}")


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


def _where_filter(stmt: exp.Expression, table: TableDef) -> dict[str, Any]:
    where = stmt.args.get("where")
    return _expr_to_filter(where.this, table) if where is not None else {}


# ---------------------------------------------------------------------------
# Statement planners
# ---------------------------------------------------------------------------


def plan_create_table(stmt: exp.Create) -> CreateTablePlan:
    schema = stmt.this
    if not isinstance(schema, exp.Schema):
        raise errors.feature_not_supported("CREATE TABLE requires a column list")
    table_name = schema.this.name
    columns: list[Column] = []
    pk_seen = False
    for coldef in schema.expressions:
        if isinstance(coldef, exp.PrimaryKey):
            # Table-level PRIMARY KEY (col) — mark the named column.
            names = [_column_name(c) for c in coldef.expressions]
            if len(names) != 1:
                raise errors.feature_not_supported("composite primary keys are not supported")
            columns = [_with_pk(c, names[0]) for c in columns]
            pk_seen = True
            continue
        if not isinstance(coldef, exp.ColumnDef):
            raise errors.feature_not_supported(f"unsupported table element: {coldef.sql()}")
        tag = typemap.type_tag_for_sql(coldef.args["kind"])
        if tag is None:
            raise errors.feature_not_supported(
                f"unsupported column type for {coldef.name}: {coldef.args['kind'].sql()}"
            )
        constraints = [type(c.kind).__name__ for c in (coldef.args.get("constraints") or [])]
        is_pk = "PrimaryKeyColumnConstraint" in constraints
        nullable = not is_pk and "NotNullColumnConstraint" not in constraints
        if is_pk:
            pk_seen = True
        columns.append(
            Column(
                name=coldef.name,
                type_tag=tag,
                field="_id" if is_pk else coldef.name,
                pk=is_pk,
                nullable=nullable,
            )
        )
    if not pk_seen:
        # No PK: the _id is auto-assigned by storage and not surfaced as a
        # column. Fine for the spike.
        pass
    table = TableDef(name=table_name, collection=table_name, columns=columns)
    return CreateTablePlan(table=table, if_not_exists=bool(stmt.args.get("exists")))


def _with_pk(col: Column, pk_name: str) -> Column:
    if col.name != pk_name:
        return col
    return Column(name=col.name, type_tag=col.type_tag, field="_id", pk=True, nullable=False)


def plan_drop_table(stmt: exp.Drop) -> DropTablePlan:
    return DropTablePlan(name=stmt.this.name, if_exists=bool(stmt.args.get("exists")))


def plan_insert(stmt: exp.Insert, table: TableDef) -> InsertPlan:
    schema = stmt.this
    if isinstance(schema, exp.Schema):
        col_names = [_column_name(c) for c in schema.expressions]
    else:
        col_names = [c.name for c in table.columns]
    values = stmt.expression
    if not isinstance(values, exp.Values):
        raise errors.feature_not_supported("INSERT requires a VALUES clause")
    docs: list[dict[str, Any]] = []
    for tup in values.expressions:
        cells = tup.expressions
        if len(cells) != len(col_names):
            raise errors.syntax_error(
                f"INSERT has {len(cells)} values but {len(col_names)} columns"
            )
        doc: dict[str, Any] = {}
        provided = set()
        for name, cell in zip(col_names, cells, strict=True):
            col = table.column(name)
            if col is None:
                raise errors.undefined_column(name)
            raw = _literal(cell)
            if raw is None and not col.nullable:
                raise errors.not_null_violation(name)
            doc[col.field] = typemap.coerce(raw, col.type_tag)
            provided.add(name)
        # NOT NULL columns omitted entirely are a violation (no DEFAULT support).
        for col in table.columns:
            if col.name not in provided and not col.nullable:
                raise errors.not_null_violation(col.name)
        docs.append(doc)
    return InsertPlan(table=table, docs=docs)


def _order_sort(stmt: exp.Expression, table: TableDef) -> dict[str, int] | None:
    order = stmt.args.get("order")
    if order is None:
        return None
    sort: dict[str, int] = {}
    for ordered in order.expressions:
        col = _column_name(ordered.this)
        sort[table.field_for(col)] = -1 if ordered.args.get("desc") else 1
    return sort


def _limit_skip(stmt: exp.Expression) -> tuple[int, int]:
    limit_node = stmt.args.get("limit")
    offset_node = stmt.args.get("offset")
    limit = int(_literal(limit_node.expression)) if limit_node is not None else 0
    skip = int(_literal(offset_node.expression)) if offset_node is not None else 0
    return limit, skip


def _infer_value_tag(value: Any) -> str:
    if value is None:
        return "text"
    if isinstance(value, bool):
        return "bool"
    if isinstance(value, int):
        return "int4" if -(2**31) <= value < 2**31 else "int8"
    if isinstance(value, float):
        return "float8"
    return "text"


def plan_constant_select(stmt: exp.Select) -> ConstantSelectPlan:
    """Plan a FROM-less ``SELECT <literal>, ...`` into a single constant row."""
    if stmt.args.get("where") or stmt.args.get("group") or stmt.args.get("joins"):
        raise errors.feature_not_supported("FROM-less SELECT supports only constant projections")
    columns: list[tuple[str, str, Any]] = []
    for e in stmt.expressions:
        if isinstance(e, exp.Alias):
            name = e.alias
            target = e.this
        else:
            # Postgres names a bare literal column "?column?".
            name = "?column?"
            target = e
        value = _literal(target)
        columns.append((name, _infer_value_tag(value), value))
    return ConstantSelectPlan(columns=columns)


def plan_select(stmt: exp.Select, table: TableDef) -> SelectPlan:
    if stmt.args.get("joins"):
        raise errors.feature_not_supported("JOIN is not supported yet")
    if stmt.args.get("group") or stmt.args.get("having"):
        raise errors.feature_not_supported("GROUP BY / HAVING is not supported yet")
    if stmt.args.get("distinct"):
        raise errors.feature_not_supported("SELECT DISTINCT is not supported yet")

    filt = _where_filter(stmt, table)
    sort = _order_sort(stmt, table)
    limit, skip = _limit_skip(stmt)

    exprs = stmt.expressions
    # COUNT(*) as the sole projection (no GROUP BY) -> whole-table count.
    if len(exprs) == 1 and isinstance(exprs[0], (exp.Count, exp.Alias)):
        inner = exprs[0].this if isinstance(exprs[0], exp.Alias) else exprs[0]
        if isinstance(inner, exp.Count) and isinstance(inner.this, exp.Star):
            alias = exprs[0].alias if isinstance(exprs[0], exp.Alias) else "count"
            return SelectPlan(
                table=table,
                filter=filt,
                sort=sort,
                limit=limit,
                skip=skip,
                count_star=True,
                count_alias=alias or "count",
            )

    out_columns: list[tuple[str, Column]] = []
    for e in exprs:
        if isinstance(e, exp.Star):
            for col in table.columns:
                out_columns.append((col.name, col))
            continue
        if isinstance(e, exp.Alias):
            name = e.alias
            target = e.this
        else:
            name = _column_name(e)
            target = e
        col = table.column(_column_name(target))
        if col is None:
            raise errors.undefined_column(_column_name(target))
        out_columns.append((name, col))
    return SelectPlan(
        table=table, filter=filt, sort=sort, limit=limit, skip=skip, out_columns=out_columns
    )


def plan_update(stmt: exp.Update, table: TableDef) -> UpdatePlan:
    set_doc: dict[str, Any] = {}
    for assign in stmt.expressions:
        if not isinstance(assign, exp.EQ):
            raise errors.feature_not_supported(f"unsupported SET item: {assign.sql()}")
        col_name = _column_name(assign.this)
        col = table.column(col_name)
        if col is None:
            raise errors.undefined_column(col_name)
        if col.pk:
            raise errors.feature_not_supported("updating the primary key is not supported")
        raw = _literal(assign.expression)
        if raw is None and not col.nullable:
            raise errors.not_null_violation(col_name)
        set_doc[col.field] = typemap.coerce(raw, col.type_tag)
    return UpdatePlan(table=table, filter=_where_filter(stmt, table), update={"$set": set_doc})


def plan_delete(stmt: exp.Delete, table: TableDef) -> DeletePlan:
    return DeletePlan(table=table, filter=_where_filter(stmt, table))


def parse(sql: str) -> list[exp.Expression]:
    """Parse a (possibly multi-statement) SQL string into AST statements."""
    try:
        return [s for s in sqlglot.parse(sql, read="postgres") if s is not None]
    except sqlglot.errors.ParseError as exc:
        raise errors.syntax_error(str(exc).splitlines()[0]) from exc
