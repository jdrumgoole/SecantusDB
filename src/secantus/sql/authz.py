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
        return (rbac.A_FIND, False)
    return None


def authorize(stmt: exp.Expression, session: Session, storage: Any) -> None:
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
    if not rbac.check_privilege(
        session.roles,
        action,
        target_db=None if cluster else session.database,
        cluster=cluster,
        role_resolver=resolver,
    ):
        raise errors.insufficient_privilege(session.database, action)
