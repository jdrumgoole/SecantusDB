"""Plan-time comparison-operator resolution (Postgres' 42883).

In Postgres a comparison is resolved to a concrete operator during *parse
analysis*, before any row is read, so comparing two incompatible types is an
ERROR rather than a predicate that matches nothing::

    -- real postgres
    SELECT * FROM t WHERE text_col = 42;
    ERROR:  operator does not exist: text = integer

The SQL layer evaluates comparisons on decoded BSON values with Python's
``==`` / ``<``, which silently absorbs the mismatch and yields FALSE (see
``scalar._eval_compare``). This module restores the plan-time error for the
cases we can decide *soundly*, and only those.

Soundness is the whole design constraint here: a spurious 42883 breaks a query
that works today, which is far worse than the lenient FALSE. So the analysis
is deliberately **sound but very incomplete** — every uncertainty resolves to
"say nothing":

* Only a comparison whose BOTH operands have a confidently-known static type
  is judged. An untyped string literal (Postgres' ``unknown``), a bound
  parameter, a subquery, an unrecognised function, an unresolvable column —
  any of these makes the comparison unjudged.
* Only four type *categories* participate (numeric / text / boolean /
  date-time). Within a category Postgres has implicit casts both ways, so a
  same-category pair is always fine; across these four there is no implicit
  cast, so the pair is always an error. Every other type tag (``bytea``,
  ``uuid``, ``json``, ``money``, ``oid``, ``interval``, ``time``, ranges,
  geo, network, bit, …) is treated as unknown — some of those pairs genuinely
  error in Postgres, but the categories are subtler and the payoff is small.
* **Reflected (schema-on-read) tables are exempt entirely.** A reflected
  column's type comes from sampling 50 documents (``reflect.reflect``), so a
  heterogeneous BSON field can be declared ``text`` while holding integers.
  Erroring there would break the dual-protocol path, where a cross-BSON-type
  comparison is deliberate.
* A statement whose FROM cannot be resolved to plain, declared, non-reflected
  tables (CTEs, derived tables, set operations, subqueries, functions in FROM)
  is skipped wholesale rather than analysed with partial information. A
  statement with **no** FROM at all (``SELECT 'a'::text = 1``) is likewise not
  judged: this analysis is driven by declared column types, and constant-only
  expressions are the bulk of what the psycopg / SQLAlchemy gauges evaluate,
  so widening to them is a separate, separately-measured change.
"""

from __future__ import annotations

from typing import Any

from sqlglot import exp

from secantus.sql import errors, typemap
from secantus.sql.catalog import Column, TableDef

#: Comparison operators whose resolution we check. Postgres resolves all of
#: these through the same ``btree`` operator families, so one category rule
#: covers them.
_COMPARISONS = (exp.EQ, exp.NEQ, exp.GT, exp.GTE, exp.LT, exp.LTE)

#: Type categories with mutual implicit casts *inside* the category and no
#: implicit cast *between* categories in Postgres' default catalog. A pair drawn
#: from two different categories has no candidate operator → 42883.
_CATEGORY: dict[str, str] = {
    # Numeric category: int2/int4/int8/numeric/float4/float8 are mutually
    # implicit-castable, so any numeric-vs-numeric comparison resolves.
    "int2": "numeric",
    "int4": "numeric",
    "int8": "numeric",
    "float4": "numeric",
    "float8": "numeric",
    "numeric": "numeric",
    # String category: varchar/bpchar/name/citext are all binary-coercible or
    # implicitly castable to text. (PG's one-byte ``"char"`` is NOT here — the
    # pgtest char corpus shows it resolving through text in some contexts.)
    "text": "string",
    "citext": "string",
    "name": "string",
    # Boolean has no implicit cast to or from numeric or text in Postgres.
    "bool": "boolean",
    # date -> timestamp -> timestamptz are implicit, so these three compare
    # freely. ``time`` / ``timetz`` / ``interval`` are deliberately absent.
    "date": "datetime",
    "timestamp": "datetime",
    "timestamptz": "datetime",
}

#: Categories for types that only ever arrive as a DECLARED parameter type —
#: a column of one of these is judged through ``_CATEGORY`` above, but a
#: ``Parse`` that declares ``$1`` as one of them makes the comparison decidable
#: even though no column carries the tag. Each is its own category: Postgres has
#: no implicit cast between them and text/numeric, which is exactly why
#: ``varchar_col = $1::uuid`` is an error rather than a no-match.
_PARAM_CATEGORY: dict[str, str] = {
    "uuid": "uuid",
    "bytea": "bytea",
    "inet": "inet",
    "cidr": "inet",
    "macaddr": "macaddr",
    "json": "json",
    "jsonb": "json",
    "xml": "xml",
}

#: Human-facing Postgres spelling for the error message, per tag.
_PG_NAME = {
    "int2": "smallint",
    "int4": "integer",
    "int8": "bigint",
    "float4": "real",
    "float8": "double precision",
    "numeric": "numeric",
    "text": "text",
    "citext": "citext",
    "name": "name",
    "bool": "boolean",
    "date": "date",
    "timestamp": "timestamp without time zone",
    "timestamptz": "timestamp with time zone",
}

#: Postgres spellings for the declared types whose storage tag folds into
#: another (``varchar`` / ``bpchar`` both store as ``text``). The error message
#: names the DECLARED type, so a varchar column reads "character varying".
_DECL_OID_NAME: dict[int, str] = {
    1042: "character",
    1043: "character varying",
}

#: Operator spelling for the error message.
_OP_TEXT = {
    exp.EQ: "=",
    exp.NEQ: "<>",
    exp.GT: ">",
    exp.GTE: ">=",
    exp.LT: "<",
    exp.LTE: "<=",
}

#: Functions that return ``text`` when handed a text argument. Postgres has no
#: numeric overload for any of them, so ``f(text_col)`` is text — which is what
#: makes ``int_col = substr(text_col, 1, 1)`` a 42883 rather than a no-match.
#: Gated on the first argument itself typing to the string category, so
#: ``substr(bytea_col, …)`` (bytea) and ``substring(bit_col, …)`` (bit) stay
#: unjudged.
_TEXT_PRESERVING = (
    exp.Lower,
    exp.Upper,
    exp.Initcap,
    exp.Trim,
    exp.Substring,
)

#: Functions returning an integer regardless of argument type (``length`` is
#: defined for text / bytea / bit / tsvector, and all overloads return int).
_INT_RETURNING = (exp.Length,)


class _Resolver:
    """Column-name -> ``Column`` resolution over a statement's FROM tables."""

    def __init__(
        self,
        tables: list[tuple[str | None, TableDef]],
        shadowed: frozenset[str],
        param_oids: list[int] | None = None,
    ) -> None:
        self._tables = tables
        self._shadowed = shadowed
        #: Parameter type OIDs as resolved at Parse, indexed from $1. Empty
        #: when the statement is being checked without them (the execution
        #: path), which leaves every parameter unknown as before.
        self._param_oids = param_oids or []

    def param_type(self, index: int) -> tuple[str, str] | None:
        """``(category, display_name)`` for ``$index``'s DECLARED type, or None
        when it wasn't declared (oid 0 — Postgres' ``unknown``, which takes the
        other operand's type) or isn't a type we judge."""
        if index < 1 or index > len(self._param_oids):
            return None
        oid = self._param_oids[index - 1]
        if not oid:
            return None
        tag = typemap.OID_TO_TAG.get(oid)
        if tag is None:
            return None
        cat = _CATEGORY.get(tag) or _PARAM_CATEGORY.get(tag)
        return (cat, _describe(tag)) if cat is not None else None

    def column(self, node: exp.Column) -> Column | None:
        qualifier = node.table or None
        name = node.name
        if qualifier is None and name in self._shadowed:
            # An output alias of the same name (``SELECT n AS txt … ORDER BY
            # txt``) may be what this reference resolves to; the alias rewrite
            # runs after us, so refuse to guess.
            return None
        found: Column | None = None
        for alias, table in self._tables:
            if qualifier is not None and qualifier not in {alias, table.name}:
                continue
            col = table.column(name)
            if col is None:
                continue
            if found is not None and found is not col:
                return None  # ambiguous — say nothing
            found = col
        return found


def _from_arg(stmt: exp.Expression) -> exp.From | None:
    """The statement's FROM clause. sqlglot spells the arg key ``from_`` in
    current releases and ``from`` in older ones — read both."""
    node = stmt.args.get("from_") or stmt.args.get("from")
    return node if isinstance(node, exp.From) else None


def _table_sources(stmt: exp.Expression) -> list[exp.Expression] | None:
    """Every FROM-ish source of ``stmt``, or None when the shape is one we
    refuse to analyse (CTE, set operation, derived table, subquery, …)."""
    if stmt.args.get("with") is not None:
        return None
    if isinstance(stmt, (exp.Union, exp.Except, exp.Intersect)):
        return None
    sources: list[exp.Expression] = []
    if isinstance(stmt, exp.Select):
        from_ = _from_arg(stmt)
        if from_ is not None:
            sources.append(from_.this)
        for join in stmt.args.get("joins") or []:
            sources.append(join.this)
        if stmt.args.get("laterals"):
            return None
    elif isinstance(stmt, exp.Update):
        sources.append(stmt.this)
        from_ = _from_arg(stmt)
        if from_ is not None:
            sources.append(from_.this)
    elif isinstance(stmt, exp.Delete):
        sources.append(stmt.this)
        for using in stmt.args.get("using") or []:
            sources.append(using.this if isinstance(using, exp.From) else using)
    else:
        return None
    return sources


def _resolver(
    stmt: exp.Expression, catalog: Any, db: str, param_oids: list[int] | None = None
) -> _Resolver | None:
    """A resolver over ``stmt``'s FROM tables, or None when any source is not a
    plain declared non-reflected table."""
    sources = _table_sources(stmt)
    if sources is None:
        return None
    tables: list[tuple[str | None, TableDef]] = []
    for src in sources:
        if src is None or not isinstance(src, exp.Table):
            return None
        if not isinstance(src.this, exp.Identifier):
            return None  # a table function / VALUES in FROM
        table = catalog.get(db, _table_name(src))
        if table is None or table.reflected:
            # Unknown name (a view, a CTE, a missing relation) or a
            # schema-on-read collection whose column types came from sampling
            # 50 documents — either way we have no declared types to reason on.
            return None
        tables.append((src.alias or None, table))
    if not tables:
        return None
    return _Resolver(tables, _output_aliases(stmt), param_oids)


def _output_aliases(stmt: exp.Expression) -> frozenset[str]:
    """Names the statement's own select list introduces. ORDER BY / GROUP BY may
    resolve to one of these rather than to a base column, so a reference by that
    name is treated as untyped."""
    if not isinstance(stmt, exp.Select):
        return frozenset()
    return frozenset(e.alias for e in stmt.expressions if isinstance(e, exp.Alias) and e.alias)


def _table_name(node: exp.Table) -> str:
    """The catalog lookup name for a FROM table — the same mapping
    ``planner.qualified_table_name`` applies (``public.t`` -> ``t``)."""
    schema = node.args.get("db")
    sname = schema.name if schema is not None else None
    if not sname or sname == "public":
        return node.name
    return f"{sname}.{node.name}"


def _static_type(node: exp.Expression, resolver: _Resolver) -> tuple[str, str] | None:
    """``(category, display_name)`` for ``node`` when its Postgres type is
    statically certain, else None. ``display_name`` is the spelling Postgres
    puts in the error message.

    None means "unknown" and is the answer for everything not enumerated here —
    string literals (Postgres' ``unknown``, which takes the other operand's
    type), parameters, subqueries, arithmetic, unrecognised functions.
    """
    while isinstance(node, exp.Paren):
        node = node.this
    if isinstance(node, exp.Column):
        col = resolver.column(node)
        if col is None:
            return None
        cat = _CATEGORY.get(col.type_tag)
        if cat is None:
            return None
        # An enum / domain column stores its base type's tag but Postgres names
        # the declared type in the message ("operator does not exist: mood =
        # integer"). Both are implicitly coercible to the base, so the category
        # verdict is the base type's either way. varchar / bpchar fold to the
        # text tag for storage the same way, and Postgres names THEM too —
        # "character varying = uuid", not "text = uuid".
        declared = col.enum_type or col.domain_type or _DECL_OID_NAME.get(col.decl_oid or 0)
        return (cat, declared or _describe(col.type_tag))
    if isinstance(node, exp.Cast):
        to = node.to
        if not isinstance(to, exp.DataType):
            return None
        tag = typemap.type_tag_for_sql(to)
        cat = _CATEGORY.get(tag) if tag is not None else None
        return (cat, _describe(tag)) if cat is not None and tag is not None else None
    if isinstance(node, exp.Boolean):
        return ("boolean", "boolean")
    if isinstance(node, exp.Literal):
        if node.is_string:
            return None  # untyped literal: PG resolves it to the other side
        # A numeric constant is typed immediately by PG (integer / numeric),
        # never ``unknown`` — this is what makes ``text_col = 42`` an error.
        return ("numeric", "integer" if _looks_integral(node.name) else "numeric")
    if isinstance(node, _INT_RETURNING):
        return ("numeric", "integer")
    if isinstance(node, exp.Parameter):
        try:
            return resolver.param_type(int(node.name))
        except (TypeError, ValueError):
            return None
    if isinstance(node, _TEXT_PRESERVING):
        arg = node.this
        inner = _static_type(arg, resolver) if arg is not None else None
        if inner is not None and inner[0] == "string":
            return ("string", "text")
        return None
    return None


def _looks_integral(text: str) -> bool:
    return text.lstrip("+-").isdigit()


def _describe(tag: str) -> str:
    return _PG_NAME.get(tag, typemap.SQL_TYPE_NAME.get(tag, tag))


def _check_comparison(node: exp.Expression, resolver: _Resolver) -> None:
    left = _static_type(node.this, resolver)
    right = _static_type(node.expression, resolver)
    if left is None or right is None:
        return
    if left[0] == right[0]:
        return
    op = _OP_TEXT[type(node)]
    raise errors.SQLError("42883", f"operator does not exist: {left[1]} {op} {right[1]}")


def _has_nested_query(node: exp.Expression, root: exp.Expression) -> bool:
    """Whether ``node`` sits inside a nested query relative to ``root`` — its
    columns may resolve against a scope the resolver knows nothing about."""
    cur = node.parent
    while cur is not None and cur is not root:
        if isinstance(cur, (exp.Select, exp.Subquery, exp.Union, exp.Except, exp.Intersect)):
            return True
        cur = cur.parent
    return False


def check_statement(
    stmt: exp.Expression, catalog: Any, db: str, *, param_oids: list[int] | None = None
) -> None:
    """Raise 42883 when ``stmt`` contains a comparison Postgres could not
    resolve. A no-op for every statement shape or operand pair the analysis
    cannot decide soundly — see the module docstring."""
    try:
        _analyse(stmt, catalog, db, param_oids)
    except errors.SQLError:
        raise
    except Exception:
        # An advisory analysis must never be the reason a statement fails: a
        # catalog read hiccup or an AST shape we mis-walked leaves the query to
        # the normal (lenient) path rather than surfacing an internal error.
        return


def _analyse(
    stmt: exp.Expression, catalog: Any, db: str, param_oids: list[int] | None = None
) -> None:
    if not isinstance(stmt, (exp.Select, exp.Update, exp.Delete)):
        return
    if any(True for _ in stmt.find_all(exp.Select, exp.Subquery)) and not isinstance(
        stmt, exp.Select
    ):
        # A subquery inside UPDATE/DELETE brings scopes we don't model.
        return
    resolver = _resolver(stmt, catalog, db, param_oids)
    if resolver is None:
        return
    assignments = _set_assignments(stmt)
    for node in stmt.find_all(*_COMPARISONS):
        if id(node) in assignments or _has_nested_query(node, stmt):
            continue
        _check_comparison(node, resolver)


def _set_assignments(stmt: exp.Expression) -> set[int]:
    """``id()`` of every ``UPDATE … SET col = expr`` node. sqlglot parses an
    assignment as an ``EQ``, but it is not a comparison: Postgres reports an
    unassignable value as ``42804 datatype_mismatch`` ("column is of type text
    but expression is of type integer"), a different analysis with different
    coercion rules (assignment casts, not implicit ones)."""
    if not isinstance(stmt, exp.Update):
        return set()
    return {id(e) for e in stmt.args.get("expressions") or []}
