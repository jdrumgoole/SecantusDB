"""Execute a plan against ``Storage`` and shape the result rows.

Each function takes a plan (already lowered to Mongo structures by the planner),
performs the corresponding ``Storage`` call, and returns a ``SQLResult`` with a
Postgres ``CommandComplete`` tag. This is the only layer that touches storage;
the planner stays pure translation.
"""

from __future__ import annotations

import contextlib
import functools
import operator
import re
import threading
import weakref
from typing import Any

import bson

from secantus.paths import get_path, has_path
from secantus.sql import errors, planner, subms, typemap
from secantus.sql.catalog import USER_TYPE_ARRAY_OID_OFFSET, Catalog
from secantus.sql.result import ColumnDesc, SQLResult

# One write-statement lock per shared ``Storage``. A DML statement is a
# read-modify-write spanning several storage calls (constraint probes,
# post-image computation, the write itself); the storage ``RLock`` only
# serializes each *call*, so two connections interleaving between the probe
# and the write could both pass a UNIQUE check or both derive a post-image
# from the same pre-image (lost update). Postgres serializes these with row
# locks; we serialize the whole statement — writers on one storage already
# serialize inside WiredTiger, so this costs no real concurrency. RLock so a
# write nested inside another's enforcement (FK cascades) re-enters.
_write_locks: weakref.WeakKeyDictionary[Any, threading.RLock] = weakref.WeakKeyDictionary()
_write_locks_guard = threading.Lock()
_fallback_write_lock = threading.RLock()  # storage objects that refuse weakrefs


def _write_lock(storage: Any) -> threading.RLock:
    with _write_locks_guard:
        try:
            lock = _write_locks.get(storage)
            if lock is None:
                lock = threading.RLock()
                _write_locks[storage] = lock
            return lock
        except TypeError:
            return _fallback_write_lock


def _serialized_write(fn: Any) -> Any:
    """Run a ``(plan, storage, db, ...)`` write executor under the storage's
    statement-write lock."""

    @functools.wraps(fn)
    def wrapper(plan: Any, storage: Any, db: str, *args: Any, **kwargs: Any) -> Any:
        with _write_lock(storage):
            return fn(plan, storage, db, *args, **kwargs)

    return wrapper


def _pg_sort(items: list[Any], key_of: Any, specs: list[tuple[int, bool]]) -> None:
    """Stable in-place sort with Postgres ORDER BY semantics.

    ``key_of(item)`` returns the tuple of ordering-key values; ``specs`` is the
    parallel list of ``(direction, nulls_first)`` (direction 1 asc / -1 desc).
    NULLs sort to the front or back per ``nulls_first`` independent of direction —
    Postgres orders NULL as though it were the largest value, so this can't be
    delegated to Mongo's sort (which treats NULL/missing as the smallest)."""

    def cmp(a: Any, b: Any) -> int:
        ka, kb = key_of(a), key_of(b)
        for i, (direction, nulls_first) in enumerate(specs):
            x, y = ka[i], kb[i]
            if x is None and y is None:
                continue
            if x is None:
                return -1 if nulls_first else 1
            if y is None:
                return 1 if nulls_first else -1
            if x == y:
                continue
            base = -1 if x < y else 1
            return -base if direction == -1 else base
        return 0

    items.sort(key=functools.cmp_to_key(cmp))


def _order_key_fn(
    order: list[tuple[str, int, bool]],
    enum_orders: dict[str, list[str]] | None = None,
    citext_orders: set[str] | None = None,
) -> Any:
    """Build the ``key_of(doc)`` used by ``_pg_sort`` for a list of ORDER BY field
    paths. An enum-typed order field maps its label value to the label's ordinal in
    the enum's declared order (``enum_orders[field]``) so sorting follows the
    declared order, not lexical text order — a NULL stays NULL for placement. A
    citext field (``citext_orders``) folds its string value to lower case so the
    sort is case-insensitive."""
    ordinals: dict[str, dict[str, int]] = {}
    if enum_orders:
        ordinals = {
            f: {lbl: i for i, lbl in enumerate(labels)} for f, labels in enum_orders.items()
        }
    citext_fields = citext_orders or set()

    def key_of(doc: Any) -> tuple:
        out = []
        for field_path, _, _ in order:
            value = get_path(doc, field_path)
            omap = ordinals.get(field_path)
            if omap is not None and value is not None:
                value = omap.get(value, len(omap))  # unknown label sorts last
            elif field_path in citext_fields and isinstance(value, str):
                value = value.lower()
            out.append(value)
        return tuple(out)

    return key_of


def _resolve_user_type_column(col: Any, catalog: Catalog, db: str) -> Any:
    """Resolve a column whose declared type is a user-defined name. The planner
    tags any unknown type as ``enum_type`` (it can't reach storage); here we
    disambiguate: a declared enum keeps that tag, a declared domain is rewritten
    to ``domain_type`` with the domain's base type tag (inheriting the domain's
    DEFAULT when the column has none), and anything else is 42704."""
    import dataclasses

    if col.enum_type is None:
        return col
    name = col.enum_type
    if catalog.enum_exists(db, name):
        return col
    if typemap.is_array_tag(col.type_tag):
        # An array of a declared composite (``custom[]`` — pgjdbc's
        # DatabaseMetaDataTest customtable) stores a list of subdocuments and
        # reports the composite's minted array-companion oid. Other user-type
        # arrays (domains) stay unsupported.
        composite = catalog.get_composite(db, name)
        if composite is not None:
            return dataclasses.replace(
                col,
                enum_type=None,
                composite_type=name,
                composite_fields=tuple(composite),
                type_tag="composite[]",
            )
        raise errors.SQLError("42704", f'type "{name}[]" does not exist')
    composite = catalog.get_composite(db, name)
    if composite is not None:
        # A composite-typed column stores a subdocument; carry the type's ordered
        # fields on the column so INSERT (ROW → named subdoc) and (col).field can
        # use them without a catalog round-trip.
        return dataclasses.replace(
            col,
            enum_type=None,
            composite_type=name,
            composite_fields=tuple(composite),
            type_tag="composite",
        )
    domain = catalog.get_domain(db, name)
    if domain is None:
        # A TABLE's name is also its ROW TYPE (typtype 'c') in PG — a column
        # declared ``col rsmd1`` where rsmd1 is a table stores that row shape
        # (pgjdbc's ResultSetMetaDataTest builds its compositetest this way).
        rel = catalog.get(db, name)
        if rel is not None and getattr(rel, "columns", None):
            fields = tuple((c.name, c.type_tag, None) for c in rel.columns)
            return dataclasses.replace(
                col,
                enum_type=None,
                composite_type=name,
                composite_fields=fields,
                type_tag="composite",
            )
        raise errors.SQLError("42704", f'type "{name}" does not exist')
    inherit_default = not col.has_default and bool(domain.get("has_default"))
    return dataclasses.replace(
        col,
        enum_type=None,
        domain_type=name,
        type_tag=domain["base_tag"],
        has_default=col.has_default or inherit_default,
        default=domain.get("default") if inherit_default else col.default,
    )


def unique_index_name(constraint_name: str) -> str:
    """The storage index backing a UNIQUE constraint takes the constraint's own
    name, which is also what Postgres calls the index it creates for one — so
    catalog reflection reports a single index, not the constraint's plus a
    differently-named implementation detail."""
    return constraint_name


def _create_unique_index(storage: Any, db: str, table: planner.TableDef, uq: Any) -> None:
    """Back a SQL UNIQUE constraint with a storage unique index.

    The constraint was upheld only by a probe read before writing, which cannot
    see a value another transaction committed after the writer's snapshot, nor
    one a second writer is inserting right now — so duplicates were stored. The
    storage index makes WiredTiger the arbiter instead.

    NULLs are distinct in SQL — any number of them satisfy a UNIQUE constraint,
    and a multi-column constraint is unconstrained if ANY of its columns is NULL
    — whereas a Mongo unique index would collide them. A partial filter
    excluding NULL from every column reproduces the SQL rule. Sparse would not:
    a SQL NULL is stored as an explicit null, not a missing field.
    """
    if getattr(uq, "deferrable", False):
        # A DEFERRABLE constraint may be violated transiently inside a
        # transaction and is only judged at COMMIT — swapping two values is the
        # classic case. An index enforcing on every write would reject the
        # intermediate state, so those keep the deferred check instead.
        return
    fields = [table.field_for(c) for c in uq.columns]
    if not fields:
        return
    key_spec = dict.fromkeys(fields, 1)
    clauses = [{f: {"$ne": None}} for f in fields]
    partial = clauses[0] if len(clauses) == 1 else {"$and": clauses}
    storage.create_index(
        db,
        table.collection,
        unique_index_name(uq.name),
        key_spec,
        {"unique": True, "partialFilterExpression": partial},
    )


def execute_create_table(
    plan: planner.CreateTablePlan, catalog: Catalog, storage: Any, db: str
) -> SQLResult:
    if catalog.exists(db, plan.table.name):
        if plan.if_not_exists:
            return SQLResult(command_tag="CREATE TABLE")
        # A temp table's catalog key carries its session namespace; the error
        # names the bare relation like real PG ('relation "foo" already exists').
        name = plan.table.name
        raise errors.duplicate_table(name.split(".", 1)[1] if plan.table.temp else name)
    if "." in plan.table.name and not plan.table.temp:
        schema = plan.table.name.split(".", 1)[0]
        if not catalog.schema_exists(db, schema):
            raise errors.SQLError("3F000", f'schema "{schema}" does not exist')
    # A user-defined column type (the planner records it as ``enum_type``, since
    # it can't reach storage to tell enum from domain) must resolve to a declared
    # enum *or* domain — else 42704. A domain column adopts the domain's base tag
    # and inherits its DEFAULT when the column declares none.
    plan.table.columns = [_resolve_user_type_column(col, catalog, db) for col in plan.table.columns]
    catalog.put(db, plan.table)
    storage.create_collection(db, plan.table.collection)
    for uq in plan.table.unique_constraints:
        _create_unique_index(storage, db, plan.table, uq)
    # Auto-create the sequence behind each SERIAL column (owned by the table).
    for seq in plan.sequences:
        catalog.create_sequence(
            db,
            seq["name"],
            start=seq.get("start", 1),
            increment=seq.get("increment", 1),
            owned_by=f"{plan.table.name}.{seq['column']}",
        )
    return SQLResult(command_tag="CREATE TABLE")


def execute_drop_table(
    plan: planner.DropTablePlan, catalog: Catalog, storage: Any, db: str
) -> SQLResult:
    table = catalog.get(db, plan.name)
    if table is None:
        if plan.if_exists:
            return SQLResult(command_tag="DROP TABLE")
        raise errors.undefined_table(plan.name)
    catalog.drop_triggers_for_table(db, plan.name)  # triggers die with the table
    catalog.drop(db, plan.name)
    storage.drop_collection(db, table.collection)
    # Drop any sequences the table owned (SERIAL columns).
    for col in table.columns:
        if col.sequence is not None:
            seq = catalog.get_sequence(db, col.sequence)
            if seq is not None and seq.get("owned_by") == f"{table.name}.{col.name}":
                catalog.drop_sequence(db, col.sequence)
    return SQLResult(command_tag="DROP TABLE")


def execute_alter_table(
    plan: planner.AlterTablePlan, catalog: Catalog, storage: Any, db: str
) -> SQLResult:
    """Apply ``ALTER TABLE`` actions to the catalog (and the backing collection
    where the data must follow — a dropped column's field is ``$unset``, a renamed
    non-PK column's field is ``$rename``d)."""
    table = catalog.get(db, plan.name)
    if table is None:
        if plan.if_exists:
            return SQLResult(command_tag="ALTER TABLE")
        raise errors.undefined_table(plan.name)
    old_name = table.name
    for action in plan.actions:
        _apply_alter_action(action, table, storage, db)
    catalog.replace(db, table, old_name=old_name)
    return SQLResult(command_tag="ALTER TABLE")


def _add_primary_key(
    table: Any, cols: list[str], con_name: str | None, storage: Any, db: str
) -> None:
    """Apply ``ADD PRIMARY KEY`` to an existing table: every row's ``_id`` is
    rewritten to the key column value(s) (single column → scalar ``_id``,
    composite → subdocument), after NOT NULL and uniqueness validation."""
    import dataclasses

    from secantus.sql import planner

    if table.pk_columns:
        raise errors.SQLError(
            "42P16", f'multiple primary keys for table "{table.name}" are not allowed'
        )
    for c in cols:
        if table.column(c) is None:
            raise errors.undefined_column(c)
    docs = storage.find_matching(db, table.collection, {})
    seen: set[str] = set()
    new_docs = []
    for doc in docs:
        vals = []
        for c in cols:
            v = doc.get(c)
            if v is None:
                raise errors.SQLError(
                    "23502",
                    f'column "{c}" of relation "{table.name}" contains null values',
                )
            vals.append(v)
        new_id: Any = vals[0] if len(cols) == 1 else dict(zip(cols, vals, strict=True))
        key = repr(new_id)
        if key in seen:
            raise errors.SQLError(
                "23505",
                f'could not create unique index "{con_name or table.name + "_pkey"}" '
                "— duplicate key values",
            )
        seen.add(key)
        nd = {k: v for k, v in doc.items() if k not in cols and k != "_id"}
        nd["_id"] = new_id
        new_docs.append(nd)
    storage.delete_matching(db, table.collection, {})
    if new_docs:
        storage.insert(db, table.collection, new_docs)
    table.columns = [
        planner._with_pk(dataclasses.replace(c, nullable=False) if c.name in cols else c, cols)
        for c in table.columns
    ]
    table.pk_name = con_name
    table.pk_column_order = tuple(cols) if len(cols) > 1 else None


def execute_comment(stmt: Any, catalog: Catalog, storage: Any, db: str) -> SQLResult:
    """``COMMENT ON TABLE t IS '…'`` / ``COMMENT ON COLUMN t.c IS '…'`` — store the
    comment in the catalog so it reflects via ``pg_description`` (SQLAlchemy's
    ``get_columns`` / ``get_table_comment``). ``IS NULL`` removes it."""
    import dataclasses

    from sqlglot import exp

    kind = str(stmt.args.get("kind") or "").upper()
    expr = stmt.args.get("expression")
    text = expr.this if isinstance(expr, exp.Literal) else None
    if text == planner.UNCOMMENT_SENTINEL:
        text = None
    if kind == "TABLE":
        _tn = (
            planner.qualified_table_name(stmt.this)
            if hasattr(stmt.this, "args")
            else stmt.this.name
        )
        table = catalog.get(db, _tn)
        if table is None:
            raise errors.undefined_table(_tn)
        table.comment = text
        catalog.replace(db, table)
        return SQLResult(command_tag="COMMENT")
    if kind == "COLUMN":
        col_node = stmt.this  # exp.Column: [schema.]table.col
        cname = col_node.name
        _schema = col_node.args.get("db")
        _sname = _schema.name if _schema is not None else None
        tname = f"{_sname}.{col_node.table}" if _sname and _sname != "public" else col_node.table
        table = catalog.get(db, tname)
        if table is None:
            raise errors.undefined_table(tname)
        if table.column(cname) is None:
            raise errors.undefined_column(cname)
        table.columns = [
            dataclasses.replace(c, comment=text) if c.name == cname else c for c in table.columns
        ]
        catalog.replace(db, table)
        return SQLResult(command_tag="COMMENT")
    if kind == "FUNCTION":
        # ``COMMENT ON FUNCTION f([argtypes]) IS '…'`` — store on the function
        # doc for pg_description reflection. The arg list picks the overload
        # by arity; the bare-name form comments the sole overload.
        node = stmt.this
        nargs: int | None = None
        if isinstance(node, exp.UserDefinedFunction):
            nargs = len(node.args.get("expressions") or [])
            node = node.this
        fname = node.name.lower()
        target = None
        if nargs is not None:
            target = catalog.get_function(db, fname, nargs)
        else:
            matches = [f for f in catalog.list_functions(db) if f["name"].lower() == fname]
            if len(matches) == 1:
                target = matches[0]
            elif len(matches) > 1:
                raise errors.SQLError("42725", f'function name "{fname}" is not unique')
        if target is None:
            raise errors.SQLError("42883", f"function {fname} does not exist")
        target = {k: v for k, v in target.items() if k != "_id"}
        target["comment"] = text
        catalog.put_function(db, target)
        return SQLResult(command_tag="COMMENT")
    if kind == "INDEX":
        # ``COMMENT ON INDEX idx IS '…'`` — stored keyed by index name and
        # surfaced through pg_description on the index relation's oid
        # (pgjdbc's getIndexInfo REMARKS join; remarkIndexInfo).
        iname = stmt.this.name
        known = {
            ix.get("name")
            for tn in catalog.list_tables(db)
            if (t := catalog.get(db, tn)) is not None
            for ix in storage.list_indexes(db, t.collection)
        }
        if iname not in known:
            raise errors.SQLError("42704", f'relation "{iname}" does not exist')
        catalog.set_index_comment(db, iname, text)
        return SQLResult(command_tag="COMMENT")
    raise errors.feature_not_supported(f"COMMENT ON {kind} is not supported")


def _view_star_columns(select: Any, catalog: Catalog, storage: Any, db: str) -> list[Any]:
    """``select``'s output expressions with any ``*`` expanded to real columns.

    A declared column list has to be matched up positionally against the view's
    output columns, so ``CREATE VIEW v (v1, v2) AS SELECT * FROM t`` has to know
    what ``*`` stands for. Postgres resolves the star once, at creation — adding
    a column to ``t`` afterwards does NOT add it to ``v`` (probed against 14) —
    so freezing the expansion here matches it. Raises when a star's source isn't
    a plain table (a subquery / VALUES / SRF), rather than guessing.
    """
    from sqlglot import exp as _exp

    from secantus.sql import reflect

    out: list[Any] = []
    for e in select.expressions:
        is_star = isinstance(e, _exp.Star) or (
            isinstance(e, _exp.Column) and isinstance(e.this, _exp.Star)
        )
        if not is_star:
            out.append(e)
            continue
        qualifier = e.table if isinstance(e, _exp.Column) else None
        # This statement's OWN sources: ``find_all`` would also descend into a
        # subquery in the WHERE and expand that table's columns into the view.
        from_node = select.args.get("from_") or select.args.get("from")
        holders = ([from_node] if from_node is not None else []) + list(
            select.args.get("joins") or []
        )
        sources = [h.this for h in holders if isinstance(h.this, _exp.Table)]
        if not sources or len(sources) != len(holders):
            raise errors.feature_not_supported(
                "CREATE VIEW with a column list over a non-table source is not supported"
            )
        for src in sources:
            alias = src.alias or src.name
            if qualifier and alias != qualifier:
                continue
            qn = planner.qualified_table_name(src)
            tdef = catalog.get(db, qn) or reflect.reflect(storage, db, qn)
            if tdef is None:
                raise errors.SQLError("42P01", f'relation "{qn}" does not exist')
            for c in tdef.columns:
                out.append(_exp.column(c.name, table=alias))
    return out


def _apply_view_column_names(
    select: Any, names: list[str], catalog: Catalog, storage: Any, db: str
) -> Any:
    """Rename ``select``'s outputs to the view's declared column names.

    Postgres applies them positionally and stores the rewritten query — a view
    declared ``v (x)`` over ``SELECT a, b`` renders as ``SELECT a AS x, b``, so
    surplus outputs keep their own names while surplus *names* are an error
    (both probed against 14).
    """
    from sqlglot import exp as _exp

    exprs = _view_star_columns(select, catalog, storage, db)
    if len(names) > len(exprs):
        raise errors.SQLError("42601", "CREATE VIEW specifies more column names than columns")
    aliased = list(exprs)
    for i, nm in enumerate(names):
        inner = aliased[i]
        inner = inner.this if isinstance(inner, _exp.Alias) else inner
        aliased[i] = _exp.alias_(inner.copy(), nm)
    out = select.copy()
    out.set("expressions", aliased)
    return out


def execute_create_view(
    stmt: Any, catalog: Catalog, storage: Any, db: str, check_option: str | None = None
) -> SQLResult:
    """``CREATE [OR REPLACE] VIEW v [(cols)] AS SELECT … [WITH [LOCAL|CASCADED]
    CHECK OPTION]`` — store the SELECT definition. Querying the view expands it
    as a subquery (see ``engine._expand_views``); ``check_option`` (``"LOCAL"`` /
    ``"CASCADED"``) is enforced on write-through against each written row.

    A declared column list parses as a ``Schema`` node wrapping the name, so the
    name is unwrapped from it — reading it straight off produced an empty view
    name, and the view was filed under "" while CREATE VIEW still reported
    success (every later reference then failed as an undefined relation)."""
    from sqlglot import exp as _exp

    name_node = stmt.this
    column_names: list[str] = []
    if isinstance(name_node, _exp.Schema):
        column_names = [c.name for c in name_node.expressions]
        name_node = name_node.this
    name = planner.qualified_table_name(name_node)
    replace = bool(stmt.args.get("replace"))
    if catalog.exists(db, name):
        raise errors.SQLError("42P07", f'relation "{name}" already exists')
    if not replace and catalog.get_view(db, name) is not None:
        raise errors.SQLError("42P07", f'relation "{name}" already exists')
    body = stmt.expression
    if column_names:
        if not isinstance(body, _exp.Select):
            raise errors.feature_not_supported(
                "CREATE VIEW with a column list is supported for SELECT bodies only"
            )
        body = _apply_view_column_names(body, column_names, catalog, storage, db)
    catalog.put_view(db, name, body.sql(dialect="postgres"), check_option=check_option)
    return SQLResult(command_tag="CREATE VIEW")


def execute_drop_view(stmt: Any, catalog: Catalog, storage: Any, db: str) -> SQLResult:
    name = planner.qualified_table_name(stmt.this)
    if not catalog.drop_view(db, name) and not stmt.args.get("exists"):
        raise errors.SQLError("42P01", f'view "{name}" does not exist')
    return SQLResult(command_tag="DROP VIEW")


def _apply_alter_action(action: Any, table: Any, storage: Any, db: str) -> None:
    from sqlglot import exp

    from secantus.sql.catalog import Column

    coll = table.collection
    if isinstance(action, exp.ColumnDef):  # ADD COLUMN [IF NOT EXISTS] name type
        name = action.name
        if table.column(name) is not None:
            if action.args.get("exists"):
                return
            raise errors.SQLError(
                "42701", f'column "{name}" of relation "{table.name}" already exists'
            )
        tag = typemap.type_tag_for_sql(action.args["kind"])
        if tag is None:
            raise errors.feature_not_supported(
                f"unsupported column type: {action.args['kind'].sql()}"
            )
        cons = [type(c.kind).__name__ for c in (action.args.get("constraints") or [])]
        table.columns.append(
            Column(
                name=name,
                type_tag=tag,
                field=name,
                pk=False,
                nullable="NotNullColumnConstraint" not in cons,
            )
        )
        return
    if (
        isinstance(action, exp.Drop)
        and (action.args.get("kind") or "COLUMN").upper() == "CONSTRAINT"
    ):
        # DROP CONSTRAINT [IF EXISTS] name — remove a declared FK / CHECK / UNIQUE.
        name = action.this.name
        buckets = (table.foreign_keys, table.check_constraints, table.unique_constraints)
        if not any(any(c.name == name for c in b) for b in buckets):
            if action.args.get("exists"):
                return
            raise errors.SQLError(
                "42704", f'constraint "{name}" of relation "{table.name}" does not exist'
            )
        table.foreign_keys = [c for c in table.foreign_keys if c.name != name]
        table.check_constraints = [c for c in table.check_constraints if c.name != name]
        for _uq in [c for c in table.unique_constraints if c.name == name]:
            with contextlib.suppress(Exception):
                storage.drop_index(db, table.collection, unique_index_name(_uq.name))
        table.unique_constraints = [c for c in table.unique_constraints if c.name != name]
        return
    if isinstance(action, exp.Drop):  # DROP COLUMN [IF EXISTS] name
        name = action.this.name
        col = table.column(name)
        if col is None:
            if action.args.get("exists"):
                return
            raise errors.undefined_column(name)
        if col.pk:
            raise errors.feature_not_supported("dropping the PRIMARY KEY column is not supported")
        table.columns = [c for c in table.columns if c.name != name]
        storage.update_matching(db, coll, {}, {"$unset": {col.field: ""}}, multi=True)
        return
    if isinstance(action, exp.RenameColumn):  # RENAME COLUMN a TO b
        old = action.this.name
        new = action.args["to"].name
        col = table.column(old)
        if col is None:
            if action.args.get("exists"):
                return
            raise errors.undefined_column(old)
        if table.column(new) is not None:
            raise errors.SQLError(
                "42701", f'column "{new}" of relation "{table.name}" already exists'
            )
        new_field = col.field if col.pk else new
        table.columns = [
            Column(new, c.type_tag, new_field, c.pk, c.nullable) if c.name == old else c
            for c in table.columns
        ]
        if not col.pk and col.field != new_field:
            storage.update_matching(db, coll, {}, {"$rename": {col.field: new_field}}, multi=True)
        return
    if isinstance(action, exp.AlterRename):  # RENAME TO newname
        new_name = action.this.name
        if table.collection == table.name:
            # A declared table maps 1:1 to a same-named collection — move the
            # collection too, so the old name stops resolving (a leftover
            # collection would otherwise reflect as a phantom table).
            ok, err = storage.rename_collection(db, table.collection, db, new_name)
            if not ok:
                raise errors.SQLError("42P07", err or f'relation "{new_name}" already exists')
            table.collection = new_name
        table.name = new_name
        return
    if isinstance(action, exp.AlterColumn):
        # ALTER COLUMN c { TYPE t | SET/DROP DEFAULT | SET/DROP NOT NULL }.
        import dataclasses

        from secantus.sql import planner

        name = action.this.name
        col = table.column(name)
        if col is None:
            raise errors.undefined_column(name)
        if action.args.get("dtype") is not None:  # TYPE t — retype in the catalog
            dtype = action.args["dtype"]
            tag = typemap.type_tag_for_sql(dtype)
            if tag is None:
                raise errors.feature_not_supported(f"unsupported column type: {dtype.sql()}")
            # The declared identity has to be recomputed, not inherited: a column
            # retyped from ``char(8)`` to ``text`` kept reporting bpchar/12 in
            # RowDescription (pgtest row_description reads it after an ALTER).
            identity = typemap.cast_type_identity(dtype)
            decl_oid, typmod = identity if identity is not None else (None, -1)
            new_col = dataclasses.replace(
                col, type_tag=tag, decl_oid=decl_oid, typmod=typmod, json_plain=decl_oid == 114
            )
        elif action.args.get("default") is not None:  # SET DEFAULT <literal | expr>
            node = action.args["default"]
            has_def, value = planner._literal_default(node, col.type_tag)
            if has_def:
                new_col = dataclasses.replace(
                    col, has_default=True, default=value, default_expr=None
                )
            else:  # expression default — now() / gen_random_uuid() / arithmetic (#166)
                new_col = dataclasses.replace(
                    col,
                    has_default=False,
                    default=None,
                    default_expr=node.sql(dialect="postgres"),
                )
        elif action.args.get("allow_null") is not None:  # SET/DROP NOT NULL
            new_col = dataclasses.replace(col, nullable=bool(action.args["allow_null"]))
        elif action.args.get("drop"):  # DROP DEFAULT
            new_col = dataclasses.replace(col, has_default=False, default=None, default_expr=None)
        else:
            raise errors.feature_not_supported(f"unsupported ALTER COLUMN action: {action.sql()}")
        table.columns = [new_col if c.name == name else c for c in table.columns]
        return
    if isinstance(action, exp.AddConstraint):
        # ADD [CONSTRAINT name] { FOREIGN KEY (…) REFERENCES … | CHECK (…) |
        # UNIQUE (…) } — declared, reflected, never enforced (same as a CREATE
        # TABLE constraint). Unnamed ``ADD UNIQUE (…)`` is accepted; unnamed ``ADD
        # CHECK (…)`` isn't parseable by sqlglot, so a CHECK needs CONSTRAINT name.
        from secantus.sql import planner

        for con in action.args.get("expressions") or []:
            con_name = None
            node = con
            if isinstance(con, exp.Constraint):
                con_name = con.this.name if con.this else None
                inner = con.args.get("expressions") or []
                node = inner[0] if inner else None
            if isinstance(node, exp.ForeignKey):
                cols = tuple(c.name for c in node.args.get("expressions") or [])
                ref = node.args.get("reference")
                if ref is None:
                    raise errors.feature_not_supported(
                        f"unsupported ADD FOREIGN KEY: {action.sql()}"
                    )
                table.foreign_keys.append(planner._make_fk(table.name, cols, ref, con_name))
            elif isinstance(node, exp.CheckColumnConstraint):
                table.check_constraints.append(
                    planner.make_check_constraint(node, table.name, con_name)
                )
            elif isinstance(node, exp.UniqueColumnConstraint):
                added = planner.make_unique_constraint(node, table.name, con_name)
                table.unique_constraints.append(added)
                # Back it with a storage index like CREATE TABLE does, or a
                # constraint added later would be the only one still relying on
                # the probe alone.
                _create_unique_index(storage, db, table, added)
            elif isinstance(node, exp.PrimaryKey):
                # ``ALTER TABLE t ADD [CONSTRAINT c] PRIMARY KEY (cols)`` —
                # validates NOT NULL + uniqueness, re-keys every row from the
                # auto-assigned ``_id`` onto the column value(s), and updates
                # the catalog mapping (pgbench -i's post-load step).
                pk_cols = [planner._column_name(c) for c in node.expressions]
                _add_primary_key(table, pk_cols, con_name, storage, db)
            else:
                raise errors.feature_not_supported(f"unsupported ADD CONSTRAINT: {action.sql()}")
        return
    if type(action).__name__ == "Command":
        # ``ALTER TABLE t DROP name`` (no COLUMN keyword — valid PG; pgjdbc's
        # droppedColumns test) exceeds sqlglot's action parser and lands here
        # as a raw Command. Re-parse with the keyword and apply that action.
        import re as _re

        from secantus.sql import planner as _planner

        m = _re.match(
            r"(?is)^\s*DROP\s+(?!COLUMN\b|CONSTRAINT\b)(?P<ie>IF\s+EXISTS\s+)?"
            r'(?P<col>"[^"]+"|[A-Za-z_]\w*)\s*(?:CASCADE|RESTRICT)?\s*$',
            action.sql(),
        )
        if m is not None:
            ie = "IF EXISTS " if m.group("ie") else ""
            reparsed = _planner.parse(
                f'ALTER TABLE "{table.name}" DROP COLUMN {ie}{m.group("col")}'
            )[0]
            for sub in reparsed.args.get("actions") or []:
                _apply_alter_action(sub, table, storage, db)
            return
    raise errors.feature_not_supported(f"unsupported ALTER TABLE action: {action.sql()}")


def execute_create_index(
    plan: planner.CreateIndexPlan, catalog: Catalog, storage: Any, db: str, session: Any = None
) -> SQLResult:
    existing = [ix.get("name") for ix in storage.list_indexes(db, plan.collection)]
    if plan.name in existing:
        if plan.if_not_exists:
            return SQLResult(command_tag="CREATE INDEX")
        raise errors.SQLError("42P07", f'relation "{plan.name}" already exists')
    # An expression index materialises the indexed expression into a hidden field
    # (registered on the table, recomputed on every write); backfill it into every
    # existing row *before* building the B-tree so the entries are populated.
    if plan.expr_index is not None:
        _create_expr_index(plan, catalog, storage, db, session)
    options: dict[str, Any] = {}
    if plan.unique:
        options["unique"] = True
    if plan.partial_filter:
        options["partialFilterExpression"] = plan.partial_filter
    if plan.include:
        options["include"] = list(plan.include)
    storage.create_index(db, plan.collection, plan.name, plan.key_spec, options or None)
    return SQLResult(command_tag="CREATE INDEX")


def _create_expr_index(
    plan: planner.CreateIndexPlan, catalog: Catalog, storage: Any, db: str, session: Any
) -> None:
    from secantus.sql import scalar

    ei = plan.expr_index
    owner = next(
        (
            t
            for tn in catalog.list_tables(db)
            if (t := catalog.get(db, tn)) is not None and t.collection == plan.collection
        ),
        None,
    )
    if owner is None:
        raise errors.SQLError("42P01", f'relation for collection "{plan.collection}" not found')
    if any(existing.field == ei.field for existing in owner.expr_indexes):
        return  # already registered (idempotent re-create)
    owner.expr_indexes.append(ei)
    catalog.replace(db, owner)
    # Backfill: compute the expression per existing row and persist the hidden field.
    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    for doc in storage.find_matching(db, plan.collection, {}):
        _apply_expr_index_fields([doc], owner, ctx)
        storage.update_matching(
            db,
            plan.collection,
            {"_id": doc["_id"]},
            {"$set": {ei.field: doc[ei.field]}},
            multi=False,
        )


def execute_drop_index(
    plan: planner.DropIndexPlan, catalog: Catalog, storage: Any, db: str
) -> SQLResult:
    # Postgres DROP INDEX names the index, not its table — find the owning
    # collection by scanning the catalog's tables for the index name.
    for tname in catalog.list_tables(db):
        table = catalog.get(db, tname)
        if table is None:
            continue
        if any(ix.get("name") == plan.name for ix in storage.list_indexes(db, table.collection)):
            storage.drop_index(db, table.collection, plan.name)
            _drop_expr_index(table, plan.name, catalog, storage, db)
            return SQLResult(command_tag="DROP INDEX")
    if plan.if_exists:
        return SQLResult(command_tag="DROP INDEX")
    raise errors.SQLError("42704", f'index "{plan.name}" does not exist')


def _drop_expr_index(table: Any, index_name: str, catalog: Catalog, storage: Any, db: str) -> None:
    """When the dropped index is an expression index, unregister it from the owning
    table (so the WHERE/ORDER rewrite stops firing) and strip its hidden field from
    every row."""
    ei = next((e for e in table.expr_indexes if e.name == index_name), None)
    if ei is None:
        return
    table.expr_indexes = [e for e in table.expr_indexes if e.name != index_name]
    catalog.replace(db, table)
    for doc in storage.find_matching(db, table.collection, {}):
        if ei.field in doc:
            storage.update_matching(
                db, table.collection, {"_id": doc["_id"]}, {"$unset": {ei.field: ""}}, multi=False
            )


def _source_column_identity(table: Any, storage: Any, db: str | None) -> tuple[int, dict[str, int]]:
    """``(table_oid, {column_name: attnum})`` for a result's base table.

    RowDescription reports these per column, and a JDBC updatable ResultSet
    resolves each result column back to its base column through them. Sending
    0/0 left it unable to, so ``updateRow()`` emitted ``SET "" = ?``. Returns
    ``(0, {})`` when there is no single base table (a reflected collection has
    no pg_class row, and a computed column has no source column anyway).
    """
    if table is None or storage is None or db is None or getattr(table, "reflected", False):
        return 0, {}
    from secantus.sql import virtual

    try:
        oid = virtual._table_oids(db, Catalog(storage)).get(table.name, 0)
    except Exception:  # pragma: no cover - catalog unavailable
        return 0, {}
    return oid, {c.name: i + 1 for i, c in enumerate(table.columns)}


def _view_oid(name: str, storage: Any, db: str | None) -> int:
    """The minted pg_class oid for view ``name``, or 0 when it can't be resolved."""
    if storage is None or db is None:
        return 0
    from secantus.sql import virtual

    try:
        return virtual._view_oids(db, Catalog(storage)).get(name, 0)
    except Exception:  # pragma: no cover - catalog unavailable
        return 0


def _with_subms(doc: dict[str, Any], col: Any) -> Any:
    """``col``'s stored value with its sub-millisecond remainder added back.

    BSON dates hold whole milliseconds, so a ``timestamp`` column's microseconds
    are carried in a hidden companion field (see `secantus.sql.subms`). Only the
    paths that still have the whole document can restore them — a value already
    projected through the aggregation pipeline has lost the companion.
    """
    value = get_path(doc, col.field)
    if getattr(col, "type_tag", None) not in subms.SUBMS_TAGS:
        return value
    return subms.merge(value, doc.get(subms.companion_field(col.field)))


def _out_column_descs(
    cols: list[tuple[str, Any]], storage: Any, db: str | None, table: Any = None
) -> list[ColumnDesc]:
    """Describe ``(out_name, Column)`` output pairs for a result.

    An enum-typed column reports the enum's minted pg_type oid — the same mint
    ``pg_type`` / ``pg_enum`` reflect — instead of text's 25, so a client that
    registered the type from the catalog (psycopg's ``EnumInfo.fetch``)
    recognises result columns. The value bytes are the label text either way
    (an enum's binary wire form IS its text), so only the oid changes."""
    enum_oids: dict[str, int] | None = None
    table_oid, attnums = _source_column_identity(table, storage, db)
    out: list[ColumnDesc] = []
    for name, col in cols:
        oid = typemap.PG_OID.get(col.type_tag, 25)
        # A declared identity (varchar/bpchar fold to text for storage but
        # reflect their real oid; numeric/timestamp precision rides the
        # atttypmod) — JDBC derives display size and scale from these.
        decl = getattr(col, "decl_oid", None)
        if decl:
            oid = decl
        typmod = getattr(col, "typmod", -1)
        if getattr(col, "json_plain", False):
            oid = 114  # a ``json`` (not jsonb) column keeps the plain-json oid
        if (
            getattr(col, "composite_type", None) is not None
            and storage is not None
            and db is not None
        ):
            # A declared-composite column reports its type's MINTED oid (not
            # generic RECORD/2249) so a registered psycopg loader fires and
            # parses nested fields by their reflected types.
            from secantus.sql import virtual

            minted = virtual._composite_oids(db, Catalog(storage)).get(col.composite_type)
            if minted is None:
                # A column typed by a TABLE's row type: report the table's
                # rowtype oid — its pg_type row has typtype 'c', which is what
                # pgjdbc's getSQLType maps to java.sql.Types.STRUCT (generic
                # RECORD/2249 mapped to OTHER — ResultSetMetaDataTest's
                # testComposite trio).
                minted = virtual._table_rowtype_oids(db, Catalog(storage)).get(col.composite_type)
            if minted is not None:
                # A composite-array column reports the minted array-companion
                # oid, same scheme as enum arrays.
                if typemap.is_array_tag(col.type_tag):
                    oid = minted + USER_TYPE_ARRAY_OID_OFFSET
                else:
                    oid = minted
        if getattr(col, "enum_type", None) is not None and storage is not None and db is not None:
            if enum_oids is None:
                enum_oids = Catalog(storage).enum_type_oids(db)
            enum_oid = enum_oids.get(col.enum_type)
            if enum_oid is not None:
                # An enum-array column (``mood[]``) reports the minted array
                # companion oid, a scalar enum column the enum oid itself.
                if typemap.is_array_tag(col.type_tag):
                    oid = enum_oid + USER_TYPE_ARRAY_OID_OFFSET
                else:
                    oid = enum_oid
        attnum = attnums.get(getattr(col, "name", ""), 0)
        out.append(
            ColumnDesc(
                name,
                col.type_tag,
                oid,
                typmod,
                table_oid=table_oid if attnum else 0,
                attnum=attnum,
            )
        )
    return out


def _tagged_out_column_descs(
    cols: list[tuple[str, str]],
    enum_types: dict[int, str],
    storage: Any,
    db: str | None,
    *,
    out_exprs: list | None = None,
    base_table: Any = None,
    out_sources: list[tuple[Any, int] | None] | None = None,
) -> list[ColumnDesc]:
    """Describe ``(out_name, type_tag)`` output pairs (the pipeline/evaluated
    plans' string-tag form). ``enum_types`` maps output positions to enum type
    names — the tag alone can't carry the identity (labels are stored as text) —
    so those positions resolve the minted enum oid like `_out_column_descs`.

    ``out_exprs`` + ``base_table`` (single-table evaluated plans) attribute
    bare-column outputs to their source table/attnum — JDBC's
    getBaseColumnName resolves aliases through these RowDescription fields,
    and a computed projection in the list must not strip the identity from
    its plain-column siblings (ResultSetMetaDataTest's base-column asserts).

    ``out_sources`` carries the same identity per output position for plans with
    no single base table (a JOIN): ``(TableDef, attnum)`` or None. It takes
    precedence over the ``base_table`` derivation where both are present."""
    from sqlglot import exp as _exp

    enum_oids: dict[str, int] | None = None
    table_oid, attnums = _source_column_identity(base_table, storage, db)
    identities: dict[int, tuple[int, dict[str, int]]] = {}

    def _identity_of(tdef: Any) -> tuple[int, dict[str, int]]:
        """``_source_column_identity`` memoised per joined table (the oid lookup
        reflects the whole catalog, so a wide join would repeat it per column).
        A ``ViewSource`` resolves through the view oid range instead — a view has
        no ``TableDef``, but it is a relation and reports its own pg_class oid."""
        key = id(tdef)
        if key not in identities:
            if isinstance(tdef, planner.ViewSource):
                identities[key] = (_view_oid(tdef.name, storage, db), {})
            else:
                identities[key] = _source_column_identity(tdef, storage, db)
        return identities[key]

    out: list[ColumnDesc] = []
    for i, (name, tag) in enumerate(cols):
        oid = typemap.PG_OID.get(tag, 25)
        enum_name = enum_types.get(i)
        if enum_name is not None and storage is not None and db is not None:
            if enum_oids is None:
                enum_oids = Catalog(storage).enum_type_oids(db)
            enum_oid = enum_oids.get(enum_name)
            if enum_oid is not None:
                oid = (
                    enum_oid + USER_TYPE_ARRAY_OID_OFFSET if typemap.is_array_tag(tag) else enum_oid
                )
        attnum = 0
        typmod = -1
        src_table = base_table
        col_table_oid = table_oid
        if out_sources is not None and i < len(out_sources) and out_sources[i] is not None:
            src_table, src_attnum = out_sources[i]  # type: ignore[misc]
            col_table_oid, _ = _identity_of(src_table)
            attnum = src_attnum if col_table_oid else 0
        elif col_table_oid and out_exprs is not None and i < len(out_exprs):
            expr = out_exprs[i]
            if isinstance(expr, _exp.Column):
                attnum = attnums.get(expr.name, 0)
        src_columns = getattr(src_table, "columns", None)
        if attnum and src_columns and attnum <= len(src_columns):
            # The evaluated plans carry only string tags, so the declared
            # identity (varchar's 1043, numeric's precision typmod) comes from
            # the source table's column def. A view source has no column defs —
            # its outputs keep the tag-derived identity.
            src = src_columns[attnum - 1]
            decl = getattr(src, "decl_oid", None)
            if decl and enum_types.get(i) is None:
                oid = decl
            typmod = getattr(src, "typmod", -1)
        out.append(
            ColumnDesc(
                name,
                tag,
                oid,
                typmod,
                table_oid=col_table_oid if attnum else 0,
                attnum=attnum,
            )
        )
    return out


def _returning_result(
    docs: list[dict[str, Any]],
    returning: list[tuple[str, Any, Any]],
    command_tag: str,
    rowcount: int,
    table: Any = None,
    storage: Any = None,
    db: str | None = None,
) -> SQLResult:
    """Shape a write statement's ``RETURNING`` rows the same way a SELECT does —
    so the wire layer emits a RowDescription + DataRows ahead of CommandComplete.

    Each returning item is ``(name, Column, expr)``; a plain item (``expr`` None)
    reads straight from the doc, a computed one is evaluated per row against a
    scope over the returned doc."""
    columns = _out_column_descs([(name, col) for name, col, _ in returning], storage, db)
    ctx = None
    if any(expr is not None for _, _, expr in returning):
        from secantus.sql import scalar

        ctx = scalar.ScalarContext(storage=storage, catalog=None, db=db, session=None)

    def cell(doc: dict[str, Any], col: Any, expr: Any) -> Any:
        if expr is None:
            return typemap.to_py(_with_subms(doc, col), col.type_tag)
        from secantus.sql import scalar

        def scope(node: Any) -> Any:
            return get_path(doc, table.field_for(node.name))

        return typemap.to_py(scalar.evaluate(expr, scope, ctx), col.type_tag)

    rows = [tuple(cell(doc, col, expr) for _, col, expr in returning) for doc in docs]
    return SQLResult(command_tag=command_tag, columns=columns, rows=rows, rowcount=rowcount)


def _fire_before_insert_triggers(
    plan: Any, storage: Any, db: str, catalog: Any, session: Any
) -> list[dict[str, Any]]:
    """Run BEFORE INSERT FOR EACH ROW triggers over the planned rows.

    Each row becomes a column-name-keyed NEW record for the plpgsql trigger
    function, which may mutate fields (``new.ts := to_tsvector(new.t)``) or
    return NULL to skip the row — PG's BEFORE-trigger semantics. The returned
    record is written back through each column's storage field."""
    if catalog is None or getattr(plan.table, "reflected", False):
        return plan.docs
    triggers = [
        t
        for t in catalog.triggers_for_table(db, plan.table.name)
        if t.get("timing") == "BEFORE" and t.get("event") == "INSERT"
    ]
    if not triggers:
        return plan.docs
    from secantus.paths import set_path
    from secantus.sql import plpgsql, scalar

    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    out: list[dict[str, Any]] = []
    for doc in plan.docs:
        record: dict[str, Any] | None = {c.name: get_path(doc, c.field) for c in plan.table.columns}
        for trg in triggers:
            func = catalog.get_function(db, trg["function"], 0)
            if func is None:
                raise errors.SQLError("42883", f"function {trg['function']}() does not exist")
            record = plpgsql.invoke_trigger(func, record, ctx)
            if record is None:
                break  # RETURN NULL: skip this row
        if record is None:
            continue
        for c in plan.table.columns:
            value = record.get(c.name)
            # Structured values (tsvector / jsonb dicts) pass through as-is;
            # scalars get best-effort coercion to the column type.
            if value is not None and not isinstance(value, dict):
                with contextlib.suppress(errors.SQLError, ValueError, TypeError):
                    value = typemap.coerce(value, c.type_tag)
            set_path(doc, c.field, value)
        out.append(doc)
    return out


@_serialized_write
def execute_insert(
    plan: planner.InsertPlan,
    storage: Any,
    db: str,
    catalog: Catalog | None = None,
    session: Any = None,
) -> SQLResult:
    if plan.on_conflict is not None:
        return _execute_insert_on_conflict(plan, storage, db, catalog, session)
    plan.docs = _fire_before_insert_triggers(plan, storage, db, catalog, session)
    _assign_sequences(plan.docs, plan.table, db, catalog, session)
    if plan.check_option is not None:
        from secantus.sql import scalar

        _validate_check_option(
            plan.docs,
            plan.check_option,
            plan.table,
            scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session),
        )
    if plan.returning is not None:
        # Pin an ``_id`` on every doc up front so the in-hand list is the
        # authoritative inserted set to project from (storage may deep-copy on
        # insert, so we can't rely on it back-filling ``_id`` into plan.docs).
        for doc in plan.docs:
            doc.setdefault("_id", bson.ObjectId())
    enforce_insert_rows(plan.docs, plan.table, storage, db, catalog, session)
    inserted, write_errors = storage.insert(db, plan.table.collection, plan.docs)
    if write_errors:
        _raise_write_error(write_errors[0], plan.table)
    if plan.returning is not None:
        return _returning_result(
            plan.docs[:inserted],
            plan.returning,
            f"INSERT 0 {inserted}",
            inserted,
            plan.table,
            storage,
            db,
        )
    return SQLResult(command_tag=f"INSERT 0 {inserted}", rowcount=inserted)


def _raise_write_error(err: dict[str, Any], table: planner.TableDef) -> None:
    if err.get("code") == 11000:
        constraint = _dup_key_constraint_name(err, table)
        uq = next((u for u in table.unique_constraints if u.name == constraint), None)
        if uq is not None and uq.exclusion:
            raise errors.SQLError(
                "23P01",
                f'conflicting key value violates exclusion constraint "{constraint}"',
                diag=_error_diag(table, n=constraint),
            )
        raise errors.SQLError(
            "23505",
            f'duplicate key value violates unique constraint "{constraint}"',
            diag=_error_diag(table, n=constraint),
        )
    raise errors.SQLError("XX000", err.get("errmsg", "insert failed"))


def _dup_key_constraint_name(err: dict[str, Any], table: planner.TableDef) -> str:
    """The violated constraint's PG name from a storage duplicate-key error:
    the ``_id`` index is the primary key; any other index name maps back
    through the ``unique_index_name`` scheme to the declared constraint."""
    m = re.search(r"index: (\S+) dup key", err.get("errmsg", ""))
    index_name = m.group(1) if m else ""
    if index_name in ("", "_id_"):
        return table.pk_constraint_name()
    for uq in table.unique_constraints:
        if unique_index_name(uq.name) == index_name or uq.name == index_name:
            return uq.name
    return index_name


@functools.lru_cache(maxsize=256)
def _parse_check_expr(text: str) -> Any:
    import sqlglot

    return sqlglot.parse_one(text, read="postgres")


def _validate_write_row(doc: dict[str, Any], table: Any, ctx: Any) -> None:
    """Enforce a declared table's NOT NULL and CHECK constraints against a row
    (the post-image for an UPDATE). Raises on violation; no-op for reflected
    (schema-on-read) tables, which carry no declared constraints.

    NOT NULL — a non-nullable, non-PK column that is null / missing violates
    ``23502`` (the PK is skipped: storage auto-assigns ``_id``). CHECK — a
    predicate that evaluates to FALSE violates ``23514``; NULL passes (Postgres
    treats an unknown CHECK result as satisfied)."""
    from secantus.sql import scalar

    if getattr(table, "reflected", False):
        return
    for col in table.columns:
        if col.pk or col.nullable:
            continue
        if get_path(doc, col.field) is None:
            raise errors.SQLError(
                "23502",
                f'null value in column "{col.name}" of relation "{table.name}" '
                "violates not-null constraint",
                diag=_error_diag(table, c=col.name),
            )
    if not table.check_constraints:
        return

    def scope(node: Any) -> Any:
        col = table.column(node.name)
        return get_path(doc, col.field if col is not None else node.name)

    for ck in table.check_constraints:
        value = scalar.evaluate(_parse_check_expr(ck.expression), scope, ctx)
        if value is not None and not scalar._truthy(value):
            raise errors.SQLError(
                "23514",
                f'new row for relation "{table.name}" violates check constraint "{ck.name}"',
                diag=_error_diag(table, n=ck.name),
            )


def _table_schema(table: Any) -> str:
    name = getattr(table, "name", "")
    if getattr(table, "temp", False):
        return name.split(".", 1)[0] if name.startswith("pg_temp_") else "pg_temp_1"
    return name.split(".", 1)[0] if "." in name else "public"


def _bare_table_name(table: Any) -> str:
    name = getattr(table, "name", "")
    return name.split(".", 1)[1] if "." in name else name


def _error_diag(table: Any, **extra: str) -> dict[str, str]:
    """PG's ErrorResponse identity fields for a constraint violation on
    ``table``: s=schema, t=bare table name, plus any of c(olumn)/
    n(constraint)/d(atatype) the caller supplies. pgjdbc's
    ``ServerErrorMessage`` surfaces these via getSchema()/getTable()/...."""
    diag = {"s": _table_schema(table), "t": _bare_table_name(table)}
    diag.update({k: v for k, v in extra.items() if v})
    return diag


def _validate_check_option(
    docs: list[dict[str, Any]], check_option: Any, table: Any, ctx: Any
) -> None:
    """Enforce an auto-updatable view's ``WITH CHECK OPTION`` — every written row
    (an INSERT row or an UPDATE post-image) must satisfy the view's WHERE
    predicate, else raise ``44000`` (``WITH CHECK OPTION`` violation). Unlike a
    CHECK constraint, a predicate that is not TRUE (FALSE *or* NULL) violates:
    the row would not be visible through the view. ``check_option`` is a
    ``(predicate, view_name)`` pair; ``predicate`` is an sqlglot expression over
    base columns (view columns == base columns for an auto-updatable view)."""
    from secantus.sql import scalar

    if check_option is None:
        return
    predicate, view_name = check_option

    def scope_for(row: dict[str, Any]):
        def scope(node: Any) -> Any:
            col = table.column(node.name)
            return get_path(row, col.field if col is not None else node.name)

        return scope

    for doc in docs:
        value = scalar.evaluate(predicate, scope_for(doc), ctx)
        if not scalar._truthy(value):
            raise errors.SQLError(
                "44000",
                f'new row violates check option for view "{view_name}"',
            )


def _validate_rows(
    docs: list[dict[str, Any]], table: Any, storage: Any, db: str, session: Any, catalog: Any = None
) -> None:
    from secantus.sql import scalar

    if getattr(table, "reflected", False) or not (table.check_constraints or _has_not_null(table)):
        return
    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    for doc in docs:
        _validate_write_row(doc, table, ctx)


def _has_not_null(table: Any) -> bool:
    return any(not c.pk and not c.nullable for c in table.columns)


# -- combined enforcement entry points (shared by INSERT / UPDATE / ON CONFLICT /
#    MERGE) so every write path enforces the same NOT NULL / CHECK / UNIQUE / FK.


def _assign_sequences(
    docs: list[dict[str, Any]], table: Any, db: str, catalog: Any, session: Any
) -> None:
    """Fill sequence-backed columns (SERIAL / ``DEFAULT nextval``) omitted from an
    INSERT: draw each doc's next value from the sequence and record it as the
    session's ``currval`` / ``lastval``. A supplied value is left untouched."""
    if catalog is None or getattr(table, "reflected", False):
        return
    seq_cols = [c for c in table.columns if c.sequence is not None]
    if not seq_cols:
        return
    for doc in docs:
        for col in seq_cols:
            if col.field in doc and doc[col.field] is not None:
                continue
            if not catalog.sequence_exists(db, col.sequence):
                continue  # dropped sequence — leave unset (NOT NULL will catch it)
            value = catalog.sequence_nextval(db, col.sequence)
            doc[col.field] = value
            if session is not None:
                session.record_sequence_value(col.sequence, value)


def _validate_enum_columns(docs: list[dict[str, Any]], table: Any, catalog: Any, db: str) -> None:
    """Every enum-typed column value must be one of the enum's labels (22P02). A
    NULL value is exempt (the NOT NULL check handles required columns)."""
    if catalog is None or getattr(table, "reflected", False):
        return
    enum_cols = [c for c in table.columns if c.enum_type is not None]
    if not enum_cols:
        return
    label_cache: dict[str, set] = {}
    for col in enum_cols:
        if col.enum_type not in label_cache:
            enum = catalog.get_enum(db, col.enum_type)
            label_cache[col.enum_type] = set(enum["labels"]) if enum else set()
    for doc in docs:
        for col in enum_cols:
            value = get_path(doc, col.field)
            if value is None:
                continue
            # An enum-array column (``mood[]``) validates each element.
            values = value if typemap.is_array_tag(col.type_tag) else [value]
            if not isinstance(values, list):
                values = [values]
            for v in values:
                if v is not None and v not in label_cache[col.enum_type]:
                    raise errors.SQLError(
                        "22P02",
                        f'invalid input value for enum {col.enum_type}: "{v}"',
                    )


def _validate_domain_columns(
    docs: list[dict[str, Any]], table: Any, storage: Any, db: str, session: Any, catalog: Any
) -> None:
    """Every domain-typed column value must satisfy the domain's NOT NULL and
    CHECK constraints. NOT NULL → ``23502`` (``domain <name> does not allow null
    values``, matching Postgres' not_null_violation); a CHECK evaluating to FALSE
    → ``23514`` (NULL passes, matching Postgres' three-valued CHECK semantics). The
    CHECK expression references the value via the ``VALUE`` keyword."""
    from secantus.sql import scalar

    if catalog is None or getattr(table, "reflected", False):
        return
    domain_cols = [c for c in table.columns if c.domain_type is not None]
    if not domain_cols:
        return
    cache: dict[str, dict[str, Any] | None] = {}
    for col in domain_cols:
        if col.domain_type not in cache:
            cache[col.domain_type] = catalog.get_domain(db, col.domain_type)
    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    for doc in docs:
        for col in domain_cols:
            domain = cache[col.domain_type]
            if domain is None:
                continue
            value = get_path(doc, col.field)
            if value is None:
                if domain.get("not_null"):
                    raise errors.SQLError(
                        "23502", f"domain {col.domain_type} does not allow null values"
                    )
                continue
            for check in domain.get("checks") or []:

                def scope(node: Any, _v: Any = value) -> Any:
                    return _v if node.name.lower() == "value" else None

                result = scalar.evaluate(_parse_check_expr(check["expression"]), scope, ctx)
                if result is not None and not scalar._truthy(result):
                    raise errors.SQLError(
                        "23514",
                        f"value for domain {col.domain_type} violates check "
                        f'constraint "{check["name"]}"',
                        diag={
                            "s": "public",
                            "d": str(col.domain_type),
                            "n": str(check["name"]),
                        },
                    )


def _apply_generated_columns(docs: list[dict[str, Any]], table: Any, ctx: Any) -> None:
    """Compute each ``GENERATED ALWAYS AS (expr) STORED`` column from the row's
    other columns and store the result (runs before validation so NOT NULL / CHECK
    / UNIQUE see the computed value). No-op for reflected tables."""
    from secantus.sql import scalar

    if getattr(table, "reflected", False):
        return
    gen_cols = [c for c in table.columns if c.generated is not None]
    if not gen_cols:
        return
    for doc in docs:

        def scope(node: Any, _doc: Any = doc) -> Any:
            col = table.column(node.name)
            return get_path(_doc, col.field if col is not None else node.name)

        for col in gen_cols:
            value = scalar.evaluate(_parse_check_expr(col.generated), scope, ctx)
            doc[col.field] = typemap.coerce(value, col.type_tag)


@functools.lru_cache(maxsize=256)
def _parse_expr_index(expr_sql: str) -> Any:
    import sqlglot

    return sqlglot.parse_one(expr_sql, read="postgres")


def _apply_expr_index_fields(docs: list[dict[str, Any]], table: Any, ctx: Any) -> None:
    """Compute each expression index's value from the row and store it in the index's
    hidden ``field`` (so the storage B-tree over that field stays current). Mirrors
    ``_apply_generated_columns``; runs on every insert / update post-image."""
    from secantus.sql import scalar

    if getattr(table, "reflected", False):
        return
    eis = getattr(table, "expr_indexes", None)
    if not eis:
        return
    for doc in docs:

        def scope(node: Any, _doc: Any = doc) -> Any:
            col = table.column(node.name)
            return get_path(_doc, col.field if col is not None else node.name)

        for ei in eis:
            value = scalar.evaluate(_parse_expr_index(ei.expr_sql), scope, ctx)
            doc[ei.field] = typemap.coerce(value, ei.type_tag)


def _validate_rls_check(docs: list[dict[str, Any]], table: Any, command: str, ctx: Any) -> None:
    """Enforce each row's RLS ``WITH CHECK`` predicate (#129). No-op unless the
    session enforces RLS and the table has policies enabled."""
    if getattr(table, "reflected", False):
        return
    from secantus.sql import rls

    for doc in docs:
        rls.check_write_row(doc, table, command, ctx)


def enforce_insert_rows(
    docs: list[dict[str, Any]], table: Any, storage: Any, db: str, catalog: Any, session: Any
) -> None:
    """Enforce every declared constraint against rows about to be inserted."""
    from secantus.sql import scalar

    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    _apply_generated_columns(docs, table, ctx)
    _apply_expr_index_fields(docs, table, ctx)
    _validate_rows(docs, table, storage, db, session, catalog)
    _validate_enum_columns(docs, table, catalog, db)
    _validate_domain_columns(docs, table, storage, db, session, catalog)
    _validate_unique_rows(docs, table, storage, db, session=session)
    _validate_fk_child_rows(docs, table, storage, db, catalog, session)
    _validate_rls_check(docs, table, "INSERT", ctx)


def enforce_update_images(
    post_images: list[dict[str, Any]],
    matched_ids: Any,
    table: Any,
    storage: Any,
    db: str,
    catalog: Any,
    session: Any,
) -> None:
    """Enforce constraints against an UPDATE's post-images (UNIQUE probes exclude
    the rows being rewritten). Parent-side FK actions are the caller's job."""
    from secantus.sql import scalar

    if getattr(table, "reflected", False):
        return
    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    _apply_generated_columns(post_images, table, ctx)
    _apply_expr_index_fields(post_images, table, ctx)
    for post in post_images:
        _validate_write_row(post, table, ctx)
    _validate_rls_check(post_images, table, "UPDATE", ctx)
    _validate_enum_columns(post_images, table, catalog, db)
    _validate_domain_columns(post_images, table, storage, db, session, catalog)
    _validate_unique_rows(
        post_images,
        table,
        storage,
        db,
        exclude_ids=frozenset(_hashable_id(i) for i in matched_ids),
        session=session,
    )
    _validate_fk_child_rows(post_images, table, storage, db, catalog, session)


def enforce_parent_delete(
    victims: list[dict[str, Any]], table: Any, storage: Any, db: str, catalog: Any
) -> None:
    """Apply referential actions before deleting parent rows (RESTRICT / CASCADE /
    SET NULL). Safe to call for any table — a no-op when nothing references it."""
    if catalog is None or getattr(table, "reflected", False):
        return
    _enforce_fk_on_parent_delete(victims, table, storage, db, catalog)


def _hashable_id(value: Any) -> Any:
    """A hashable key for an ``_id`` value. A composite PK's ``_id`` is a
    subdocument (dict) — unhashable — so canonicalize it to a sorted tuple of
    items; a scalar ``_id`` passes through."""
    if isinstance(value, dict):
        return tuple(sorted((k, _hashable_id(v)) for k, v in value.items()))
    if isinstance(value, list):
        return tuple(_hashable_id(v) for v in value)
    return value


def _uq_violation(uq: Any, table: Any = None) -> errors.SQLError:
    diag = _error_diag(table, n=uq.name) if table is not None else {"n": uq.name}
    if getattr(uq, "exclusion", False):
        return errors.SQLError(
            "23P01",
            f'conflicting key value violates exclusion constraint "{uq.name}"',
            diag=diag,
        )
    return errors.SQLError(
        "23505", f'duplicate key value violates unique constraint "{uq.name}"', diag=diag
    )


def _maybe_defer(session: Any, kind: str, table_name: str, constraint: Any) -> bool:
    """If ``constraint`` is DEFERRABLE and currently deferred inside an open
    transaction, record a pending re-check on the session and return True (the
    caller skips the immediate raise — the constraint is re-validated at COMMIT or
    ``SET CONSTRAINTS … IMMEDIATE``). Returns False otherwise (raise now)."""
    if session is None or getattr(session, "txn_handle", None) is None:
        return False
    if not getattr(constraint, "deferrable", False):
        return False
    if not session.constraint_is_deferred(constraint.name, constraint.initially_deferred):
        return False
    record = (kind, table_name, constraint.name)
    if record not in session.pending_deferred:
        session.pending_deferred.append(record)
    return True


def _validate_unique_rows(
    docs: list[dict[str, Any]],
    table: Any,
    storage: Any,
    db: str,
    *,
    exclude_ids: frozenset = frozenset(),
    session: Any = None,
) -> None:
    """Enforce declared UNIQUE constraints against a batch of rows (23505). A row
    with any NULL in a constraint's columns is exempt (NULLs are distinct in a
    UNIQUE constraint). Duplicates *within* the batch collide, and each row is
    probed against stored rows (``exclude_ids`` skips the rows an UPDATE is
    rewriting, so a row keeping its own value doesn't conflict with itself). A
    deferred constraint records a pending re-check instead of raising."""
    if getattr(table, "reflected", False) or not table.unique_constraints:
        return
    seen: dict[str, set] = {uq.name: set() for uq in table.unique_constraints}
    for doc in docs:
        for uq in table.unique_constraints:
            fields = [table.field_for(c) for c in uq.columns]
            key = tuple(get_path(doc, f) for f in fields)
            if any(v is None for v in key):
                continue
            violated = key in seen[uq.name]
            if not violated:
                probe = dict(zip(fields, key, strict=True))
                # Two probes, because neither alone is sufficient inside a
                # transaction: the plain one sees this transaction's own
                # uncommitted rows, and the committed one sees rows other
                # transactions committed after our snapshot was taken (which
                # the snapshot hides, so the duplicate used to be stored).
                candidates = list(storage.find_matching(db, table.collection, probe))
                committed = getattr(storage, "find_matching_committed", None)
                if committed is not None:
                    candidates += committed(db, table.collection, probe)
                for existing in candidates:
                    if _hashable_id(existing.get("_id")) not in exclude_ids:
                        violated = True
                        break
            if violated:
                if _maybe_defer(session, "unique", table.name, uq):
                    continue
                raise _uq_violation(uq, table)
            seen[uq.name].add(key)


def _execute_insert_on_conflict(
    plan: planner.InsertPlan, storage: Any, db: str, catalog: Catalog | None, session: Any
) -> SQLResult:
    """Execute ``INSERT … ON CONFLICT``. Each proposed row is probed against the
    conflict target: a clean row inserts; a conflicting row is skipped
    (``DO NOTHING``) or updated in place (``DO UPDATE``). The command tag counts
    rows inserted *or* updated (matching Postgres); skipped rows don't count.
    ``RETURNING`` projects the inserted and updated rows, not the skipped ones."""
    from secantus.sql import scalar

    oc = plan.on_conflict
    coll = plan.table.collection
    sctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    affected = 0
    result_docs: list[dict[str, Any]] = []
    for doc in plan.docs:
        _assign_sequences([doc], plan.table, db, catalog, session)
        if plan.returning is not None:
            doc.setdefault("_id", bson.ObjectId())
        existing = _find_conflict(storage, db, coll, oc, doc)
        if existing is None:
            # Full enforcement — including any UNIQUE / CHECK / FK other than the
            # arbiter target (which _find_conflict already cleared).
            if plan.check_option is not None:
                _validate_check_option([doc], plan.check_option, plan.table, sctx)
            enforce_insert_rows([doc], plan.table, storage, db, catalog, session)
            inserted, write_errors = storage.insert(db, coll, [doc])
            if write_errors:
                # A bare ``DO NOTHING`` (no conflict target) absorbs any unique
                # collision — including one on an index other than the probed
                # target; with a target, a collision elsewhere is still an error.
                if not oc.conflict_fields and write_errors[0].get("code") == 11000:
                    continue
                _raise_write_error(write_errors[0], plan.table)
            affected += inserted
            if inserted:
                result_docs.append(doc)
            continue
        if oc.action == "nothing":
            continue
        updated = _apply_conflict_update(plan.table, storage, db, oc, existing, doc, sctx)
        if updated is not None:
            # DO UPDATE post-image must also satisfy a view's CHECK OPTION.
            if plan.check_option is not None:
                _validate_check_option([updated], plan.check_option, plan.table, sctx)
            affected += 1
            result_docs.append(updated)
    tag = f"INSERT 0 {affected}"
    if plan.returning is not None:
        return _returning_result(
            result_docs, plan.returning, tag, affected, plan.table, storage, db
        )
    return SQLResult(command_tag=tag, rowcount=affected)


def _find_conflict(
    storage: Any, db: str, coll: str, oc: planner.OnConflict, doc: dict[str, Any]
) -> dict[str, Any] | None:
    """The existing row that ``doc`` would conflict with on the conflict target,
    or None. A row with any target field unset can't collide (a fresh PK is
    unique), so it short-circuits to an insert."""
    # A composite-PK conflict field is a dotted path (``_id.a``) into the ``_id``
    # subdocument, so probe with has_path / get_path, not flat dict access.
    if not oc.conflict_fields or any(not has_path(doc, f) for f in oc.conflict_fields):
        return None
    found = storage.find_matching(
        db, coll, {f: get_path(doc, f) for f in oc.conflict_fields}, limit=1
    )
    return found[0] if found else None


def _apply_conflict_update(
    table: planner.TableDef,
    storage: Any,
    db: str,
    oc: planner.OnConflict,
    existing: dict[str, Any],
    excluded: dict[str, Any],
    sctx: Any,
) -> dict[str, Any] | None:
    """Apply a ``DO UPDATE`` to the conflicting ``existing`` row. ``EXCLUDED``
    references resolve to the proposed ``excluded`` row; bare / target-qualified
    columns to the existing row. Returns the post-update doc, or None when a
    ``WHERE`` predicate gates the update out (the row is left untouched)."""
    import copy

    from secantus.sql import scalar

    def scope(node: Any) -> Any:
        field = table.field_for(node.name)
        tbl = (node.table or "").lower()
        source = excluded if tbl == "excluded" else existing
        return get_path(source, field)  # field may be a dotted composite-PK path

    if oc.where is not None and not scalar._truthy(scalar.evaluate(oc.where, scope, sctx)):
        return None
    set_doc: dict[str, Any] = {}
    for field, type_tag, expr in oc.set_exprs:
        set_doc[field] = typemap.coerce(scalar.evaluate(expr, scope, sctx), type_tag)
    updated = copy.deepcopy(existing)
    updated.update(set_doc)
    # Enforce every constraint on the DO UPDATE post-image (UNIQUE excludes the
    # row itself; NOT NULL / CHECK / FK-child all apply).
    enforce_update_images(
        [updated], [existing["_id"]], table, storage, db, sctx.catalog, sctx.session
    )
    storage.update_matching(db, table.collection, {"_id": existing["_id"]}, {"$set": set_doc})
    return updated


def execute_constant_select(plan: planner.ConstantSelectPlan) -> SQLResult:
    oids = plan.pg_oids or [None] * len(plan.columns)
    typmods = plan.typmods or [-1] * len(plan.columns)
    columns = [
        ColumnDesc(name, tag, oid if oid is not None else typemap.PG_OID.get(tag, 25), typmod)
        for (name, tag, _), oid, typmod in zip(plan.columns, oids, typmods, strict=True)
    ]
    rows = [tuple(value for _, _, value in plan.columns)] if plan.emit else []
    return SQLResult(
        command_tag=f"SELECT {len(rows)}", columns=columns, rows=rows, rowcount=len(rows)
    )


def execute_select(plan: planner.SelectPlan, storage: Any, db: str) -> SQLResult:
    if plan.count_star:
        # COUNT(*) ignores LIMIT/OFFSET — count everything the filter matches.
        n = len(storage.find_matching(db, plan.table.collection, plan.filter))
        return SQLResult(
            command_tag="SELECT 1",
            columns=[ColumnDesc(plan.count_alias, "int8", typemap.PG_OID["int8"])],
            rows=[(n,)],
            rowcount=1,
        )

    if plan.order:
        # NULL placement follows Postgres, not Mongo sort order, so order in
        # Python; that also pulls OFFSET/LIMIT off the storage fetch.
        docs = storage.find_matching(db, plan.table.collection, plan.filter)
        key_of = _order_key_fn(plan.order, plan.enum_orders, getattr(plan, "citext_orders", None))
        _pg_sort(docs, key_of, [(direction, nf) for _, direction, nf in plan.order])
        if plan.skip:
            docs = docs[plan.skip :]
        if plan.limit:
            docs = docs[: plan.limit]
    else:
        docs = storage.find_matching(
            db, plan.table.collection, plan.filter, skip=plan.skip, limit=plan.limit
        )
    columns = _out_column_descs(plan.out_columns, storage, db, getattr(plan, "table", None))
    rows: list[tuple[Any, ...]] = []
    for doc in docs:
        # get_path walks dotted field paths (jsonb navigation); a plain field
        # name resolves to a top-level lookup.
        rows.append(
            tuple(typemap.to_py(_with_subms(doc, col), col.type_tag) for _, col in plan.out_columns)
        )
    return SQLResult(
        command_tag=f"SELECT {len(rows)}", columns=columns, rows=rows, rowcount=len(rows)
    )


def execute_correlated_select(
    plan: planner.CorrelatedSelectPlan, storage: Any, db: str, catalog: Catalog, session: Any
) -> SQLResult:
    """Execute a single-table SELECT whose WHERE references the outer row.

    The WHERE can't push down to a Mongo filter, so we fetch every candidate row
    (in ORDER BY order) and evaluate the predicate per row with the scalar
    evaluator — a correlated subquery reads inner-table rows through the same
    storage view, with outer-row references falling through to ``scope``."""
    from secantus.sql import scalar

    sctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    table = plan.table
    docs = storage.find_matching(db, table.collection, {})

    def make_scope(doc: dict[str, Any]):
        # Only outer-table columns reach this scope; a correlated subquery's own
        # inner columns are resolved inside scalar._inner_row_scopes.
        def scope(node: Any) -> Any:
            return get_path(doc, table.field_for(node.name))

        return scope

    def keep(doc: dict[str, Any]) -> bool:
        result = scalar.evaluate(plan.where, make_scope(doc), sctx)
        return bool(result) if result is not None else False

    matched = [doc for doc in docs if keep(doc)]

    if plan.count_star:
        return SQLResult(
            command_tag="SELECT 1",
            columns=[ColumnDesc(plan.count_alias, "int8", typemap.PG_OID["int8"])],
            rows=[(len(matched),)],
            rowcount=1,
        )

    # Order the survivors (Postgres NULL placement), then slice OFFSET/LIMIT. An
    # enum-typed ORDER BY key sorts by declared label order (``plan.enum_orders``).
    if plan.order:
        _pg_sort(
            matched,
            _order_key_fn(plan.order, plan.enum_orders),
            [(direction, nf) for _, direction, nf in plan.order],
        )
    if plan.skip:
        matched = matched[plan.skip :]
    if plan.limit:
        matched = matched[: plan.limit]

    columns = _out_column_descs(plan.out_columns, storage, db, getattr(plan, "table", None))
    rows = [
        tuple(typemap.to_py(_with_subms(doc, col), col.type_tag) for _, col in plan.out_columns)
        for doc in matched
    ]
    return SQLResult(
        command_tag=f"SELECT {len(rows)}", columns=columns, rows=rows, rowcount=len(rows)
    )


def _scalar_ctx(storage: Any, db: str, sctx: Any) -> Any:
    """The ScalarContext for evaluating derived sub-plans — reuse the caller's
    when present, else build one over the (catalog-backed) storage."""
    if sctx is not None:
        return sctx
    from secantus.sql import scalar
    from secantus.sql.catalog import Catalog

    return scalar.ScalarContext(storage=storage, catalog=Catalog(storage), db=db, session=None)


def _materialize_derived(plan: Any, storage: Any, db: str, sctx: Any = None) -> None:
    """Materialize a plan's derived-table subqueries into ephemeral collections.

    Each ``(SELECT ...) AS alias`` join source is run to rows (its own derived
    tables first) and registered under its alias so the main pipeline's
    ``$lookup`` can read it."""
    for dt in getattr(plan, "derived", []):
        rows = _run_subplan_to_docs(dt.plan, storage, db, sctx)
        storage.register_ephemeral(dt.name, rows)


def _run_subplan_to_docs(
    plan: Any, storage: Any, db: str, sctx: Any = None
) -> list[dict[str, Any]]:
    from secantus.aggregate import PipelineContext, apply_pipeline

    if isinstance(plan, (planner.exp.Expression, planner.RawDerived)):
        # A raw statement sub-plan (a set-operation or VALUES derived table) —
        # run it through the engine and shape the result rows into docs,
        # renaming positionally when the alias declared column names.
        from secantus.sql import engine

        stmt = plan.stmt if isinstance(plan, planner.RawDerived) else plan
        rename = plan.names if isinstance(plan, planner.RawDerived) else None
        res = engine._run_query(
            stmt, storage, db, getattr(sctx, "catalog", None), getattr(sctx, "session", None)
        )
        names = rename or [c.name for c in res.columns]
        return [dict(zip(names, row, strict=True)) for row in res.rows]
    _materialize_derived(plan, storage, db, sctx)
    if isinstance(plan, planner.PipelineSelectPlan):
        docs, remaining = _pipeline_input_docs(plan, storage, db, sctx)
        ctx = PipelineContext(storage=storage, db_name=db, coll_name=plan.base_collection)
        return _apply_post_aggregates(plan, apply_pipeline(docs, remaining, ctx))
    if isinstance(plan, planner.EvaluatedSelectPlan):
        rows = _evaluated_value_rows(plan, storage, db, _scalar_ctx(storage, db, sctx))
        names = [n for n, _ in plan.out_columns]
        return [dict(zip(names, row, strict=True)) for row in rows]
    raise errors.feature_not_supported("unsupported derived-table plan")


def _expand_lateral(
    docs: list[dict[str, Any]],
    lat: Any,
    resolve: Any,
    storage: Any,
    db: str,
    sctx: Any,
) -> list[dict[str, Any]]:
    """Nested-loop expansion of one rich ``JOIN LATERAL``: for each outer ``doc``,
    substitute its column values into the correlated subquery, run the (now plain)
    inner query in full, and pair each inner row with the outer row under
    ``doc[lat.alias]``. LEFT keeps an outer row whose subquery is empty (inner
    columns NULL); INNER/CROSS drops it."""
    from secantus.sql import engine

    out: list[dict[str, Any]] = []
    for doc in docs:

        def value_of(col: Any, _doc: dict[str, Any] = doc) -> Any:
            return get_path(_doc, resolve(col)[0])

        substituted = planner._substitute_outer_columns(lat.select, lat.inner_aliases, value_of)
        res = engine.run_inner_select(substituted, storage, db, sctx.catalog, sctx.session)
        names = [c.name for c in res.columns]
        if res.rows:
            for row in res.rows:
                nd = dict(doc)
                nd[lat.alias] = {names[i]: row[i] for i in range(len(names))}
                out.append(nd)
        elif lat.side == "LEFT":
            nd = dict(doc)
            nd[lat.alias] = dict.fromkeys(names)
            out.append(nd)
    return out


def _evaluated_value_rows(
    plan: planner.EvaluatedSelectPlan, storage: Any, db: str, sctx: Any
) -> list[tuple[Any, ...]]:
    """Run an evaluated plan's pipeline, evaluate each output expression per row
    (expanding set-returning functions), then apply ORDER BY / DISTINCT / LIMIT."""
    from secantus.aggregate import PipelineContext, apply_pipeline
    from secantus.sql import scalar, window

    _materialize_derived(plan, storage, db, sctx)
    docs = storage.find_matching(db, plan.base_collection, plan.base_filter)
    ctx = PipelineContext(storage=storage, db_name=db, coll_name=plan.base_collection)
    # A correlated / EXISTS WHERE that couldn't push into ``base_filter`` filters
    # *before* the pipeline's ``$group`` (WHERE precedes grouping). ``pre_where_split``
    # leading stages (a JOIN's $lookup/$unwind prefix) run first so the residual sees
    # the joined rows; then the rest of the pipeline runs over the survivors.
    if plan.pre_where is not None:
        split = plan.pre_where_split
        if split:
            docs = apply_pipeline(docs, plan.pipeline[:split], ctx)
        presolve = plan.pre_where_resolve

        def keep_base(doc: dict[str, Any]) -> bool:
            r = scalar.evaluate(plan.pre_where, lambda n: get_path(doc, presolve(n)[0]), sctx)
            return bool(r) if r is not None else False

        docs = apply_pipeline([d for d in docs if keep_base(d)], plan.pipeline[split:], ctx)
    else:
        docs = apply_pipeline(docs, plan.pipeline, ctx)
    if not docs:
        # An implicit whole-table aggregate over zero input rows still returns one
        # row (same synthesis as execute_pipeline_select, here for the
        # group-then-evaluate path: ``SELECT -AVG(x) FROM t WHERE <matches nothing>``).
        tail = (
            plan.pipeline[plan.pre_where_split :] if plan.pre_where is not None else plan.pipeline
        )
        synthesized = _empty_implicit_aggregate_row(tail, ctx)
        if synthesized is not None:
            docs = synthesized
    # A correlated / EXISTS WHERE that couldn't push into the pipeline is applied
    # per joined row here (before windows / projection see the survivors); the
    # scope resolves outer columns via the join resolver, and the subquery reads
    # its inner rows through the same storage view.
    if plan.where is not None:

        def keep(doc: dict[str, Any]) -> bool:
            def scope(node: Any) -> Any:
                return get_path(doc, plan.resolve(node)[0])

            r = scalar.evaluate(plan.where, scope, sctx)
            return bool(r) if r is not None else False

        docs = [d for d in docs if keep(d)]
    # Rich JOIN LATERAL sources: expand each outer doc by running the correlated
    # subquery with this row's values substituted (nested-loop LATERAL), before
    # windows / projection see the rows.
    for lat in getattr(plan, "lateral_joins", None) or []:
        docs = _expand_lateral(docs, lat, plan.resolve, storage, db, sctx)
    # Window functions depend on the whole partition, so they're computed over all
    # rows up front and stored on each doc; the scope resolves an exp.Window node
    # (keyed by id) to that precomputed field.
    win_field = window.compute_windows(plan.out_exprs, docs, plan.resolve, sctx)

    def make_scope(doc: dict[str, Any]):
        def scope(node: Any) -> Any:
            field = win_field.get(id(node))
            if field is not None:
                return get_path(doc, field)
            path, _ = plan.resolve(node)
            return get_path(doc, path)

        # Optional protocol: lets ``scalar._eval_cast`` learn a column's type
        # tag (a naive datetime from storage is a ``timestamp`` or a decoded
        # ``timestamptz`` — only the tag can tell, and ``col::text`` must
        # render a timestamptz with the session-zone offset like Postgres).
        def column_tag(node: Any) -> str | None:
            try:
                _, tag = plan.resolve(node)
            except Exception:  # noqa: BLE001 — unresolvable: no tag claim
                return None
            return tag

        scope.column_tag = column_tag  # type: ignore[attr-defined]
        return scope

    # An enum ORDER BY term sorts by the label's declared ordinal, not lexically.
    enum_ordinals = {
        i: {lbl: k for k, lbl in enumerate(labels)}
        for i, labels in getattr(plan, "enum_orders", {}).items()
    }

    def _order_key(oe: Any, i: int, scope: Any) -> Any:
        v = scalar.evaluate(oe, scope, sctx)
        omap = enum_ordinals.get(i)
        return omap.get(v, len(omap)) if omap is not None and v is not None else v

    scored: list[tuple[tuple[Any, ...], tuple[Any, ...], tuple[Any, ...]]] = []
    for doc in docs:
        scope = make_scope(doc)
        keys = tuple(_order_key(oe, i, scope) for i, (oe, _, _) in enumerate(plan.order))
        # DISTINCT ON key (row-level, evaluated before any SRF expansion).
        don_key = (
            tuple(repr(scalar.evaluate(e, scope, sctx)) for e in plan.distinct_on)
            if plan.distinct_on
            else ()
        )
        for vt in _expand_srf(plan, scope, sctx):
            scored.append((keys, don_key, vt))

    _pg_sort(scored, lambda r: r[0], [(direction, nf) for _, direction, nf in plan.order])

    if plan.distinct_on:
        # Keep the first row (in the sorted order above) per DISTINCT ON key.
        seen_on: set = set()
        rows = []
        for _keys, don_key, vt in scored:
            if don_key not in seen_on:
                seen_on.add(don_key)
                rows.append(vt)
    else:
        rows = [vt for _, _, vt in scored]
        if plan.distinct:
            seen: set = set()
            deduped: list[tuple[Any, ...]] = []
            for row in rows:
                key = tuple(repr(v) for v in row)
                if key not in seen:
                    seen.add(key)
                    deduped.append(row)
            rows = deduped
    if plan.skip:
        rows = rows[plan.skip :]
    if plan.limit:
        rows = rows[: plan.limit]
    return rows


def _expandarray_cell(kind: str, arr: list[Any], k: int) -> Any:
    """One output cell for ``information_schema._pg_expandarray(arr)``.

    The function yields a ``(x, n)`` record per element — the value and its
    1-based subscript. ``kind`` carries which field was selected
    (``_pg_expandarray.n`` / ``.x``); the bare kind means the whole record was
    selected, and it stays a subdocument rather than composite text so that
    field access still works a level up. pgjdbc selects the record into a
    subquery column and then reads ``(result.KEYS).x`` from the outer query,
    which needs the composite intact.
    """
    if k >= len(arr):
        return None
    field = kind.partition(".")[2]
    if field == "n":
        return k + 1
    if field == "x":
        return arr[k]
    return {"x": arr[k], "n": k + 1}


def _expand_srf(plan: planner.EvaluatedSelectPlan, scope: Any, sctx: Any) -> list[tuple[Any, ...]]:
    """Evaluate one source row's output columns, expanding set-returning
    functions (unnest / generate_subscripts) into one row per array element.
    Multiple SRFs over the same array zip in parallel; the longest wins, shorter
    arrays pad with NULL (Postgres semantics)."""
    from secantus.sql import scalar

    srfs: dict[int, tuple[str, list[Any]]] = {}
    for idx, expr in enumerate(plan.out_exprs):
        srf = planner._srf_of(expr)
        if srf is None:
            continue
        kind, arr_expr = srf
        val = scalar.evaluate(arr_expr, scope, sctx)
        if kind == "jsonb_object_keys":
            items = list(val.keys()) if isinstance(val, dict) else []
        elif isinstance(val, (list, tuple)):
            items = list(val)
        else:
            items = [] if val is None else [val]
        srfs[idx] = (kind, items)

    if not srfs:
        return [tuple(scalar.evaluate(e, scope, sctx) for e in plan.out_exprs)]

    scalars = {
        idx: scalar.evaluate(e, scope, sctx)
        for idx, e in enumerate(plan.out_exprs)
        if idx not in srfs
    }
    length = max((len(arr) for _, arr in srfs.values()), default=0)
    rows: list[tuple[Any, ...]] = []
    for k in range(length):
        row: list[Any] = []
        for idx in range(len(plan.out_exprs)):
            if idx in srfs:
                kind, arr = srfs[idx]
                if kind == "generate_subscripts":  # 1-based ordinal
                    row.append(k + 1 if k < len(arr) else None)
                elif kind.startswith("_pg_expandarray"):
                    row.append(_expandarray_cell(kind, arr, k))
                else:  # unnest / jsonb_array_elements / jsonb_object_keys → element/key
                    row.append(arr[k] if k < len(arr) else None)
            else:
                row.append(scalars[idx])
        rows.append(tuple(row))
    return rows


def _pipeline_input_docs(
    plan: planner.PipelineSelectPlan, storage: Any, db: str, sctx: Any
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    """Fetch the docs to feed the aggregation and return ``(docs, remaining_stages)``.

    A correlated / EXISTS residual WHERE can't lower to a ``$match``, so it's
    applied in Python: the leading ``residual_split`` stages (a JOIN prefix, or
    none for a single-table GROUP BY) run first, the rows are filtered, and the
    remaining stages (the ``$group`` etc.) run over the survivors."""
    from secantus.aggregate import PipelineContext, apply_pipeline
    from secantus.sql import scalar

    docs = storage.find_matching(db, plan.base_collection, plan.base_filter)
    if getattr(plan, "pre_eval_fields", None):
        # Materialize computed GROUP BY keys the aggregation engine can't
        # lower — evaluated per doc by the scalar engine before the pipeline.
        resolve = plan.pre_eval_resolve
        sc0 = sctx or _scalar_ctx(storage, db, None)
        docs = [dict(d) for d in docs]
        for doc in docs:

            def scope(node: Any, _doc: dict[str, Any] = doc) -> Any:
                return get_path(_doc, resolve(node)[0])

            for fname, ast in plan.pre_eval_fields.items():
                doc[fname] = scalar.evaluate(ast, scope, sc0)
    if plan.residual_where is None:
        return docs, plan.pipeline
    if plan.residual_split:
        ctx = PipelineContext(storage=storage, db_name=db, coll_name=plan.base_collection)
        docs = apply_pipeline(docs, plan.pipeline[: plan.residual_split], ctx)
    remaining = plan.pipeline[plan.residual_split :]
    sc = sctx or _scalar_ctx(storage, db, None)
    resolve = plan.residual_resolve

    def keep(doc: dict[str, Any]) -> bool:
        def scope(node: Any) -> Any:
            return get_path(doc, resolve(node)[0])

        r = scalar.evaluate(plan.residual_where, scope, sc)
        return bool(r) if r is not None else False

    return [d for d in docs if keep(d)], remaining


def _ordered_set_value(kind: str, fraction: float | None, values: Any) -> Any:
    """Compute an ordered-set aggregate from the pushed ORDER BY values (NULLs
    dropped, then sorted ascending):

    - ``percentile_cont(f)``: continuous percentile with linear interpolation
      between the two nearest ranks (``f`` in [0, 1]).
    - ``percentile_disc(f)``: the first value whose cumulative fraction ≥ ``f``.
    - ``mode``: the most frequent value; on a tie, the smallest.

    Returns NULL when the (non-NULL) set is empty."""
    import math

    vals = sorted(v for v in (values or []) if v is not None)
    if not vals:
        return None
    n = len(vals)
    if kind == "mode":
        best_val, best_count = vals[0], 0
        i = 0
        while i < n:
            j = i
            while j < n and vals[j] == vals[i]:
                j += 1
            if j - i > best_count:
                best_val, best_count = vals[i], j - i
            i = j
        return best_val
    assert fraction is not None
    if kind == "percentile_disc":
        # Smallest index whose 1-based position / n ≥ fraction.
        idx = 0 if fraction == 0 else min(math.ceil(fraction * n) - 1, n - 1)
        return vals[max(idx, 0)]
    # percentile_cont: linear interpolation between the neighbouring ranks.
    rank = fraction * (n - 1)
    lo = math.floor(rank)
    hi = math.ceil(rank)
    if lo == hi:
        return float(vals[lo])
    frac = rank - lo
    return float(vals[lo]) + (float(vals[hi]) - float(vals[lo])) * frac


def _apply_post_aggregates(plan: Any, result: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Finish any ordered / ordered-set aggregates in a pipeline plan's result
    (Python-side, since the aggregation engine can't sort). Shared by the top-level
    pipeline executor and derived-table materialization so the ``{v, k}`` push
    pairs never leak past either path."""
    for field_name, kind, payload in getattr(plan, "post_aggregates", ()) or ():
        for doc in result:
            if kind in ("sorted_array", "sorted_string"):
                doc[field_name] = _sorted_agg_value(kind, payload, doc.get(field_name))
            elif kind in ("variance", "bit_and", "bit_or", "bit_xor"):
                doc[field_name] = _stat_bit_value(kind, doc.get(field_name))
            elif kind == "range_agg":
                from secantus.sql import ranges as _ranges

                doc[field_name] = _ranges.make_multirange(doc.get(field_name) or [])
            else:
                doc[field_name] = _ordered_set_value(kind, payload, doc.get(field_name))
    return result


def _stat_bit_value(kind: str, value: Any) -> Any:
    """Finish a statistical / bitwise aggregate. ``variance`` squares the pushed
    stdDev (NULL stays NULL — matching Postgres' single-row sample variance);
    the ``bit_*`` kinds fold the pushed integers (NULLs skipped, NULL when empty)."""
    if kind == "variance":
        if value is None:
            return None
        v = float(value)
        return v * v
    ints = [int(x) for x in (value or []) if x is not None]
    if not ints:
        return None
    if kind == "bit_and":
        return functools.reduce(operator.and_, ints)
    if kind == "bit_or":
        return functools.reduce(operator.or_, ints)
    return functools.reduce(operator.xor, ints)  # bit_xor


def _sorted_agg_value(kind: str, payload: Any, pairs: Any) -> Any:
    """Compute an ordered ``array_agg`` / ``string_agg`` from the pushed ``{v, k}``
    pairs: sort by the key list ``k`` (Postgres ORDER BY semantics per key), then
    return the ``v`` values as a list (``sorted_array``) or joined with the
    separator, skipping NULLs (``sorted_string`` — NULL when all values are NULL)."""
    items = list(pairs or [])
    if kind == "sorted_array":
        specs = payload
        _pg_sort(items, lambda p: tuple(p.get("k") or []), specs)
        return [p.get("v") for p in items]
    specs, sep = payload  # sorted_string
    _pg_sort(items, lambda p: tuple(p.get("k") or []), specs)
    parts = [str(p.get("v")) for p in items if p.get("v") is not None]
    return sep.join(parts) if parts else None


def _empty_implicit_aggregate_row(pipeline: list, ctx: Any) -> list | None:
    """One synthesized row for an implicit whole-table aggregate over zero rows.

    Postgres returns a single row from ``SELECT AVG(x) FROM t WHERE false`` —
    count-shaped accumulators are 0, every other aggregate is NULL. Mongo's
    ``$group`` with ``_id: null`` over empty input emits nothing, so the row is
    built here and run through the stages after the ``$group`` (projection,
    computed fields)."""
    from secantus.aggregate import apply_pipeline

    gidx = next(
        (
            i
            for i, st in enumerate(pipeline)
            if isinstance(st, dict) and "$group" in st and st["$group"].get("_id") is None
        ),
        None,
    )
    if gidx is None:
        return None

    def _is_count_acc(spec: Any) -> bool:
        if not (isinstance(spec, dict) and "$sum" in spec):
            return False
        body = spec["$sum"]
        if isinstance(body, int):
            return True  # {"$sum": 1} — COUNT(*)
        return (
            isinstance(body, dict)
            and "$cond" in body
            and isinstance(body["$cond"], list)
            and body["$cond"][1:] == [1, 0]  # COUNT(col) / FILTERed count
        )

    doc: dict[str, Any] = {"_id": None}
    for name, spec in pipeline[gidx]["$group"].items():
        if name == "_id":
            continue
        if _is_count_acc(spec):
            doc[name] = 0
        elif isinstance(spec, dict) and ("$push" in spec or "$addToSet" in spec):
            # Pushed / distinct-set collections (bit_agg / sorted aggs / DISTINCT
            # reductions) fold over [].
            doc[name] = []
        else:
            doc[name] = None
    return apply_pipeline([doc], pipeline[gidx + 1 :], ctx)


def execute_pipeline_select(
    plan: planner.PipelineSelectPlan, storage: Any, db: str, sctx: Any = None
) -> SQLResult:
    """Run a JOIN / GROUP BY / aggregate SELECT through the aggregation engine."""
    from secantus.aggregate import PipelineContext, apply_pipeline

    _materialize_derived(plan, storage, db, sctx)
    docs, remaining = _pipeline_input_docs(plan, storage, db, sctx)
    ctx = PipelineContext(storage=storage, db_name=db, coll_name=plan.base_collection)
    result = _apply_post_aggregates(plan, apply_pipeline(docs, remaining, ctx))
    if not result:
        synthesized = _empty_implicit_aggregate_row(remaining, ctx)
        if synthesized is not None:
            result = _apply_post_aggregates(plan, synthesized)
    columns = _tagged_out_column_descs(
        plan.out_columns,
        plan.out_enum_types,
        storage,
        db,
        out_sources=getattr(plan, "out_sources", None) or None,
    )
    rows = [
        tuple(typemap.to_py(doc.get(name), tag) for name, tag in plan.out_columns) for doc in result
    ]
    return SQLResult(
        command_tag=f"SELECT {len(rows)}", columns=columns, rows=rows, rowcount=len(rows)
    )


def execute_evaluated_select(
    plan: planner.EvaluatedSelectPlan, storage: Any, db: str, sctx: Any
) -> SQLResult:
    """Run a SELECT whose list / ORDER BY needs per-row evaluation (scalar /
    set-returning functions, CASE, correlated subqueries)."""
    rows = _evaluated_value_rows(plan, storage, db, sctx)
    columns = _tagged_out_column_descs(
        plan.out_columns,
        plan.out_enum_types,
        storage,
        db,
        out_exprs=plan.out_exprs,
        base_table=getattr(plan, "base_table", None),
        out_sources=getattr(plan, "out_sources", None) or None,
    )
    out_rows = [
        tuple(typemap.to_py(v, tag) for v, (_, tag) in zip(row, plan.out_columns, strict=True))
        for row in rows
    ]
    return SQLResult(
        command_tag=f"SELECT {len(out_rows)}",
        columns=columns,
        rows=out_rows,
        rowcount=len(out_rows),
    )


@_serialized_write
def execute_update(
    plan: planner.UpdatePlan, storage: Any, db: str, catalog: Any = None, session: Any = None
) -> SQLResult:
    if getattr(plan, "rekey", False) or getattr(plan, "computed", None):
        return _execute_update_materialized(plan, storage, db, catalog, session)
    coll = plan.table.collection
    _validate_update_post_images(plan, storage, db, session, catalog)
    if plan.check_option is not None:
        from secantus.sql import scalar
        from secantus.update import apply_update

        matched_rows = storage.find_matching(db, coll, plan.filter)
        _validate_check_option(
            [apply_update(d, plan.update) for d in matched_rows],
            plan.check_option,
            plan.table,
            scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session),
        )
    reflected = getattr(plan.table, "reflected", False)
    gen_cols = [c for c in plan.table.columns if c.generated is not None] if not reflected else []
    expr_idxs = list(getattr(plan.table, "expr_indexes", [])) if not reflected else []
    # A generated column / expression index depends on each row's post-image, so the
    # bulk ``$set`` can't carry it; capture the target ids first, then recompute and
    # persist per row after the update lands.
    if plan.returning is not None or gen_cols or expr_idxs:
        ids = [d["_id"] for d in storage.find_matching(db, coll, plan.filter)]
    res = storage.update_matching(db, coll, plan.filter, plan.update, multi=True)
    matched = int(res["matched"])
    if (gen_cols or expr_idxs) and ids:
        from secantus.sql import scalar

        ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
        for doc in storage.find_matching(db, coll, {"_id": {"$in": ids}}):
            _apply_generated_columns([doc], plan.table, ctx)
            _apply_expr_index_fields([doc], plan.table, ctx)
            gset = {c.field: doc[c.field] for c in gen_cols}
            gset.update({ei.field: doc[ei.field] for ei in expr_idxs})
            storage.update_matching(db, coll, {"_id": doc["_id"]}, {"$set": gset}, multi=False)
    if plan.returning is not None:
        post = storage.find_matching(db, coll, {"_id": {"$in": ids}}) if ids else []
        return _returning_result(
            post, plan.returning, f"UPDATE {matched}", matched, plan.table, storage, db
        )
    return SQLResult(command_tag=f"UPDATE {matched}", rowcount=matched)


def _execute_update_materialized(
    plan: planner.UpdatePlan, storage: Any, db: str, catalog: Any, session: Any
) -> SQLResult:
    # Outside a transaction block, the whole read-compute-write must be ONE
    # WT snapshot transaction. It used to be a bare read followed by
    # autocommit writes, atomic only because nothing could COMMIT between
    # them under the statement-write lock — but an extended-protocol implicit
    # transaction commits at Sync, outside that lock, and its commit landing
    # inside the window was silently overwritten by a value computed from the
    # pre-commit row (the deterministic straddle in
    # test_sync_commit_serializes_with_bare_statements). Inside a snapshot
    # transaction the mid-window commit surfaces as a write conflict and the
    # statement retries from a fresh read.
    if session is not None and getattr(session, "txn_handle", None) is None:
        from secantus.storage import WriteConflictError, _is_wt_rollback

        while True:
            handle = storage.begin_user_transaction()
            try:
                with storage.use_user_transaction(handle):
                    result = _execute_update_materialized_body(plan, storage, db, catalog, session)
                storage.commit_user_transaction(handle)
                return result
            except errors.SQLError as exc:
                storage.abort_user_transaction(handle)
                if exc.sqlstate == "40001":
                    continue  # statement-level retry, like the autocommit path
                raise
            except WriteConflictError:
                storage.abort_user_transaction(handle)
                continue
            except Exception as exc:
                storage.abort_user_transaction(handle)
                if _is_wt_rollback(exc):
                    continue  # raw WT rollback from a storage op inside the txn
                raise
    return _execute_update_materialized_body(plan, storage, db, catalog, session)


def _execute_update_materialized_body(
    plan: planner.UpdatePlan, storage: Any, db: str, catalog: Any, session: Any
) -> SQLResult:
    """An UPDATE that must be materialized per row rather than a bulk ``$set``:

    * ``SET col = <expr>`` (``plan.computed``) — each RHS is evaluated against the
      *old* row (Postgres semantics: every assignment sees the pre-image), and
    * a primary-key change (``plan.rekey``) — ``_id`` is immutable, so the row is
      deleted and re-inserted under the new key.

    Post-images are computed and validated (NOT NULL / CHECK / UNIQUE / FK / enum /
    domain / generated) before any write, so a violating UPDATE leaves the table
    unchanged. A re-key also duplicate-checks each new ``_id``."""
    import copy

    from secantus.paths import get_path, set_path
    from secantus.sql import scalar
    from secantus.update import apply_update

    table = plan.table
    coll = table.collection
    set_doc = plan.update.get("$set", {})
    id_sets = {k: v for k, v in set_doc.items() if k == "_id" or k.startswith("_id.")}
    other_sets = {k: v for k, v in set_doc.items() if k not in id_sets}
    ctx = (
        scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
        if plan.computed
        else None
    )

    def scope_for(doc: dict[str, Any]):
        def scope(node: Any) -> Any:
            col = table.column(node.name)
            return get_path(doc, col.field if col is not None else table.field_for(node.name))

        return scope

    def post_image(doc: dict[str, Any]) -> dict[str, Any]:
        # ``apply_update`` refuses to touch ``_id`` (immutable in Mongo), so PK sets
        # are written by path; computed RHS see the old row via ``scope``.
        new = copy.deepcopy(doc)
        if other_sets:
            new = apply_update(new, {"$set": other_sets})
        for k, v in id_sets.items():
            set_path(new, k, v)
        if plan.computed:
            scope = scope_for(doc)
            for field, tag, expr in plan.computed:
                val = scalar.evaluate(expr, scope, ctx)
                if tag != "any":
                    val = typemap.coerce(val, tag)
                set_path(new, field, val)
        return new

    matched = storage.find_matching(db, coll, plan.filter)
    posts = [post_image(doc) for doc in matched]
    # Shared post-image validation (also applies generated columns to ``posts``).
    _validate_update_post_images(
        plan, storage, db, session, catalog, matched=matched, post_images=posts
    )
    if plan.check_option is not None:
        _validate_check_option(
            posts,
            plan.check_option,
            table,
            ctx or scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session),
        )

    if plan.rekey:
        # New ``_id`` values must be free: not held by an unmatched row, and not
        # duplicated within this statement's own re-keyed set.
        old_ids = {_hashable_id(d["_id"]) for d in matched}
        seen: set[Any] = set()
        for old, new in zip(matched, posts, strict=True):
            new_h = _hashable_id(new["_id"])
            if new_h == _hashable_id(old["_id"]):
                continue  # PK unchanged for this row
            collides = new_h in seen or (
                new_h not in old_ids and bool(storage.find_matching(db, coll, {"_id": new["_id"]}))
            )
            if collides:
                raise errors.SQLError(
                    "23505",
                    "duplicate key value violates unique constraint "
                    f'"{table.pk_constraint_name()}"',
                    diag=_error_diag(table, n=table.pk_constraint_name()),
                )
            seen.add(new_h)
        # Delete every matched row, then insert the re-keyed rows (a PK swap needs
        # all deletes before any insert so the two rows don't collide).
        for doc in matched:
            storage.delete_matching(db, coll, {"_id": doc["_id"]})
        if posts:
            storage.insert(db, coll, posts)
    else:
        # In-place: write back only the fields the statement changed (literal sets,
        # computed sets, and any generated column the post-image recomputed).
        gen_fields = [c.field for c in table.columns if c.generated is not None]
        changed = list(other_sets) + [f for f, _, _ in plan.computed] + gen_fields
        for old, new in zip(matched, posts, strict=True):
            write_set = {f: get_path(new, f) for f in changed}
            if write_set:
                storage.update_matching(
                    db, coll, {"_id": old["_id"]}, {"$set": write_set}, multi=False
                )

    n = len(matched)
    if plan.returning is not None:
        return _returning_result(posts, plan.returning, f"UPDATE {n}", n, table, storage, db)
    return SQLResult(command_tag=f"UPDATE {n}", rowcount=n)


def _validate_update_post_images(
    plan: planner.UpdatePlan,
    storage: Any,
    db: str,
    session: Any,
    catalog: Any,
    *,
    matched: list[dict[str, Any]] | None = None,
    post_images: list[dict[str, Any]] | None = None,
) -> None:
    """Enforce NOT NULL / CHECK / UNIQUE on an UPDATE's post-image: apply the
    update in memory to each matched row and validate the result before touching
    storage, so a violating UPDATE leaves the table unchanged (Postgres
    statement-atomic). UNIQUE probes exclude every row the statement is rewriting
    (so unchanged rows and value swaps across the matched set don't self-conflict).
    The re-key path passes ``matched`` / ``post_images`` it already computed (the
    ``_id`` can't go through ``apply_update``)."""
    from secantus.sql import scalar
    from secantus.update import apply_update

    table = plan.table
    needs = (
        table.check_constraints
        or table.unique_constraints
        or table.foreign_keys
        or _has_not_null(table)
        or any(c.enum_type for c in table.columns)
        or any(c.domain_type for c in table.columns)
        or any(c.generated for c in table.columns)
        or (catalog is not None and _referencing_fks(catalog, db, table.name))
    )
    if getattr(table, "reflected", False) or not needs:
        return
    ctx = scalar.ScalarContext(storage=storage, catalog=catalog, db=db, session=session)
    if matched is None:
        matched = storage.find_matching(db, table.collection, plan.filter)
    if post_images is None:
        post_images = [apply_update(doc, plan.update) for doc in matched]
    _apply_generated_columns(post_images, table, ctx)
    _apply_expr_index_fields(post_images, table, ctx)
    for post in post_images:
        _validate_write_row(post, table, ctx)
    _validate_enum_columns(post_images, table, catalog, db)
    _validate_domain_columns(post_images, table, storage, db, session, catalog)
    _validate_unique_rows(
        post_images,
        table,
        storage,
        db,
        exclude_ids=frozenset(d["_id"] for d in matched),
        session=session,
    )
    _validate_fk_child_rows(post_images, table, storage, db, catalog, session)
    _enforce_fk_on_parent_update(matched, post_images, table, storage, db, catalog)


# --------------------------------------------------------------------------- #
# Foreign-key enforcement
# --------------------------------------------------------------------------- #


def _fk_ref_columns(fk: Any, parent: Any) -> list[str]:
    """The parent columns a FK targets — its explicit ``ref_columns`` or, when the
    reference had no column list (``REFERENCES t``), the parent's PRIMARY KEY."""
    if fk.ref_columns:
        return list(fk.ref_columns)
    return [c.name for c in parent.pk_columns] if parent is not None else []


def _parent_probe(fk: Any, parent: Any, values: list[Any]) -> dict[str, Any]:
    ref_cols = _fk_ref_columns(fk, parent)
    return {parent.field_for(rc): v for rc, v in zip(ref_cols, values, strict=False)}


def _validate_fk_child_rows(
    docs: list[dict[str, Any]], table: Any, storage: Any, db: str, catalog: Any, session: Any = None
) -> None:
    """Referential integrity on the child side: every INSERT/UPDATE row whose FK
    columns are all non-NULL must have a matching parent row (23503). MATCH SIMPLE
    — a NULL in any FK column exempts the row. A deferred FK records a pending
    re-check instead of raising."""
    if catalog is None or getattr(table, "reflected", False) or not table.foreign_keys:
        return
    for doc in docs:
        for fk in table.foreign_keys:
            fields = [table.field_for(c) for c in fk.columns]
            values = [get_path(doc, f) for f in fields]
            if any(v is None for v in values):
                continue
            parent = catalog.get(db, fk.ref_table)
            if parent is None:
                continue  # parent table not declared — can't check
            probe = _parent_probe(fk, parent, values)
            if not storage.find_matching(db, parent.collection, probe, limit=1):
                if _maybe_defer(session, "fk", table.name, fk):
                    continue
                raise errors.SQLError(
                    "23503",
                    f'insert or update on table "{table.name}" violates foreign key '
                    f'constraint "{fk.name}"',
                    diag=_error_diag(table, n=fk.name),
                )


def flush_deferred(
    session: Any, storage: Any, db: str, catalog: Any, names: set | None = None
) -> None:
    """Re-validate every constraint whose deferred violation was recorded during
    the transaction, against the current (uncommitted) state. Raises on the first
    that still fails — the caller aborts the transaction. Clears the flushed
    records; ``names`` (when given) restricts the flush to those constraint names,
    leaving the rest pending (for ``SET CONSTRAINTS <name> IMMEDIATE``)."""
    if names is None:
        pending = session.pending_deferred
        session.pending_deferred = []
    else:
        pending = [r for r in session.pending_deferred if r[2] in names]
        session.pending_deferred = [r for r in session.pending_deferred if r[2] not in names]
    for kind, table_name, cname in pending:
        table = catalog.get(db, table_name) if catalog is not None else None
        if table is None:
            continue  # table dropped inside the txn — nothing to re-check
        if kind == "unique":
            _recheck_unique(table, cname, storage, db)
        elif kind == "fk":
            _recheck_fk(table, cname, storage, db, catalog)


def _recheck_unique(table: Any, cname: str, storage: Any, db: str) -> None:
    uq = next((u for u in table.unique_constraints if u.name == cname), None)
    if uq is None:
        return
    fields = [table.field_for(c) for c in uq.columns]
    seen: set = set()
    for doc in storage.find_matching(db, table.collection, {}):
        key = tuple(get_path(doc, f) for f in fields)
        if any(v is None for v in key):
            continue
        if key in seen:
            raise _uq_violation(uq, table)
        seen.add(key)


def _recheck_fk(table: Any, cname: str, storage: Any, db: str, catalog: Any) -> None:
    fk = next((f for f in table.foreign_keys if f.name == cname), None)
    if fk is None:
        return
    parent = catalog.get(db, fk.ref_table)
    if parent is None:
        return
    fields = [table.field_for(c) for c in fk.columns]
    for doc in storage.find_matching(db, table.collection, {}):
        values = [get_path(doc, f) for f in fields]
        if any(v is None for v in values):
            continue
        probe = _parent_probe(fk, parent, values)
        if not storage.find_matching(db, parent.collection, probe, limit=1):
            raise errors.SQLError(
                "23503",
                f'insert or update on table "{table.name}" violates foreign key '
                f'constraint "{fk.name}"',
                diag=_error_diag(table, n=fk.name),
            )


def _referencing_fks(catalog: Any, db: str, parent_name: str) -> list[tuple[Any, Any]]:
    """Every ``(child_table, fk)`` whose FK targets ``parent_name`` — the reverse
    of a table's declared foreign keys, scanned across the catalog."""
    out: list[tuple[Any, Any]] = []
    for tname in catalog.list_tables(db):
        child = catalog.get(db, tname)
        if child is None:
            continue
        for fk in child.foreign_keys:
            if fk.ref_table == parent_name:
                out.append((child, fk))
    return out


def _child_match_filter(child: Any, fk: Any, parent: Any, parent_row: dict[str, Any]) -> Any:
    """A storage filter selecting the child rows that reference ``parent_row``, or
    None when the parent's referenced columns are NULL (nothing references NULL)."""
    ref_cols = _fk_ref_columns(fk, parent)
    filt: dict[str, Any] = {}
    for child_col, ref_col in zip(fk.columns, ref_cols, strict=False):
        val = get_path(parent_row, parent.field_for(ref_col))
        if val is None:
            return None
        filt[child.field_for(child_col)] = val
    return filt


def _enforce_fk_on_parent_delete(
    victims: list[dict[str, Any]], parent: Any, storage: Any, db: str, catalog: Any, _depth: int = 0
) -> None:
    """Apply referential actions when parent rows are deleted: RESTRICT / NO ACTION
    reject if any child references them (23503); CASCADE deletes the children
    (recursively); SET NULL / SET DEFAULT clears the child FK columns."""
    if catalog is None or _depth > 20:
        return
    for child, fk in _referencing_fks(catalog, db, parent.name):
        for parent_row in victims:
            filt = _child_match_filter(child, fk, parent, parent_row)
            if filt is None:
                continue
            children = storage.find_matching(db, child.collection, filt)
            if not children:
                continue
            action = (fk.on_delete or "NO ACTION").upper()
            if action in ("NO ACTION", "RESTRICT"):
                raise errors.SQLError(
                    "23503",
                    f'update or delete on table "{parent.name}" violates foreign key '
                    f'constraint "{fk.name}" on table "{child.name}"',
                    diag=_error_diag(child, n=fk.name),
                )
            if action == "CASCADE":
                _enforce_fk_on_parent_delete(children, child, storage, db, catalog, _depth + 1)
                storage.delete_matching(db, child.collection, filt)
            else:  # SET NULL / SET DEFAULT — clear the child's FK columns
                clear = {child.field_for(c): _fk_clear_value(child, c, action) for c in fk.columns}
                storage.update_matching(db, child.collection, filt, {"$set": clear}, multi=True)


def _fk_clear_value(child: Any, col_name: str, action: str) -> Any:
    if action == "SET DEFAULT":
        col = child.column(col_name)
        if col is not None and col.has_default:
            return col.default
    return None


def _enforce_fk_on_parent_update(
    matched: list[dict[str, Any]],
    post_images: list[dict[str, Any]],
    parent: Any,
    storage: Any,
    db: str,
    catalog: Any,
) -> None:
    """Apply referential actions when an UPDATE changes a parent's referenced
    columns: RESTRICT / NO ACTION reject (23503); CASCADE rewrites the children's
    FK to the new value; SET NULL / SET DEFAULT clears them. Usually a no-op —
    references target the PK (``_id``), which isn't updatable."""
    if catalog is None:
        return
    refs = _referencing_fks(catalog, db, parent.name)
    if not refs:
        return
    for child, fk in refs:
        ref_cols = _fk_ref_columns(fk, parent)
        for pre, post in zip(matched, post_images, strict=False):
            old = [get_path(pre, parent.field_for(rc)) for rc in ref_cols]
            new = [get_path(post, parent.field_for(rc)) for rc in ref_cols]
            if old == new or any(v is None for v in old):
                continue
            filt = {child.field_for(c): v for c, v in zip(fk.columns, old, strict=False)}
            if not storage.find_matching(db, child.collection, filt, limit=1):
                continue
            action = (fk.on_update or "NO ACTION").upper()
            if action in ("NO ACTION", "RESTRICT"):
                raise errors.SQLError(
                    "23503",
                    f'update or delete on table "{parent.name}" violates foreign key '
                    f'constraint "{fk.name}" on table "{child.name}"',
                    diag=_error_diag(child, n=fk.name),
                )
            if action == "CASCADE":
                new_set = {child.field_for(c): v for c, v in zip(fk.columns, new, strict=False)}
                storage.update_matching(db, child.collection, filt, {"$set": new_set}, multi=True)
            else:
                clear = {child.field_for(c): _fk_clear_value(child, c, action) for c in fk.columns}
                storage.update_matching(db, child.collection, filt, {"$set": clear}, multi=True)


@_serialized_write
def execute_delete(
    plan: planner.DeletePlan, storage: Any, db: str, catalog: Any = None, session: Any = None
) -> SQLResult:
    coll = plan.table.collection
    # RETURNING yields the deleted rows, so snapshot them before the delete. FK
    # enforcement also needs the victims, so read them whenever either applies.
    enforce_fk = (
        catalog is not None
        and not getattr(plan.table, "reflected", False)
        and bool(_referencing_fks(catalog, db, plan.table.name))
    )
    victims = (
        storage.find_matching(db, coll, plan.filter)
        if (plan.returning is not None or enforce_fk)
        else []
    )
    if enforce_fk:
        _enforce_fk_on_parent_delete(victims, plan.table, storage, db, catalog)
    n = storage.delete_matching(db, coll, plan.filter)
    if plan.returning is not None:
        return _returning_result(victims, plan.returning, f"DELETE {n}", n, plan.table, storage, db)
    return SQLResult(command_tag=f"DELETE {n}", rowcount=n)
