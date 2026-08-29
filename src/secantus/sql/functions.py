"""Scalar session/info functions for FROM-less SELECT.

Covers the niladic and simple functions Postgres clients fire to introspect the
session — ``version()``, ``current_database()``, ``current_schema()``,
``current_user``, ``current_setting(name)``, ``set_config(name, value, local)``,
``pg_backend_pid()`` — resolved against the connection ``Session``. Returns
``(output_column_name, value, type_tag)``; unknown functions raise
``feature_not_supported``.
"""

from __future__ import annotations

from typing import Any

from sqlglot import exp

from secantus.sql import errors
from secantus.sql.session import VERSION_STRING, Session


def _arg_literals(node: exp.Anonymous) -> list[Any]:
    out: list[Any] = []
    for e in node.expressions:
        if isinstance(e, exp.Literal):
            out.append(e.this if e.is_string else _num(e.this))
        elif isinstance(e, exp.Boolean):
            out.append(bool(e.this))
        elif isinstance(e, exp.Null):
            out.append(None)
        else:
            raise errors.feature_not_supported(f"unsupported function argument: {e.sql()}")
    return out


def _num(text: str) -> Any:
    return float(text) if ("." in text or "e" in text.lower()) else int(text)


def terminate_backend(args: list, session: Session, *, cancel: bool = False) -> bool:
    """``pg_terminate_backend`` (close the target's connection) and
    ``pg_cancel_backend`` (fire the target's cancel_event, observed at
    cancellation points like pg_sleep — the connection stays up, like real
    PG). False when the pid isn't live."""
    pid = int(args[0]) if args and args[0] is not None else -1
    registry = getattr(session, "activity_registry", None)
    target = None
    if registry is not None:
        target = next((s for s in registry.snapshot() if s.backend_pid == pid), None)
    if target is None:
        return False
    if cancel:
        event = getattr(target, "cancel_event", None)
        if event is None:
            return False
        event.set()
        return True
    terminate = getattr(target, "terminate_cb", None)
    if terminate is not None:
        terminate()
        return True
    return False


def evaluate_scalar_by_name(name: str, args: list, session: Session) -> Any:
    """Session-function dispatch by bare name + evaluated args — the scalar
    evaluator's escape hatch for calls that appear in non-constant contexts."""
    if name in ("pg_terminate_backend", "pg_cancel_backend"):
        return terminate_backend(args, session, cancel=(name == "pg_cancel_backend"))
    if name == "pg_backend_pid":
        return session.backend_pid
    if name == "pg_sleep":
        # Per-row pg_sleep (``select pg_sleep(0.01) from generate_series(…)``)
        # — same cancellation-point semantics as the FROM-less form.
        return _evaluate_named("pg_sleep", args, session)[1]
    raise errors.feature_not_supported(f"function {name}() is not supported in this context")


def evaluate_scalar(node: exp.Expression, session: Session) -> tuple[str, Any, str]:
    if isinstance(node, exp.Paren):
        return evaluate_scalar(node.this, session)
    if isinstance(node, exp.Dot):
        # A schema-qualified call like ``pg_catalog.version()`` — evaluate the
        # rightmost call and ignore the catalog qualifier.
        return evaluate_scalar(node.expression, session)
    if isinstance(node, exp.CurrentVersion):
        return ("version", VERSION_STRING, "text")
    if isinstance(node, exp.CurrentDatabase):
        return ("current_database", session.database, "text")
    if isinstance(node, exp.CurrentSchema):
        return ("current_schema", session.current_schema, "text")
    if isinstance(node, exp.CurrentUser):
        return ("current_user", session.effective_user, "text")
    if getattr(exp, "SessionUser", None) is not None and isinstance(node, exp.SessionUser):
        return ("session_user", session.user, "text")
    if isinstance(node, exp.Column) and not node.table:
        # ``current_role`` / ``user`` / ``session_user`` are keyword synonyms that
        # sqlglot leaves as bare column references in a FROM-less SELECT.
        low = node.name.lower()
        if low in ("current_user", "current_role", "user"):
            return (low, session.effective_user, "text")
        if low == "session_user":
            return (low, session.user, "text")

    if isinstance(node, exp.Anonymous):
        name = (node.this if isinstance(node.this, str) else node.name).lower()
        # Drop a pg_catalog. qualifier if present.
        name = name.rsplit(".", 1)[-1]
        args = _arg_literals(node)
        return _evaluate_named(name, args, session)

    raise errors.feature_not_supported(f"unsupported expression in SELECT: {node.sql()}")


def _evaluate_named(name: str, args: list[Any], session: Session) -> tuple[str, Any, str]:
    if name == "version":
        return ("version", VERSION_STRING, "text")
    if name in ("current_database", "current_catalog"):
        return (name, session.database, "text")
    if name == "current_schema":
        return ("current_schema", session.current_schema, "text")
    if name == "session_user":
        # The login identity — changed only by SET SESSION AUTHORIZATION.
        return (name, session.user, "text")
    if name in ("current_user", "current_role", "user"):
        # The current role — the SET ROLE override, else the session user.
        return (name, session.effective_user, "text")
    if name == "current_setting":
        if not args:
            raise errors.syntax_error("current_setting() requires a setting name")
        return ("current_setting", session.get_setting(str(args[0])), "text")
    if name == "set_config":
        if len(args) < 2:
            raise errors.syntax_error("set_config() requires (name, value, is_local)")
        from secantus.sql.session import (
            REPORTABLE_GUCS,
            canonical_client_encoding,
            canonical_guc_name,
        )

        guc = canonical_guc_name(str(args[0]))
        value = "" if args[1] is None else str(args[1])
        if guc == "client_encoding":
            value = canonical_client_encoding(value) or value
        session.settings[guc] = value
        if guc in REPORTABLE_GUCS:
            session.pending_parameter_status.append((guc, session.settings[guc]))
        return ("set_config", session.settings[guc], "text")
    if name == "pg_backend_pid":
        return ("pg_backend_pid", session.backend_pid, "int4")
    if name == "pg_sleep":
        # PG returns void after sleeping. The sleep is a cancellation point:
        # it waits on the session's cancel_event (set by a wire CancelRequest
        # or pg_cancel_backend) and raises PG's 57014 when cancelled. Still
        # capped so an embedded session with no cancel path can't pin a
        # thread forever.
        # str() first: the per-row scalar path hands numerics over as
        # Decimal128, which float() rejects directly.
        seconds = float(str(args[0])) if args and args[0] is not None else 0.0
        sleep_for = max(0.0, min(seconds, 30.0))
        deadline = session.statement_deadline
        if deadline is not None:
            import time as _time

            remaining = deadline - _time.monotonic()
            if remaining <= 0:
                raise errors.SQLError("57014", "canceling statement due to statement timeout")
            if sleep_for > remaining:
                # statement_timeout fires partway through this sleep.
                if session.cancel_event.wait(remaining):
                    session.cancel_event.clear()
                    raise errors.SQLError("57014", "canceling statement due to user request")
                raise errors.SQLError("57014", "canceling statement due to statement timeout")
        if session.cancel_event.wait(sleep_for):
            session.cancel_event.clear()
            raise errors.SQLError("57014", "canceling statement due to user request")
        # PG types pg_sleep as void (oid 2278, typlen 4), value NULL on the wire.
        return ("pg_sleep", None, "void")
    if name in ("pg_is_in_recovery",):
        return (name, False, "bool")
    if name in ("pg_terminate_backend", "pg_cancel_backend"):
        killed = terminate_backend(args, session, cancel=(name == "pg_cancel_backend"))
        return (name, killed, "bool")
    if name in ("jsonb_build_object", "json_build_object"):
        out: dict[str, Any] = {}
        for i in range(0, len(args) - 1, 2):
            out[str(args[i])] = args[i + 1]
        return (name, out, "json")
    if name in ("jsonb_build_array", "json_build_array"):
        return (name, list(args), "json")
    raise errors.feature_not_supported(f"function {name}() is not supported")


# Anonymous calls that the full scalar evaluator (``scalar._call_func``) handles,
# so a FROM-less ``SELECT <fn>(...)`` must defer to it rather than the session path.
_SCALAR_EVAL_ANON = frozenset(
    {
        "isempty",
        # ``row(...)`` builds an anonymous record value.
        "row",
        # to_regtype needs the catalog (user-declared enum/composite oids).
        "to_regtype",
        # Array introspection (multi-dimensional aware) is done by the full scalar
        # evaluator, which can evaluate an ``ARRAY[...]`` / nested-array argument.
        "cardinality",
        "array_length",
        "array_ndims",
        "array_dims",
        "array_upper",
        "array_lower",
        "to_jsonb",
        "to_json",
        "row_to_json",
        "range_merge",
        "to_tsvector",
        "to_tsquery",
        "plainto_tsquery",
        "phraseto_tsquery",
        "websearch_to_tsquery",
        "ts_rank",
        "ts_rank_cd",
        "ts_headline",
        "masklen",
        "network",
        "netmask",
        "broadcast",
        "family",
        "abbrev",
        "hostmask",
        "set_bit",
        "get_bit",
        "bit_length",
        "octet_length",
        "get_byte",
        "set_byte",
        "hstore",
        "akeys",
        "avals",
        "hstore_to_json",
        "hstore_to_jsonb",
        "defined",
        "xmlforest",
        "xpath",
        "xml_is_well_formed",
        "xml_is_well_formed_document",
        "xmlconcat",
        "pg_notify",
        "make_interval",
        "justify_days",
        "justify_hours",
        "justify_interval",
        "age",
        "uuid_generate_v4",
        "uuid_generate_v1",
        "has_table_privilege",
        "has_column_privilege",
        "pg_get_functiondef",
        "pg_get_function_arguments",
        "pg_get_function_result",
        "pg_get_indexdef",
        # Advisory locks (#135) — session-tracked single-node no-op locking.
        "pg_advisory_lock",
        "pg_advisory_lock_shared",
        "pg_advisory_xact_lock",
        "pg_advisory_xact_lock_shared",
        "pg_try_advisory_lock",
        "pg_try_advisory_lock_shared",
        "pg_try_advisory_xact_lock",
        "pg_try_advisory_xact_lock_shared",
        "pg_advisory_unlock",
        "pg_advisory_unlock_shared",
        "pg_advisory_unlock_all",
    }
)


def is_scalar_function(node: exp.Expression) -> bool:
    """Whether ``plan_constant_select`` should evaluate ``node`` as a function."""
    if isinstance(node, exp.Dot):
        return is_scalar_function(node.expression)
    if isinstance(node, exp.Anonymous):
        # Range constructors (``int4range(1,5)`` …) and range predicates like
        # ``isempty(...)`` are handled by the full scalar evaluator, not the
        # session/info-function path — let them fall through.
        from secantus.sql import typemap

        name = str(node.this).lower()
        # Range constructors / isempty and the jsonb builders (to_jsonb / to_json /
        # row_to_json) are implemented by the full scalar evaluator, not here.
        if (
            name in typemap._RANGE_TAGS
            or name in typemap._MULTIRANGE_TAGS
            or name in _SCALAR_EVAL_ANON
        ):
            return False
    # ``session_user`` (exp.SessionUser) and the keyword synonyms ``current_role`` /
    # ``user`` (bare columns in a FROM-less SELECT) resolve to the session identity.
    if getattr(exp, "SessionUser", None) is not None and isinstance(node, exp.SessionUser):
        return True
    if isinstance(node, exp.Column) and not node.table:
        return node.name.lower() in ("current_user", "current_role", "user", "session_user")
    return isinstance(
        node,
        exp.CurrentVersion
        | exp.CurrentDatabase
        | exp.CurrentSchema
        | exp.CurrentUser
        | exp.Anonymous,
    )
