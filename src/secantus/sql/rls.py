"""Row-level security (RLS) enforcement (#129).

``ALTER TABLE t ENABLE ROW LEVEL SECURITY`` + ``CREATE POLICY`` restrict which
rows a role can see or write. A policy carries a ``USING`` predicate (the rows a
command may *read* / target) and/or a ``WITH CHECK`` predicate (the rows a write
may *add*). This module builds those predicates for the current session and:

* injects the combined ``USING`` predicate into a SELECT / UPDATE / DELETE
  ``WHERE`` (so only permitted rows are returned / affected), and
* validates the combined ``WITH CHECK`` predicate against each row an INSERT /
  UPDATE would write (raising ``42501`` on violation).

Permissive policies are OR'd, restrictive policies are AND'd, and identity
functions (``current_user`` / ``session_user`` / …) are substituted with the
session's identities before the predicate is evaluated by the ordinary filter /
scalar machinery. Like the rest of the SQL RBAC surface, enforcement is gated on
``session.authz_active`` and a superuser (``root``) bypasses it — trust mode and
the embedded API record policies but don't enforce them.
"""

from __future__ import annotations

from typing import Any

import sqlglot
from sqlglot import exp

# The four row-affecting commands a policy can scope (``ALL`` covers every one).
SELECT, INSERT, UPDATE, DELETE = "SELECT", "INSERT", "UPDATE", "DELETE"


def _is_superuser(session: Any) -> bool:
    for r in getattr(session, "roles", []) or []:
        name = r.get("role") if isinstance(r, dict) else getattr(r, "role", None)
        if name == "root":
            return True
    return False


def enforced(session: Any) -> bool:
    """Whether RLS should bite for this session (active authorization, non-root)."""
    return bool(getattr(session, "authz_active", False)) and not _is_superuser(session)


def _grantees(session: Any) -> set[str]:
    ids = {session.effective_user, session.user, "public", "PUBLIC"}
    for r in getattr(session, "roles", []) or []:
        name = r.get("role") if isinstance(r, dict) else getattr(r, "role", None)
        if name:
            ids.add(name)
    return ids


def _applies(policy: dict[str, Any], command: str, grantees: set[str]) -> bool:
    cmd = str(policy.get("command") or "ALL").upper()
    if cmd not in ("ALL", command):
        return False
    roles = policy.get("roles") or ["public"]
    if any(str(r).lower() == "public" for r in roles):
        return True
    return bool(set(roles) & grantees)


def _true() -> exp.Expression:
    return exp.true()


def _false() -> exp.Expression:
    return exp.false()


def _parse(text: str | None) -> exp.Expression | None:
    if not text:
        return None
    return sqlglot.parse_one(text, dialect="postgres")


def _substitute_identity(node: exp.Expression, session: Any) -> exp.Expression:
    """Replace ``current_user`` / ``session_user`` / synonyms in ``node`` with the
    session's identity as a string literal, so the predicate needs no live
    function evaluation."""
    eff, sess_user = session.effective_user, session.user

    def _lit(v: str) -> exp.Expression:
        return exp.Literal(this=v, is_string=True)

    def _repl(n: exp.Expression) -> exp.Expression:
        if isinstance(n, exp.CurrentUser):
            return _lit(eff)
        if getattr(exp, "SessionUser", None) is not None and isinstance(n, exp.SessionUser):
            return _lit(sess_user)
        if isinstance(n, exp.Column) and not n.table:
            low = n.name.lower()
            if low in ("current_user", "current_role", "user"):
                return _lit(eff)
            if low == "session_user":
                return _lit(sess_user)
        return n

    return node.transform(_repl)


def _combine(policies: list[dict[str, Any]], kind: str, session: Any) -> exp.Expression:
    """Combine ``policies`` on their ``kind`` (``"using"`` / ``"check"``) predicate:
    permissive OR'd, restrictive AND'd, then AND'd together. No applicable
    permissive policy → default-deny (FALSE)."""
    perms = [p for p in policies if p.get("permissive", True)]
    restr = [p for p in policies if not p.get("permissive", True)]
    if not perms:
        return _false()  # RLS on, nothing permits the row

    def _pred(p: dict[str, Any]) -> exp.Expression:
        # WITH CHECK falls back to USING when absent (Postgres semantics); a policy
        # with no predicate of this kind permits everything.
        text = p.get(kind) or (p.get("using") if kind == "check" else None)
        parsed = _parse(text)
        return _substitute_identity(parsed, session) if parsed is not None else _true()

    def _or(exprs: list[exp.Expression]) -> exp.Expression:
        acc = exprs[0]
        for e in exprs[1:]:
            acc = exp.Or(this=acc, expression=e)
        return acc

    def _and(exprs: list[exp.Expression]) -> exp.Expression:
        acc = exprs[0]
        for e in exprs[1:]:
            acc = exp.And(this=acc, expression=e)
        return acc

    pred = _or([_pred(p) for p in perms])
    if restr:
        pred = _and([pred, _and([_pred(p) for p in restr])])
    return pred


def _applicable(catalog: Any, db: str, table: str, command: str, session: Any) -> list[dict] | None:
    """The policies on ``table`` that apply to ``command`` for this session, or None
    when RLS isn't enabled / enforced (so the caller leaves the query untouched)."""
    if not enforced(session):
        return None
    getr = getattr(catalog, "get_rls", None)
    if getr is None or not getr(db, table).get("enabled"):
        return None
    grantees = _grantees(session)
    return [p for p in catalog.get_policies(db, table) if _applies(p, command, grantees)]


def read_predicate(catalog: Any, db: str, table: str, command: str, session: Any):
    """The ``USING`` predicate (identity-substituted) to AND into a SELECT / UPDATE
    / DELETE ``WHERE`` for ``table``, or None when RLS doesn't apply."""
    applicable = _applicable(catalog, db, table, command, session)
    if applicable is None:
        return None
    return _combine(applicable, "using", session)


def write_predicate(catalog: Any, db: str, table: str, command: str, session: Any):
    """The ``WITH CHECK`` predicate (identity-substituted) each new row of an INSERT
    / UPDATE must satisfy on ``table``, or None when RLS doesn't apply."""
    applicable = _applicable(catalog, db, table, command, session)
    if applicable is None:
        return None
    return _combine(applicable, "check", session)


def apply_read(stmt: exp.Expression, table: str, catalog: Any, db: str, session: Any) -> None:
    """Inject the RLS ``USING`` predicate into ``stmt``'s WHERE in place, for a
    single-table SELECT / UPDATE / DELETE. No-op when RLS doesn't apply."""
    if isinstance(stmt, exp.Select):
        command = SELECT
    elif isinstance(stmt, exp.Update):
        command = UPDATE
    elif isinstance(stmt, exp.Delete):
        command = DELETE
    else:
        return
    pred = read_predicate(catalog, db, table, command, session)
    if pred is not None:
        stmt.where(pred, copy=False)


def check_write_row(doc: dict[str, Any], table: Any, command: str, ctx: Any) -> None:
    """Validate the RLS ``WITH CHECK`` predicate against ``doc`` (a to-be-written
    row, keyed by storage field). Raises ``42501`` on violation. No-op when RLS
    doesn't apply to this session / table."""
    catalog, db, session = ctx.catalog, ctx.db, ctx.session
    pred = write_predicate(catalog, db, table.name, command, session)
    if pred is None:
        return
    from secantus.paths import get_path
    from secantus.sql import errors, scalar

    def scope(node: exp.Column) -> Any:
        col = table.column(node.name)
        return get_path(doc, col.field if col is not None else node.name)

    value = scalar.evaluate(pred, scope, ctx)
    if not scalar._truthy(value):
        raise errors.SQLError(
            "42501",
            f'new row violates row-level security policy for table "{table.name}"',
        )
