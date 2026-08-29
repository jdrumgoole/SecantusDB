"""Per-statement authorization for the SQL server.

The SQL surface reuses the Mongo RBAC engine (:mod:`secantus.rbac`) rather than
inventing a second role model — so a SQL client and a Mongo client on the *same*
``Storage`` are gated by the same roles (``read`` / ``readWrite`` / ``dbAdmin`` /
``dbOwner`` / ``root`` and any custom roles in the shared users/roles tables).
That closes the gap where an authenticated SQL client had broader effective
access than an equivalently-authenticated Mongo client on the same data. (#193)

Authorization is **opt-in and off by default**: it only runs when the session
was marked active (``authz_active``), which the wire server sets when it was
started with ``require_auth`` *and* explicit per-user role bindings. The
embedded ``run_sql`` API and the documented trust mode (no ``require_auth``)
leave ``authz_active`` false, so the SQL surface stays unrestricted there — no
behaviour change for existing callers.

Each statement maps to one RBAC *action* on the connection's database; the check
is a pure ``rbac.check_privilege`` call. Transaction control, ``SET`` / ``SHOW``,
cursor navigation (``FETCH`` / ``MOVE`` / ``CLOSE`` — the ``DECLARE`` that
created the cursor was already gated), and other session-only statements need no
privilege (the SQL analogue of the Mongo server's ``_NO_PRIVILEGE_COMMANDS``).
"""

from __future__ import annotations

from typing import Any

from sqlglot import exp

from secantus import rbac
from secantus.sql import errors
from secantus.sql.session import Session


def _command_tail(stmt: exp.Command) -> str:
    """The text following an ``exp.Command`` verb (e.g. the ``MATERIALIZED …``
    of a ``REFRESH MATERIALIZED VIEW``), lowercased-safe for a prefix test."""
    arg = stmt.expression
    return str(arg.name if isinstance(arg, exp.Literal) else (arg or "")).strip()


def _command_privilege(stmt: exp.Command) -> tuple[str, bool] | None:
    """RBAC ``(action, cluster)`` for a statement sqlglot parsed as a raw
    ``exp.Command``, or None when it needs no privilege."""
    verb = str(stmt.this).upper()
    if verb == "DECLARE":  # DECLARE … CURSOR runs its SELECT now.
        return (rbac.A_FIND, False)
    if verb == "REFRESH":  # REFRESH MATERIALIZED VIEW recomputes and rewrites.
        return (rbac.A_CREATE_COLLECTION, False)
    if verb in ("GRANT", "REVOKE"):
        return (rbac.A_GRANT_ROLE, False)
    tail = _command_tail(stmt).upper()
    if verb in ("CREATE", "DROP", "ALTER") and tail.startswith(("ROLE ", "USER ", "GROUP ")):
        return (rbac.A_CREATE_USER, False)
    if verb == "CREATE" and tail.startswith("MATERIALIZED"):
        return (rbac.A_CREATE_COLLECTION, False)
    if verb == "DROP" and tail.startswith("MATERIALIZED"):
        return (rbac.A_DROP_COLLECTION, False)
    if verb == "ALTER" and tail.startswith(("MATERIALIZED", "SEQUENCE", "TYPE")):
        return (rbac.A_COLL_MOD, False)
    # FETCH / MOVE / CLOSE (cursor navigation), SET CONSTRAINTS, and any other
    # session-only command carry no data privilege.
    return None


def required_privilege(stmt: exp.Expression) -> tuple[str, bool] | None:
    """The RBAC ``(action, cluster)`` ``stmt`` requires, or None if it's exempt
    (transaction control, ``SET`` / ``SHOW``, cursor navigation, session info)."""
    if isinstance(stmt, exp.Insert):
        return (rbac.A_INSERT, False)
    if isinstance(stmt, exp.Update):
        return (rbac.A_UPDATE, False)
    if isinstance(stmt, exp.Delete):
        return (rbac.A_REMOVE, False)
    if isinstance(stmt, exp.TruncateTable):  # empties a table — a write.
        return (rbac.A_REMOVE, False)
    if isinstance(stmt, exp.Merge):  # a write (readWrite grants it).
        return (rbac.A_UPDATE, False)
    if isinstance(stmt, exp.Create):
        kind = (stmt.args.get("kind") or "TABLE").upper()
        return (rbac.A_CREATE_INDEX if kind == "INDEX" else rbac.A_CREATE_COLLECTION, False)
    if isinstance(stmt, exp.Drop):
        kind = (stmt.args.get("kind") or "TABLE").upper()
        return (rbac.A_DROP_INDEX if kind == "INDEX" else rbac.A_DROP_COLLECTION, False)
    if isinstance(stmt, (exp.Alter, exp.Comment)):
        return (rbac.A_COLL_MOD, False)
    if isinstance(stmt, (exp.Grant, exp.Revoke)):
        return (rbac.A_GRANT_ROLE, False)
    if isinstance(stmt, exp.Command):
        return _command_privilege(stmt)
    if isinstance(stmt, (exp.Select, exp.SetOperation)):
        # A data-modifying CTE — ``WITH x AS (DELETE …) SELECT …`` — writes
        # despite the Select top. Require a write privilege so a read-only role
        # can't smuggle a write past the gate.
        if stmt.find(exp.Insert, exp.Update, exp.Delete, exp.Merge) is not None:
            return (rbac.A_UPDATE, False)
        # A mutating large-object scalar (``SELECT lo_unlink(oid)`` /
        # ``lo_creat`` / ``lo_create``) writes despite the SELECT top — a
        # find-only role must not run it (#836).
        for fn in stmt.find_all(exp.Anonymous):
            if str(fn.this).lower() in _MUTATING_LO_FUNCS:
                return (rbac.A_INSERT, False)
        return (rbac.A_FIND, False)
    return None


# Large-object scalar functions that mutate stored data (the ones `scalar.py`
# implements). A statement calling one needs a write grant even when its top
# node is a SELECT (#836).
_MUTATING_LO_FUNCS: frozenset[str] = frozenset({"lo_creat", "lo_create", "lo_unlink"})


# Data actions that a table-level GRANT can authorize, and the SQL privilege
# keyword each maps to. A table grant is an *additive* path: it lets a user run
# an operation their Mongo role wouldn't otherwise cover (it never restricts a
# role that already grants the action db-wide — see docs/sql.md).
_ACTION_TO_PRIVILEGE = {
    rbac.A_FIND: "SELECT",
    rbac.A_INSERT: "INSERT",
    rbac.A_UPDATE: "UPDATE",
    rbac.A_REMOVE: "DELETE",
}


def _grantee_identities(session: Session) -> set[str]:
    """Every identity a table grant can be recorded against for this session: the
    current role (``effective_user`` — the SET ROLE override or the session user),
    each of the login's role names, and the implicit ``PUBLIC`` role."""
    ids = {session.effective_user, session.user, "PUBLIC", "public"}
    for r in session.roles:
        name = r.get("role") if isinstance(r, dict) else getattr(r, "role", None)
        if name:
            ids.add(name)
    return ids


def _target_table(stmt: exp.Expression) -> str | None:
    """The single user table an INSERT/UPDATE/DELETE/simple-SELECT reads or writes,
    or None when the target isn't a single identifiable table (multi-table /
    subquery / data-modifying CTE) — those get no table-grant fallback."""
    if isinstance(stmt, exp.Insert):
        tbl = stmt.this
        if isinstance(tbl, exp.Schema):  # INSERT INTO t (cols) — table is Schema.this
            tbl = tbl.this
        return tbl.name if isinstance(tbl, exp.Table) else None
    if isinstance(stmt, (exp.Update, exp.Delete)):
        tbl = stmt.this if isinstance(stmt, exp.Delete) else stmt.args.get("this")
        return tbl.name if isinstance(tbl, exp.Table) else None
    if isinstance(stmt, exp.Select):
        if stmt.args.get("joins") or stmt.find(exp.Subquery) is not None:
            return None
        # The pg dialect exposes the FROM under the ``from_`` arg (falling back to
        # ``from`` on parses that use the older key).
        frm = stmt.args.get("from_") or stmt.args.get("from")
        tbl = frm.this if frm is not None else None
        return tbl.name if isinstance(tbl, exp.Table) else None
    return None


def _touched_columns(stmt: exp.Expression, table: str, catalog: Any, db: str) -> set[str] | None:
    """The columns ``stmt`` reads / writes on its single target table — for
    column-grant enforcement. None means "can't tell" (a ``count(*)`` with no
    column refs, an unresolvable shape) so the caller falls back to table-level.
    A ``SELECT *`` / ``INSERT`` with no column list expands to the table's columns."""

    def _all_columns() -> set[str] | None:
        tdef = catalog.get(db, table) if catalog is not None else None
        return {c.name for c in tdef.columns} if tdef is not None else None

    if isinstance(stmt, exp.Insert):
        target = stmt.this
        if isinstance(target, exp.Schema) and target.expressions:  # INSERT INTO t (cols)
            return {c.name for c in target.expressions if isinstance(c, exp.Column)} or {
                str(getattr(c, "name", c)) for c in target.expressions
            }
        return _all_columns()  # no column list — every column
    if isinstance(stmt, exp.Update):
        cols = set()
        for setter in stmt.args.get("expressions") or []:
            tgt = setter.this if isinstance(setter, exp.EQ) else None
            if isinstance(tgt, exp.Column):
                cols.add(tgt.name)
        return cols or None
    if isinstance(stmt, exp.Select):
        if any(isinstance(e, exp.Star) for e in stmt.expressions):
            return _all_columns()
        cols = {c.name for c in stmt.find_all(exp.Column) if not c.table or c.table == table}
        return cols or None
    return None


def _primary_write_target(stmt: exp.Expression) -> str | None:
    """The single table a write statement mutates (the one its write privilege
    covers), even when the statement also reads other tables. Unlike
    ``_target_table`` this does *not* bail on multi-table statements — it is used
    only to exclude the write target from the set of tables that need a *read*
    grant. ``CREATE TABLE ... AS`` counts its new table as the write target."""
    if isinstance(stmt, exp.Insert):
        tbl = stmt.this
        if isinstance(tbl, exp.Schema):
            tbl = tbl.this
        return tbl.name if isinstance(tbl, exp.Table) else None
    if isinstance(stmt, exp.Update):
        tbl = stmt.args.get("this")
        return tbl.name if isinstance(tbl, exp.Table) else None
    if isinstance(stmt, exp.Delete):
        tbl = stmt.this
        return tbl.name if isinstance(tbl, exp.Table) else None
    if isinstance(stmt, exp.Create):
        tbl = stmt.this
        if isinstance(tbl, exp.Schema):
            tbl = tbl.this
        return tbl.name if isinstance(tbl, exp.Table) else None
    return None


def _source_read_tables(stmt: exp.Expression) -> set[str]:
    """Base tables ``stmt`` READS as a source, needing a ``find`` (SELECT) grant
    beyond the primary write privilege: the ``SELECT`` behind ``INSERT ... SELECT``
    and ``CREATE TABLE ... AS SELECT``, the ``FROM`` of ``UPDATE ... FROM``, the
    ``USING`` of ``DELETE ... USING``, and any subquery. Without this a principal
    holding only a write grant on the target could exfiltrate an unrelated table's
    rows through the source clause (issues #785, #881).

    CTE names are excluded (they're query-local, not base tables). The primary
    write target is excluded — its own privilege is checked separately; a
    self-referential ``INSERT INTO a SELECT * FROM a`` therefore isn't charged a
    read grant, an accepted narrowing (the actor already writes ``a``, so no
    *other* table leaks)."""
    tables = {t.name for t in stmt.find_all(exp.Table)}
    ctes = {c.alias_or_name for c in stmt.find_all(exp.CTE)}
    target = _primary_write_target(stmt)
    return {t for t in tables - ctes if t and t != target}


def _table_grant_allows(stmt: exp.Expression, action: str, session: Session, catalog: Any) -> bool:
    """Whether recorded grants cover ``action`` on ``stmt``'s target table for this
    session — a whole-table grant, or (finer) a column grant on *every* column the
    statement touches (the additive GRANT/REVOKE enforcement path, #127 + #131)."""
    privilege = _ACTION_TO_PRIVILEGE.get(action)
    if privilege is None or catalog is None:
        return False
    table = _target_table(stmt)
    if table is None:
        return False
    grantees = _grantee_identities(session)
    if catalog.has_table_privilege(session.database, table, grantees, privilege):
        return True
    # Column-level: allow only when every touched column is granted (DELETE has no
    # column granularity, so it never reaches here with a column grant).
    if not hasattr(catalog, "has_column_privilege"):
        return False
    columns = _touched_columns(stmt, table, catalog, session.database)
    if not columns:
        return False
    return all(
        catalog.has_column_privilege(session.database, table, grantees, col, privilege)
        for col in columns
    )


def authorize(stmt: exp.Expression, session: Session, storage: Any, catalog: Any = None) -> None:
    """Raise ``42501 insufficient_privilege`` if ``session``'s roles don't grant
    what ``stmt`` needs. No-op unless authorization is active on the session."""
    if not session.authz_active:
        return
    need = required_privilege(stmt)
    if need is None:
        return
    action, cluster = need
    # Custom (non-built-in) roles resolve through Storage.get_role; a mock store
    # without it still authorizes the built-in roles.
    resolver = getattr(storage, "get_role", None)
    if rbac.check_privilege(
        session.roles,
        action,
        target_db=None if cluster else session.database,
        cluster=cluster,
        role_resolver=resolver,
    ):
        return
    # The Mongo role doesn't cover it — a table-level GRANT still might.
    if not cluster and _table_grant_allows(stmt, action, session, catalog):
        return
    raise errors.insufficient_privilege(session.database, action)


def _find_grant_allows(table: str, session: Session, catalog: Any, resolver: Any) -> bool:
    """Whether ``session`` may read ``table``: a db-wide ``find`` role, or a
    table-level SELECT grant."""
    if rbac.check_privilege(
        session.roles,
        rbac.A_FIND,
        target_db=session.database,
        role_resolver=resolver,
    ):
        return True
    if catalog is None:
        return False
    grantees = _grantee_identities(session)
    return bool(catalog.has_table_privilege(session.database, table, grantees, "SELECT"))


def authorize_source_reads(
    stmt: exp.Expression, session: Session, storage: Any, catalog: Any = None
) -> None:
    """Enforce a ``find`` (SELECT) grant on every table a write statement reads
    as a *source* — the ``SELECT`` behind ``INSERT ... SELECT`` /
    ``CREATE TABLE ... AS``, the ``FROM`` of ``UPDATE ... FROM``, the ``USING``
    of ``DELETE ... USING``, and subqueries. The primary write privilege is
    checked by :func:`authorize`; this closes the secondary-read bypass (#785,
    #881) where a write-only grant leaked an unrelated table's rows through the
    source clause. No-op unless authorization is active."""
    if not session.authz_active:
        return
    # Only WRITE statements have a "source read beyond the write target". A
    # plain SELECT's own read authorization is handled by `authorize` (which
    # honours table- and column-level SELECT grants); running the whole-table
    # source gate over it would wrongly reject a column-granted read.
    if not isinstance(stmt, (exp.Insert, exp.Update, exp.Delete, exp.Merge, exp.Create)):
        return
    if isinstance(stmt, exp.Create) and stmt.args.get("expression") is None:
        return  # a plain CREATE TABLE with no AS-SELECT source reads nothing
    resolver = getattr(storage, "get_role", None)
    for table in sorted(_source_read_tables(stmt)):
        if not _find_grant_allows(table, session, catalog, resolver):
            raise errors.insufficient_privilege(session.database, rbac.A_FIND)
